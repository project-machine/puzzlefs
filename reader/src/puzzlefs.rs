use std::cmp::min;
use std::convert::TryFrom;
use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Component, Path};

use nix::errno::Errno;

use format::{FileChunk, Ino, InodeAdditional, MetadataBlob, Result, WireFormatError};
use oci::{Digest, Image};

#[derive(Debug)]
pub struct Inode {
    pub inode: format::Inode,
    pub mode: InodeMode,
    pub additional: Option<InodeAdditional>,
}

impl Inode {
    fn new(layer: &mut MetadataBlob, inode: format::Inode) -> Result<Inode> {
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

        let additional = inode
            .additional
            .map(|additional_ref| layer.read_inode_additional(&additional_ref))
            .transpose()?;

        Ok(Inode {
            inode,
            mode,
            additional,
        })
    }

    pub fn dir_entries(&self) -> Result<&Vec<(OsString, Ino)>> {
        match &self.mode {
            InodeMode::Dir { entries } => Ok(entries),
            _ => Err(WireFormatError::from_errno(Errno::ENOTDIR)),
        }
    }

    pub fn dir_lookup(&self, name: &OsStr) -> Result<u64> {
        let entries = self.dir_entries()?;
        entries
            .iter()
            .find(|(cur, _)| cur == name)
            .map(|(_, ino)| ino)
            .cloned()
            .ok_or_else(|| WireFormatError::from_errno(Errno::ENOENT))
    }

    pub fn file_len(&self) -> Result<u64> {
        let chunks = match &self.mode {
            InodeMode::File { chunks } => chunks,
            _ => return Err(WireFormatError::from_errno(Errno::ENOTDIR)),
        };
        Ok(chunks.iter().map(|c| c.len).sum())
    }

    pub fn symlink_target(&self) -> Result<&OsString> {
        self.additional
            .as_ref()
            .and_then(|a| a.symlink_target.as_ref())
            .ok_or_else(|| WireFormatError::from_errno(Errno::ENOENT))
    }
}

#[derive(Debug)]
pub enum InodeMode {
    File { chunks: Vec<FileChunk> },
    Dir { entries: Vec<(OsString, Ino)> },
    Other,
}

pub(crate) fn file_read(
    oci: &Image,
    inode: &Inode,
    offset: usize,
    data: &mut [u8],
) -> Result<usize> {
    let chunks = match &inode.mode {
        InodeMode::File { chunks } => chunks,
        _ => return Err(WireFormatError::from_errno(Errno::ENOTDIR)),
    };

    // TODO: fix all this casting...
    let end = offset + data.len();

    let mut file_offset = 0;
    let mut buf_offset = 0;
    for chunk in chunks {
        // have we read enough?
        if file_offset > end {
            break;
        }

        // should we skip this chunk?
        if file_offset + (chunk.len as usize) < offset {
            file_offset += chunk.len as usize;
            continue;
        }

        // ok, need to read this chunk; how much?
        let left_in_buf = data.len() - buf_offset;
        let to_read = min(left_in_buf, chunk.len as usize);

        let start = buf_offset;
        let finish = start + to_read;
        let addl_offset = if offset > file_offset {
            offset - file_offset
        } else {
            0
        };
        file_offset += addl_offset;

        // how many did we actually read?
        let n = oci.fill_from_chunk(chunk.blob, addl_offset as u64, &mut data[start..finish])?;
        file_offset += n;
        buf_offset += n;
    }

    // discard any extra if we hit EOF
    Ok(buf_offset)
}

pub struct PuzzleFS<'a> {
    pub(crate) oci: &'a Image<'a>,
    layers: Vec<format::MetadataBlob>,
}

