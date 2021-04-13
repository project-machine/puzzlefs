use std::ffi::OsStr;
use std::io;
use std::path::Path;
use std::process::Command;

use assert_cmd::cargo::CommandCargoExt;

pub fn get_image<P: AsRef<Path>>(to_dir: P) -> io::Result<()> {
    let image = "ubuntu";
    let tag = "latest";
    if to_dir.as_ref().exists() {
        return Ok(());
    }
    docker_extract::extract_image(image, tag, to_dir.as_ref())
}

pub fn puzzlefs<I, S>(args: I)
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut cmd = Command::cargo_bin("puzzlefs").unwrap();
    assert!(cmd.args(args).status().unwrap().success());
}
