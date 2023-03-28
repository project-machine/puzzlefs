use std::ffi::OsStr;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::str;

use anyhow::bail;
use assert_cmd::cargo::CommandCargoExt;
use std::env;

pub fn get_image<P: AsRef<Path>>(to_dir: P) -> anyhow::Result<PathBuf> {
    let image = "ubuntu";
    let tag = "latest";
    if to_dir.as_ref().exists() {
        let rootfs = to_dir.as_ref().join("rootfs");
        if rootfs.exists() {
            return Ok(rootfs);
        } else {
            bail!(
                "{:?} exists but does not have a rootfs directory in it",
                to_dir.as_ref().display()
            );
        }
    }

    let mut xdg_data_home_default = env::var("HOME").unwrap();
    xdg_data_home_default.push_str("/.local/share");

    let mut xdg_data_home = env::var("XDG_DATA_HOME").unwrap_or(xdg_data_home_default.clone());
    if xdg_data_home.is_empty() {
        xdg_data_home = xdg_data_home_default;
    }

    let puzzlefs_data_path = format!("{xdg_data_home}/puzzlefs");
    fs::create_dir_all(&puzzlefs_data_path)?;

    // skopeo copy docker://ubuntu:latest oci:$HOME/.local/share/puzzlefs/ubuntu:latest
    let output = Command::new("skopeo")
        .args([
            "copy",
            &format!("docker://{image}:{tag}"),
            &format!("oci:{puzzlefs_data_path}/{image}:{tag}"),
        ])
        .output()?;
    if !output.status.success() {
        bail!(
            "skopeo exited with error:\n{}",
            str::from_utf8(&output.stderr)?,
        );
    }

    if !output.stdout.is_empty() {
        println!(
            "skopeo output\n{}",
            str::from_utf8(&output.stdout).expect("Script output should not contain non-UTF8")
        );
    }

    // umoci unpack --rootless --image ubuntu:latest /tmp/.tmpxyz/ubuntu
    let output = Command::new("umoci")
        .args([
            OsStr::new("unpack"),
            OsStr::new("--rootless"),
            OsStr::new("--image"),
            OsStr::new(&format!("{puzzlefs_data_path}/{image}:{tag}")),
            to_dir.as_ref().as_os_str(),
        ])
        .output()?;
    if !output.status.success() {
        bail!(
            "umoci exited with error:\n{}",
            str::from_utf8(&output.stderr)?,
        );
    }

    if !output.stdout.is_empty() {
        println!(
            "umoci output\n{}",
            str::from_utf8(&output.stdout).expect("Script output should not contain non-UTF8")
        );
    }

    Ok(to_dir.as_ref().join("rootfs"))
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
