extern crate serde_cbor;
extern crate xattr;

use std::convert::TryInto;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::mem;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::Path;
use std::vec::Vec;

use nix::sys::stat;
use serde::de::Error as SerdeError;
use serde::de::Visitor;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

// To get off the ground here, we just use serde and cbor for most things, except for the fixed
// size Inode which depends being a fixed size (and cbor won't generate it that way) in the later
// format.

#[derive(Error, Debug)]
pub enum WireFormatError {
    #[error("cannot seek to other blob")]
    SeekOtherError,
    #[error("no value present")]
    ValueMissing,
    #[error("fs error")]
    IOError(#[from] io::Error),
    #[error("deserialization error")]
    CBORError(#[from] serde_cbor::Error),
}

pub type Result<T> = std::result::Result<T, WireFormatError>;

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

#[derive(Serialize, Deserialize, Debug)]
pub struct Rootfs {
    pub metadatas: Vec<BlobRef>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub enum BlobRefKind {
    Local,
    Other { digest: [u8; 32] },
}

// TODO: should this be an ociv1 digest and include size and media type?
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct BlobRef {
    pub offset: u64,
    pub kind: BlobRefKind,
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

const INODE_MODE_SIZE: usize = 1 + 8 + 8;

// InodeMode needs to have custom serialization because inodes must be a fixed size.
#[derive(Debug, PartialEq)]
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

const INODE_SIZE: usize =
    mem::size_of::<Ino>() + INODE_MODE_SIZE + mem::size_of::<u32>() + mem::size_of::<u32>();

pub const fn cbor_size_of_list_header(size: usize) -> usize {
    match size {
        0..=23 => 1,
        24..=255 => 2,
        256..=65535 => 3,
        65536..=4294967295 => 4,
        _ => 8,
    }
}

pub const INODE_WIRE_SIZE: usize = cbor_size_of_list_header(INODE_SIZE) + INODE_SIZE;

#[derive(Debug, PartialEq)]
pub struct Inode {
    pub ino: Ino,
    pub mode: InodeMode,
    pub uid: u32,
    pub gid: u32,
    // TODO: FIXME: need fixed length serialization for this
    //pub additional: Option<BlobRef>,
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
                formatter.write_fmt(format_args!("expected {} bytes for Inode", INODE_MODE_SIZE))
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

                Ok(Inode {
                    // ugh there must be a nicer way to do this with arrays, which we already have
                    // from above...
                    ino: u64::from_le_bytes(state[0..8].try_into().unwrap()),
                    mode,
                    uid: u32::from_le_bytes(state[25..29].try_into().unwrap()),
                    gid: u32::from_le_bytes(state[29..33].try_into().unwrap()),
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
                format!("{} is a dir", ino),
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
                format!("{} is a file", ino),
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
                format!("{} is a dir", ino),
            ));
        } else if file_type.is_block_device() {
            let major = stat::major(md.rdev());
            let minor = stat::minor(md.rdev());
            InodeMode::Blk { major, minor }
        } else if file_type.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                format!("{} is a file", ino),
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

    fn new_inode(ino: Ino, md: &fs::Metadata, mode: InodeMode, _: Option<BlobRef>) -> Self {
        Inode {
            ino,
            mode,
            uid: md.uid(),
            gid: md.gid(),
            // additional, TODO: fixed length serialize
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
            },
            Inode {
                ino: 0,
                mode: InodeMode::Lnk,
                uid: 0,
                gid: 0,
            },
            Inode {
                ino: 0,
                mode: InodeMode::Reg { offset: 64 },
                uid: 0,
                gid: 0,
            },
            Inode {
                ino: 65343,
                mode: InodeMode::Chr {
                    major: 64,
                    minor: 65536,
                },
                uid: 10,
                gid: 10000,
            },
        ];

        for test in testcases {
            let wire = test.to_wire().unwrap();
            let after = serde_cbor::from_reader(&*wire).unwrap();
            assert_eq!(wire.len(), INODE_WIRE_SIZE, "{:?}", test);
            assert_eq!(test, after);
        }
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
                    val: value,
                })
            })
            .collect()
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Xattr {
    pub key: OsString,
    pub val: Option<Vec<u8>>,
}
