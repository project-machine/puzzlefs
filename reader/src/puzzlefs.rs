use std::backtrace::Backtrace;
use std::cmp::min;
use std::convert::TryFrom;
use std::ffi::OsStr;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path};
use std::sync::Arc;

use nix::errno::Errno;

use format::{FileChunk, Ino, InodeAdditional, MetadataBlob, Result, VerityData, WireFormatError};
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
                    .collect::<Vec<(Vec<u8>, Ino)>>();
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

    pub fn dir_entries(&self) -> Result<&Vec<(Vec<u8>, Ino)>> {
        match &self.mode {
            InodeMode::Dir { entries } => Ok(entries),
            _ => Err(WireFormatError::from_errno(Errno::ENOTDIR)),
        }
    }

    pub fn dir_lookup(&self, name: &[u8]) -> Result<u64> {
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
}

#[derive(Debug)]
pub enum InodeMode {
    File { chunks: Vec<FileChunk> },
    Dir { entries: Vec<(Vec<u8>, Ino)> },
    Other,
}

pub(crate) fn file_read(
    oci: &Image,
    inode: &Inode,
    offset: usize,
    data: &mut [u8],
    verity_data: &Option<VerityData>,
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

        let addl_offset = if offset > file_offset {
            offset - file_offset
        } else {
            0
        };

        // ok, need to read this chunk; how much?
        let left_in_buf = data.len() - buf_offset;
        let to_read = min(left_in_buf, chunk.len as usize - addl_offset);

        let start = buf_offset;
        let finish = start + to_read;
        file_offset += addl_offset;

        // how many did we actually read?
        let n = oci.fill_from_chunk(
            chunk.blob,
            addl_offset as u64,
            &mut data[start..finish],
            verity_data,
        )?;
        file_offset += n;
        buf_offset += n;
    }

    // discard any extra if we hit EOF
    Ok(buf_offset)
}

pub struct PuzzleFS {
    pub oci: Arc<Image>,
    layers: Vec<format::MetadataBlob>,
    pub verity_data: Option<VerityData>,
    pub manifest_verity: Option<Vec<u8>>,
}

impl PuzzleFS {
    pub fn open(oci: Image, tag: &str, manifest_verity: Option<&[u8]>) -> format::Result<PuzzleFS> {
        let rootfs = oci.open_rootfs_blob::<compression::Noop>(tag, manifest_verity)?;
        let verity_data = if manifest_verity.is_some() {
            Some(rootfs.fs_verity_data)
        } else {
            None
        };
        let layers = rootfs
            .metadatas
            .iter()
            .map(|md| -> Result<MetadataBlob> {
                let digest = <Digest>::try_from(md)?;
                let file_verity = if let Some(verity) = &verity_data {
                    Some(
                        &verity.get(&digest.underlying()).ok_or(
                            WireFormatError::InvalidFsVerityData(
                                format!("missing verity data {digest}"),
                                Backtrace::capture(),
                            ),
                        )?[..],
                    )
                } else {
                    None
                };
                oci.open_metadata_blob(&digest, file_verity)
            })
            .collect::<format::Result<Vec<MetadataBlob>>>()?;
        Ok(PuzzleFS {
            oci: Arc::new(oci),
            layers,
            verity_data,
            manifest_verity: manifest_verity.map(|e| e.to_vec()),
        })
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
                        if let Some((_, ino)) =
                            entries.into_iter().find(|(path, _)| path == p.as_bytes())
                        {
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
    oci: &'a Image,
    inode: &'a Inode,
    offset: usize,
    len: usize,
}

impl<'a> FileReader<'a> {
    pub fn new(oci: &'a Image, inode: &'a Inode) -> Result<FileReader<'a>> {
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

        let read = file_read(
            self.oci,
            self.inode,
            self.offset,
            &mut buf[0..to_read],
            &None,
        )
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
        image.add_tag("test", rootfs_desc).unwrap();
        let mut pfs = PuzzleFS::open(image, "test", None).unwrap();

        let inode = pfs.find_inode(2).unwrap();
        let mut reader = FileReader::new(&pfs.oci, &inode).unwrap();
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
        image.add_tag("test", rootfs_desc).unwrap();
        let mut pfs = PuzzleFS::open(image, "test", None).unwrap();

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
