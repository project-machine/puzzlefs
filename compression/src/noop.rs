use crate::{Compression, Compressor, Decompressor};
use std::io;
use std::io::{Read, Seek, Write};

pub struct Noop {}

pub struct NoopCompressor<W: Write> {
    encoder: Box<W>,
}

impl<W: Write> io::Write for NoopCompressor<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.encoder.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.encoder.flush()
    }
}

impl<W: Write> Compressor for NoopCompressor<W> {
    fn end(self: Box<Self>) -> io::Result<()> {
        Ok(())
    }
}

pub struct NoopDecompressor<R: Read + Seek + Send> {
    decoder: Box<R>,
}

impl<R: Read + io::Seek + Send> Seek for NoopDecompressor<R> {
    fn seek(&mut self, offset: io::SeekFrom) -> io::Result<u64> {
        self.decoder.seek(offset)
    }
}

impl<R: Read + Seek + Send> Read for NoopDecompressor<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        self.decoder.read(out)
    }
}

impl<R: Read + Seek + Send> Decompressor for NoopDecompressor<R> {
    fn get_uncompressed_length(&mut self) -> io::Result<u64> {
        self.decoder.stream_len()
    }
}

impl<'a> Compression<'a> for Noop {
    fn compress<W: std::io::Write + 'a>(dest: W) -> io::Result<Box<dyn Compressor + 'a>> {
        Ok(Box::new(NoopCompressor {
            encoder: Box::new(dest),
        }))
    }

    fn decompress<R: std::io::Read + Seek + Send + 'a>(
        source: R,
    ) -> io::Result<Box<dyn Decompressor + 'a>> {
        Ok(Box::new(NoopDecompressor {
            decoder: Box::new(source),
        }))
    }

    fn append_extension(media_type: &str) -> String {
        media_type.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{compress_decompress, compression_is_seekable, TRUTH};
    use std::fs;
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
