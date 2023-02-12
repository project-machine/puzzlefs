extern crate serde_cbor;
extern crate xattr;

use std::backtrace::Backtrace;
use std::convert::TryInto;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::io::{Read, Seek};
use std::mem;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::Path;
use std::vec::Vec;

use nix::sys::stat;
use serde::de::Error as SerdeError;
use serde::de::Visitor;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use compression::Decompressor;

use crate::error::{Result, WireFormatError};

mod cbor_helpers;
use cbor_helpers::cbor_get_array_size;
pub use cbor_helpers::cbor_size_of_list_header;
// To get off the ground here, we just use serde and cbor for most things, except for the fixed
// size Inode which depends being a fixed size (and cbor won't generate it that way) in the later
// format.

/*
 *
 * TODO: use these wrappers like the spec says

#[derive(Serialize, Deserialize)]
enum BlobType {
    Root,
    Metadata,
    File,
}

#[derive(Serialize, Deserialize)]
struct Blob {
    kind: BlobType,
}
*/

fn read_one<'a, T: Deserialize<'a>, R: Read>(r: R) -> Result<T> {
    // serde complains when we leave extra bytes on the wire, which we often want to do. as a
    // hack, we create a streaming deserializer for the type we're about to read, and then only
    // read one value.
    let mut iter = serde_cbor::Deserializer::from_reader(r).into_iter::<T>();
    let v = iter.next().transpose()?;
    v.ok_or_else(|| WireFormatError::ValueMissing(Backtrace::capture()))
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Rootfs {
    pub metadatas: Vec<BlobRef>,
}

impl Rootfs {
    pub fn open<R: Read>(f: R) -> Result<Rootfs> {
        read_one(f)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlobRefKind {
    Local,
    Other { digest: [u8; 32] },
}

const BLOB_REF_SIZE: usize = 1 /* mode */ + 32 /* digest */ + 8 /* offset */;

// TODO: should this be an ociv1 digest and include size and media type?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlobRef {
    pub offset: u64,
    pub kind: BlobRefKind,
}

impl BlobRef {
    fn fixed_length_serialize(&self, state: &mut [u8; BLOB_REF_SIZE]) {
        state[0..8].copy_from_slice(&self.offset.to_le_bytes());
        match self.kind {
            BlobRefKind::Local => state[8] = 0,
            BlobRefKind::Other { ref digest } => {
                state[8] = 1;
                state[9..41].copy_from_slice(digest);
            }
        };
    }

    fn fixed_length_deserialize<E: SerdeError>(
        state: &[u8; BLOB_REF_SIZE],
    ) -> std::result::Result<BlobRef, E> {
        let offset = u64::from_le_bytes(state[0..8].try_into().unwrap());

        let kind = match state[8] {
            0 => BlobRefKind::Local,
            1 => BlobRefKind::Other {
                digest: state[9..41].try_into().unwrap(),
            },
            _ => {
                return Err(SerdeError::custom(format!(
                    "bad blob ref kind {}",
                    state[0]
                )))
            }
        };

        Ok(BlobRef { offset, kind })
    }
}

impl Serialize for BlobRef {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state: [u8; BLOB_REF_SIZE] = [0; BLOB_REF_SIZE];
        self.fixed_length_serialize(&mut state);
        serializer.serialize_bytes(&state)
    }
}

impl<'de> Deserialize<'de> for BlobRef {
    fn deserialize<D>(deserializer: D) -> std::result::Result<BlobRef, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct BlobRefVisitor;

        impl<'de> Visitor<'de> for BlobRefVisitor {
            type Value = BlobRef;

            fn expecting(&self, formatter: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                formatter.write_fmt(format_args!("expected {BLOB_REF_SIZE} bytes for BlobRef"))
            }

            fn visit_bytes<E>(self, v: &[u8]) -> std::result::Result<BlobRef, E>
            where
                E: SerdeError,
            {
                let state: [u8; BLOB_REF_SIZE] = v
                    .try_into()
                    .map_err(|_| SerdeError::invalid_length(v.len(), &self))?;
                BlobRef::fixed_length_deserialize(&state)
            }
        }

        deserializer.deserialize_bytes(BlobRefVisitor)
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Metadata {
    pub inodes: Vec<Inode>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct DirEnt {
    pub ino: Ino,
    pub name: OsString,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct DirList {
    // TODO: flags instead?
    pub look_below: bool,
    pub entries: Vec<DirEnt>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct FileChunkList {
    pub chunks: Vec<FileChunk>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct FileChunk {
    pub blob: BlobRef,
    pub len: u64,
}

const INODE_MODE_SIZE: usize = 1 /* mode */ + mem::size_of::<u64>() * 2 /* major/minor/offset */;

// InodeMode needs to have custom serialization because inodes must be a fixed size.
#[derive(Debug, PartialEq, Eq)]
pub enum InodeMode {
    Unknown,
    Fifo,
    Chr { major: u64, minor: u64 },
    Dir { offset: u64 },
    Blk { major: u64, minor: u64 },
    Reg { offset: u64 },
    Lnk,
    Sock,
    Wht,
}

pub type Ino = u64;

const INODE_SIZE: usize = mem::size_of::<Ino>() + INODE_MODE_SIZE + 2 * mem::size_of::<u32>() /* uid and gid */
+ mem::size_of::<u16>() /* permissions */ + 1 /* Option<BlobRef> */ + BLOB_REF_SIZE;

pub const INODE_WIRE_SIZE: usize = cbor_size_of_list_header(INODE_SIZE) + INODE_SIZE;

pub const DEFAULT_FILE_PERMISSIONS: u16 = 0o644;
pub const DEFAULT_DIRECTORY_PERMISSIONS: u16 = 0o755;

#[derive(Debug, PartialEq, Eq)]
pub struct Inode {
    pub ino: Ino,
    pub mode: InodeMode,
    pub uid: u32,
    pub gid: u32,
    pub permissions: u16,
    pub additional: Option<BlobRef>,
}

impl Serialize for Inode {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state: [u8; INODE_SIZE] = [0; INODE_SIZE];
        state[0..8].copy_from_slice(&self.ino.to_le_bytes());

        // TODO: could do this better with mem::discriminant(), but it is complex :). constants
        // from dirent.h, and rust doesn't like us mixing those with struct-variant enums anyway...
        match self.mode {
            InodeMode::Unknown => state[8] = 0,
            InodeMode::Fifo => state[8] = 1,
            InodeMode::Chr { major, minor } => {
                state[8] = 2;
                state[9..17].copy_from_slice(&major.to_le_bytes());
                state[17..25].copy_from_slice(&minor.to_le_bytes());
            }
            InodeMode::Dir { offset } => {
                state[8] = 4;
                state[9..17].copy_from_slice(&offset.to_le_bytes());
            }
            InodeMode::Blk { major, minor } => {
                state[8] = 6;
                state[9..17].copy_from_slice(&major.to_le_bytes());
                state[17..25].copy_from_slice(&minor.to_le_bytes());
            }
            InodeMode::Reg { offset } => {
                state[8] = 8;
                state[9..17].copy_from_slice(&offset.to_le_bytes());
            }
            InodeMode::Lnk => state[8] = 10,
            InodeMode::Sock => state[8] = 12,
            InodeMode::Wht => state[8] = 14,
        }

        state[25..29].copy_from_slice(&self.uid.to_le_bytes());
        state[29..33].copy_from_slice(&self.gid.to_le_bytes());
        state[33..35].copy_from_slice(&self.permissions.to_le_bytes());
        if let Some(additional) = self.additional {
            state[35] = 1;
            additional
                .fixed_length_serialize((&mut state[36..36 + BLOB_REF_SIZE]).try_into().unwrap());
        } else {
            state[35] = 0;
        }
        serializer.serialize_bytes(&state)
    }
}

impl<'de> Deserialize<'de> for Inode {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Inode, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct InodeVisitor;

        impl<'de> Visitor<'de> for InodeVisitor {
            type Value = Inode;

            fn expecting(&self, formatter: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                formatter.write_fmt(format_args!("expected {INODE_MODE_SIZE} bytes for Inode"))
            }

            fn visit_bytes<E>(self, v: &[u8]) -> std::result::Result<Inode, E>
            where
                E: SerdeError,
            {
                let state: [u8; INODE_SIZE] = v
                    .try_into()
                    .map_err(|_| SerdeError::invalid_length(v.len(), &self))?;

                let mode = match state[8] {
                    0 => InodeMode::Unknown,
                    1 => InodeMode::Fifo,
                    2 => {
                        let major = u64::from_le_bytes(state[9..17].try_into().unwrap());
                        let minor = u64::from_le_bytes(state[17..25].try_into().unwrap());
                        InodeMode::Chr { major, minor }
                    }
                    4 => {
                        let offset = u64::from_le_bytes(state[9..17].try_into().unwrap());
                        InodeMode::Dir { offset }
                    }
                    6 => {
                        let major = u64::from_le_bytes(state[9..17].try_into().unwrap());
                        let minor = u64::from_le_bytes(state[17..25].try_into().unwrap());
                        InodeMode::Blk { major, minor }
                    }
                    8 => {
                        let offset = u64::from_le_bytes(state[9..17].try_into().unwrap());
                        InodeMode::Reg { offset }
                    }
                    10 => InodeMode::Lnk,
                    12 => InodeMode::Sock,
                    14 => InodeMode::Wht,
                    _ => {
                        return Err(SerdeError::custom(format!(
                            "bad inode mode value {}",
                            state[8]
                        )))
                    }
                };

                let additional = if state[35] > 0 {
                    Some(BlobRef::fixed_length_deserialize(
                        state[36..36 + BLOB_REF_SIZE].try_into().unwrap(),
                    )?)
                } else {
                    None
                };

                Ok(Inode {
                    // ugh there must be a nicer way to do this with arrays, which we already have
                    // from above...
                    ino: u64::from_le_bytes(state[0..8].try_into().unwrap()),
                    mode,
                    uid: u32::from_le_bytes(state[25..29].try_into().unwrap()),
                    gid: u32::from_le_bytes(state[29..33].try_into().unwrap()),
                    permissions: u16::from_le_bytes(state[33..35].try_into().unwrap()),
                    additional,
                })
            }
        }

        deserializer.deserialize_bytes(InodeVisitor)
    }
}

impl Inode {
    pub fn new_dir(
        ino: Ino,
        md: &fs::Metadata,
        dir_list: u64,
        additional: Option<BlobRef>,
    ) -> io::Result<Self> {
        if !md.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                format!("{ino} is a dir"),
            ));
        }

        let mode = InodeMode::Dir { offset: dir_list };
        Ok(Self::new_inode(ino, md, mode, additional))
    }

    pub fn new_file(
        ino: Ino,
        md: &fs::Metadata,
        chunk_list: u64,
        additional: Option<BlobRef>,
    ) -> io::Result<Self> {
        if !md.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                format!("{ino} is a file"),
            ));
        }

        let mode = InodeMode::Reg { offset: chunk_list };
        Ok(Self::new_inode(ino, md, mode, additional))
    }

    pub fn new_other(ino: Ino, md: &fs::Metadata, additional: Option<BlobRef>) -> io::Result<Self> {
        let file_type = md.file_type();
        let mode = if file_type.is_fifo() {
            InodeMode::Fifo
        } else if file_type.is_char_device() {
            let major = stat::major(md.rdev());
            let minor = stat::minor(md.rdev());
            InodeMode::Chr { major, minor }
        } else if file_type.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                format!("{ino} is a dir"),
            ));
        } else if file_type.is_block_device() {
            let major = stat::major(md.rdev());
            let minor = stat::minor(md.rdev());
            InodeMode::Blk { major, minor }
        } else if file_type.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                format!("{ino} is a file"),
            ));
        } else if file_type.is_symlink() {
            InodeMode::Lnk
        } else if file_type.is_socket() {
            InodeMode::Sock
        } else {
            InodeMode::Unknown
        };

        Ok(Self::new_inode(ino, md, mode, additional))
    }

    pub fn new_whiteout(ino: Ino) -> Self {
        Inode {
            ino,
            mode: InodeMode::Wht,
            uid: 0,
            gid: 0,
            permissions: DEFAULT_FILE_PERMISSIONS,
            additional: None,
        }
    }

    fn new_inode(
        ino: Ino,
        md: &fs::Metadata,
        mode: InodeMode,
        additional: Option<BlobRef>,
    ) -> Self {
        Inode {
            ino,
            mode,
            uid: md.uid(),
            gid: md.gid(),
            // only preserve rwx permissions for user, group, others (9 bits) and SUID/SGID/sticky bit (3 bits)
            permissions: (md.permissions().mode() & 0xFFF) as u16,
            additional,
        }
    }

    #[cfg(test)]
    fn to_wire(&self) -> io::Result<Vec<u8>> {
        let mut buf = Vec::<u8>::new();

        serde_cbor::to_writer(&mut buf, &self)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
        Ok(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_inode_is_constant_serialized_size() {
        // TODO: this is the sort of think quickcheck is perfect for...
        let testcases = vec![
            Inode {
                ino: 0,
                mode: InodeMode::Unknown,
                uid: 0,
                gid: 0,
                permissions: 0,
                additional: None,
            },
            Inode {
                ino: 0,
                mode: InodeMode::Lnk,
                uid: 0,
                gid: 0,
                permissions: 0,
                additional: None,
            },
            Inode {
                ino: 0,
                mode: InodeMode::Reg { offset: 64 },
                uid: 0,
                gid: 0,
                permissions: DEFAULT_FILE_PERMISSIONS,
                additional: None,
            },
            Inode {
                ino: 65343,
                mode: InodeMode::Chr {
                    major: 64,
                    minor: 65536,
                },
                uid: 10,
                gid: 10000,
                permissions: DEFAULT_DIRECTORY_PERMISSIONS,
                additional: None,
            },
            Inode {
                ino: 0,
                mode: InodeMode::Lnk,
                uid: 0,
                gid: 0,
                permissions: 0xFFFF,
                additional: Some(BlobRef {
                    offset: 42,
                    kind: BlobRefKind::Local,
                }),
            },
        ];

        for test in testcases {
            let wire = test.to_wire().unwrap();
            let after = serde_cbor::from_reader(&*wire).unwrap();
            assert_eq!(wire.len(), INODE_WIRE_SIZE, "{test:?}");
            assert_eq!(test, after);
        }
    }

    fn blobref_roundtrip(original: BlobRef) {
        let mut serialized = [0_u8; BLOB_REF_SIZE];
        original.fixed_length_serialize(&mut serialized);
        // we lie here and say this is a serde_cbor error, even though it really doesn't matter...
        let deserialized =
            BlobRef::fixed_length_deserialize::<serde_cbor::error::Error>(&serialized).unwrap();
        assert_eq!(original, deserialized);
    }

    #[test]
    fn test_local_blobref_serialization() {
        let local = BlobRef {
            offset: 42,
            kind: BlobRefKind::Local,
        };
        blobref_roundtrip(local)
    }

    #[test]
    fn test_other_blobref_serialization() {
        let mut digest = [0_u8; 32];
        digest[0] = 0;
        digest[31] = 31;
        let other = BlobRef {
            offset: 42,
            kind: BlobRefKind::Other { digest },
        };
        blobref_roundtrip(other)
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct InodeAdditional {
    pub xattrs: Vec<Xattr>,
    pub symlink_target: Option<OsString>,
}

impl InodeAdditional {
    pub fn new(p: &Path, md: &fs::Metadata) -> io::Result<Option<Self>> {
        let symlink_target = if md.file_type().is_symlink() {
            let t = fs::read_link(p)?;
            Some(t.into())
        } else {
            None
        };
        let xattrs = Self::get_xattrs(p)?;
        if symlink_target.is_none() && xattrs.is_empty() {
            Ok(None)
        } else {
            Ok(Some(InodeAdditional {
                xattrs,
                symlink_target,
            }))
        }
    }

    fn get_xattrs(p: &Path) -> io::Result<Vec<Xattr>> {
        xattr::list(p)?
            .map(|xa| {
                let value = xattr::get(p, &xa)?;
                Ok(Xattr {
                    key: xa,
                    val: value.unwrap(),
                })
            })
            .collect()
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Xattr {
    pub key: OsString,
    pub val: Vec<u8>,
}

pub struct MetadataBlob {
    f: Box<dyn Decompressor>,
}

impl MetadataBlob {
    pub fn new(f: fs::File) -> MetadataBlob {
        MetadataBlob { f: Box::new(f) }
    }

    pub fn seek_ref(&mut self, r: &BlobRef) -> Result<u64> {
        match r.kind {
            BlobRefKind::Other { .. } => Err(WireFormatError::SeekOtherError(Backtrace::capture())),
            BlobRefKind::Local => self
                .f
                .seek(io::SeekFrom::Start(r.offset))
                .map_err(|e| e.into()),
        }
    }

    pub fn read_file_chunks(&mut self, offset: u64) -> Result<Vec<FileChunk>> {
        self.f.seek(io::SeekFrom::Start(offset))?;
        read_one::<FileChunkList, _>(&mut self.f).map(|cl| cl.chunks)
    }

    pub fn read_dir_list(&mut self, offset: u64) -> Result<DirList> {
        self.f.seek(io::SeekFrom::Start(offset))?;
        read_one(&mut self.f)
    }

    pub fn read_inode_additional(&mut self, r: &BlobRef) -> Result<InodeAdditional> {
        self.seek_ref(r)?;
        read_one(&mut self.f)
    }

    pub fn find_inode(&mut self, ino: Ino) -> Result<Option<Inode>> {
        self.f.rewind()?;
        let inode_count = cbor_get_array_size(&mut self.f)?;
        let mut left = 0;
        let mut right = inode_count;

        while left <= right {
            let mid = left + (right - left) / 2;

            self.f.seek(io::SeekFrom::Start(
                (mid * INODE_WIRE_SIZE as u64)
                    + cbor_size_of_list_header(inode_count as usize) as u64,
            ))?;
            let i = read_one::<Inode, _>(&mut self.f)?;
            if i.ino == ino {
                return Ok(Some(i));
            }

            if i.ino < ino {
                left = mid + 1;
            } else {
                // don't underflow...
                if mid == 0 {
                    break;
                }
                right = mid - 1;
            };
        }

        Ok(None)
    }

    pub fn read_inodes(&mut self) -> Result<Vec<Inode>> {
        self.f.rewind()?;
        read_one(&mut self.f)
    }

    pub fn max_ino(&mut self) -> Result<Option<Ino>> {
        Ok(self.read_inodes()?.last().map(|inode| inode.ino))
    }
}
