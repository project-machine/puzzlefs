use std::ffi::OsStr;
use tempfile::tempdir;

// see https://github.com/rust-lang/rust/issues/46379#issuecomment-548787629
pub mod helpers;
use helpers::{get_image, puzzlefs};

#[test]
fn build_and_extract_is_noop() -> anyhow::Result<()> {
    let dir = tempdir().unwrap();
    let ubuntu = dir.path().join("ubuntu");
    let ubuntu_rootfs = get_image(ubuntu)?;

    // TODO: figure out a better way to do all this osstr stuff...
    let oci = dir.path().join("oci");
    puzzlefs([
        OsStr::new("build"),
        ubuntu_rootfs.as_ref(),
        oci.as_ref(),
        OsStr::new("test"),
    ])?;

    let extracted = dir.path().join("extracted");
    puzzlefs([
        OsStr::new("extract"),
        oci.as_os_str(),
        OsStr::new("test"),
        extracted.as_os_str(),
    ])?;
    assert!(!dir_diff::is_different(ubuntu_rootfs, extracted).unwrap());
    Ok(())
}
