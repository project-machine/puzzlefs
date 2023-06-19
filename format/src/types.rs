use capnp::{message, serialize};
use memmap2::{Mmap, MmapOptions};
use nix::errno::Errno;
use nix::sys::stat;
use std::backtrace::Backtrace;
use std::collections::BTreeMap;
use std::convert::{TryFrom, TryInto};
use std::ffi::OsStr;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::Path;
use std::vec::Vec;

use serde::de::Error as SerdeError;
use serde::de::Visitor;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::{Result, WireFormatError};
use hex::FromHexError;

pub mod metadata_capnp {
    include!(concat!(env!("OUT_DIR"), "/metadata_capnp.rs"));
}

pub mod manifest_capnp {
    include!(concat!(env!("OUT_DIR"), "/manifest_capnp.rs"));
}

pub const DEFAULT_FILE_PERMISSIONS: u16 = 0o644;
pub const SHA256_BLOCK_SIZE: usize = 32;
// We use a BTreeMap instead of a HashMap because the BTreeMap is sorted, thus we get a
// reproducible representation of the serialized metadata
pub type VerityData = BTreeMap<[u8; SHA256_BLOCK_SIZE], [u8; SHA256_BLOCK_SIZE]>;

#[derive(Debug)]
pub struct Rootfs {
    pub metadatas: Vec<BlobRef>,
    pub fs_verity_data: VerityData,
    pub manifest_version: u64,
}

impl Rootfs {
    pub fn open<R: Read>(f: R) -> Result<Rootfs> {
        let message_reader = serialize::read_message(f, ::capnp::message::ReaderOptions::new())?;
        let rootfs = message_reader.get_root::<crate::manifest_capnp::rootfs::Reader<'_>>()?;
        Self::from_capnp(rootfs)
    }

    pub fn from_capnp(reader: crate::manifest_capnp::rootfs::Reader<'_>) -> Result<Self> {
        let metadatas = reader.get_metadatas()?;

        let metadata_vec = metadatas
            .iter()
            .map(BlobRef::from_capnp)
            .collect::<Result<Vec<BlobRef>>>()?;

        let capnp_verities = reader.get_fs_verity_data()?;
        let mut fs_verity_data = VerityData::new();

        for capnp_verity in capnp_verities {
            let digest = capnp_verity.get_digest()?.try_into()?;
            let verity = capnp_verity.get_verity()?.try_into()?;
            fs_verity_data.insert(digest, verity);
        }

        Ok(Rootfs {
            metadatas: metadata_vec,
            fs_verity_data,
            manifest_version: reader.get_manifest_version(),
        })
    }

    pub fn to_capnp(&self, builder: &mut crate::manifest_capnp::rootfs::Builder<'_>) -> Result<()> {
        builder.set_manifest_version(self.manifest_version);

        let metadatas_len = self.metadatas.len().try_into()?;
        let mut capnp_metadatas = builder.reborrow().init_metadatas(metadatas_len);

        for (i, metadata) in self.metadatas.iter().enumerate() {
            // we already checked that the length of metadatas fits inside a u32
            let mut capnp_metadata = capnp_metadatas.reborrow().get(i as u32);
            metadata.to_capnp(&mut capnp_metadata);
        }

        let verity_data_len = self.fs_verity_data.len().try_into()?;
        let mut capnp_verities = builder.reborrow().init_fs_verity_data(verity_data_len);

        for (i, (digest, verity)) in self.fs_verity_data.iter().enumerate() {
            // we already checked that the length of verity_data fits inside a u32
            let mut capnp_verity = capnp_verities.reborrow().get(i as u32);
            capnp_verity.set_digest(digest);
            capnp_verity.set_verity(verity);
        }

        Ok(())
    }
}

// TODO: should this be an ociv1 digest and include size and media type?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlobRef {
    pub digest: [u8; SHA256_BLOCK_SIZE],
    pub offset: u64,
    pub compressed: bool,
}

