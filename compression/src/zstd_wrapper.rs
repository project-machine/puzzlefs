use common::MAX_CHUNK_SIZE;
use std::cmp::min;
use std::convert::TryFrom;
use std::convert::TryInto;
use std::io;
use std::io::{Read, Write};

use crate::{Compression, Compressor, Decompressor};

const COMPRESSION_LEVEL: i32 = 3;

fn err_to_io<E: 'static + std::error::Error + Send + Sync>(e: E) -> io::Error {
    io::Error::new(io::ErrorKind::Other, e)
}

pub struct ZstdCompressor<W: Write> {
    encoder: zstd::stream::write::Encoder<'static, W>,
}

impl<W: Write> Compressor for ZstdCompressor<W> {
    fn end(self: Box<Self>) -> io::Result<()> {
        self.encoder.finish()?;
        Ok(())
    }
}

impl<W: Write> io::Write for ZstdCompressor<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.encoder.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.encoder.flush()
    }
}

pub struct ZstdDecompressor {
    buf: Vec<u8>,
    offset: u64,
    uncompressed_length: u64,
}

impl Decompressor for ZstdDecompressor {
    fn get_uncompressed_length(&mut self) -> io::Result<u64> {
        Ok(self.uncompressed_length)
    }
}

impl io::Seek for ZstdDecompressor {
    fn seek(&mut self, offset: io::SeekFrom) -> io::Result<u64> {
        match offset {
            io::SeekFrom::Start(s) => {
                self.offset = s;
            }
            io::SeekFrom::End(e) => {
                if e > 0 {
                    return Err(io::Error::new(io::ErrorKind::Other, "zstd seek past end"));
                }
                self.offset = self.uncompressed_length - u64::try_from(-e).map_err(err_to_io)?;
            }
            io::SeekFrom::Current(c) => {
                if c > 0 {
                    self.offset += u64::try_from(c).map_err(err_to_io)?;
                } else {
                    self.offset -= u64::try_from(-c).map_err(err_to_io)?;
                }
            }
        }
        Ok(self.offset)
    }
}

impl io::Read for ZstdDecompressor {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let len = min(
            out.len(),
            (self.uncompressed_length - self.offset)
                .try_into()
                .map_err(err_to_io)?,
        );
        let offset: usize = self.offset.try_into().map_err(err_to_io)?;
        out[..len].copy_from_slice(&self.buf[offset..offset + len]);
        Ok(len)
    }
}
pub struct Zstd {}

impl<'a> Compression<'a> for Zstd {
    fn compress<W: Write + 'a>(dest: W) -> io::Result<Box<dyn Compressor + 'a>> {
        let encoder = zstd::stream::write::Encoder::new(dest, COMPRESSION_LEVEL)?;
        Ok(Box::new(ZstdCompressor { encoder }))
    }

    fn decompress<R: Read>(mut source: R) -> io::Result<Box<dyn Decompressor>> {
        let mut contents = Vec::new();
        source.read_to_end(&mut contents)?;
        let mut decompressor = zstd::bulk::Decompressor::new()?;
        let decompressed_buffer =
            decompressor.decompress(&contents, MAX_CHUNK_SIZE.try_into().map_err(err_to_io)?)?;
        let uncompressed_length = decompressed_buffer.len();
        Ok(Box::new(ZstdDecompressor {
            buf: decompressed_buffer,
            offset: 0,
            uncompressed_length: uncompressed_length.try_into().map_err(err_to_io)?,
        }))
    }

    fn append_extension(media_type: &str) -> String {
        format!("{media_type}+zstd")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{compress_decompress, compression_is_seekable};

    #[test]
    fn test_ztsd_roundtrip() -> anyhow::Result<()> {
        compress_decompress::<Zstd>()
    }

    #[test]
    fn test_zstd_seekable() -> anyhow::Result<()> {
        compression_is_seekable::<Zstd>()
    }
}
