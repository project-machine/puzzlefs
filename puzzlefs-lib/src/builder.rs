use crate::common::{AVG_CHUNK_SIZE, MAX_CHUNK_SIZE, MIN_CHUNK_SIZE};
use crate::compression::{Compression, Noop, Zstd};
use crate::fsverity_helpers::{
    check_fs_verity, fsverity_enable, InnerHashAlgorithm, FS_VERITY_BLOCK_SIZE_DEFAULT,
};
use crate::oci::Digest;
use std::any::Any;
use std::backtrace::Backtrace;
use std::cmp::min;
use std::collections::{BTreeMap, HashMap};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::Arc;

use walkdir::WalkDir;

use crate::format::{
    BlobRef, DirEnt, DirList, FileChunk, FileChunkList, Ino, Inode, InodeAdditional, InodeMode,
    Result, Rootfs, VerityData, WireFormatError,
};
use crate::metadata_capnp;
use crate::oci::media_types;
use crate::oci::{Descriptor, Image};
use crate::reader::{PuzzleFS, PUZZLEFS_IMAGE_MANIFEST_VERSION};
use ocidir::oci_spec::image::{ImageManifest, Platform};

use nix::errno::Errno;

use fastcdc::v2020::StreamCDC;
mod filesystem;
use filesystem::FilesystemStream;

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
        self.dir_list.entries.push(DirEnt {
            name: OsString::into_vec(name),
            ino,
        });
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

fn serialize_metadata(rootfs: Rootfs) -> Result<Vec<u8>> {
    let mut message = ::capnp::message::Builder::new_default();
    let mut capnp_rootfs = message.init_root::<metadata_capnp::rootfs::Builder<'_>>();

    rootfs.fill_capnp(&mut capnp_rootfs)?;

    let mut buf = Vec::new();
    ::capnp::serialize::write_message(&mut buf, &message)?;
    Ok(buf)
}

