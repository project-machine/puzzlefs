use std::convert::TryFrom;
use std::fs;
use std::io;
use std::io::Write;

use zstd_seekable::{SeekableCStream, Seekable};

use crate::{Compression, Compressor, Decompressor};

// We compress files in 5MB frames; it's not clear what the ideal size for this is, but each frame
// is compressed independently so the bigger they are the more compression savings we get. However,
// the bigger they are the more decompression we have to do to get to the data in the middle of a
// frame if someone e.g. mmap()s something in the middle of a frame.
//
// Another consideration is the average chunk size from FastCDC: if we make this the same as the
// chunk size, there's no real point in using seekable compression at all, at least for files. It's
// also possible that we want different frame sizes for metadata blobs and file content.
const FRAME_SIZE: usize = 5 * 1024 * 1024;

fn err_to_io<E: 'static + std::error::Error + Send + Sync>(e: E) -> io::Error {
    io::Error::new(io::ErrorKind::Other, e)
}

pub struct ZstdCompressor {
    f: fs::File,
    stream: SeekableCStream,
    buf: Vec<u8>
}

impl Compressor for ZstdCompressor {
    fn end(&mut self) -> io::Result<()> {
        let size = self.stream.end_stream(&mut self.buf).map_err(err_to_io)?;
        self.f.write_all(&self.buf[0..size])
    }
}

impl io::Write for ZstdCompressor {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // TODO: we could try to consume all the input, but for now we just consume a single block
        let (out_pos, in_pos) = self.stream.compress(&mut self.buf, buf).map_err(err_to_io)?;
        self.f.write_all(&self.buf[0..out_pos])?;
        Ok(in_pos)
    }

    fn flush(&mut self) -> io::Result<()> {
        // we could self.stream.flush(), but that adversely affects compression ratio... let's
        // cheat for now.
        Ok(())
    }
}

pub struct ZstdDecompressor {
    stream: Seekable<'static, fs::File>,
    offset: u64,
}

impl Decompressor for ZstdDecompressor {}

impl io::Seek for ZstdDecompressor {
    fn seek(&mut self, offset: io::SeekFrom) -> io::Result<u64> {
        let prev = self.offset;
        match offset {
            io::SeekFrom::Start(s) => {
                self.offset = s;
                Ok(prev + s)
            },
            io::SeekFrom::End(_) => Err(io::Error::new(io::ErrorKind::Other, "zstd end seek not implemented")),
            io::SeekFrom::Current(c) => {
                if c > 0 {
                    self.offset += u64::try_from(c).map_err(err_to_io)?;
                } else {
                    self.offset -= u64::try_from(-c).map_err(err_to_io)?;
                }
                Ok(self.offset - prev)
            },
        }
    }
}

impl io::Read for ZstdDecompressor {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let size = self.stream.decompress(out, self.offset).map_err(err_to_io)?;
        self.offset += size as u64;
        Ok(size)
    }
}

pub struct Zstd {}

impl Compression for Zstd {
    fn compress(dest: fs::File) -> Box<dyn Compressor> {
        // a "pretty high" compression level, since decompression should be nearly the same no
        // matter what compression level. Maybe we should turn this to 22 or whatever the max is...
        let stream = SeekableCStream::new(17, FRAME_SIZE).unwrap();
        Box::new(ZstdCompressor { f: dest, stream, buf: vec![0_u8; FRAME_SIZE] })
    }

    fn decompress(source: fs::File) -> Box<dyn Decompressor> {
        let stream = Seekable::init(Box::new(source)).unwrap();
        Box::new(ZstdDecompressor { stream, offset: 0 })
    }
}



#[cfg(test)]
mod tests {
    use crate::tests::{compress_decompress_noop, compression_is_seekable};
    use super::*;

    #[test]
    fn test_ztsd_roundtrip() {
        compress_decompress_noop::<Zstd>();
    }

    #[test]
    fn test_zstd_seekable() {
        compression_is_seekable::<Zstd>();
    }
}
