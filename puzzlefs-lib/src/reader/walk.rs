use std::collections::VecDeque;
use std::path::PathBuf;

use crate::format::{Inode, InodeMode, Result};
use crate::oci::Image;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::sync::Arc;

use super::puzzlefs::{FileReader, PuzzleFS};

/// A in iterator over a PuzzleFS filesystem. This iterates breadth first, since file content is
/// stored that way in a puzzlefs image so it'll be faster reading actual content if clients want
/// to do that.
pub struct WalkPuzzleFS<'a> {
    pfs: &'a mut PuzzleFS,
    q: VecDeque<DirEntry>,
}

impl<'a> WalkPuzzleFS<'a> {
    pub fn walk(pfs: &'a mut PuzzleFS) -> Result<WalkPuzzleFS<'a>> {
        let mut q = VecDeque::new();

        let inode = pfs.find_inode(1)?; // root inode number
        let de = DirEntry {
            oci: Arc::clone(&pfs.oci),
            path: PathBuf::from("/"),
            inode,
        };
        q.push_back(de);
        Ok(WalkPuzzleFS { pfs, q })
    }

    fn add_dir_entries(&mut self, dir: &DirEntry) -> Result<()> {
        if let InodeMode::Dir { ref dir_list } = dir.inode.mode {
            for entry in &dir_list.entries {
                let inode = self.pfs.find_inode(entry.ino)?;
                let path = dir.path.join(OsStr::from_bytes(&entry.name));
                self.q.push_back(DirEntry {
                    oci: Arc::clone(&self.pfs.oci),
                    path,
                    inode,
                })
            }
        };

        Ok(())
    }
}

impl Iterator for WalkPuzzleFS<'_> {
    type Item = Result<DirEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        let de = self.q.pop_front()?;
        Some(self.add_dir_entries(&de).map(|_| de))
    }
}

pub struct DirEntry {
    oci: Arc<Image>,
    pub path: PathBuf,
    pub inode: Inode,
}

impl DirEntry {
    /// Opens this DirEntry if it is a file.
    pub fn open(&self) -> Result<FileReader<'_>> {
        FileReader::new(&self.oci, &self.inode)
    }
}

#[cfg(test)]
mod tests {
    use tempfile::{tempdir, TempDir};

    use std::fs;
    use std::path::Path;

    use crate::builder::build_test_fs;

    use super::*;

    #[test]
    fn test_walk() {
        // make ourselves a test image
        let oci_dir = tempdir().unwrap();
        let image = Image::new(oci_dir.path()).unwrap();
        build_test_fs(Path::new("src/builder/test/test-1"), &image, "test").unwrap();
        let mut pfs = PuzzleFS::open(image, "test", None).unwrap();

        let mut walker = WalkPuzzleFS::walk(&mut pfs).unwrap();

        let root = walker.next().unwrap().unwrap();
        assert_eq!(root.path.to_string_lossy(), "/");
        assert_eq!(root.inode.ino, 1);
        assert_eq!(root.inode.dir_entries().unwrap().len(), 1);

        let jpg_file = walker.next().unwrap().unwrap();
        assert_eq!(jpg_file.path.to_string_lossy(), "/SekienAkashita.jpg");
        assert_eq!(jpg_file.inode.ino, 2);
        assert_eq!(jpg_file.inode.file_len().unwrap(), 109466);
    }

    #[test]
    fn test_xattrs() {
        // since walk provides us a nice API, we test some other basics of the builder here too.
        let dir = TempDir::new_in(".").unwrap();
        let oci_dir = dir.path().join("oci");
        let image = Image::new(&oci_dir).unwrap();
        let rootfs = dir.path().join("rootfs");

        let foo = rootfs.join("foo");
        let bar = rootfs.join("bar");

        // test directory, file types. we should probably also test "other" types, but on fifos and
        // symlinks on linux xattrs aren't allowed, so we just punt for now. maybe when 5.8 is more
        // prevalent, we can use mknod c 0 0?
        fs::create_dir_all(&foo).unwrap();
        fs::write(&bar, b"bar").unwrap();

        // set some xattrs
        for f in [&foo, &bar] {
            xattr::set(f, "user.meshuggah", b"rocks").unwrap();
        }

        build_test_fs(&rootfs, &image, "test").unwrap();

        let mut pfs = PuzzleFS::open(image, "test", None).unwrap();

        let mut walker = WalkPuzzleFS::walk(&mut pfs).unwrap();

        let root = walker.next().unwrap().unwrap();
        assert_eq!(root.path.to_string_lossy(), "/");
        assert_eq!(root.inode.ino, 1);
        assert_eq!(root.inode.dir_entries().unwrap().len(), 2);

        fn check_inode_xattrs(inode: Inode) {
            let additional = inode.additional.unwrap();
            assert_eq!(additional.xattrs[0].key, b"user.meshuggah");
            assert_eq!(additional.xattrs[0].val, b"rocks");
        }

        let bar_i = walker.next().unwrap().unwrap();
        assert_eq!(bar_i.path.to_string_lossy(), "/bar");
        check_inode_xattrs(bar_i.inode);

        let foo_i = walker.next().unwrap().unwrap();
        assert_eq!(foo_i.path.to_string_lossy(), "/foo");
        check_inode_xattrs(foo_i.inode);
    }
}
