use std::cmp::min;
use std::collections::HashMap;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use walkdir::WalkDir;

use format::{
    BlobRef, BlobRefKind, DirEnt, DirList, FileChunk, FileChunkList, Ino, Inode, InodeAdditional,
    Result, Rootfs,
};
use oci::media_types;
use oci::{Descriptor, Image};

mod fastcdc_fs;
use fastcdc_fs::{ChunkWithData, FastCDCWrapper};

fn walker(rootfs: &Path) -> WalkDir {
    // breadth first search for sharing, don't cross filesystems just to be safe, order by file
    // name.
    WalkDir::new(rootfs)
        .contents_first(false)
        .follow_links(false)
        .same_file_system(true)
        .sort_by(|a, b| a.file_name().cmp(b.file_name()))
}

// a struct to hold a directory's information before it can be rendered into a InodeSpecific::Dir
// (aka the offset is unknown because we haven't accumulated all the inodes yet)
struct Dir {
    ino: u64,
    dir_list: DirList,
    md: fs::Metadata,
    additional: Option<InodeAdditional>,
}

impl Dir {
    fn add_entry(&mut self, p: &Path, ino: Ino) -> io::Result<()> {
        let name = p.file_name().ok_or_else(|| {
            io::Error::new(io::ErrorKind::Other, format!("no path for {}", p.display()))
        })?;
        self.dir_list.entries.push(DirEnt {
            name: name.to_os_string(),
            ino,
        });
        Ok(())
    }
}

// similar to the above, but holding file metadata
struct File {
    ino: u64,
    chunk_list: FileChunkList,
    md: fs::Metadata,
    additional: Option<InodeAdditional>,
}

fn write_chunks_to_oci(oci: &Image, fcdc: &mut FastCDCWrapper) -> Result<Vec<FileChunk>> {
    let mut pending_chunks = Vec::<ChunkWithData>::new();
    fcdc.get_pending_chunks(&mut pending_chunks);
    pending_chunks
        .iter_mut()
        .map(|c| {
            let desc = oci.put_blob::<_, compression::Noop, media_types::Chunk>(&*c.data)?;
            Ok(FileChunk {
                blob: BlobRef {
                    kind: BlobRefKind::Other {
                        digest: desc.digest.underlying(),
                    },
                    offset: 0,
                },
                len: desc.size,
            })
        })
        .collect::<Result<Vec<FileChunk>>>()
}

fn take_first_chunk<FileChunk>(v: &mut Vec<FileChunk>) -> io::Result<FileChunk> {
    if !v.is_empty() {
        Ok(v.remove(0))
    } else {
        Err(io::Error::new(io::ErrorKind::Other, "missing blob"))
    }
}

fn merge_chunks_and_prev_files(
    chunks: &mut Vec<FileChunk>,
    files: &mut Vec<File>,
    prev_files: &mut Vec<File>,
) -> io::Result<FileChunk> {
    let mut chunk_used = 0;
    let mut chunk = take_first_chunk(chunks)?;

    for mut file in prev_files.drain(..) {
        let mut file_used: u64 = Iterator::sum(file.chunk_list.chunks.iter().map(|c| c.len));
        while file_used < file.md.len() {
            if chunk_used == chunk.len {
                chunk_used = 0;
                chunk = take_first_chunk(chunks)?;
            }

            let room = min(file.md.len() - file_used, chunk.len - chunk_used);
            let blob = BlobRef {
                offset: chunk_used,
                kind: chunk.blob.kind,
            };
            file.chunk_list.chunks.push(FileChunk { blob, len: room });
            chunk_used += room;
            file_used += room;
        }
        files.push(file);
    }

    if chunk_used == chunk.len {
        take_first_chunk(chunks)
    } else {
        // fix up the first chunk to have the right offset for this file
        Ok(FileChunk {
            blob: BlobRef {
                kind: chunk.blob.kind,
                offset: chunk_used,
            },
            len: chunk.len - chunk_used,
        })
    }
}

fn inode_encoded_size(num_inodes: usize) -> usize {
    format::cbor_size_of_list_header(num_inodes) + num_inodes * format::INODE_WIRE_SIZE
}

