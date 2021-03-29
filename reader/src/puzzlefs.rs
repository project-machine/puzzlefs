use std::cmp::min;
use std::convert::TryFrom;
use std::ffi::{OsStr, OsString};

use nix::errno::Errno;

use format::{FileChunk, Ino, MetadataBlob};
use oci::Image;

use super::error::{FSError, FSResult};

#[derive(Debug)]
pub struct Inode {
    pub inode: format::Inode,
    pub mode: InodeMode,
}

impl Inode {
    fn new(layer: &mut MetadataBlob, inode: format::Inode) -> FSResult<Inode> {
        let mode = match inode.mode {
            format::InodeMode::Reg { offset } => {
                let chunks = layer.read_file_chunks(offset)?;
                InodeMode::File { chunks }
            }
            format::InodeMode::Dir { offset } => {
                let mut entries = layer
                    .read_dir_list(offset)?
                    .entries
                    .iter_mut()
                    .map(|de| (de.name.clone(), de.ino))
                    .collect::<Vec<(OsString, Ino)>>();
                entries.sort_by(|(a, _), (b, _)| a.cmp(b));
                InodeMode::Dir { entries }
            }
            _ => InodeMode::Other,
        };

        Ok(Inode { inode, mode })
    }

    pub fn dir_entries(&self) -> FSResult<&Vec<(OsString, Ino)>> {
        match &self.mode {
            InodeMode::Dir { entries } => Ok(entries),
            _ => Err(FSError::from_errno(Errno::ENOTDIR)),
        }
    }

    pub fn dir_lookup(&self, name: &OsStr) -> FSResult<u64> {
        let entries = self.dir_entries()?;
        entries
            .iter()
            .find(|(cur, _)| cur == name)
            .map(|(_, ino)| ino)
            .cloned()
            .ok_or_else(|| FSError::from_errno(Errno::ENOENT))
    }

    pub fn file_len(&self) -> FSResult<u64> {
        let chunks = match &self.mode {
            InodeMode::File { chunks } => chunks,
            _ => return Err(FSError::from_errno(Errno::ENOTDIR)),
        };
        Ok(chunks.iter().map(|c| c.len).sum())
    }
}

#[derive(Debug)]
pub enum InodeMode {
    File { chunks: Vec<FileChunk> },
    Dir { entries: Vec<(OsString, Ino)> },
    Other,
}

pub struct PuzzleFS<'a> {
    oci: &'a Image<'a>,
    layers: Vec<format::MetadataBlob>,
}

impl<'a> PuzzleFS<'a> {
    pub fn open(oci: &'a Image, tag: &str) -> FSResult<PuzzleFS<'a>> {
        let rootfs = oci.open_rootfs_blob::<compression::Noop>(tag)?;
        let layers = rootfs
            .metadatas
            .iter()
            .map(|md| -> FSResult<MetadataBlob> {
                let digest = &<[u8; 32]>::try_from(md)?;
                oci.open_metadata_blob::<compression::Noop>(digest)
                    .map_err(|e| e.into())
            })
            .collect::<FSResult<Vec<MetadataBlob>>>()?;
        Ok(PuzzleFS { layers, oci })
    }

    pub fn find_inode(&mut self, ino: u64) -> FSResult<Inode> {
        for mut layer in self.layers.iter_mut() {
            if let Some(inode) = layer.find_inode(ino)? {
                return Inode::new(&mut layer, inode);
            }
        }

        Err(FSError::from_errno(Errno::ENOENT))
    }

    pub fn file_read(&self, inode: &Inode, offset: u64, size: u64) -> FSResult<Vec<u8>> {
        let chunks = match &inode.mode {
            InodeMode::File { chunks } => chunks,
            _ => return Err(FSError::from_errno(Errno::ENOTDIR)),
        };

        // TODO: fix all this casting...
        let end = offset + size;
        let mut data = vec![0_u8; size as usize];

        let mut file_offset = 0;
        let mut buf_offset = 0;
        for chunk in chunks {
            // have we read enough?
            if file_offset > end {
                break;
            }

            // should we skip this chunk?
            if file_offset + chunk.len < offset {
                file_offset += chunk.len;
                continue;
            }

            // ok, need to read this chunk; how much?
            let left_in_buf = u64::from(size) - buf_offset;
            let to_read: usize = min(left_in_buf, chunk.len) as usize;

            let start = buf_offset as usize;
            let finish = start + to_read;
            let addl_offset = if offset > file_offset {
                offset - file_offset
            } else {
                0
            };
            file_offset += addl_offset;

            // how many did we actually read?
            let n = self
                .oci
                .fill_from_chunk(chunk.blob, addl_offset, &mut data[start..finish])?;
            file_offset += n as u64;
            buf_offset += n as u64;
        }

        // discard any extra if we hit EOF
        data.truncate(buf_offset as usize);
        Ok(data)
    }
}
