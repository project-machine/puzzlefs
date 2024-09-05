use crate::fsverity_helpers::{check_fs_verity, get_fs_verity_digest};
use std::any::Any;
use std::backtrace::Backtrace;
use std::fs;
use std::io;
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest as Sha2Digest, Sha256};
use tempfile::NamedTempFile;

use crate::compression::{Compression, Decompressor, Noop, Zstd};
use crate::format::{Result, RootfsReader, VerityData, WireFormatError, SHA256_BLOCK_SIZE};
use openat::Dir;
use std::io::{Error, ErrorKind};

mod descriptor;
pub use descriptor::{Descriptor, Digest};

mod index;
pub use index::Index;
use std::io::Cursor;
use std::io::Write;

pub mod media_types;

// this is a string, probably intended to be a real version format (though the spec doesn't say
// anything) so let's just say "puzzlefs-dev" for now since the format is in flux.
const PUZZLEFS_IMAGE_LAYOUT_VERSION: &str = "puzzlefs-dev";

const IMAGE_LAYOUT_PATH: &str = "oci-layout";

#[derive(Serialize, Deserialize, Debug)]
struct OCILayout {
    #[serde(rename = "imageLayoutVersion")]
    version: String,
}

pub struct Image {
    oci_dir: PathBuf,
    oci_dir_fd: Dir,
}

impl Image {
    pub fn new(oci_dir: &Path) -> Result<Self> {
        fs::create_dir_all(oci_dir)?;
        let image = Image {
            oci_dir: oci_dir.to_path_buf(),
            oci_dir_fd: Dir::open(oci_dir)?,
        };
        fs::create_dir_all(image.blob_path())?;
        let layout_file = fs::File::create(oci_dir.join(IMAGE_LAYOUT_PATH))?;
        let layout = OCILayout {
            version: PUZZLEFS_IMAGE_LAYOUT_VERSION.to_string(),
        };
        serde_json::to_writer(layout_file, &layout)?;
        Ok(image)
    }

    pub fn open(oci_dir: &Path) -> Result<Self> {
        let layout_file = fs::File::open(oci_dir.join(IMAGE_LAYOUT_PATH))?;
        let layout = serde_json::from_reader::<_, OCILayout>(layout_file)?;
        if layout.version != PUZZLEFS_IMAGE_LAYOUT_VERSION {
            Err(WireFormatError::InvalidImageVersion(
                layout.version,
                Backtrace::capture(),
            ))
        } else {
            Ok(Image {
                oci_dir: oci_dir.to_path_buf(),
                oci_dir_fd: Dir::open(oci_dir)?,
            })
        }
    }

    pub fn blob_path(&self) -> PathBuf {
        self.oci_dir.join("blobs/sha256")
    }

    pub fn blob_path_relative(&self) -> PathBuf {
        PathBuf::from("blobs/sha256")
    }

    pub fn put_blob<C: Compression + Any, MT: media_types::MediaType>(
        &self,
        buf: &[u8],
    ) -> Result<(Descriptor, [u8; SHA256_BLOCK_SIZE], bool)> {
        let mut compressed_data = Cursor::new(Vec::<u8>::new());
        let mut compressed = C::compress(&mut compressed_data)?;
        let mut hasher = Sha256::new();
        // generics may not be the best way to implement compression, alternatives:
        // trait objects, but they add runtime overhead
        // an enum together with enum_dispatch
        let mut compressed_blob = std::any::TypeId::of::<C>() != std::any::TypeId::of::<Noop>();

        // without the clone, the io::copy leaves us with an empty slice
        // we're only cloning the reference, which is ok because the slice itself gets mutated
        // i.e. the slice advances through the buffer as it is being read
        let uncompressed_size = io::copy(&mut <&[u8]>::clone(&buf), &mut compressed)?;
        compressed.end()?;
        let compressed_size = compressed_data.get_ref().len() as u64;

        // store the uncompressed blob if the compressed version has bigger size
        let final_data = if compressed_blob && compressed_size >= uncompressed_size {
            compressed_blob = false;
            buf
        } else {
            compressed_data.get_ref()
        };

        hasher.update(final_data);
        let digest = hasher.finalize();
        let media_type = C::append_extension(MT::name());
        let descriptor = Descriptor::new(digest.into(), uncompressed_size, media_type);
        let fs_verity_digest = get_fs_verity_digest(&compressed_data.get_ref()[..])?;
        let path = self.blob_path().join(descriptor.digest.to_string());

        // avoid replacing the data blob so we don't drop fsverity data
        if path.exists() {
            let mut hasher = Sha256::new();
            let mut file = fs::File::open(path)?;
            io::copy(&mut file, &mut hasher)?;
            let existing_digest = hasher.finalize();
            if existing_digest != digest {
                return Err(Error::new(
                    ErrorKind::AlreadyExists,
                    format!("blob already exists and it's not content addressable existing digest {}, new digest {}",
                    hex::encode(existing_digest), hex::encode(digest))
                )
                .into());
            }
        } else {
            let mut tmp = NamedTempFile::new_in(&self.oci_dir)?;
            tmp.write_all(final_data)?;
            tmp.persist(path).map_err(|e| e.error)?;
        }
        Ok((descriptor, fs_verity_digest, compressed_blob))
    }

