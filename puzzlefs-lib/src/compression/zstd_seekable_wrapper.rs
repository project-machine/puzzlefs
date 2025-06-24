use std::io;
use std::io::{Read, Seek, Write};

use zeekstd::{Decoder, EncodeOptions, Encoder, FrameSizePolicy};

use crate::compression::{Compression, Compressor, Decompressor};

// We compress files in 4KB frames; it's not clear what the ideal size for this is, but each frame
// is compressed independently so the bigger they are the more compression savings we get. However,
// the bigger they are the more decompression we have to do to get to the data in the middle of a
// frame if someone e.g. mmap()s something in the middle of a frame.
//
// Another consideration is the average chunk size from FastCDC: if we make this the same as the
// chunk size, there's no real point in using seekable compression at all, at least for files. It's
// also possible that we want different frame sizes for metadata blobs and file content.
const FRAME_SIZE: u32 = 4096;
const COMPRESSION_LEVEL: i32 = 3;

fn err_to_io<E: 'static + std::error::Error + Send + Sync>(e: E) -> io::Error {
    io::Error::other(e)
}

pub struct ZstdCompressor<'a, W> {
    encoder: Encoder<'a, W>,
}

impl<'a, W: Write> Compressor for ZstdCompressor<'a, W> {
    fn end(self: Box<Self>) -> io::Result<()> {
        self.encoder.finish().map_err(err_to_io)?;
        Ok(())
    }
}

impl<'a, W: Write> Write for ZstdCompressor<'a, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.encoder.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.encoder.flush()
    }
}

pub struct ZstdDecompressor<'a, R: Read + Seek> {
    decoder: Decoder<'a, R>,
    uncompressed_length: u64,
}

impl<R: Seek + Read> Decompressor for ZstdDecompressor<'_, R> {
    fn get_uncompressed_length(&mut self) -> io::Result<u64> {
        Ok(self.uncompressed_length)
    }
}

impl<R: Seek + Read> Seek for ZstdDecompressor<'_, R> {
    fn seek(&mut self, offset: io::SeekFrom) -> io::Result<u64> {
        self.decoder.seek(offset)
    }
}

impl<R: Seek + Read> Read for ZstdDecompressor<'_, R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        self.decoder.read(out)
    }
}

pub struct Zstd {}

impl Compression for Zstd {
    fn compress<'a, W: Write + 'a>(dest: W) -> io::Result<Box<dyn Compressor + 'a>> {
        // a "pretty high" compression level, since decompression should be nearly the same no
        // matter what compression level. Maybe we should turn this to 22 or whatever the max is...
        let encoder = EncodeOptions::new()
            .compression_level(COMPRESSION_LEVEL)
            .frame_size_policy(FrameSizePolicy::Uncompressed(FRAME_SIZE))
            .into_encoder(dest)
            .map_err(err_to_io)?;
        Ok(Box::new(ZstdCompressor { encoder }))
    }

    fn decompress<'a, R: Read + Seek + 'a>(source: R) -> io::Result<Box<dyn Decompressor + 'a>> {
        // let decoder = Seekable::init(Box::new(source)).map_err(err_to_io)?;
        let decoder = Decoder::new(source).map_err(err_to_io)?;
        let seek_table = decoder.seek_table();

        // zstd-seekable doesn't like it when we pass a buffer past the end of the uncompressed
        // stream, so let's figure out the size of the uncompressed file so we can implement
        // ::read() in a reasonable way. This also lets us implement SeekFrom::End.
        let uncompressed_length = seek_table.size_decomp();
        Ok(Box::new(ZstdDecompressor {
            decoder,
            uncompressed_length,
        }))
    }

    fn append_extension(media_type: &str) -> String {
        format!("{media_type}+zstd")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compression::tests::{compress_decompress, compression_is_seekable};

    #[test]
    fn test_ztsd_roundtrip() -> anyhow::Result<()> {
        compress_decompress::<Zstd>()
    }

    #[test]
    fn test_zstd_seekable() -> anyhow::Result<()> {
        compression_is_seekable::<Zstd>()
    }
}
