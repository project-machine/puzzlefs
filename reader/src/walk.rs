use std::collections::VecDeque;

use super::error::FSResult;
use super::puzzlefs::{Inode, InodeMode, PuzzleFS};

/// A in iterator over a PuzzleFS filesystem. This iterates breadth first, since file content is
/// stored that way in a puzzlefs image so it'll be faster reading actual content if clients want
/// to do that.
pub struct WalkPuzzleFS<'a> {
    pfs: &'a mut PuzzleFS<'a>,
    q: VecDeque<u64>,
}

impl<'a> WalkPuzzleFS<'a> {
    pub fn walk(pfs: &'a mut PuzzleFS<'a>) -> WalkPuzzleFS<'a> {
        let mut q = VecDeque::new();
        q.push_back(1); // root inode number
        WalkPuzzleFS { pfs, q }
    }

    fn handle_next(&mut self, ino: u64) -> FSResult<Inode> {
        let inode = self.pfs.find_inode(ino)?;
        if let InodeMode::Dir { ref entries } = inode.mode {
            for (_name, ino) in entries {
                self.q.push_back(*ino);
            }
        };

        Ok(inode)
    }
}

impl Iterator for WalkPuzzleFS<'_> {
    type Item = FSResult<Inode>;

    fn next(&mut self) -> Option<Self::Item> {
        let ino = self.q.pop_front()?;
        Some(self.handle_next(ino))
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use builder::build_test_fs;
    use oci::Image;

    use super::*;

    #[test]
    fn test_walk() {
        // make ourselves a test image
        let oci_dir = tempdir().unwrap();
        let image = Image::new(oci_dir.path()).unwrap();
        let rootfs_desc = build_test_fs(&image).unwrap();
        image.add_tag("test".to_string(), rootfs_desc).unwrap();
        let mut pfs = PuzzleFS::open(&image, "test").unwrap();

        let mut walker = WalkPuzzleFS::walk(&mut pfs);

        let root = walker.next().unwrap().unwrap();
        assert_eq!(root.inode.ino, 1);
        assert_eq!(root.dir_entries().unwrap().len(), 1);

        let jpg_file = walker.next().unwrap().unwrap();
        assert_eq!(jpg_file.inode.ino, 2);
        assert_eq!(jpg_file.file_len().unwrap(), 109466);
    }
}
