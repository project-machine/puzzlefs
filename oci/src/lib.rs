extern crate hex;

use std::backtrace::Backtrace;
use std::convert::TryFrom;
use std::fs;
use std::io;
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest as Sha2Digest, Sha256};
use tee::TeeReader;
use tempfile::NamedTempFile;

use compression::{Compression, Decompressor};
use format::{MetadataBlob, Result, Rootfs, WireFormatError};
use openat::Dir;

mod descriptor;
pub use descriptor::{Descriptor, Digest};

mod index;
pub use index::Index;

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

    pub fn put_blob<R: io::Read, C: Compression, MT: media_types::MediaType>(
        &self,
        buf: R,
    ) -> Result<Descriptor> {
        let tmp = NamedTempFile::new_in(&self.oci_dir)?;
        let mut compressed = C::compress(tmp.reopen()?);
        let mut hasher = Sha256::new();

        let mut t = TeeReader::new(buf, &mut hasher);
        let size = io::copy(&mut t, &mut compressed)?;

        let digest = hasher.finalize();
        let media_type = C::append_extension(MT::name());
        let descriptor = Descriptor::new(digest.into(), size, media_type);

        tmp.persist(self.blob_path().join(descriptor.digest.to_string()))
            .map_err(|e| e.error)?;
        Ok(descriptor)
    }

    fn open_raw_blob(&self, digest: &Digest) -> io::Result<fs::File> {
        self.oci_dir_fd
            .open_file(&self.blob_path_relative().join(digest.to_string()))
    }

    pub fn open_compressed_blob<C: Compression>(
        &self,
        digest: &Digest,
    ) -> io::Result<Box<dyn Decompressor>> {
        let f = self.open_raw_blob(digest)?;
        Ok(C::decompress(f))
    }

    pub fn open_metadata_blob(&self, digest: &Digest) -> io::Result<MetadataBlob> {
        let f = self.open_raw_blob(digest)?;
        Ok(MetadataBlob::new(f))
    }

    pub fn get_image_manifest_fd(&self, tag: &str) -> Result<fs::File> {
        let index = self.get_index()?;
        let desc = index
            .find_tag(tag)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("no tag {tag}")))?;
        let file = self.open_raw_blob(&desc.digest)?;
        Ok(file)
    }

    pub fn open_rootfs_blob<C: Compression>(&self, tag: &str) -> Result<Rootfs> {
        let index = self.get_index()?;
        let desc = index
            .find_tag(tag)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("no tag {tag}")))?;
        let rootfs = Rootfs::open(self.open_compressed_blob::<C>(&desc.digest)?)?;
        Ok(rootfs)
    }

    pub fn fill_from_chunk(
        &self,
        chunk: format::BlobRef,
        addl_offset: u64,
        buf: &mut [u8],
    ) -> format::Result<usize> {
        let digest = &<Digest>::try_from(chunk)?;
        let mut blob = self.open_raw_blob(digest)?;
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

    pub fn add_tag(&self, name: String, mut desc: Descriptor) -> Result<()> {
        // check that the blob exists...
        self.open_raw_blob(&desc.digest)?;

        let mut index = self.get_index().unwrap_or_default();

        // untag anything that has this tag
        for m in index.manifests.iter_mut() {
            if m.get_name()
                .map(|existing_tag| existing_tag == &name)
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

    #[test]
    fn test_put_blob_correct_hash() {
        let dir = tempdir().unwrap();
        let image: Image = Image::new(dir.path()).unwrap();
        let desc = image
            .put_blob::<_, compression::Noop, media_types::Chunk>("meshuggah rocks".as_bytes())
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
        let mut desc = image
            .put_blob::<_, compression::Noop, media_types::Chunk>("meshuggah rocks".as_bytes())
            .unwrap();
        desc.set_name("foo".to_string());
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
            .put_blob::<_, compression::Noop, media_types::Chunk>("meshuggah rocks".as_bytes())
            .unwrap();
        let desc2 = image
            .put_blob::<_, compression::Noop, media_types::Chunk>("meshuggah rocks".as_bytes())
            .unwrap();
        assert_eq!(desc1, desc2);
    }
}
