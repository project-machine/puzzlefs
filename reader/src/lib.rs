extern crate fuser as fuse_ffi;

use std::path::Path;

use format::Result;
use oci::Image;

mod puzzlefs;
pub use puzzlefs::{Inode, InodeMode, PuzzleFS};

pub mod fuse;
pub use crate::fuse::Fuse;

mod walk;
use os_pipe::PipeWriter;
pub use walk::WalkPuzzleFS;

// copied from the fuser function 'MountOption::from_str' because it's not exported
fn mount_option_from_str(s: &str) -> fuse_ffi::MountOption {
    match s {
        "auto_unmount" => fuse_ffi::MountOption::AutoUnmount,
        "allow_other" => fuse_ffi::MountOption::AllowOther,
        "allow_root" => fuse_ffi::MountOption::AllowRoot,
        "default_permissions" => fuse_ffi::MountOption::DefaultPermissions,
        "dev" => fuse_ffi::MountOption::Dev,
        "nodev" => fuse_ffi::MountOption::NoDev,
        "suid" => fuse_ffi::MountOption::Suid,
        "nosuid" => fuse_ffi::MountOption::NoSuid,
        "ro" => fuse_ffi::MountOption::RO,
        "rw" => fuse_ffi::MountOption::RW,
        "exec" => fuse_ffi::MountOption::Exec,
        "noexec" => fuse_ffi::MountOption::NoExec,
        "atime" => fuse_ffi::MountOption::Atime,
        "noatime" => fuse_ffi::MountOption::NoAtime,
        "dirsync" => fuse_ffi::MountOption::DirSync,
        "sync" => fuse_ffi::MountOption::Sync,
        "async" => fuse_ffi::MountOption::Async,
        x if x.starts_with("fsname=") => fuse_ffi::MountOption::FSName(x[7..].into()),
        x if x.starts_with("subtype=") => fuse_ffi::MountOption::Subtype(x[8..].into()),
        x => fuse_ffi::MountOption::CUSTOM(x.into()),
    }
}

pub fn mount(
    image: Image,
    tag: &str,
    mountpoint: &Path,
    options: &[&str],
    init_notify: Option<PipeWriter>,
) -> Result<()> {
    let pfs = PuzzleFS::open(image, tag)?;
    let fuse = Fuse::new(pfs, None, init_notify);
    fuse_ffi::mount2(
        fuse,
        mountpoint,
        &options
            .iter()
            .map(|option| mount_option_from_str(option))
            .collect::<Vec<_>>(),
    )?;
    Ok(())
}

pub fn spawn_mount(
    image: Image,
    tag: &str,
    mountpoint: &Path,
    options: &[&str],
    sender: Option<std::sync::mpsc::Sender<()>>,
) -> Result<fuse_ffi::BackgroundSession> {
    let pfs = PuzzleFS::open(image, tag)?;
    let fuse = Fuse::new(pfs, sender, None);
    Ok(fuse_ffi::spawn_mount2(
        fuse,
        mountpoint,
        &options
            .iter()
            .map(|option| mount_option_from_str(option))
            .collect::<Vec<_>>(),
    )?)
}
