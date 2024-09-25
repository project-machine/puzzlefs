use crate::fsverity_helpers::{check_fs_verity, get_fs_verity_digest};
use std::any::Any;
use std::backtrace::Backtrace;
use std::fs;
use std::io;
use std::io::Write;
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};

use sha2::{Digest as Sha2Digest, Sha256};

use crate::compression::{Compression, Decompressor, Noop, Zstd};
use crate::format::{Result, RootfsReader, VerityData, WireFormatError, SHA256_BLOCK_SIZE};
use std::io::{Error, ErrorKind};

pub use crate::format::Digest;
use crate::oci::media_types::{PuzzleFSMediaType, PUZZLEFS_ROOTFS, VERITY_ROOT_HASH_ANNOTATION};
use ocidir::oci_spec::image;
pub use ocidir::oci_spec::image::Descriptor;
use ocidir::oci_spec::image::{
    DescriptorBuilder, ImageIndex, ImageManifest, ImageManifestBuilder, MediaType, Sha256Digest,
};
use ocidir::OciDir;
use std::collections::HashMap;
use std::str::FromStr;

use std::io::Cursor;

pub mod media_types;
const OCI_TAG_ANNOTATION: &str = "org.opencontainers.image.ref.name";

pub struct Image(pub OciDir);

impl Image {
    pub fn new(oci_dir: &Path) -> Result<Self> {
        fs::create_dir_all(oci_dir)?;
        let d = cap_std::fs::Dir::open_ambient_dir(oci_dir, cap_std::ambient_authority())?;
        let oci_dir = OciDir::ensure(d)?;

        Ok(Self(oci_dir))
    }

    pub fn open(oci_dir: &Path) -> Result<Self> {
        let d = cap_std::fs::Dir::open_ambient_dir(oci_dir, cap_std::ambient_authority())?;
        let blobs_dir = cap_std::fs::Dir::open_ambient_dir(
            oci_dir.join(Self::blob_path()),
            cap_std::ambient_authority(),
        )?;
        let oci_dir = OciDir::open_with_external_blobs(d, blobs_dir)?;
        Ok(Self(oci_dir))
    }

    pub fn blob_path() -> PathBuf {
        // TODO: use BLOBDIR constant from ocidir after making it public
        PathBuf::from("blobs/sha256")
    }

    pub fn put_blob<C: Compression + Any>(
        &self,
        buf: &[u8],
        image_manifest: &mut ImageManifest,
        media_type: impl PuzzleFSMediaType,
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
        let media_type_with_extension = C::append_extension(media_type.name());
        let mut digest_string = "sha256:".to_string();
        digest_string.push_str(&hex::encode(digest.as_slice()));

        let fs_verity_digest = get_fs_verity_digest(&compressed_data.get_ref()[..])?;
        let mut descriptor = Descriptor::new(
            MediaType::Other(media_type_with_extension),
            uncompressed_size,
            image::Digest::from_str(&digest_string)?,
        );
        // We need to store the PuzzleFS Rootfs verity digest as an annotation (obviously we cannot
        // store it in the Rootfs itself)
        if media_type.name() == PUZZLEFS_ROOTFS {
            let mut annotations = HashMap::new();
            annotations.insert(
                VERITY_ROOT_HASH_ANNOTATION.to_string(),
                hex::encode(fs_verity_digest),
            );
            descriptor.set_annotations(Some(annotations));
        }
        let path = Self::blob_path().join(descriptor.digest().digest());

        // avoid replacing the data blob so we don't drop fsverity data
        if self.0.dir().exists(&path) {
            let mut hasher = Sha256::new();
            let mut file = self.0.dir().open(&path)?;
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
            self.0.dir().write(&path, final_data)?;
        }

        // Let's make the PuzzleFS image rootfs the first layer so it's easy to find
        // The LXC oci template also looks at the first layer in the array to identify the image
        // type (see getlayermediatype):
        // https://github.com/lxc/lxc/commit/1a2da75b6e8431f3530ebd3f75442d3bd5eec5e2
        if media_type.name() == PUZZLEFS_ROOTFS {
            image_manifest.layers_mut().insert(0, descriptor.clone());
        } else {
            image_manifest.layers_mut().push(descriptor.clone());
        }
        Ok((descriptor, fs_verity_digest, compressed_blob))
    }

