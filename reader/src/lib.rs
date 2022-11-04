extern crate fuse as fuse_ffi;

use std::path::Path;

use format::Result;
use oci::Image;

mod puzzlefs;
pub use puzzlefs::{Inode, InodeMode, PuzzleFS};

pub mod fuse;
pub use crate::fuse::Fuse;

mod walk;
pub use walk::WalkPuzzleFS;

pub fn mount(image: &Image, tag: &str, mountpoint: &Path) -> Result<()> {
    let pfs = PuzzleFS::open(image, tag)?;
    let fuse = Fuse::new(pfs, None);
    fuse_ffi::mount(fuse, &mountpoint, &[])?;
    Ok(())
}

pub fn spawn_mount<'a>(
    image: &'a Image,
    tag: &str,
    mountpoint: &Path,
    sender: Option<std::sync::mpsc::Sender<()>>,
) -> Result<fuse_ffi::BackgroundSession<'a>> {
    let pfs = PuzzleFS::open(image, tag)?;
    let fuse = Fuse::new(pfs, sender);
    unsafe { Ok(fuse_ffi::spawn_mount(fuse, &mountpoint, &[])?) }
}
