use fs_verity::FsVeritySha256;
use std::cmp::min;
use std::collections::{BTreeMap, HashMap};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::Arc;

use sha2::digest::FixedOutput;
use walkdir::WalkDir;

use format::{
    BlobRef, BlobRefKind, DirEnt, DirList, FileChunk, FileChunkList, Ino, Inode, InodeAdditional,
    Result, Rootfs, VerityData, WireFormatError, SHA256_BLOCK_SIZE,
};
use oci::media_types;
use oci::{Descriptor, Image};
use reader::PuzzleFS;

use nix::errno::Errno;

use fastcdc::v2020::StreamCDC;
mod filesystem;
use filesystem::FilesystemStream;

const MIN_CHUNK_SIZE: u32 = 1024 * 1024;
const AVG_CHUNK_SIZE: u32 = 4 * 1024 * 1024;
const MAX_CHUNK_SIZE: u32 = 16 * 1024 * 1024;

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

fn get_fs_verity_digest(data: &[u8]) -> Result<[u8; SHA256_BLOCK_SIZE]> {
    let mut digest = FsVeritySha256::new();
    digest.write_all(data)?;
    let result = digest.finalize_fixed();
    Ok(result.into())
}

fn process_chunks(
    oci: &Image,
    mut chunker: StreamCDC,
    files: &mut [File],
    verity_data: &mut VerityData,
) -> Result<()> {
    let mut file_iter = files.iter_mut();
    let mut file_used = 0;
    let mut file = None;
    for f in file_iter.by_ref() {
        if f.md.size() > 0 {
            file = Some(f);
            break;
        }
    }

    'outer: for result in &mut chunker {
        let chunk = result.unwrap();
        let mut chunk_used: u64 = 0;

        let desc = oci.put_blob::<_, compression::Noop, media_types::Chunk>(&*chunk.data)?;
        let blob_kind = BlobRefKind::Other {
            digest: desc.digest.underlying(),
        };

        let verity_hash = get_fs_verity_digest(&chunk.data)?;
        verity_data.insert(desc.digest.underlying(), verity_hash);

        while chunk_used < chunk.length as u64 {
            let room = min(
                file.as_ref().unwrap().md.len() - file_used,
                chunk.length as u64 - chunk_used,
            );

            let blob = BlobRef {
                offset: chunk_used,
                kind: blob_kind,
            };

            file.as_mut()
                .unwrap()
                .chunk_list
                .chunks
                .push(FileChunk { blob, len: room });

            chunk_used += room;
            file_used += room;

            // get next file
            if file_used == file.as_ref().unwrap().md.len() {
                file_used = 0;
                file = None;

                for f in file_iter.by_ref() {
                    if f.md.size() > 0 {
                        file = Some(f);
                        break;
                    }
                }

                if file.is_none() {
                    break 'outer;
                }
            }
        }
    }

    // If there are no files left we also expect there are no chunks left
    assert!(chunker.next().is_none());

    Ok(())
}

fn inode_encoded_size(num_inodes: usize) -> usize {
    format::cbor_size_of_list_header(num_inodes) + num_inodes * format::INODE_WIRE_SIZE
}