fn process_chunks<C: Compression + Any>(
    oci: &Image,
    mut chunker: StreamCDC,
    files: &mut [File],
    verity_data: &mut VerityData,
    image_manifest: &mut ImageManifest,
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

        let (desc, fs_verity_digest, compressed) =
            oci.put_blob::<C>(&chunk.data, image_manifest, media_types::Chunk {})?;
        let digest = Digest::try_from(desc.digest().digest())?.underlying();

        let verity_hash = fs_verity_digest;
        verity_data.insert(digest, verity_hash);

        while chunk_used < chunk.length as u64 {
            let room = min(
                file.as_ref().unwrap().md.len() - file_used,
                chunk.length as u64 - chunk_used,
            );

            let blob = BlobRef {
                offset: chunk_used,
                digest,
                compressed,
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

fn build_delta<C: Compression + Any>(
    rootfs: &Path,
    oci: &Image,
    mut existing: Option<PuzzleFS>,
    verity_data: &mut VerityData,
    image_manifest: &mut ImageManifest,
) -> Result<Vec<Inode>> {
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

    fn lookup_existing(existing: &mut Option<PuzzleFS>, p: &Path) -> Result<Option<Inode>> {
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
                if let InodeMode::Dir { dir_list } = ex.mode {
                    Some(dir_list.entries)
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
        for dir_ent in existing_dirents {
            if !(new_dirents).iter().any(|new| {
                new.path().file_name().unwrap_or_else(|| OsStr::new(""))
                    == OsStr::from_bytes(&dir_ent.name)
            }) {
                pfs_inodes.push(Inode::new_whiteout(dir_ent.ino));
                this_dir.add_entry(OsString::from_vec(dir_ent.name), dir_ent.ino);
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

            let cur_ino = existing_inode.map(|ex| ex.ino).unwrap_or_else(|| {
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
                if host_to_pfs.contains_key(&md.ino()) {
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
    process_chunks::<C>(oci, fcdc, &mut files, verity_data, image_manifest)?;

    // TODO: not render this whole thing in memory, stick it all in the same blob, etc.
    let mut sorted_dirs = dirs.into_values().collect::<Vec<_>>();

    // render dirs
    pfs_inodes.extend(
        sorted_dirs
            .drain(..)
            .map(|d| Ok(Inode::new_dir(d.ino, &d.md, d.dir_list, d.additional)?))
            .collect::<Result<Vec<Inode>>>()?,
    );

    // render files
    pfs_inodes.extend(
        files
            .drain(..)
            .map(|f| {
                Ok(Inode::new_file(
                    f.ino,
                    &f.md,
                    f.chunk_list.chunks,
                    f.additional,
                )?)
            })
            .collect::<Result<Vec<Inode>>>()?,
    );

    pfs_inodes.extend(
        others
            .drain(..)
            .map(|o| Ok(Inode::new_other(o.ino, &o.md, o.additional)?))
            .collect::<Result<Vec<Inode>>>()?,
    );

    pfs_inodes.sort_by(|a, b| a.ino.cmp(&b.ino));

    Ok(pfs_inodes)
}

pub fn build_initial_rootfs<C: Compression + Any>(
    rootfs: &Path,
    oci: &Image,
    tag: &str,
) -> Result<Descriptor> {
    let mut verity_data: VerityData = BTreeMap::new();
    let mut image_manifest = oci.get_empty_manifest()?;
    let inodes = build_delta::<C>(rootfs, oci, None, &mut verity_data, &mut image_manifest)?;

    let rootfs_buf = serialize_metadata(Rootfs {
        metadatas: vec![inodes],
        fs_verity_data: verity_data,
        manifest_version: PUZZLEFS_IMAGE_MANIFEST_VERSION,
    })?;

    let rootfs_descriptor = oci
        .put_blob::<Noop>(
            rootfs_buf.as_slice(),
            &mut image_manifest,
            media_types::Rootfs {},
        )?
        .0;
    oci.0
        .insert_manifest(image_manifest, Some(tag), Platform::default())?;

    Ok(rootfs_descriptor)
}

// add_rootfs_delta adds whatever the delta between the current rootfs and the puzzlefs
// representation from the tag is.
pub fn add_rootfs_delta<C: Compression + Any>(
    rootfs_path: &Path,
    oci: Image,
    tag: &str,
    base_layer: &str,
) -> Result<(Descriptor, Arc<Image>)> {
    let mut verity_data: VerityData = BTreeMap::new();
    let mut image_manifest = oci.get_empty_manifest()?;

    let pfs = PuzzleFS::open(oci, base_layer, None)?;
    let oci = Arc::clone(&pfs.oci);
    let mut rootfs = Rootfs::try_from(oci.open_rootfs_blob(base_layer, None)?)?;

    let inodes = build_delta::<C>(
        rootfs_path,
        &oci,
        Some(pfs),
        &mut verity_data,
        &mut image_manifest,
    )?;

    if !rootfs.metadatas.iter().any(|x| *x == inodes) {
        rootfs.metadatas.insert(0, inodes);
    }

    rootfs.fs_verity_data.extend(verity_data);
    let rootfs_buf = serialize_metadata(rootfs)?;
    let rootfs_descriptor = oci
        .put_blob::<Noop>(
            rootfs_buf.as_slice(),
            &mut image_manifest,
            media_types::Rootfs {},
        )?
        .0;
    oci.0
        .insert_manifest(image_manifest, Some(tag), Platform::default())?;
    Ok((rootfs_descriptor, oci))
}

fn enable_verity_for_file(file: &cap_std::fs::File) -> Result<()> {
    if let Err(e) = fsverity_enable(
        file.as_raw_fd(),
        FS_VERITY_BLOCK_SIZE_DEFAULT,
        InnerHashAlgorithm::Sha256,
        &[],
    ) {
        // if fsverity is enabled, ignore the error
        if e.kind() != std::io::ErrorKind::AlreadyExists {
            return Err(WireFormatError::from(e));
        }
    }
    Ok(())
}

fn enable_and_check_verity_for_file(file: &cap_std::fs::File, expected: &[u8]) -> Result<()> {
    enable_verity_for_file(file)?;
    check_fs_verity(file, expected)
}

pub fn enable_fs_verity(oci: Image, tag: &str, manifest_root_hash: &str) -> Result<()> {
    // first enable fs verity for the puzzlefs image manifest
    let manifest_fd = oci.get_image_manifest_fd(tag)?;
    enable_and_check_verity_for_file(&manifest_fd, &hex::decode(manifest_root_hash)?[..])?;

    let pfs = PuzzleFS::open(oci, tag, None)?;
    let oci = Arc::clone(&pfs.oci);
    let rootfs = oci.open_rootfs_blob(tag, None)?;

    let rootfs_fd = oci.get_pfs_rootfs(tag, None)?;
    let rootfs_verity = oci.get_pfs_rootfs_verity(tag)?;

    enable_and_check_verity_for_file(&rootfs_fd, &rootfs_verity[..])?;

    let manifest = oci
        .0
        .find_manifest_with_tag(tag)?
        .ok_or_else(|| WireFormatError::MissingManifest(tag.to_string(), Backtrace::capture()))?;
    let config_digest = manifest.config().digest().digest();
    let config_digest_path = oci.blob_path().join(config_digest);
    enable_verity_for_file(&oci.0.dir.open(config_digest_path)?)?;

    for (content_addressed_file, verity_hash) in rootfs.get_verity_data()? {
        let file_path = oci
            .blob_path()
            .join(Digest::new(&content_addressed_file).to_string());
        let fd = oci.0.dir.open(&file_path)?;
        if let Err(e) = fsverity_enable(
            fd.as_raw_fd(),
            FS_VERITY_BLOCK_SIZE_DEFAULT,
            InnerHashAlgorithm::Sha256,
            &[],
        ) {
            // if fsverity is enabled, ignore the error
            if e.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(WireFormatError::from(e));
            }
        }
        check_fs_verity(&fd, &verity_hash)?;
    }

    Ok(())
}

// TODO: figure out how to guard this with #[cfg(test)]
pub fn build_test_fs(path: &Path, image: &Image, tag: &str) -> Result<Descriptor> {
    build_initial_rootfs::<Zstd>(path, image, tag)
}

#[cfg(test)]
pub mod tests {
    use super::*;

    use tempfile::tempdir;

    use crate::reader::WalkPuzzleFS;
    use cap_std::fs::MetadataExt;
    use std::path::PathBuf;
    use tempfile::TempDir;

    type DefaultCompression = Zstd;

    #[test]
    fn test_fs_generation() -> anyhow::Result<()> {
        // TODO: verify the hash value here since it's only one thing? problem is as we change the
        // encoding/add stuff to it, the hash will keep changing and we'll have to update the
        // test...
        //
        // but once all that's stabalized, we should verify the metadata hash too.
        let dir = tempdir().unwrap();
        let image = Image::new(dir.path()).unwrap();
        build_test_fs(Path::new("src/builder/test/test-1"), &image, "test-tag").unwrap();
        let rootfs = image.open_rootfs_blob("test-tag", None).unwrap();

        // there should be a blob that matches the hash of the test data, since it all gets input
        // as one chunk and there's only one file
        const FILE_DIGEST: &str =
            "3eee1082ab3babf6c1595f1069d11ebc2a60135890a11e402e017ddd831a220d";

        let md = image
            .0
            .dir
            .symlink_metadata(image.blob_path().join(FILE_DIGEST))
            .unwrap();
        assert!(md.is_file());

        let mut decompressor = image
            .open_compressed_blob::<DefaultCompression>(
                &Digest::try_from(FILE_DIGEST).unwrap(),
                None,
            )
            .unwrap();

        let mut inodes = Vec::new();

        // we can at least deserialize inodes and they look sane
        for i in 0..2 {
            inodes.push(rootfs.find_inode(i + 1)?);
        }

        assert_eq!(inodes[0].ino, 1);
        if let InodeMode::Dir { ref dir_list } = inodes[0].mode {
            assert_eq!(dir_list.entries.len(), 1);
            assert_eq!(dir_list.entries[0].ino, 2);
            assert_eq!(dir_list.entries[0].name, b"SekienAkashita.jpg");
        } else {
            panic!("bad inode mode: {:?}", inodes[0].mode);
        }
        assert_eq!(inodes[0].uid, md.uid());
        assert_eq!(inodes[0].gid, md.gid());

        assert_eq!(inodes[1].ino, 2);
        assert_eq!(inodes[1].uid, md.uid());
        assert_eq!(inodes[1].gid, md.gid());
        if let InodeMode::File { ref chunks } = inodes[1].mode {
            assert_eq!(chunks.len(), 1);
            assert_eq!(
                chunks[0].len,
                decompressor.get_uncompressed_length().unwrap()
            );
            Ok(())
        } else {
            panic!("bad inode mode: {:?}", inodes[1].mode);
        }
    }

    #[test]
    fn test_delta_generation() {
        let dir = tempdir().unwrap();
        let image = Image::new(dir.path()).unwrap();
        let tag = "test";
        build_test_fs(Path::new("src/builder/test/test-1"), &image, tag).unwrap();

        let delta_dir = dir.path().join(Path::new("delta"));
        fs::create_dir_all(delta_dir.join(Path::new("foo"))).unwrap();
        fs::copy(
            Path::new("src/builder/test/test-1/SekienAkashita.jpg"),
            delta_dir.join("SekienAkashita.jpg"),
        )
        .unwrap();

        let new_tag = "test2";
        let (_desc, image) =
            add_rootfs_delta::<DefaultCompression>(&delta_dir, image, new_tag, tag).unwrap();
        let delta = Rootfs::try_from(image.open_rootfs_blob(new_tag, None).unwrap()).unwrap();
        assert_eq!(delta.metadatas.len(), 2);

        let image = Image::new(dir.path()).unwrap();
        let mut pfs = PuzzleFS::open(image, new_tag, None).unwrap();
        assert_eq!(pfs.max_inode().unwrap(), 3);
        let mut walker = WalkPuzzleFS::walk(&mut pfs).unwrap();

        let root = walker.next().unwrap().unwrap();
        assert_eq!(root.path.to_string_lossy(), "/");
        assert_eq!(root.inode.ino, 1);
        assert_eq!(root.inode.dir_entries().unwrap().len(), 2);

        let jpg_file = walker.next().unwrap().unwrap();
        assert_eq!(jpg_file.path.to_string_lossy(), "/SekienAkashita.jpg");
        assert_eq!(jpg_file.inode.ino, 2);
        assert_eq!(jpg_file.inode.file_len().unwrap(), 109466);

        let foo_dir = walker.next().unwrap().unwrap();
        assert_eq!(foo_dir.path.to_string_lossy(), "/foo");
        assert_eq!(foo_dir.inode.ino, 3);
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
            build_test_fs(path, image, "test").unwrap();
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
            build_test_fs(&path[i], image, "test").unwrap();
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
