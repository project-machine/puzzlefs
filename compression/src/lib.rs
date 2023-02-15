use std::fs;
use std::io;

mod zstd_seekable_wrapper;
pub use zstd_seekable_wrapper::*;

pub trait Compressor: io::Write {
    fn end(&mut self) -> io::Result<()>;
}

impl Compressor for fs::File {
    fn end(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub trait Decompressor: io::Read + io::Seek + Send {}

impl Decompressor for fs::File {}

pub trait Compression {
    fn compress(dest: fs::File) -> Box<dyn Compressor>;
    fn decompress(source: fs::File) -> Box<dyn Decompressor>;
    fn append_extension(media_type: &str) -> String;
}

pub struct Noop {}

impl Compression for Noop {
    fn compress(dest: fs::File) -> Box<dyn Compressor> {
        Box::new(dest)
    }

    fn decompress(source: fs::File) -> Box<dyn Decompressor> {
        Box::new(source)
    }

    fn append_extension(media_type: &str) -> String {
        media_type.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    const TRUTH: &str = "meshuggah rocks";

    pub fn compress_decompress_noop<C: Compression>() {
        let f = NamedTempFile::new().unwrap();
        let mut compressed = C::compress(f.reopen().unwrap());
        compressed.write_all(TRUTH.as_bytes()).unwrap();
        compressed.end().unwrap();

        let mut buf = vec![0_u8; TRUTH.len()];
        let n = C::decompress(f.reopen().unwrap()).read(&mut buf).unwrap();
        assert_eq!(n, TRUTH.len());

        assert_eq!(TRUTH.as_bytes(), buf);
    }

    pub fn compression_is_seekable<C: Compression>() {
        let f = NamedTempFile::new().unwrap();
        let mut compressed = C::compress(f.reopen().unwrap());
        compressed.write_all(TRUTH.as_bytes()).unwrap();
        compressed.end().unwrap();

        let mut buf = vec![0_u8; 1024];
        let mut decompressor = C::decompress(f.reopen().unwrap());
        decompressor
            .seek(io::SeekFrom::Start("meshuggah ".len() as u64))
            .unwrap();
        let n = decompressor.read(&mut buf).unwrap();
        assert_eq!(n, 5);

        assert_eq!("rocks".as_bytes(), &buf[0..5]);
    }

    #[test]
    fn test_noop_roundtrip() {
        compress_decompress_noop::<Noop>();
    }

    #[test]
    fn test_noop_seekable() {
        compression_is_seekable::<Noop>();
    }

    #[test]
    fn test_noop_is_noop() {
        // shouldn't mangle the file content if in no-op mode
        let f = NamedTempFile::new().unwrap();
        Noop::compress(f.reopen().unwrap())
            .write_all(TRUTH.as_bytes())
            .unwrap();

        let content = fs::read_to_string(f.path()).unwrap();
        assert_eq!(TRUTH, content);
    }
}
