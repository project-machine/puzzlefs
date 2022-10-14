#[macro_use]
extern crate anyhow;

use nix::sys::stat::{makedev, mknod, Mode, SFlag};
use nix::unistd::{mkfifo, symlinkat};
use oci::Image;
use reader::{InodeMode, PuzzleFS, WalkPuzzleFS};
use std::path::{Component, Path, PathBuf};
use std::{fs, io};

fn safe_path(dir: &Path, image_path: &Path) -> anyhow::Result<PathBuf> {
    // need to be a bit careful here about paths in the case of malicious images so we don't write
    // things outside where we're supposed to. Bad cases are paths like "/../../.." or images
    // /usr/bin -> /bin and files in /usr/bin, we shouldn't write files anywhere outside the target
    // dir.

    let mut buf = PathBuf::new();
    buf.push(dir);
    let mut level = 1;

    for component in image_path.components() {
        match component {
            Component::Prefix(..) => bail!("Path prefix not understood"), // "Does not occur on Unix."
            Component::RootDir => {}
            Component::CurDir => {}
            Component::Normal(c) => {
                buf.push(c);
                level += 1;

                // make sure this isn't a symlink
                match fs::symlink_metadata(&buf) {
                    Ok(md) => {
                        if md.file_type().is_symlink() {
                            bail!("symlink prefixes are not allowed: {:#?}", buf)
                        }
                    }
                    Err(e) => {
                        if e.kind() != io::ErrorKind::NotFound {
                            bail!("problem accessing path component {:#?}: {}", buf, e)
                        }

                        // we render each dir, so the first ENOENT should be the lowest path. could
                        // maybe double check this if we really felt it was necessary...
                        return Ok(buf);
                    }
                }
            }
            Component::ParentDir => {
                level -= 1;
                if level <= 0 {
                    bail!("image path escapes extract dir: {:#?}", image_path)
                }
                buf.pop();
            }
        }
    }

    Ok(buf)
}

pub fn extract_rootfs(oci_dir: &str, tag: &str, extract_dir: &str) -> anyhow::Result<()> {
    let oci_dir = Path::new(oci_dir);
    let image = Image::new(oci_dir)?;
    let dir = Path::new(extract_dir);
    fs::create_dir_all(dir)?;
    let mut pfs = PuzzleFS::open(&image, tag)?;
    let mut walker = WalkPuzzleFS::walk(&mut pfs)?;
    walker.try_for_each(|de| -> anyhow::Result<()> {
        let dir_entry = de?;
        let path = safe_path(dir, &dir_entry.path)?;
        // TODO: real logging :)
        eprintln!("extracting {:#?}", path);
        match dir_entry.inode.mode {
            InodeMode::File { .. } => {
                let mut reader = dir_entry.open()?;
                let mut f = fs::File::create(path)?;
                io::copy(&mut reader, &mut f)?;
            }
            InodeMode::Dir { .. } => fs::create_dir_all(path)?,
            InodeMode::Other => {
                match dir_entry.inode.inode.mode {
                    // TODO: fix all the hard coded modes when we have modes
                    format::InodeMode::Fifo => {
                        mkfifo(&path, Mode::S_IRWXU)?;
                    }
                    format::InodeMode::Chr { major, minor } => {
                        mknod(&path, SFlag::S_IFCHR, Mode::S_IRWXU, makedev(major, minor))?;
                    }
                    format::InodeMode::Blk { major, minor } => {
                        mknod(&path, SFlag::S_IFBLK, Mode::S_IRWXU, makedev(major, minor))?;
                    }
                    format::InodeMode::Lnk => {
                        let target = dir_entry.inode.symlink_target()?;
                        symlinkat(target.as_os_str(), None, &path)?;
                    }
                    format::InodeMode::Sock => {
                        todo!();
                    }
                    format::InodeMode::Wht => {
                        todo!();
                    }
                    _ => {
                        bail!("bad inode mode {:#?}", dir_entry.inode.inode.mode)
                    }
                }
            }
        }
        Ok(())
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use std::fs;

    use builder::build_initial_rootfs;
    use oci::Image;
    use std::collections::HashMap;
    use walkdir::WalkDir;

    use super::*;

    #[test]
    fn test_extracted_xattrs() {
        let dir = TempDir::new_in(".").unwrap();
        let oci_dir = dir.path().join("oci");
        let image = Image::new(&oci_dir).unwrap();
        let rootfs = dir.path().join("rootfs");
        let extract_dir = TempDir::new_in(".").unwrap();

        let foo = rootfs.join("foo");
        let bar = rootfs.join("bar");

        let mut file_attributes = HashMap::<String, Vec<u8>>::new();
        file_attributes.insert("user.meshuggah".to_string(), b"rocks".to_vec());
        file_attributes.insert("user.nothing".to_string(), b"".to_vec());

        // test directory, file types. we should probably also test "other" types, but on fifos and
        // symlinks on linux xattrs aren't allowed, so we just punt for now. maybe when 5.8 is more
        // prevalent, we can use mknod c 0 0?
        fs::create_dir_all(&foo).unwrap();
        fs::write(&bar, b"bar").unwrap();

        // set some xattrs
        for f in [&foo, &bar] {
            for (key, val) in &file_attributes {
                xattr::set(f, key, val).unwrap();
                xattr::set(f, key, val).unwrap();
            }
        }

        let rootfs_desc = build_initial_rootfs(&rootfs, &image).unwrap();

        image.add_tag("test".to_string(), rootfs_desc).unwrap();

        extract_rootfs(
            oci_dir.to_str().unwrap(),
            "test",
            extract_dir.path().to_str().unwrap(),
        )
        .unwrap();

        let ents = WalkDir::new(&extract_dir)
            .contents_first(false)
            .follow_links(false)
            .same_file_system(true)
            .sort_by(|a, b| a.file_name().cmp(b.file_name()))
            .into_iter()
            .collect::<Result<Vec<walkdir::DirEntry>, walkdir::Error>>()
            .unwrap();

        // the first directory is extract_dir, we don't check xattrs for it
        for ent in ents.into_iter().skip(1) {
            for (key, val) in &file_attributes {
                let attribute = xattr::get(ent.path(), key);
                println!(
                    "path: {:?} key: {:?} attribute: {:?}",
                    ent.path(),
                    key,
                    attribute
                );
                assert!(attribute.unwrap().as_ref().unwrap() == val);
            }
        }
    }
}
