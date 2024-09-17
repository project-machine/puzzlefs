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
    let mut oci_arg = oci.into_os_string();
    oci_arg.push(OsStr::new(":test"));
    puzzlefs([
        OsStr::new("build"),
        ubuntu_rootfs.as_ref(),
        oci_arg.as_ref(),
    ])?;

    let extracted = dir.path().join("extracted");
    puzzlefs([
        OsStr::new("extract"),
        oci_arg.as_os_str(),
        extracted.as_os_str(),
    ])?;
    assert!(!dir_diff::is_different(ubuntu_rootfs, extracted).unwrap());
    Ok(())
}
