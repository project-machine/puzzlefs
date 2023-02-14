use std::io;
use std::io::Read;
use std::path::{Path, PathBuf};

struct ReaderLink {
    file: PathBuf,
    done: bool,
}

/// A structure used to chain multiple readers, similar to
/// [chain](https://doc.rust-lang.org/std/io/trait.Read.html#method.chain)
/// and [multi_reader](https://docs.rs/multi_reader/latest/multi_reader/)
pub struct FilesystemStream {
    reader_chain: Vec<ReaderLink>,
    current_reader: Option<std::fs::File>,
}

impl FilesystemStream {
    pub fn new() -> Self {
        FilesystemStream {
            reader_chain: Vec::new(),
            current_reader: None,
        }
    }

    pub fn push(&mut self, file: &Path) {
        self.reader_chain.push(ReaderLink {
            file: file.into(),
            done: false,
        })
    }
}

impl Read for FilesystemStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        for link in &mut self.reader_chain {
            if link.done {
                continue;
            }

            let current_reader = match self.current_reader.as_mut() {
                Some(reader) => reader,
                None => self.current_reader.insert(std::fs::File::open(&link.file)?),
            };

            match current_reader.read(buf)? {
                0 if !buf.is_empty() => {
                    self.current_reader = None;
                    link.done = true
                }
                n => return Ok(n),
            }
        }
        Ok(0)
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    use std::fs::File;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn test_fs_stream() -> anyhow::Result<()> {
        let dir = tempdir().unwrap();
        let file_name1 = dir.path().join(Path::new("foo"));
        let mut file1 = File::create(&file_name1)?;
        let file_name2 = dir.path().join(Path::new("bar"));
        let mut file2 = File::create(&file_name2)?;
        let file_name3 = dir.path().join(Path::new("baz"));
        let mut file3 = File::create(&file_name3)?;
        let mut buffer = Vec::new();

        file1.write_all(b"Lorem ipsum ")?;
        file2.write_all(b"dolor sit amet, ")?;
        file3.write_all(b"consectetur adipiscing elit.")?;

        let mut fs_stream = FilesystemStream::new();
        fs_stream.push(&file_name1);
        fs_stream.push(&file_name2);
        fs_stream.push(&file_name3);

        fs_stream.read_to_end(&mut buffer)?;
        assert_eq!(
            buffer,
            "Lorem ipsum dolor sit amet, consectetur adipiscing elit.".as_bytes()
        );

        Ok(())
    }
}