    fn open_raw_blob(&self, digest: &Digest, verity: Option<&[u8]>) -> io::Result<fs::File> {
        let file = self
            .oci_dir_fd
            .open_file(&self.blob_path_relative().join(digest.to_string()))?;
        if let Some(verity) = verity {
            check_fs_verity(&file, verity).map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        }
        Ok(file)
    }

    pub fn open_compressed_blob<C: Compression>(
        &self,
        digest: &Digest,
        verity: Option<&[u8]>,
    ) -> io::Result<Box<dyn Decompressor>> {
        let f = self.open_raw_blob(digest, verity)?;
        C::decompress(f)
    }

    pub fn get_image_manifest_fd(&self, tag: &str) -> Result<fs::File> {
        let index = self.get_index()?;
        let desc = index
            .find_tag(tag)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("no tag {tag}")))?;
        let file = self.open_raw_blob(&desc.digest, None)?;
        Ok(file)
    }

    pub fn open_rootfs_blob(&self, tag: &str, verity: Option<&[u8]>) -> Result<RootfsReader> {
        let index = self.get_index()?;
        let desc = index
            .find_tag(tag)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("no tag {tag}")))?;

        let rootfs = self.open_raw_blob(&desc.digest, verity)?;
        RootfsReader::open(rootfs)
    }

    pub fn fill_from_chunk(
        &self,
        chunk: crate::format::BlobRef,
        addl_offset: u64,
        buf: &mut [u8],
        verity_data: &Option<VerityData>,
    ) -> crate::format::Result<usize> {
        let digest = &<Digest>::try_from(chunk)?;
        let file_verity;
        if let Some(verity) = verity_data {
            file_verity = Some(
                &verity
                    .get(&digest.underlying())
                    .ok_or(WireFormatError::InvalidFsVerityData(
                        format!("missing verity data {digest}"),
                        Backtrace::capture(),
                    ))?[..],
            );
        } else {
            file_verity = None;
        }
        let mut blob = if chunk.compressed {
            self.open_compressed_blob::<Zstd>(digest, file_verity)?
        } else {
            self.open_compressed_blob::<Noop>(digest, file_verity)?
        };
        blob.seek(io::SeekFrom::Start(chunk.offset + addl_offset))?;
        let n = blob.read(buf)?;
        Ok(n)
    }

    pub fn get_index(&self) -> Result<Index> {
        Index::open(&self.oci_dir.join(index::PATH))
    }

    pub fn put_index(&self, i: &Index) -> Result<()> {
        i.write(&self.oci_dir.join(index::PATH))
    }

    pub fn add_tag(&self, name: &str, mut desc: Descriptor) -> Result<()> {
        // check that the blob exists...
        self.open_raw_blob(&desc.digest, None)?;

        let mut index = self.get_index().unwrap_or_default();

        // untag anything that has this tag
        for m in index.manifests.iter_mut() {
            if m.get_name()
                .map(|existing_tag| existing_tag == name)
                .unwrap_or(false)
            {
                m.remove_name()
            }
        }
        desc.set_name(name);

        index.manifests.push(desc);
        self.put_index(&index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    type DefaultCompression = Zstd;

    #[test]
    fn test_put_blob_correct_hash() {
        let dir = tempdir().unwrap();
        let image: Image = Image::new(dir.path()).unwrap();
        let (desc, ..) = image
            .put_blob::<Noop, media_types::Chunk>("meshuggah rocks".as_bytes())
            .unwrap();

        const DIGEST: &str = "3abd5ce0f91f640d88dca1f26b37037b02415927cacec9626d87668a715ec12d";
        assert_eq!(desc.digest.to_string(), DIGEST);

        let md = fs::symlink_metadata(image.blob_path().join(DIGEST)).unwrap();
        assert!(md.is_file());
    }

    #[test]
    fn test_open_can_open_new_image() {
        let dir = tempdir().unwrap();
        Image::new(dir.path()).unwrap();
        Image::open(dir.path()).unwrap();
    }

    #[test]
    fn test_put_get_index() {
        let dir = tempdir().unwrap();
        let image = Image::new(dir.path()).unwrap();
        let (mut desc, ..) = image
            .put_blob::<DefaultCompression, media_types::Chunk>("meshuggah rocks".as_bytes())
            .unwrap();
        desc.set_name("foo");
        let mut index = Index::default();
        // TODO: make a real API for this that checks that descriptor has a name?
        index.manifests.push(desc);
        image.put_index(&index).unwrap();

        let image2 = Image::open(dir.path()).unwrap();
        let index2 = image2.get_index().unwrap();
        assert_eq!(index.manifests, index2.manifests);
    }

    #[test]
    fn double_put_ok() {
        let dir = tempdir().unwrap();
        let image = Image::new(dir.path()).unwrap();
        let desc1 = image
            .put_blob::<DefaultCompression, media_types::Chunk>("meshuggah rocks".as_bytes())
            .unwrap();
        let desc2 = image
            .put_blob::<DefaultCompression, media_types::Chunk>("meshuggah rocks".as_bytes())
            .unwrap();
        assert_eq!(desc1, desc2);
    }
}