impl<'a> PuzzleFS<'a> {
    pub fn open(oci: &'a Image, tag: &str) -> format::Result<PuzzleFS<'a>> {
        let rootfs = oci.open_rootfs_blob::<compression::Noop>(tag)?;
        let layers = rootfs
            .metadatas
            .iter()
            .map(|md| -> Result<MetadataBlob> {
                let digest = &<Digest>::try_from(md)?;
                oci.open_metadata_blob::<compression::Noop>(digest)
                    .map_err(|e| e.into())
            })
            .collect::<format::Result<Vec<MetadataBlob>>>()?;
        Ok(PuzzleFS { oci, layers })
    }

    pub fn find_inode(&mut self, ino: u64) -> Result<Inode> {
        for layer in self.layers.iter_mut() {
            if let Some(inode) = layer.find_inode(ino)? {
                if let format::InodeMode::Wht = inode.mode {
                    // TODO: seems like this should really be an Option.
                    return Err(format::WireFormatError::from_errno(Errno::ENOENT));
                }
                return Inode::new(layer, inode);
            }
        }

        Err(format::WireFormatError::from_errno(Errno::ENOENT))
    }

    // lookup performs a path-based lookup in this puzzlefs
    pub fn lookup(&mut self, p: &Path) -> Result<Option<Inode>> {
        let components = p.components().collect::<Vec<Component>>();
        if !matches!(components[0], Component::RootDir) {
            return Err(WireFormatError::from_errno(Errno::EINVAL));
        }

        let mut cur = self.find_inode(1)?;

        // TODO: better path resolution with .. and such?
        for comp in components.into_iter().skip(1) {
            match comp {
                Component::Normal(p) => {
                    if let InodeMode::Dir { entries } = cur.mode {
                        if let Some((_, ino)) = entries.into_iter().find(|(path, _)| path == p) {
                            cur = self.find_inode(ino)?;
                            continue;
                        }
                    }
                    return Ok(None);
                }
                _ => return Err(WireFormatError::from_errno(Errno::EINVAL)),
            }
        }

        Ok(Some(cur))
    }

    pub fn max_inode(&mut self) -> Result<Ino> {
        let mut max: Ino = 1;
        for layer in self.layers.iter_mut() {
            if let Some(ino) = layer.max_ino()? {
                max = std::cmp::max(ino, max)
            }
        }

        Ok(max)
    }
}

pub struct FileReader<'a> {
    oci: &'a Image<'a>,
    inode: &'a Inode,
    offset: usize,
    len: usize,
}

impl<'a> FileReader<'a> {
    pub fn new(oci: &'a Image<'a>, inode: &'a Inode) -> Result<FileReader<'a>> {
        let len = inode.file_len()? as usize;
        Ok(FileReader {
            oci,
            inode,
            offset: 0,
            len,
        })
    }
}

impl io::Read for FileReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let to_read = min(self.len - self.offset, buf.len());
        if to_read == 0 {
            return Ok(0);
        }

        let read = file_read(self.oci, self.inode, self.offset, &mut buf[0..to_read])
            .map_err(|e| io::Error::from_raw_os_error(e.to_errno()))?;
        self.offset += read;
        Ok(read)
    }
}

#[cfg(test)]
mod tests {
    use sha2::{Digest, Sha256};
    use tempfile::tempdir;

    use builder::build_test_fs;
    use oci::Image;

    use super::*;

    #[test]
    fn test_file_reader() {
        // make ourselves a test image
        let oci_dir = tempdir().unwrap();
        let image = Image::new(oci_dir.path()).unwrap();
        let rootfs_desc = build_test_fs(Path::new("../builder/test/test-1"), &image).unwrap();
        image.add_tag("test".to_string(), rootfs_desc).unwrap();
        let mut pfs = PuzzleFS::open(&image, "test").unwrap();

        let inode = pfs.find_inode(2).unwrap();
        let mut reader = FileReader::new(&image, &inode).unwrap();
        let mut hasher = Sha256::new();

        assert_eq!(io::copy(&mut reader, &mut hasher).unwrap(), 109466);
        let digest = hasher.finalize();
        assert_eq!(
            hex::encode(digest),
            "d9e749d9367fc908876749d6502eb212fee88c9a94892fb07da5ef3ba8bc39ed"
        );
        assert_eq!(pfs.max_inode().unwrap(), 2);
    }

    #[test]
    fn test_path_lookup() {
        let oci_dir = tempdir().unwrap();
        let image = Image::new(oci_dir.path()).unwrap();
        let rootfs_desc = build_test_fs(Path::new("../builder/test/test-1"), &image).unwrap();
        image.add_tag("test".to_string(), rootfs_desc).unwrap();
        let mut pfs = PuzzleFS::open(&image, "test").unwrap();

        assert_eq!(pfs.lookup(Path::new("/")).unwrap().unwrap().inode.ino, 1);
        assert_eq!(
            pfs.lookup(Path::new("/SekienAkashita.jpg"))
                .unwrap()
                .unwrap()
                .inode
                .ino,
            2
        );
        assert!(pfs.lookup(Path::new("/notexist")).unwrap().is_none());
        pfs.lookup(Path::new("./invalid-path")).unwrap_err();
        pfs.lookup(Path::new("invalid-path")).unwrap_err();
    }
}