impl BlobRef {
    pub fn from_capnp(reader: crate::metadata_capnp::blob_ref::Reader<'_>) -> Result<Self> {
        let digest = reader.get_digest()?;
        Ok(BlobRef {
            digest: digest.try_into()?,
            offset: reader.get_offset(),
            compressed: reader.get_compressed(),
        })
    }
    pub fn to_capnp(&self, builder: &mut crate::metadata_capnp::blob_ref::Builder<'_>) {
        builder.set_digest(&self.digest);
        builder.set_offset(self.offset);
        builder.set_compressed(self.compressed);
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct DirEnt {
    pub ino: Ino,
    pub name: Vec<u8>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct DirList {
    // TODO: flags instead?
    pub look_below: bool,
    pub entries: Vec<DirEnt>,
}

#[derive(Debug)]
pub struct FileChunkList {
    pub chunks: Vec<FileChunk>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct FileChunk {
    pub blob: BlobRef,
    pub len: u64,
}

pub type Ino = u64;

impl FileChunk {
    pub fn from_capnp(reader: crate::metadata_capnp::file_chunk::Reader<'_>) -> Result<Self> {
        let len = reader.get_len();
        let blob = BlobRef::from_capnp(reader.get_blob()?)?;

        Ok(FileChunk { blob, len })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFAULT_DIRECTORY_PERMISSIONS: u16 = 0o755;

    fn blobref_roundtrip(original: BlobRef) {
        let mut message = ::capnp::message::Builder::new_default();
        let mut capnp_blob_ref = message.init_root::<metadata_capnp::blob_ref::Builder<'_>>();

        original.to_capnp(&mut capnp_blob_ref);

        let mut buf = Vec::new();
        ::capnp::serialize::write_message(&mut buf, &message)
            .expect("capnp::serialize::write_message failed");

        let message_reader = serialize::read_message_from_flat_slice(
            &mut &buf[..],
            ::capnp::message::ReaderOptions::new(),
        )
        .expect("read_message_from_flat_slice failed");
        let blobref_reader = message_reader
            .get_root::<crate::metadata_capnp::blob_ref::Reader<'_>>()
            .expect("message_reader.get_root failed");
        let deserialized = BlobRef::from_capnp(blobref_reader).expect("BlobRef::from_capnp failed");

        assert_eq!(original, deserialized);
    }

    #[test]
    fn test_blobref_serialization() {
        let local = BlobRef {
            offset: 42,
            digest: [
                0xb7, 0x2e, 0x68, 0x50, 0x82, 0xd1, 0xdd, 0xfe, 0xb6, 0xcc, 0x31, 0xa5, 0x35, 0x29,
                0x12, 0xFE, 0x3f, 0x51, 0x14, 0x65, 0xf5, 0x27, 0xa5, 0x1a, 0xb3, 0xff, 0xd3, 0xb8,
                0xAA, 0x3C, 0x25, 0xDD,
            ],
            compressed: true,
        };
        blobref_roundtrip(local)
    }

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
                mode: InodeMode::File {
                    chunks: vec![FileChunk {
                        blob: BlobRef {
                            digest: [
                                0x12, 0x44, 0xFE, 0xDD, 0x13, 0x39, 0x88, 0x12, 0x48, 0xA8, 0xF8,
                                0xE4, 0x22, 0x12, 0x15, 0x16, 0x12, 0x44, 0xFE, 0xDD, 0x31, 0x93,
                                0x88, 0x21, 0x84, 0x8A, 0xF8, 0x4E, 0x22, 0x12, 0x51, 0x16,
                            ],
                            offset: 100,
                            compressed: true,
                        },
                        len: 100,
                    }],
                },
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
                additional: Some(InodeAdditional {
                    xattrs: vec![Xattr {
                        key: b"some extended attribute".to_vec(),
                        val: b"with some value".to_vec(),
                    }],
                    symlink_target: Some(b"some/other/path".to_vec()),
                }),
            },
        ];

        for test in testcases {
            let wire = test.to_wire().unwrap();
            let message_reader = serialize::read_message_from_flat_slice(
                &mut &wire[..],
                ::capnp::message::ReaderOptions::new(),
            )
            .expect("read_message_from_flat_slice failed");
            let inode_reader = message_reader
                .get_root::<crate::metadata_capnp::inode::Reader<'_>>()
                .expect("message_reader.get_root failed");
            let after = Inode::from_capnp(inode_reader).expect("BlobRef::from_capnp failed");
            assert_eq!(test, after);
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Inode {
    pub ino: Ino,
    pub mode: InodeMode,
    pub uid: u32,
    pub gid: u32,
    pub permissions: u16,
    pub additional: Option<InodeAdditional>,
}

impl Inode {
    pub fn from_capnp(reader: crate::metadata_capnp::inode::Reader<'_>) -> Result<Self> {
        Ok(Inode {
            ino: reader.get_ino(),
            mode: InodeMode::from_capnp(reader.get_mode())?,
            uid: reader.get_uid(),
            gid: reader.get_gid(),
            permissions: reader.get_permissions(),
            additional: InodeAdditional::from_capnp(reader.get_additional()?)?,
        })
    }

    pub fn to_capnp(&self, builder: &mut metadata_capnp::inode::Builder<'_>) -> Result<()> {
        builder.set_ino(self.ino);

        let mut mode_builder = builder.reborrow().init_mode();
        self.mode.to_capnp(&mut mode_builder)?;

        builder.set_uid(self.uid);
        builder.set_gid(self.gid);
        builder.set_permissions(self.permissions);

        if let Some(additional) = &self.additional {
            let mut additional_builder = builder.reborrow().init_additional();
            additional.to_capnp(&mut additional_builder)?;
        }

        Ok(())
    }

    pub fn new_dir(
        ino: Ino,
        md: &fs::Metadata,
        dir_list: DirList,
        additional: Option<InodeAdditional>,
    ) -> io::Result<Self> {
        if !md.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                format!("{ino} is a dir"),
            ));
        }

        let mode = InodeMode::Dir { dir_list };
        Ok(Self::new_inode(ino, md, mode, additional))
    }

    pub fn new_file(
        ino: Ino,
        md: &fs::Metadata,
        file_chunks: Vec<FileChunk>,
        additional: Option<InodeAdditional>,
    ) -> io::Result<Self> {
        if !md.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                format!("{ino} is a file"),
            ));
        }