fn build_delta(
    rootfs: &Path,
    oci: &Image,
    mut existing: Option<PuzzleFS>,
    verity_data: &mut VerityData,
) -> Result<Descriptor> {
    let mut dirs = HashMap::<u64, Dir>::new();
    let mut files = Vec::<File>::new();
    let mut others = Vec::<Other>::new();
    let mut pfs_inodes = Vec::<Inode>::new();
    let mut fs_stream = FilesystemStream::new();

    // host to puzzlefs inode mapping for hard link deteciton
    let mut host_to_pfs = HashMap::<u64, Ino>::new();

    let mut next_ino: u64 = existing
        .as_mut()
        .map(|pfs| pfs.max_inode().map(|i| i + 1))
        .unwrap_or_else(|| Ok(2))?;

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
    let root_metadata = fs::symlink_metadata(rootfs)?;
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

        let mut new_dirents = fs::read_dir(d.path())?.collect::<io::Result<Vec<fs::DirEntry>>>()?;
        // sort the entries so we have reproducible puzzlefs images
        new_dirents.sort_by_key(|a| a.file_name());

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
                fs_stream.push(&e.path());

                let file = File {
                    ino: cur_ino,
                    md,
                    chunk_list: FileChunkList {
                        chunks: Vec::<FileChunk>::new(),
                    },
                    additional,
                };

                files.push(file);
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

    let fcdc = StreamCDC::new(
        Box::new(fs_stream),
        MIN_CHUNK_SIZE,
        AVG_CHUNK_SIZE,
        MAX_CHUNK_SIZE,
    );
    process_chunks(oci, fcdc, &mut files, verity_data)?;

    // total inode serailized size
    let num_inodes = pfs_inodes.len() + dirs.len() + files.len() + others.len();
    let inodes_serial_size = inode_encoded_size(num_inodes);

    // TODO: not render this whole thing in memory, stick it all in the same blob, etc.
    let mut dir_buf = Vec::<u8>::new();

    let mut sorted_dirs = dirs.values_mut().collect::<Vec<_>>();
    sorted_dirs.sort_by(|a, b| a.ino.cmp(&b.ino));

    // render dirs
    pfs_inodes.extend(
        sorted_dirs
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

    let desc = oci.put_blob::<_, compression::Noop, media_types::Inodes>(md_buf.as_slice())?;
    let verity_hash = get_fs_verity_digest(md_buf.as_slice())?;
    verity_data.insert(desc.digest.underlying(), verity_hash);

    Ok(desc)
}

pub fn build_initial_rootfs(rootfs: &Path, oci: &Image) -> Result<Descriptor> {
    let mut verity_data: VerityData = BTreeMap::new();
    let desc = build_delta(rootfs, oci, None, &mut verity_data)?;
    let metadatas = [BlobRef {
        offset: 0,
        kind: BlobRefKind::Other {
            digest: desc.digest.underlying(),
        },
    }]
    .to_vec();

    let mut rootfs_buf = Vec::new();
    serde_cbor::to_writer(
        &mut rootfs_buf,
        &Rootfs {
            metadatas,
            fs_verity_data: verity_data,
        },
    )?;
    oci.put_blob::<_, compression::Noop, media_types::Rootfs>(rootfs_buf.as_slice())
}

// add_rootfs_delta adds whatever the delta between the current rootfs and the puzzlefs
// representation from the tag is.
pub fn add_rootfs_delta(rootfs: &Path, oci: Image, tag: &str) -> Result<(Descriptor, Arc<Image>)> {
    let mut verity_data: VerityData = BTreeMap::new();
    let pfs = PuzzleFS::open(oci, tag)?;
    let oci = Arc::clone(&pfs.oci);
    let desc = build_delta(rootfs, &oci, Some(pfs), &mut verity_data)?;
    let mut rootfs = oci.open_rootfs_blob::<compression::Noop>(tag)?;
    let br = BlobRef {
        kind: BlobRefKind::Other {
            digest: desc.digest.underlying(),
        },
        offset: 0,
    };
    rootfs.metadatas.insert(0, br);
    rootfs.fs_verity_data.extend(verity_data);
    let mut rootfs_buf = Vec::new();
    serde_cbor::to_writer(&mut rootfs_buf, &rootfs)?;
    Ok((
        oci.put_blob::<_, compression::Noop, media_types::Rootfs>(rootfs_buf.as_slice())?,
        oci,
    ))
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
    use std::path::PathBuf;
    use tempfile::TempDir;

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
        let mut blob = image.open_metadata_blob(&metadata_digest).unwrap();
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

        let (desc, image) = add_rootfs_delta(&delta_dir, image, &tag).unwrap();
        let new_tag = "test2".to_string();
        image.add_tag(new_tag.to_string(), desc).unwrap();
        let delta = image
            .open_rootfs_blob::<compression::Noop>(&new_tag)
            .unwrap();
        assert_eq!(delta.metadatas.len(), 2);

        let image = Image::new(dir.path()).unwrap();
        let mut pfs = PuzzleFS::open(image, &new_tag).unwrap();
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

    fn do_vecs_match<T: PartialEq>(a: &[T], b: &[T]) -> bool {
        if a.len() != b.len() {
            return false;
        }

        let matching = a.iter().zip(b.iter()).filter(|&(a, b)| a == b).count();
        matching == a.len()
    }

    fn get_image_blobs(image: &Image) -> Vec<OsString> {
        WalkDir::new(image.blob_path())
            .contents_first(false)
            .follow_links(false)
            .same_file_system(true)
            .sort_by(|a, b| a.file_name().cmp(b.file_name()))
            .into_iter()
            .skip(1)
            .map(|x| OsString::from(x.unwrap().path().file_stem().unwrap()))
            .collect::<Vec<OsString>>()
    }

    // given the same directory, test whether building it multiple times results in the same puzzlefs image
    fn same_dir_reproducible(path: &Path) -> bool {
        let dirs: [_; 10] = std::array::from_fn(|_| tempdir().unwrap());
        let mut sha_suite = Vec::new();
        let images = dirs
            .iter()
            .map(|dir| Image::new(dir.path()).unwrap())
            .collect::<Vec<Image>>();

        for (i, image) in images.iter().enumerate() {
            build_test_fs(path, image).unwrap();
            let ents = get_image_blobs(image);
            sha_suite.push(ents);

            if i != 0 && !do_vecs_match(&sha_suite[i - 1], &sha_suite[i]) {
                println!("not matching at iteration: {i}");
                return false;
            }
        }

        true
    }

    // given the same directory contents, test whether building them from multiple paths results in the same puzzlefs image
    fn same_dir_contents_reproducible(path: &[PathBuf]) -> bool {
        let dirs = path.iter().map(|_| tempdir().unwrap()).collect::<Vec<_>>();
        let mut sha_suite = Vec::new();
        let images = dirs
            .iter()
            .map(|dir| Image::new(dir.path()).unwrap())
            .collect::<Vec<Image>>();

        for (i, image) in images.iter().enumerate() {
            build_test_fs(&path[i], image).unwrap();
            let ents = get_image_blobs(image);
            sha_suite.push(ents);

            if i != 0 && !do_vecs_match(&sha_suite[i - 1], &sha_suite[i]) {
                println!("not matching at iteration: {i}");
                return false;
            }
        }

        true
    }

    #[test]
    fn test_reproducibility() {
        fn build_dummy_fs(dir: &Path) -> PathBuf {
            let rootfs = dir.join("rootfs");
            let subdirs = ["foo", "bar", "baz"];
            let files = ["foo_file", "bar_file", "baz_file"];

            for subdir in subdirs {
                let path = rootfs.join(subdir);
                fs::create_dir_all(path).unwrap();
            }

            for file in files {
                let path = rootfs.join(file);
                fs::write(path, b"some file contents").unwrap();
            }

            rootfs
        }

        let dir = tempdir().unwrap();
        let rootfs = build_dummy_fs(dir.path());

        assert!(
            same_dir_reproducible(&rootfs),
            "build not reproducible for {}",
            rootfs.display()
        );

        let dirs: [_; 10] = std::array::from_fn(|i| match i % 2 == 0 {
            // if /tmp and the current dir reside on different filesystems there are better chances
            // for read_dir (which uses readdir under the hood) to yield a different order of the files
            true => tempdir().unwrap(),
            false => TempDir::new_in(".").unwrap(),
        });
        let rootfses = dirs
            .iter()
            .map(|dir| build_dummy_fs(dir.path()))
            .collect::<Vec<PathBuf>>();

        assert!(
            same_dir_contents_reproducible(&rootfses),
            "build not reproducible"
        );
    }
}
