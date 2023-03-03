#[macro_use]
extern crate anyhow;

use log::info;
use nix::sys::stat::{makedev, mknod, Mode, SFlag};
use nix::unistd::{chown, mkfifo, symlinkat, Gid, Uid};
use oci::Image;
use reader::{InodeMode, PuzzleFS, WalkPuzzleFS};
use std::collections::HashMap;
use std::fs::Permissions;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::{fs, io};

fn runs_privileged() -> bool {
    Uid::effective().is_root()
}

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
    let image = Image::open(oci_dir)?;
    let dir = Path::new(extract_dir);
    fs::create_dir_all(dir)?;
    let mut pfs = PuzzleFS::open(image, tag, None)?;
    let mut walker = WalkPuzzleFS::walk(&mut pfs)?;
    let mut host_to_pfs = HashMap::<format::Ino, PathBuf>::new();

    walker.try_for_each(|de| -> anyhow::Result<()> {
        let dir_entry = de?;
        let path = safe_path(dir, &dir_entry.path)?;
        let mut is_symlink = false;
        info!("extracting {:#?}", path);
        if let Some(existing_path) = host_to_pfs.get(&dir_entry.inode.inode.ino) {
            fs::hard_link(existing_path, &path)?;
            return Ok(());
        }
        host_to_pfs.insert(dir_entry.inode.inode.ino, path.clone());

        match dir_entry.inode.mode {
            InodeMode::File { .. } => {
                let mut reader = dir_entry.open()?;
                let mut f = fs::File::create(&path)?;
                io::copy(&mut reader, &mut f)?;
            }
            InodeMode::Dir { .. } => fs::create_dir_all(&path)?,
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
                        is_symlink = true;
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
        if let Some(x) = dir_entry.inode.additional {
            for x in &x.xattrs {
                xattr::set(&path, &x.key, &x.val)?;
            }
        }

        // trying to change permissions for a symlink would follow the symlink and we might not have extracted the target yet
        // anyway, symlink permissions are not used in Linux (although they are used in macOS and FreeBSD)
        if !is_symlink {
            std::fs::set_permissions(
                &path,
                Permissions::from_mode(dir_entry.inode.inode.permissions.into()),
            )?;
        }

        if runs_privileged() {
            chown(
                &path,
                Some(Uid::from_raw(dir_entry.inode.inode.uid)),
                Some(Gid::from_raw(dir_entry.inode.inode.gid)),
            )?;
        }

        Ok(())
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::{tempdir, TempDir};

    use std::fs;
    use std::fs::File;

    use builder::build_initial_rootfs;
    use builder::build_test_fs;
    use oci::Image;
    use std::collections::HashMap;
    use std::os::unix::fs::MetadataExt;
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

    #[test]
    fn test_permissions() {
        let dir = tempdir().unwrap();
        let oci_dir = dir.path().join("oci");
        let image = Image::new(&oci_dir).unwrap();
        let rootfs = dir.path().join("rootfs");
        let extract_dir = tempdir().unwrap();
        const TESTED_PERMISSION: u32 = 0o7777;

        let foo = rootfs.join("foo");

        fs::create_dir_all(&rootfs).unwrap();
        fs::write(&foo, b"foo").unwrap();

        std::fs::set_permissions(foo, Permissions::from_mode(TESTED_PERMISSION)).unwrap();

        let rootfs_desc = build_initial_rootfs(&rootfs, &image).unwrap();

        image.add_tag("test".to_string(), rootfs_desc).unwrap();

        extract_rootfs(
            oci_dir.to_str().unwrap(),
            "test",
            extract_dir.path().to_str().unwrap(),
        )
        .unwrap();

        let extracted_path = extract_dir.path().join("foo");
        let f = File::open(extracted_path).unwrap();
        let metadata = f.metadata().unwrap();

        assert_eq!(metadata.permissions().mode() & 0xFFF, TESTED_PERMISSION);
    }

    #[test]
    fn test_hardlink_extraction() {
        let dir = tempdir().unwrap();
        let oci_dir = dir.path().join("oci");
        let image = Image::new(&oci_dir).unwrap();
        let rootfs = dir.path().join("rootfs");
        let extract_dir = tempdir().unwrap();

        let foo = rootfs.join("foo");
        let bar = rootfs.join("bar");

        fs::create_dir_all(&rootfs).unwrap();
        fs::write(&foo, b"foo").unwrap();

        fs::hard_link(&foo, &bar).unwrap();

        assert_eq!(
            fs::metadata(&foo).unwrap().ino(),
            fs::metadata(&bar).unwrap().ino()
        );

        let rootfs_desc = build_initial_rootfs(&rootfs, &image).unwrap();

        image.add_tag("test".to_string(), rootfs_desc).unwrap();

        extract_rootfs(
            oci_dir.to_str().unwrap(),
            "test",
            extract_dir.path().to_str().unwrap(),
        )
        .unwrap();

        let foo = extract_dir.path().join("foo");
        let bar = extract_dir.path().join("bar");

        assert_eq!(
            fs::metadata(foo).unwrap().ino(),
            fs::metadata(bar).unwrap().ino()
        );
    }

    #[test]
    fn test_empty_file() {
        let dir = tempdir().unwrap();
        let oci_dir = dir.path().join("oci");
        let image = Image::new(&oci_dir).unwrap();
        let rootfs = dir.path().join("rootfs");
        let foo = rootfs.join("foo");
        let extract_dir = tempdir().unwrap();

        fs::create_dir_all(&rootfs).unwrap();
        std::fs::File::create(foo).unwrap();

        let rootfs_desc = build_test_fs(&rootfs, &image).unwrap();
        image.add_tag("test".to_string(), rootfs_desc).unwrap();

        extract_rootfs(
            oci_dir.to_str().unwrap(),
            "test",
            extract_dir.path().to_str().unwrap(),
        )
        .unwrap();
        let extracted_foo = extract_dir.path().join("foo");
        assert_eq!(extracted_foo.metadata().unwrap().len(), 0);
    }
}
