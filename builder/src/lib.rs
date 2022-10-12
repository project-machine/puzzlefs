use std::cmp::min;
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use walkdir::WalkDir;

use format::{
    BlobRef, BlobRefKind, DirEnt, DirList, FileChunk, FileChunkList, Ino, Inode, InodeAdditional,
    Result, Rootfs, WireFormatError,
};
use oci::media_types;
use oci::{Descriptor, Image};
use reader::PuzzleFS;

use nix::errno::Errno;

mod fastcdc_fs;
use fastcdc_fs::{ChunkWithData, FastCDCWrapper};

fn walker(rootfs: &Path) -> WalkDir {
    // breadth first search for sharing, don't cross filesystems just to be safe, order by file
    // name. we only return directories here, so we can more easily do delta generation to detect
    // what's missing in an existing puzzlefs.
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
    fn add_entry(&mut self, name: OsString, ino: Ino) {
        self.dir_list.entries.push(DirEnt { name, ino });
    }
}

// similar to the above, but holding file metadata
struct File {
    ino: u64,
    chunk_list: FileChunkList,
    md: fs::Metadata,
    additional: Option<InodeAdditional>,
}

struct Other {
    ino: u64,
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

fn build_delta(rootfs: &Path, oci: &Image, mut existing: Option<PuzzleFS>) -> Result<Descriptor> {
    let mut dirs = HashMap::<u64, Dir>::new();
    let mut files = Vec::<File>::new();
    let mut others = Vec::<Other>::new();
    let mut pfs_inodes = Vec::<Inode>::new();

    // host to puzzlefs inode mapping for hard link deteciton
    let mut host_to_pfs = HashMap::<u64, Ino>::new();

    let mut next_ino: u64 = existing
        .as_mut()
        .map(|pfs| pfs.max_inode().map(|i| i + 1))
        .unwrap_or(Ok(2))?;

    let mut fcdc = FastCDCWrapper::new();
    let mut prev_files = Vec::<File>::new();

    fn lookup_existing(existing: &mut Option<PuzzleFS>, p: &Path) -> Result<Option<reader::Inode>> {
        existing
            .as_mut()
            .map(|pfs| pfs.lookup(p))
            .transpose()
            .map(|o| o.flatten())
    }

    let rootfs_dirs = walker(rootfs)
        .into_iter()
        .filter_entry(|de| de.metadata().map(|md| md.is_dir()).unwrap_or(true));

    // we specially create the "/" InodeMode::Dir object, since we will not iterate over it as a
    // child of some other directory
    let root_metadata = fs::symlink_metadata(&rootfs)?;
    let root_additional = InodeAdditional::new(rootfs, &root_metadata)?;
    dirs.insert(
        root_metadata.ino(),
        Dir {
            ino: 1,
            md: root_metadata,
            dir_list: DirList {
                entries: Vec::<DirEnt>::new(),
                look_below: false,
            },
            additional: root_additional,
        },
    );

    let rootfs_relative = |p: &Path| {
        // .unwrap() here because we assume no programmer errors in this function (i.e. it is a
        // puzzlefs bug here)
        Path::new("/").join(p.strip_prefix(rootfs).unwrap())
    };

    for dir in rootfs_dirs {
        let d = dir.map_err(io::Error::from)?;
        let dir_path = rootfs_relative(d.path());
        let existing_dirents: Vec<_> = lookup_existing(&mut existing, &dir_path)?
            .and_then(|ex| -> Option<Vec<_>> {
                if let reader::InodeMode::Dir { entries } = ex.mode {
                    Some(entries)
                } else {
                    None
                }
            })
            .unwrap_or_default();

        let new_dirents = fs::read_dir(d.path())?.collect::<io::Result<Vec<fs::DirEntry>>>()?;

        // add whiteout information
        let this_metadata = fs::symlink_metadata(d.path())?;
        let this_dir = dirs
            .get_mut(&this_metadata.ino())
            .ok_or_else(|| WireFormatError::from_errno(Errno::ENOENT))?;
        for (name, ino) in existing_dirents {
            if !(new_dirents)
                .iter()
                .any(|new| new.path().file_name().unwrap_or_else(|| OsStr::new("")) == name)
            {
                pfs_inodes.push(Inode::new_whiteout(ino));
                this_dir.add_entry(name, ino);
            }
        }

        for e in new_dirents {
            let md = e.metadata()?;

            let existing_inode = existing
                .as_mut()
                .map(|pfs| {
                    let puzzlefs_path = rootfs_relative(&e.path());
                    pfs.lookup(&puzzlefs_path)
                })
                .transpose()?
                .flatten();

            let cur_ino = existing_inode.map(|ex| ex.inode.ino).unwrap_or_else(|| {
                let next = next_ino;
                next_ino += 1;
                next
            });

            // now that we know the ino of this thing, let's put it in the parent directory (assuming
            // this is not "/" for our image, aka inode #1)
            if cur_ino != 1 {
                // is this a hard link? if so, just use the existing ino we have rendered. otherewise,
                // use a new one
                let the_ino = host_to_pfs.get(&md.ino()).copied().unwrap_or(cur_ino);
                let parent_path = e.path().parent().map(|p| p.to_path_buf()).ok_or_else(|| {
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
                parent.add_entry(
                    e.path()
                        .file_name()
                        .unwrap_or_else(|| OsStr::new(""))
                        .to_os_string(),
                    the_ino,
                );

                // if it was a hard link, we don't need to actually render it again
                if host_to_pfs.get(&md.ino()).is_some() {
                    continue;
                }
            }

            host_to_pfs.insert(md.ino(), cur_ino);

            // render as much of the inode as we can
            // TODO: here are a bunch of optimizations we should do: no need to re-render things
            // that are the same (whole inodes, metadata, etc.). For now we just re-render the
            // whole metadata tree.
            let additional = InodeAdditional::new(&e.path(), &md)?;

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
                    let fixed_chunk = merge_chunks_and_prev_files(
                        &mut written_chunks,
                        &mut files,
                        &mut prev_files,
                    )?;
                    file.chunk_list.chunks.push(fixed_chunk);
                    file.chunk_list.chunks.append(&mut written_chunks);
                }
            } else {
                let o = Other {
                    ino: cur_ino,
                    md,
                    additional,
                };
                others.push(o);
            }
        }
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
    let num_inodes = pfs_inodes.len() + dirs.len() + files.len() + others.len();
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

    let mut others_buf = Vec::<u8>::new();

    pfs_inodes.extend(
        others
            .drain(..)
            .map(|o| {
                let additional_ref = o
                    .additional
                    .as_ref()
                    .map::<Result<BlobRef>, _>(|add| {
                        let offset =
                            inodes_serial_size + dir_buf.len() + files_buf.len() + others_buf.len();
                        serde_cbor::to_writer(&mut others_buf, &add)?;
                        Ok(BlobRef {
                            offset: offset as u64,
                            kind: BlobRefKind::Local,
                        })
                    })
                    .transpose()?;
                Ok(Inode::new_other(o.ino, &o.md, additional_ref)?)
            })
            .collect::<Result<Vec<Inode>>>()?,
    );

    pfs_inodes.sort_by(|a, b| a.ino.cmp(&b.ino));

    let mut md_buf = Vec::<u8>::with_capacity(
        inodes_serial_size + dir_buf.len() + files_buf.len() + others_buf.len(),
    );
    serde_cbor::to_writer(&mut md_buf, &pfs_inodes)?;

    assert_eq!(md_buf.len(), inodes_serial_size);

    md_buf.append(&mut dir_buf);
    md_buf.append(&mut files_buf);
    md_buf.append(&mut others_buf);

    oci.put_blob::<_, compression::Noop, media_types::Inodes>(md_buf.as_slice())
}

pub fn build_initial_rootfs(rootfs: &Path, oci: &Image) -> Result<Descriptor> {
    let desc = build_delta(rootfs, oci, None)?;
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

// add_rootfs_delta adds whatever the delta between the current rootfs and the puzzlefs
// representation from the tag is.
pub fn add_rootfs_delta(rootfs: &Path, oci: &Image, tag: &str) -> Result<()> {
    let pfs = PuzzleFS::open(oci, tag)?;
    let desc = build_delta(rootfs, oci, Some(pfs))?;
    let mut rootfs = oci.open_rootfs_blob::<compression::Noop>(tag)?;
    let br = BlobRef {
        kind: BlobRefKind::Other {
            digest: desc.digest.underlying(),
        },
        offset: 0,
    };
    rootfs.metadatas.insert(0, br);
    let mut rootfs_buf = Vec::new();
    serde_cbor::to_writer(&mut rootfs_buf, &rootfs)?;
    let rootfs_desc =
        oci.put_blob::<_, compression::Noop, media_types::Rootfs>(rootfs_buf.as_slice())?;
    oci.add_tag(tag.to_string(), rootfs_desc)
}

// TODO: figure out how to guard this with #[cfg(test)]
pub fn build_test_fs(path: &Path, image: &Image) -> Result<Descriptor> {
    build_initial_rootfs(path, image)
}

#[cfg(test)]
pub mod tests {
    use super::*;

    use std::convert::TryInto;

    use tempfile::tempdir;

    use format::{DirList, InodeMode};
    use reader::WalkPuzzleFS;

    #[test]
    fn test_fs_generation() {
        // TODO: verify the hash value here since it's only one thing? problem is as we change the
        // encoding/add stuff to it, the hash will keep changing and we'll have to update the
        // test...
        //
        // but once all that's stabalized, we should verify the metadata hash too.
        let dir = tempdir().unwrap();
        let image = Image::new(dir.path()).unwrap();
        let rootfs_desc = build_test_fs(Path::new("../builder/test/test-1"), &image).unwrap();
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

    #[test]
    fn test_delta_generation() {
        let dir = tempdir().unwrap();
        let image = Image::new(dir.path()).unwrap();
        let rootfs_desc = build_test_fs(Path::new("../builder/test/test-1"), &image).unwrap();
        let tag = "test".to_string();
        image.add_tag(tag.to_string(), rootfs_desc).unwrap();

        let delta_dir = dir.path().join(Path::new("delta"));
        fs::create_dir_all(delta_dir.join(Path::new("foo"))).unwrap();
        fs::copy(
            Path::new("../builder/test/test-1/SekienAkashita.jpg"),
            delta_dir.join("SekienAkashita.jpg"),
        )
        .unwrap();

        add_rootfs_delta(&delta_dir, &image, &tag).unwrap();
        let delta = image.open_rootfs_blob::<compression::Noop>(&tag).unwrap();
        assert_eq!(delta.metadatas.len(), 2);

        let mut pfs = PuzzleFS::open(&image, &tag).unwrap();
        assert_eq!(pfs.max_inode().unwrap(), 3);
        let mut walker = WalkPuzzleFS::walk(&mut pfs).unwrap();

        let root = walker.next().unwrap().unwrap();
        assert_eq!(root.path.to_string_lossy(), "/");
        assert_eq!(root.inode.inode.ino, 1);
        assert_eq!(root.inode.dir_entries().unwrap().len(), 2);

        let jpg_file = walker.next().unwrap().unwrap();
        assert_eq!(jpg_file.path.to_string_lossy(), "/SekienAkashita.jpg");
        assert_eq!(jpg_file.inode.inode.ino, 2);
        assert_eq!(jpg_file.inode.file_len().unwrap(), 109466);

        let foo_dir = walker.next().unwrap().unwrap();
        assert_eq!(foo_dir.path.to_string_lossy(), "/foo");
        assert_eq!(foo_dir.inode.inode.ino, 3);
        assert_eq!(foo_dir.inode.dir_entries().unwrap().len(), 0);

        assert!(walker.next().is_none());
    }
}