pub fn build_initial_rootfs(rootfs: &Path, oci: &Image) -> Result<Descriptor> {
    let mut dirs = HashMap::<u64, Dir>::new();
    let mut files = Vec::<File>::new();
    let mut pfs_inodes = Vec::<Inode>::new();

    // host to puzzlefs inode mapping for hard link deteciton
    let mut host_to_pfs = HashMap::<u64, Ino>::new();

    let mut cur_ino: u64 = 1;

    let mut fcdc = FastCDCWrapper::new();
    let mut prev_files = Vec::<File>::new();

    for entry in walker(rootfs) {
        let e = entry.map_err(io::Error::from)?;
        let md = e.metadata().map_err(io::Error::from)?;

        // now that we know the ino of this thing, let's put it in the parent directory (assuming
        // this is not "/" for our image, aka inode #1)
        if cur_ino != 1 {
            // is this a hard link? if so, just use the existing ino we have rendered. otherewise,
            // use a new one
            let the_ino = host_to_pfs.get(&md.ino()).copied().unwrap_or(cur_ino);
            let parent_path = e.path().parent().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::Other,
                    format!("no parent for {}", e.path().display()),
                )
            })?;
            let parent = dirs
                .get_mut(&fs::symlink_metadata(parent_path)?.ino())
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::Other,
                        format!("no pfs inode for {}", e.path().display()),
                    )
                })?;
            parent.add_entry(e.path(), the_ino)?;

            // if it was a hard link, we don't need to actually render it again
            if host_to_pfs.get(&md.ino()).is_some() {
                continue;
            }
        }

        host_to_pfs.insert(md.ino(), cur_ino);

        // render as much of the inode as we can
        let additional = InodeAdditional::new(e.path(), &md)?;
        if md.is_dir() {
            dirs.insert(
                md.ino(),
                Dir {
                    ino: cur_ino,
                    md,
                    dir_list: DirList {
                        entries: Vec::<DirEnt>::new(),
                        look_below: false,
                    },
                    additional,
                },
            );
        } else if md.is_file() {
            let mut f = fs::File::open(e.path())?;
            io::copy(&mut f, &mut fcdc)?;

            let mut written_chunks = write_chunks_to_oci(oci, &mut fcdc)?;
            let mut file = File {
                ino: cur_ino,
                md,
                chunk_list: FileChunkList {
                    chunks: Vec::<FileChunk>::new(),
                },
                additional,
            };

            if written_chunks.is_empty() {
                // this file wasn't big enough to cause a chunk to be generated, add it to the list
                // of files pending for this chunk
                prev_files.push(file);
            } else {
                let fixed_chunk =
                    merge_chunks_and_prev_files(&mut written_chunks, &mut files, &mut prev_files)?;
                file.chunk_list.chunks.push(fixed_chunk);
                file.chunk_list.chunks.append(&mut written_chunks);
            }
        } else {
            let inode = Inode::new_other(cur_ino, &md, None /* TODO: additional */)?;
            pfs_inodes.push(inode);
        }

        cur_ino += 1;
    }

    // all inodes done, we need to finish up the cdc chunking
    fcdc.finish();
    let mut written_chunks = write_chunks_to_oci(oci, &mut fcdc)?;

    // if we have chunks, we should have files too
    assert!(written_chunks.is_empty() || !prev_files.is_empty());
    assert!(!written_chunks.is_empty() || prev_files.is_empty());

    if !written_chunks.is_empty() {
        // merge everything leftover with all previous files. we expect an error here, since the in
        // put shoudl be exactly consumed and the final take_first_chunk() call should fail. TODO:
        // rearrange this to be less ugly.
        merge_chunks_and_prev_files(&mut written_chunks, &mut files, &mut prev_files).unwrap_err();

        // we should have consumed all the chunks.
        assert!(written_chunks.is_empty());
    }

    // total inode serailized size
    let num_inodes = pfs_inodes.len() + dirs.len() + files.len();
    let inodes_serial_size = inode_encoded_size(num_inodes);

    // TODO: not render this whole thing in memory, stick it all in the same blob, etc.
    let mut dir_buf = Vec::<u8>::new();

    // render dirs
    pfs_inodes.extend(
        dirs.values_mut()
            .collect::<Vec<_>>()
            .drain(..)
            .map(|d| {
                let dir_list_offset = inodes_serial_size + dir_buf.len();
                serde_cbor::to_writer(&mut dir_buf, &d.dir_list)?;
                let additional_ref = d
                    .additional
                    .as_ref()
                    .map::<Result<BlobRef>, _>(|add| {
                        let offset = inodes_serial_size + dir_buf.len();
                        serde_cbor::to_writer(&mut dir_buf, &add)?;
                        Ok(BlobRef {
                            offset: offset as u64,
                            kind: BlobRefKind::Local,
                        })
                    })
                    .transpose()?;
                Ok(Inode::new_dir(
                    d.ino,
                    &d.md,
                    dir_list_offset as u64,
                    additional_ref,
                )?)
            })
            .collect::<Result<Vec<Inode>>>()?,
    );

    let mut files_buf = Vec::<u8>::new();

    // render files
    pfs_inodes.extend(
        files
            .drain(..)
            .map(|f| {
                let chunk_offset = inodes_serial_size + dir_buf.len() + files_buf.len();
                serde_cbor::to_writer(&mut files_buf, &f.chunk_list)?;
                let additional_ref = f
                    .additional
                    .as_ref()
                    .map::<Result<BlobRef>, _>(|add| {
                        let offset = inodes_serial_size + dir_buf.len() + files_buf.len();
                        serde_cbor::to_writer(&mut files_buf, &add)?;
                        Ok(BlobRef {
                            offset: offset as u64,
                            kind: BlobRefKind::Local,
                        })
                    })
                    .transpose()?;
                Ok(Inode::new_file(
                    f.ino,
                    &f.md,
                    chunk_offset as u64,
                    additional_ref,
                )?)
            })
            .collect::<Result<Vec<Inode>>>()?,
    );
    pfs_inodes.sort_by(|a, b| a.ino.cmp(&b.ino));

    let mut md_buf = Vec::<u8>::with_capacity(inodes_serial_size + dir_buf.len() + files_buf.len());
    serde_cbor::to_writer(&mut md_buf, &pfs_inodes)?;

    assert_eq!(md_buf.len(), inodes_serial_size);

    md_buf.append(&mut dir_buf);
    md_buf.append(&mut files_buf);

    let desc = oci.put_blob::<_, compression::Noop, media_types::Inodes>(md_buf.as_slice())?;
    let metadatas = [BlobRef {
        offset: 0,
        kind: BlobRefKind::Other {
            digest: desc.digest.underlying(),
        },
    }]
    .to_vec();

    let mut rootfs_buf = Vec::new();
    serde_cbor::to_writer(&mut rootfs_buf, &Rootfs { metadatas })?;
    oci.put_blob::<_, compression::Noop, media_types::Rootfs>(rootfs_buf.as_slice())
}

