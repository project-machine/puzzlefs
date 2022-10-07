use std::collections::VecDeque;
use std::path::PathBuf;

use format::Result;
use oci::Image;

use super::puzzlefs::{FileReader, Inode, InodeMode, PuzzleFS};

/// A in iterator over a PuzzleFS filesystem. This iterates breadth first, since file content is
/// stored that way in a puzzlefs image so it'll be faster reading actual content if clients want
/// to do that.
pub struct WalkPuzzleFS<'a> {
    pfs: &'a mut PuzzleFS<'a>,
    q: VecDeque<DirEntry<'a>>,
}

impl<'a> WalkPuzzleFS<'a> {
    pub fn walk(pfs: &'a mut PuzzleFS<'a>) -> Result<WalkPuzzleFS<'a>> {
        let mut q = VecDeque::new();

        let inode = pfs.find_inode(1)?; // root inode number
        let de = DirEntry {
            oci: pfs.oci,
            path: PathBuf::from("/"),
            inode,
        };
        q.push_back(de);
        Ok(WalkPuzzleFS { pfs, q })
    }

    fn add_dir_entries(&mut self, dir: &DirEntry) -> Result<()> {
        if let InodeMode::Dir { ref entries } = dir.inode.mode {
            for (name, ino) in entries {
                let inode = self.pfs.find_inode(*ino)?;
                let path = dir.path.join(name);
                self.q.push_back(DirEntry {
                    oci: self.pfs.oci,
                    path,
                    inode,
                })
            }
        };

        Ok(())
    }
}

impl<'a> Iterator for WalkPuzzleFS<'a> {
    type Item = Result<DirEntry<'a>>;

    fn next(&mut self) -> Option<Self::Item> {
        let de = self.q.pop_front()?;
        Some(self.add_dir_entries(&de).map(|_| de))
    }
}

pub struct DirEntry<'a> {
    oci: &'a Image<'a>,
    pub path: PathBuf,
    pub inode: Inode,
}

impl<'a> DirEntry<'a> {
    /// Opens this DirEntry if it is a file.
    pub fn open(&'a self) -> Result<FileReader<'a>> {
        FileReader::new(self.oci, &self.inode)
    }
}

#[cfg(test)]
mod tests {
    extern crate xattr;

    use tempfile::{tempdir, TempDir};

    use std::fs;

    use builder::{build_initial_rootfs, build_test_fs};
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

        let mut walker = WalkPuzzleFS::walk(&mut pfs).unwrap();

        let root = walker.next().unwrap().unwrap();
        assert_eq!(root.path.to_string_lossy(), "/");
        assert_eq!(root.inode.inode.ino, 1);
        assert_eq!(root.inode.dir_entries().unwrap().len(), 1);

        let jpg_file = walker.next().unwrap().unwrap();
        assert_eq!(jpg_file.path.to_string_lossy(), "/SekienAkashita.jpg");
        assert_eq!(jpg_file.inode.inode.ino, 2);
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

        let rootfs_desc = build_initial_rootfs(&rootfs, &image).unwrap();

        image.add_tag("test".to_string(), rootfs_desc).unwrap();
        let mut pfs = PuzzleFS::open(&image, "test").unwrap();

        let mut walker = WalkPuzzleFS::walk(&mut pfs).unwrap();

        let root = walker.next().unwrap().unwrap();
        assert_eq!(root.path.to_string_lossy(), "/");
        assert_eq!(root.inode.inode.ino, 1);
        assert_eq!(root.inode.dir_entries().unwrap().len(), 2);

        fn check_inode_xattrs(inode: Inode) {
            let additional = inode.additional.unwrap();
            assert_eq!(additional.xattrs[0].key, "user.meshuggah");
            assert_eq!(additional.xattrs[0].val.as_ref().unwrap(), b"rocks");
        }

        let bar_i = walker.next().unwrap().unwrap();
        assert_eq!(bar_i.path.to_string_lossy(), "/bar");
        check_inode_xattrs(bar_i.inode);

        let foo_i = walker.next().unwrap().unwrap();
        assert_eq!(foo_i.path.to_string_lossy(), "/foo");
        check_inode_xattrs(foo_i.inode);
    }
}
