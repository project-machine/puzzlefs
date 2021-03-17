use std::fs;
use std::io;

pub trait Compressor: io::Write {}

pub trait Decompressor: io::Read + io::Seek + Send {}

impl Decompressor for fs::File {}

pub trait Compression {
    fn compress(dest: fs::File) -> Box<dyn io::Write>;
    fn decompress(source: fs::File) -> Box<dyn Decompressor>;
}

pub struct Noop {}

impl Compression for Noop {
    fn compress(dest: fs::File) -> Box<dyn io::Write> {
        Box::new(dest)
    }

    fn decompress(source: fs::File) -> Box<dyn Decompressor> {
        Box::new(source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    const TRUTH: &'static str = "meshuggah rocks";

    fn compress_decompress_noop<C: Compression>() {
        let f = NamedTempFile::new().unwrap();
        C::compress(f.reopen().unwrap())
            .write(TRUTH.as_bytes())
            .unwrap();

        let mut buf = vec![0_u8; TRUTH.len()];
        C::decompress(f.reopen().unwrap()).read(&mut buf).unwrap();

        assert_eq!(TRUTH.as_bytes(), buf);
    }

    #[test]
    fn test_noop_compression() {
        compress_decompress_noop::<Noop>();

        // shouldn't mangle the content if in no-op mode
        let f = NamedTempFile::new().unwrap();
        Noop::compress(f.reopen().unwrap())
            .write(TRUTH.as_bytes())
            .unwrap();

        let content = fs::read_to_string(f.path()).unwrap();
        assert_eq!(TRUTH, content);
    }
}