    fn open_raw_blob(&self, digest: &str, verity: Option<&[u8]>) -> io::Result<cap_std::fs::File> {
        let file = self.0.blobs_dir().open(digest)?;
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
        let f = self.open_raw_blob(&digest.to_string(), verity)?;
        C::decompress(f)
    }

    pub fn get_pfs_rootfs_verity(&self, tag: &str) -> Result<[u8; SHA256_BLOCK_SIZE]> {
        let manifest = self.0.find_manifest_with_tag(tag)?.ok_or_else(|| {
            WireFormatError::MissingManifest(tag.to_string(), Backtrace::capture())
        })?;

        let rootfs_desc = manifest
            .layers()
            .iter()
            .find(|desc| desc.media_type() == &MediaType::Other(PUZZLEFS_ROOTFS.to_string()))
            .ok_or_else(|| WireFormatError::MissingRootfs(Backtrace::capture()))?;

        let rootfs_verity = rootfs_desc
            .annotations()
            .as_ref()
            .ok_or_else(|| {
                WireFormatError::InvalidFsVerityData(
                    "missing rootfs annotations".to_string(),
                    Backtrace::capture(),
                )
            })?
            .get(VERITY_ROOT_HASH_ANNOTATION)
            .ok_or_else(|| {
                WireFormatError::InvalidFsVerityData(
                    "missing rootfs verity annotation".to_string(),
                    Backtrace::capture(),
                )
            })?;
        let mut verity_digest: [u8; SHA256_BLOCK_SIZE] = [0; SHA256_BLOCK_SIZE];
        hex::decode_to_slice(rootfs_verity, &mut verity_digest)?;

        Ok(verity_digest)
    }

    pub fn get_pfs_rootfs(&self, tag: &str, verity: Option<&[u8]>) -> Result<cap_std::fs::File> {
        let manifest = self.0.find_manifest_with_tag(tag)?.ok_or_else(|| {
            WireFormatError::MissingManifest(tag.to_string(), Backtrace::capture())
        })?;

        let rootfs_desc = manifest
            .layers()
            .iter()
            .find(|desc| desc.media_type() == &MediaType::Other(PUZZLEFS_ROOTFS.to_string()))
            .ok_or_else(|| WireFormatError::MissingRootfs(Backtrace::capture()))?;

        let rootfs_digest = rootfs_desc.digest().digest();
        let file = self.open_raw_blob(rootfs_digest, verity)?;
        Ok(file)
    }

    // TODO: export this function from ocidr / find another way to avoid code duplication
    fn descriptor_is_tagged(d: &Descriptor, tag: &str) -> bool {
        d.annotations()
            .as_ref()
            .and_then(|annos| annos.get(OCI_TAG_ANNOTATION))
            .filter(|tagval| tagval.as_str() == tag)
            .is_some()
    }

    pub fn get_image_manifest_fd(&self, tag: &str) -> Result<cap_std::fs::File> {
        let index = self.get_index()?;
        let image_manifest = index
            .manifests()
            .iter()
            .find(|desc| Self::descriptor_is_tagged(desc, tag))
            .ok_or_else(|| {
                WireFormatError::MissingManifest(tag.to_string(), Backtrace::capture())
            })?;
        let file = self.open_raw_blob(image_manifest.digest().digest(), None)?;
        Ok(file)
    }

