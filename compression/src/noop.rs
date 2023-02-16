use crate::{Compression, Compressor, Decompressor};
use std::fs;
use std::io;

pub struct Noop {}

impl Compressor for fs::File {
    fn end(self: Box<Self>) -> io::Result<()> {
        Ok(())
    }
}

impl Decompressor for fs::File {
    fn get_uncompressed_length(&mut self) -> io::Result<u64> {
        Ok(self.metadata()?.len())
    }
}

impl Compression for Noop {
    fn compress(dest: fs::File) -> io::Result<Box<dyn Compressor>> {
        Ok(Box::new(dest))
    }

    fn decompress(source: fs::File) -> io::Result<Box<dyn Decompressor>> {
        Ok(Box::new(source))
    }

    fn append_extension(media_type: &str) -> String {
        media_type.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{compress_decompress, compression_is_seekable, TRUTH};
    use tempfile::NamedTempFile;

    #[test]
    fn test_noop_roundtrip() -> anyhow::Result<()> {
        compress_decompress::<Noop>()
    }

    #[test]
    fn test_noop_seekable() -> anyhow::Result<()> {
        compression_is_seekable::<Noop>()
    }

    #[test]
    fn test_noop_is_noop() -> anyhow::Result<()> {
        // shouldn't mangle the file content if in no-op mode
        let f = NamedTempFile::new()?;
        Noop::compress(f.reopen()?)?.write_all(TRUTH.as_bytes())?;

        let content = fs::read_to_string(f.path())?;
        assert_eq!(TRUTH, content);
        Ok(())
    }
}
