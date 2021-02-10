extern crate hex;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use tee::TeeReader;
use tempfile::NamedTempFile;

use format::MetadataBlob;

#[derive(Debug, Copy, Clone)]
pub struct Descriptor {
    pub digest: [u8; 32],
    pub len: u64,
    // TODO: media_type
}

impl Descriptor {
    pub fn digest_as_str(self) -> String {
        hex::encode(self.digest)
    }
}

pub struct Image<'a> {
    oci_dir: &'a Path,
}

impl<'a> Image<'a> {
    pub fn new(oci_dir: &'a Path) -> Result<Self, io::Error> {
        let image = Image { oci_dir };
        fs::create_dir_all(image.blob_path())?;
        Ok(Image { oci_dir })
    }

    pub fn blob_path(&self) -> PathBuf {
        self.oci_dir.join("blobs/sha256")
    }

    pub fn put_blob<R: io::Read>(&self, buf: R) -> Result<Descriptor, io::Error> {
        let mut tmp = NamedTempFile::new_in(self.oci_dir)?;
        let mut hasher = Sha256::new();

        let mut t = TeeReader::new(buf, &mut hasher);
        let size = io::copy(&mut t, &mut tmp)?;

        let digest = hasher.finalize();
        let descriptor = Descriptor {
            digest: digest.into(),
            len: size,
        };

        tmp.persist(self.blob_path().join(descriptor.digest_as_str()))?;
        Ok(descriptor)
    }

    pub fn open_raw_blob(&self, digest: &[u8; 32]) -> io::Result<fs::File> {
        fs::File::open(self.blob_path().join(hex::encode(digest)))
    }

    pub fn open_metadata_blob(&self, digest: &[u8; 32]) -> io::Result<format::MetadataBlob> {
        let f = self.open_raw_blob(&digest)?;
        Ok(MetadataBlob::new(f))
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
        let desc = image.put_blob("meshuggah rocks".as_bytes()).unwrap();

        const DIGEST: &str = "3abd5ce0f91f640d88dca1f26b37037b02415927cacec9626d87668a715ec12d";
        assert_eq!(desc.digest_as_str(), DIGEST);

        let md = fs::symlink_metadata(image.blob_path().join(DIGEST)).unwrap();
        assert!(md.is_file());
    }
}