        let mode = InodeMode::File {
            chunks: file_chunks,
        };
        Ok(Self::new_inode(ino, md, mode, additional))
    }

    pub fn new_other(
        ino: Ino,
        md: &fs::Metadata,
        additional: Option<InodeAdditional>,
    ) -> io::Result<Self> {
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
        additional: Option<InodeAdditional>,
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

    pub fn dir_entries(&self) -> Result<&Vec<DirEnt>> {
        match &self.mode {
            InodeMode::Dir { dir_list } => Ok(&dir_list.entries),
            _ => Err(WireFormatError::from_errno(Errno::ENOTDIR)),
        }
    }

    pub fn dir_lookup(&self, name: &[u8]) -> Result<u64> {
        let entries = self.dir_entries()?;
        entries
            .iter()
            .find(|dir_ent| dir_ent.name == name)
            .map(|dir_ent| dir_ent.ino)
            .ok_or_else(|| WireFormatError::from_errno(Errno::ENOENT))
    }

    pub fn file_len(&self) -> Result<u64> {
        let chunks = match &self.mode {
            InodeMode::File { chunks } => chunks,
            _ => return Err(WireFormatError::from_errno(Errno::ENOTDIR)),
        };
        Ok(chunks.iter().map(|c| c.len).sum())
    }

    pub fn symlink_target(&self) -> Result<&OsStr> {
        self.additional
            .as_ref()
            .and_then(|a| {
                a.symlink_target
                    .as_ref()
                    .map(|x| OsStr::from_bytes(x.as_slice()))
            })
            .ok_or_else(|| WireFormatError::from_errno(Errno::ENOENT))
    }

    #[cfg(test)]
    fn to_wire(&self) -> Result<Vec<u8>> {
        let mut message = ::capnp::message::Builder::new_default();
        let mut capnp_inode = message.init_root::<metadata_capnp::inode::Builder<'_>>();

        self.to_capnp(&mut capnp_inode)?;

        let mut buf = Vec::new();
        ::capnp::serialize::write_message(&mut buf, &message)?;
        Ok(buf)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum InodeMode {
    Unknown,
    Fifo,
    Chr { major: u64, minor: u64 },
    Dir { dir_list: DirList },
    Blk { major: u64, minor: u64 },
    File { chunks: Vec<FileChunk> },
    Lnk,
    Sock,
    Wht,
}

impl InodeMode {
    fn from_capnp(reader: crate::metadata_capnp::inode::mode::Reader<'_>) -> Result<Self> {
        match reader.which() {
            Ok(crate::metadata_capnp::inode::mode::Unknown(())) => Ok(InodeMode::Unknown),
            Ok(crate::metadata_capnp::inode::mode::Fifo(())) => Ok(InodeMode::Fifo),
            Ok(crate::metadata_capnp::inode::mode::Lnk(())) => Ok(InodeMode::Lnk),
            Ok(crate::metadata_capnp::inode::mode::Sock(())) => Ok(InodeMode::Sock),
            Ok(crate::metadata_capnp::inode::mode::Wht(())) => Ok(InodeMode::Wht),
            Ok(crate::metadata_capnp::inode::mode::Chr(reader)) => {
                let r = reader?;
                Ok(InodeMode::Chr {
                    major: r.get_major(),
                    minor: r.get_minor(),
                })
            }
            Ok(crate::metadata_capnp::inode::mode::Blk(reader)) => {
                let r = reader?;
                Ok(InodeMode::Blk {
                    major: r.get_major(),
                    minor: r.get_minor(),
                })
            }
            Ok(crate::metadata_capnp::inode::mode::File(reader)) => {
                let r = reader?;
                let chunks = r
                    .get_chunks()?
                    .iter()
                    .map(FileChunk::from_capnp)
                    .collect::<Result<Vec<FileChunk>>>()?;
                Ok(InodeMode::File { chunks })
            }
            Ok(crate::metadata_capnp::inode::mode::Dir(reader)) => {
                let r = reader?;
                let entries = r
                    .get_entries()?
                    .iter()
                    .map(|entry| {
                        let ino = entry.get_ino();
                        let dir_entry = entry.get_name().map(Vec::from);
                        match dir_entry {
                            Ok(d) => Ok(DirEnt { ino, name: d }),
                            Err(e) => Err(WireFormatError::from(e)),
                        }
                    })
                    .collect::<Result<Vec<DirEnt>>>()?;
                let look_below = r.get_look_below();
                Ok(InodeMode::Dir {
                    dir_list: DirList {
                        look_below,
                        entries,
                    },
                })
            }
            Err(::capnp::NotInSchema(_e)) => {
                Err(WireFormatError::InvalidSerializedData(Backtrace::capture()))
            }
        }
    }

    fn to_capnp(
        &self,
        builder: &mut crate::metadata_capnp::inode::mode::Builder<'_>,
    ) -> Result<()> {
        match &self {
            Self::Unknown => builder.set_unknown(()),
            Self::Fifo => builder.set_fifo(()),
            Self::Chr { major, minor } => {
                let mut chr_builder = builder.reborrow().init_chr();
                chr_builder.set_minor(*minor);
                chr_builder.set_major(*major);
            }
            Self::Dir { dir_list } => {
                let mut dir_builder = builder.reborrow().init_dir();
                dir_builder.set_look_below(dir_list.look_below);
                let entries_len = dir_list.entries.len().try_into()?;
                let mut entries_builder = dir_builder.reborrow().init_entries(entries_len);

                for (i, entry) in dir_list.entries.iter().enumerate() {
                    // we already checked that the length of entries fits inside a u32
                    let mut dir_entry_builder = entries_builder.reborrow().get(i as u32);
                    dir_entry_builder.set_ino(entry.ino);
                    dir_entry_builder.set_name(&entry.name);
                }
            }
            Self::Blk { major, minor } => {
                let mut blk_builder = builder.reborrow().init_blk();
                blk_builder.set_minor(*minor);
                blk_builder.set_major(*major);
            }
            Self::File { chunks } => {
                let file_builder = builder.reborrow().init_file();
                let chunks_len = chunks.len().try_into()?;
                let mut chunks_builder = file_builder.init_chunks(chunks_len);

                for (i, chunk) in chunks.iter().enumerate() {
                    // we already checked that the length of chunks fits inside a u32
                    let mut chunk_builder = chunks_builder.reborrow().get(i as u32);
                    chunk_builder.set_len(chunk.len);
                    let mut blob_ref_builder = chunk_builder.init_blob();
                    chunk.blob.to_capnp(&mut blob_ref_builder);
                }
            }
            Self::Lnk => builder.set_lnk(()),
            Self::Sock => builder.set_sock(()),
            Self::Wht => builder.set_wht(()),
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct InodeAdditional {
    pub xattrs: Vec<Xattr>,
    pub symlink_target: Option<Vec<u8>>,
}

impl InodeAdditional {
    pub fn from_capnp(
        reader: crate::metadata_capnp::inode_additional::Reader<'_>,
    ) -> Result<Option<Self>> {
        if !(reader.has_xattrs() || reader.has_symlink_target()) {
            return Ok(None);
        }

        let mut xattrs = Vec::new();
        if reader.has_xattrs() {
            for capnp_xattr in reader.get_xattrs()? {
                let xattr = Xattr::from_capnp(capnp_xattr)?;
                xattrs.push(xattr);
            }
        }

        let symlink_target = if reader.has_symlink_target() {
            Some(reader.get_symlink_target()?.to_vec())
        } else {
            None
        };

        Ok(Some(InodeAdditional {
            xattrs,
            symlink_target,
        }))
    }

    pub fn to_capnp(
        &self,
        builder: &mut crate::metadata_capnp::inode_additional::Builder<'_>,
    ) -> Result<()> {
        let xattrs_len = self.xattrs.len().try_into()?;
        let mut xattrs_builder = builder.reborrow().init_xattrs(xattrs_len);

        for (i, xattr) in self.xattrs.iter().enumerate() {
            // we already checked that the length of xattrs fits inside a u32
            let mut xattr_builder = xattrs_builder.reborrow().get(i as u32);
            xattr.to_capnp(&mut xattr_builder);
        }

        if let Some(symlink_target) = &self.symlink_target {
            builder.set_symlink_target(symlink_target);
        }

        Ok(())
    }

    pub fn new(p: &Path, md: &fs::Metadata) -> io::Result<Option<Self>> {
        let symlink_target = if md.file_type().is_symlink() {
            let t = fs::read_link(p)?;
            Some(OsString::from(t).into_vec())
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
                    key: xa.into_vec(),
                    val: value.unwrap(),
                })
            })
            .collect()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Xattr {
    pub key: Vec<u8>,
    pub val: Vec<u8>,
}

impl Xattr {
    pub fn from_capnp(reader: crate::metadata_capnp::xattr::Reader<'_>) -> Result<Self> {
        let key = reader.get_key()?.to_vec();
        let val = reader.get_val()?.to_vec();
        Ok(Xattr { key, val })
    }

    pub fn to_capnp(&self, builder: &mut crate::metadata_capnp::xattr::Builder<'_>) {
        builder.set_val(&self.val);
        builder.set_key(&self.key);
    }
}

pub struct MetadataBlob {
    reader: message::TypedReader<
        ::capnp::serialize::BufferSegments<Mmap>,
        crate::metadata_capnp::inode_vector::Owned,
    >,
}

impl MetadataBlob {
    pub fn new(f: fs::File) -> Result<MetadataBlob> {
        // We know the loaded message is safe, so we're allowing unlimited reads.
        let unlimited_reads = message::ReaderOptions {
            traversal_limit_in_words: None,
            nesting_limit: 64,
        };
        let mmapped_region = unsafe { MmapOptions::new().map_copy_read_only(&f)? };
        let segments = serialize::BufferSegments::new(mmapped_region, unlimited_reads)?;
        let reader = message::Reader::new(segments, unlimited_reads).into_typed();

        Ok(MetadataBlob { reader })
    }

    pub fn get_inode_vector(
        &self,
    ) -> ::capnp::Result<::capnp::struct_list::Reader<'_, crate::metadata_capnp::inode::Owned>>
    {
        self.reader.get()?.get_inodes()
    }

    pub fn find_inode(
        &mut self,
        ino: Ino,
    ) -> Result<Option<crate::metadata_capnp::inode::Reader<'_>>> {
        let mut left = 0;
        let inodes = self.get_inode_vector()?;
        let mut right = inodes.len();

        while left <= right {
            let mid = left + (right - left) / 2;
            let i = inodes.get(mid);

            if i.get_ino() == ino {
                return Ok(Some(i));
            }

            if i.get_ino() < ino {
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

    pub fn max_ino(&mut self) -> Result<Option<Ino>> {
        let inodes = self.get_inode_vector()?;
        let last_index = inodes.len() - 1;
        Ok(Some(inodes.get(last_index).get_ino()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Digest([u8; SHA256_BLOCK_SIZE]);

impl Digest {
    pub fn new(digest: &[u8; SHA256_BLOCK_SIZE]) -> Self {
        Self(*digest)
    }
    pub fn underlying(&self) -> [u8; SHA256_BLOCK_SIZE] {
        let mut dest = [0_u8; SHA256_BLOCK_SIZE];
        dest.copy_from_slice(&self.0);
        dest
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", hex::encode(self.0))
    }
}

impl Serialize for Digest {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let val = format!("sha256:{}", hex::encode(self.0));
        serializer.serialize_str(&val)
    }
}

impl TryFrom<&str> for Digest {
    type Error = FromHexError;
    fn try_from(s: &str) -> std::result::Result<Self, Self::Error> {
        let digest = hex::decode(s)?;
        let digest: [u8; SHA256_BLOCK_SIZE] = digest
            .try_into()
            .map_err(|_| FromHexError::InvalidStringLength)?;
        Ok(Digest(digest))
    }
}

impl TryFrom<BlobRef> for Digest {
    type Error = WireFormatError;
    fn try_from(v: BlobRef) -> std::result::Result<Self, Self::Error> {
        Ok(Digest(v.digest))
    }
}

impl TryFrom<&BlobRef> for Digest {
    type Error = WireFormatError;
    fn try_from(v: &BlobRef) -> std::result::Result<Self, Self::Error> {
        Ok(Digest(v.digest))
    }
}

impl<'de> Deserialize<'de> for Digest {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Digest, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct DigestVisitor;

        impl<'de> Visitor<'de> for DigestVisitor {
            type Value = Digest;

            fn expecting(&self, formatter: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                formatter.write_fmt(format_args!("expected 'sha256:<hex encoded hash>'"))
            }

            fn visit_str<E>(self, s: &str) -> std::result::Result<Self::Value, E>
            where
                E: SerdeError,
            {
                let parts: Vec<&str> = s.split(':').collect();
                if parts.len() != 2 {
                    return Err(SerdeError::custom(format!("bad digest {s}")));
                }

                match parts[0] {
                    "sha256" => {
                        let buf =
                            hex::decode(parts[1]).map_err(|e| SerdeError::custom(e.to_string()))?;

                        let len = buf.len();
                        let digest: [u8; SHA256_BLOCK_SIZE] = buf.try_into().map_err(|_| {
                            SerdeError::custom(format!("invalid sha256 block length {len}"))
                        })?;
                        Ok(Digest(digest))
                    }
                    _ => Err(SerdeError::custom(format!(
                        "unknown digest type {}",
                        parts[0]
                    ))),
                }
            }
        }

        deserializer.deserialize_str(DigestVisitor)
    }
}
