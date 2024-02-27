use std::cmp::min;
use std::io;
use std::io::{Read, Seek, Write};

use zstd_seekable::{CStream, Seekable, SeekableCStream};

use crate::compression::{Compression, Compressor, Decompressor};

// We compress files in 4KB frames; it's not clear what the ideal size for this is, but each frame
// is compressed independently so the bigger they are the more compression savings we get. However,
// the bigger they are the more decompression we have to do to get to the data in the middle of a
// frame if someone e.g. mmap()s something in the middle of a frame.
//
// Another consideration is the average chunk size from FastCDC: if we make this the same as the
// chunk size, there's no real point in using seekable compression at all, at least for files. It's
// also possible that we want different frame sizes for metadata blobs and file content.
const FRAME_SIZE: usize = 4096;
const COMPRESSION_LEVEL: usize = 3;

fn err_to_io<E: 'static + std::error::Error + Send + Sync>(e: E) -> io::Error {
    io::Error::new(io::ErrorKind::Other, e)
}

pub struct ZstdCompressor<W> {
    f: W,
    stream: SeekableCStream,
    buf: Vec<u8>,
}

impl<W: Write> Compressor for ZstdCompressor<W> {
    fn end(mut self: Box<Self>) -> io::Result<()> {
        // end_stream has to be called multiple times until 0 is returned, see
        // https://docs.rs/zstd-seekable/0.1.23/src/zstd_seekable/lib.rs.html#224-237 and
        // https://fossies.org/linux/zstd/contrib/seekable_format/zstd_seekable.h
        loop {
            let size = self.stream.end_stream(&mut self.buf).map_err(err_to_io)?;
            self.f.write_all(&self.buf[0..size])?;
            if size == 0 {
                break;
            }
        }
        Ok(())
    }
}

impl<W: Write> Write for ZstdCompressor<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // TODO: we could try to consume all the input, but for now we just consume a single block
        let (out_pos, in_pos) = self
            .stream
            .compress(&mut self.buf, buf)
            .map_err(err_to_io)?;
        self.f.write_all(&self.buf[0..out_pos])?;
        Ok(in_pos)
    }

    fn flush(&mut self) -> io::Result<()> {
        // we could self.stream.flush(), but that adversely affects compression ratio... let's
        // cheat for now.
        Ok(())
    }
}

pub struct ZstdDecompressor<'a, R: Read + Seek> {
    stream: Seekable<'a, R>,
    offset: u64,
    uncompressed_length: u64,
}

impl<'a, R: Seek + Read> Decompressor for ZstdDecompressor<'a, R> {
    fn get_uncompressed_length(&mut self) -> io::Result<u64> {
        Ok(self.uncompressed_length)
    }
}

impl<'a, R: Seek + Read> Seek for ZstdDecompressor<'a, R> {
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

impl<'a, R: Seek + Read> Read for ZstdDecompressor<'a, R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        // decompress() gets angry (ZSTD("Corrupted block detected")) if you pass it a buffer
        // longer than the uncompressable data, so let's be careful to truncate the buffer if it
        // would make zstd angry. maybe soon they'll implement a real read() API :)
        let end = min(out.len(), (self.uncompressed_length - self.offset) as usize);
        let size = self
            .stream
            .decompress(&mut out[0..end], self.offset)
            .map_err(err_to_io)?;
        self.offset += size as u64;
        Ok(size)
    }
}

pub struct Zstd {}

impl Compression for Zstd {
    fn compress<'a, W: Write + 'a>(dest: W) -> io::Result<Box<dyn Compressor + 'a>> {
        // a "pretty high" compression level, since decompression should be nearly the same no
        // matter what compression level. Maybe we should turn this to 22 or whatever the max is...
        let stream = SeekableCStream::new(COMPRESSION_LEVEL, FRAME_SIZE).map_err(err_to_io)?;
        Ok(Box::new(ZstdCompressor {
            f: dest,
            stream,
            buf: vec![0_u8; CStream::out_size()],
        }))
    }

    fn decompress<'a, R: Read + Seek + 'a>(source: R) -> io::Result<Box<dyn Decompressor + 'a>> {
        let stream = Seekable::init(Box::new(source)).map_err(err_to_io)?;

        // zstd-seekable doesn't like it when we pass a buffer past the end of the uncompressed
        // stream, so let's figure out the size of the uncompressed file so we can implement
        // ::read() in a reasonable way. This also lets us implement SeekFrom::End.
        let uncompressed_length = (0..stream.get_num_frames())
            .map(|i| stream.get_frame_decompressed_size(i) as u64)
            .sum();
        Ok(Box::new(ZstdDecompressor {
            stream,
            offset: 0,
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