    pub fn open_rootfs_blob(&self, tag: &str, verity: Option<&[u8]>) -> Result<RootfsReader> {
        let temp_verity;
        let rootfs_verity = if let Some(verity) = verity {
            let manifest = self.get_image_manifest_fd(tag)?;
            check_fs_verity(&manifest, verity)?;
            temp_verity = self.get_pfs_rootfs_verity(tag)?;
            Some(&temp_verity[..])
        } else {
            None
        };

        let rootfs_file = self.get_pfs_rootfs(tag, rootfs_verity)?;
        RootfsReader::open(rootfs_file)
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

    pub fn get_index(&self) -> Result<ImageIndex> {
        Ok(self.0.read_index()?)
    }

    pub fn get_empty_manifest(&self) -> Result<ImageManifest> {
        // see https://github.com/opencontainers/image-spec/blob/main/manifest.md#guidance-for-an-empty-descriptor
        let config = DescriptorBuilder::default()
            .media_type(MediaType::EmptyJSON)
            .size(2_u32)
            .digest(Sha256Digest::from_str(
                "44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a",
            )?)
            .data("e30=")
            .build()?;

        if !self.0.dir().exists(
            Self::blob_path()
                .join("44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a"),
        ) {
            let mut blob = self.0.create_blob()?;
            blob.write_all("{}".as_bytes())?;
            // TODO: blob.complete_verified_as(&config)? once https://github.com/containers/ocidir-rs/pull/18 is merged
            blob.complete()?;
        }

        let image_manifest = ImageManifestBuilder::default()
            .schema_version(2_u32)
            .config(config)
            .layers(Vec::new())
            .build()?;
        Ok(image_manifest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ocidir::oci_spec::image::{ImageIndexBuilder, Platform, ANNOTATION_REF_NAME};
    use std::collections::HashMap;
    use tempfile::tempdir;
    type DefaultCompression = Zstd;

    #[test]
    fn test_put_blob_correct_hash() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let image: Image = Image::new(dir.path())?;
        let mut image_manifest = image.get_empty_manifest()?;
        let (desc, ..) = image.put_blob::<Noop>(
            "meshuggah rocks".as_bytes(),
            &mut image_manifest,
            media_types::Chunk {},
        )?;

        const DIGEST: &str = "3abd5ce0f91f640d88dca1f26b37037b02415927cacec9626d87668a715ec12d";
        assert_eq!(desc.digest().digest(), DIGEST);

        let md = image
            .0
            .dir()
            .symlink_metadata(Image::blob_path().join(DIGEST))?;
        assert!(md.is_file());
        Ok(())
    }

    #[test]
    fn test_open_can_open_new_image() -> anyhow::Result<()> {
        let dir = tempdir()?;
        Image::new(dir.path())?;
        Image::open(dir.path())?;
        Ok(())
    }

    #[test]
    fn test_put_get_index() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let image = Image::new(dir.path())?;
        let mut image_manifest = image.get_empty_manifest()?;
        let mut annotations = HashMap::new();
        annotations.insert(ANNOTATION_REF_NAME.to_string(), "foo".to_string());
        image_manifest.set_annotations(Some(annotations));
        let image_manifest_descriptor =
            image
                .0
                .insert_manifest(image_manifest, None, Platform::default())?;

        let index = ImageIndexBuilder::default()
            .schema_version(2_u32)
            .manifests(vec![image_manifest_descriptor])
            .build()?;

        let image2 = Image::open(dir.path())?;
        let index2 = image2.get_index()?;
        assert_eq!(index.manifests(), index2.manifests());
        Ok(())
    }

    #[test]
    fn double_put_ok() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let image = Image::new(dir.path())?;
        let mut image_manifest = image.get_empty_manifest()?;
        let desc1 = image.put_blob::<DefaultCompression>(
            "meshuggah rocks".as_bytes(),
            &mut image_manifest,
            media_types::Chunk {},
        )?;
        let desc2 = image.put_blob::<DefaultCompression>(
            "meshuggah rocks".as_bytes(),
            &mut image_manifest,
            media_types::Chunk {},
        )?;
        assert_eq!(desc1, desc2);
        Ok(())
    }
}