// TODO: figure out how to guard this with #[cfg(test)]
pub fn build_test_fs(image: &Image) -> Result<Descriptor> {
    build_initial_rootfs(Path::new("../builder/test"), image)
}

#[cfg(test)]
pub mod tests {
    use super::*;

    use std::convert::TryInto;

    use tempfile::tempdir;

    use format::{DirList, InodeMode};

    #[test]
    fn test_fs_generation() {
        // TODO: verify the hash value here since it's only one thing? problem is as we change the
        // encoding/add stuff to it, the hash will keep changing and we'll have to update the
        // test...
        //
        // but once all that's stabalized, we should verify the metadata hash too.
        let dir = tempdir().unwrap();
        let image = Image::new(dir.path()).unwrap();
        let rootfs_desc = build_test_fs(&image).unwrap();
        let rootfs = Rootfs::open(
            image
                .open_compressed_blob::<compression::Noop>(&rootfs_desc.digest)
                .unwrap(),
        )
        .unwrap();

        // there should be a blob that matches the hash of the test data, since it all gets input
        // as one chunk and there's only one file
        const FILE_DIGEST: &str =
            "d9e749d9367fc908876749d6502eb212fee88c9a94892fb07da5ef3ba8bc39ed";

        let md = fs::symlink_metadata(image.blob_path().join(FILE_DIGEST)).unwrap();
        assert!(md.is_file());

        let metadata_digest = rootfs.metadatas[0].try_into().unwrap();
        let mut blob = image
            .open_metadata_blob::<compression::Noop>(&metadata_digest)
            .unwrap();
        let inodes = blob.read_inodes().unwrap();

        // we can at least deserialize inodes and they look sane
        assert_eq!(inodes.len(), 2);

        assert_eq!(blob.find_inode(1).unwrap().unwrap(), inodes[0]);
        assert_eq!(blob.find_inode(2).unwrap().unwrap(), inodes[1]);

        assert_eq!(inodes[0].ino, 1);
        if let InodeMode::Dir { offset } = inodes[0].mode {
            let dir_list: DirList = blob.read_dir_list(offset).unwrap();
            assert_eq!(dir_list.entries.len(), 1);
            assert_eq!(dir_list.entries[0].ino, 2);
            assert_eq!(dir_list.entries[0].name, "SekienAkashita.jpg");
        } else {
            panic!("bad inode mode: {:?}", inodes[0].mode);
        }
        assert_eq!(inodes[0].uid, md.uid());
        assert_eq!(inodes[0].gid, md.gid());

        assert_eq!(inodes[1].ino, 2);
        assert_eq!(inodes[1].uid, md.uid());
        assert_eq!(inodes[1].gid, md.gid());
        if let InodeMode::Reg { offset } = inodes[1].mode {
            let chunks = blob.read_file_chunks(offset).unwrap();
            assert_eq!(chunks.len(), 1);
            assert_eq!(chunks[0].len, md.len());
        } else {
            panic!("bad inode mode: {:?}", inodes[1].mode);
        }
    }
}
