extern crate fuser as fuse_ffi;

use std::path::Path;

use format::Result;
use oci::Image;

mod puzzlefs;
pub use puzzlefs::{Inode, InodeMode, PuzzleFS};

pub mod fuse;
pub use crate::fuse::Fuse;

mod walk;
pub use walk::WalkPuzzleFS;

pub fn mount(image: Image, tag: &str, mountpoint: &Path) -> Result<()> {
    let pfs = PuzzleFS::open(image, tag)?;
    let fuse = Fuse::new(pfs, None);
    fuse_ffi::mount2(fuse, mountpoint, &[])?;
    Ok(())
}

pub fn spawn_mount(
    image: Image,
    tag: &str,
    mountpoint: &Path,
    sender: Option<std::sync::mpsc::Sender<()>>,
) -> Result<fuse_ffi::BackgroundSession> {
    let pfs = PuzzleFS::open(image, tag)?;
    let fuse = Fuse::new(pfs, sender);
    Ok(fuse_ffi::spawn_mount2(fuse, mountpoint, &[])?)
}
