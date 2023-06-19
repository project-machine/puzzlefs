extern crate fuser as fuse_ffi;

use std::path::Path;

use format::Result;
use oci::Image;

mod puzzlefs;
pub use puzzlefs::PuzzleFS;

pub mod fuse;
pub use crate::fuse::Fuse;

mod walk;
use crate::fuse::PipeDescriptor;
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

pub fn mount<T: AsRef<str>>(
    image: Image,
    tag: &str,
    mountpoint: &Path,
    options: &[T],
    init_notify: Option<PipeDescriptor>,
    manifest_verity: Option<&[u8]>,
) -> Result<()> {
    let pfs = PuzzleFS::open(image, tag, manifest_verity)?;
    let fuse = Fuse::new(pfs, None, init_notify);
    fuse_ffi::mount2(
        fuse,
        mountpoint,
        &options
            .iter()
            .map(|option| mount_option_from_str(option.as_ref()))
            .collect::<Vec<_>>(),
    )?;
    Ok(())
}

pub fn spawn_mount<T: AsRef<str>>(
    image: Image,
    tag: &str,
    mountpoint: &Path,
    options: &[T],
    init_notify: Option<PipeDescriptor>,
    sender: Option<std::sync::mpsc::Sender<()>>,
    manifest_verity: Option<&[u8]>,
) -> Result<fuse_ffi::BackgroundSession> {
    let pfs = PuzzleFS::open(image, tag, manifest_verity)?;
    let fuse = Fuse::new(pfs, sender, init_notify);
    Ok(fuse_ffi::spawn_mount2(
        fuse,
        mountpoint,
        &options
            .iter()
            .map(|option| mount_option_from_str(option.as_ref()))
            .collect::<Vec<_>>(),
    )?)
}
