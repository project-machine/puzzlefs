use std::ffi::OsStr;
use std::io;
use std::path::Path;
use std::process::Command;
use std::str;

use anyhow::bail;
use assert_cmd::cargo::CommandCargoExt;

pub fn get_image<P: AsRef<Path>>(to_dir: P) -> io::Result<()> {
    let image = "ubuntu";
    let tag = "latest";
    if to_dir.as_ref().exists() {
        return Ok(());
    }
    docker_extract::extract_image(image, tag, to_dir.as_ref())
}

pub fn puzzlefs<I, S>(args: I) -> anyhow::Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut cmd = Command::cargo_bin("puzzlefs").unwrap();
    let output = cmd.args(args).output()?;
    if !output.status.success() {
        bail!(
            "puzzlefs exited with error:\n{}",
            str::from_utf8(&output.stderr)?,
        );
    }
    let output = str::from_utf8(&output.stdout).expect("Script output should not contain non-UTF8");
    Ok(output.to_string())
}
