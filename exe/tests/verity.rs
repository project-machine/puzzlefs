mod verity_setup;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use verity_setup::VeritySetup;
pub mod helpers;
use helpers::puzzlefs;
use std::ffi::OsStr;
use std::fs::OpenOptions;
use walkdir::WalkDir;

const RANDOM_DIGEST: &str = "99a3d81481ed522712e5a8208024984778ec302971129e3f28b646a354fd27d0";

fn fuser_umount(puzzlefs_mountpoint: PathBuf) -> anyhow::Result<()> {
    // try fusermount3
    let mut cmd = Command::new("fusermount3");
    let status = cmd
        .args([
            "-u",
            &puzzlefs_mountpoint
                .clone()
                .into_os_string()
                .into_string()
                .unwrap(),
        ])
        .status();

    match status {
        Err(e) => {
            // figure out how to write if not let
            if let ErrorKind::NotFound = e.kind() {
            } else {
                return Err(e.into());
            }
        }
        Ok(res) => {
            assert!(res.success());
            return Ok(());
        }
    }

    // try fusermount
    let mut cmd = Command::new("fusermount");
    let status = cmd
        .args([
            "-u",
            &puzzlefs_mountpoint.into_os_string().into_string().unwrap(),
        ])
        .status()?;

    assert!(status.success());
    Ok(())
}

fn check_tamper(oci_path: &Path) -> anyhow::Result<()> {
    for file in WalkDir::new(oci_path.join("blobs").join("sha256")).into_iter() {
        let file = file?;
        if !file.metadata()?.is_file() {
            continue;
        }
        // we should get permission denied when trying to open blobs for writing
        let error = OpenOptions::new()
            .write(true)
            .open(file.path())
            .unwrap_err();
        if let ErrorKind::PermissionDenied = error.kind() {
        } else {
            return Err(error.into());
        }
    }
    Ok(())
}

#[test]
fn test_fs_verity() -> anyhow::Result<()> {
    let v = VeritySetup::new()?;

    let mount_path = Path::new(&v.mountpoint);
    let rootfs = Path::new("../puzzlefs-lib/src/builder/test/test-1/");

    let oci = mount_path.join("oci");
    let output = puzzlefs([
        OsStr::new("build"),
        rootfs.as_ref(),
        oci.as_ref(),
        OsStr::new("test"),
    ])?;

    let tokens = output.split_whitespace().collect::<Vec<_>>();

    let digest = tokens
        .last()
        .expect("puzzlefs build should have returned the puzzlefs image manifest digest");

    // 32 bytes in SHA256, each represented by 2 hex digits
    assert_eq!(digest.len(), 32 * 2);

    println!("digest: {digest}");

    puzzlefs([
        OsStr::new("enable-fs-verity"),
        oci.as_ref(),
        OsStr::new("test"),
        OsStr::new(digest),
    ])?;

    check_tamper(&oci)?;

    let puzzlefs_mountpoint = mount_path.join("mount");
    fs::create_dir_all(&puzzlefs_mountpoint)?;

    // test that we can't mount with the wrong digest
    let mount_output = puzzlefs([
        OsStr::new("mount"),
        // foreground mode because background mode hangs on errors
        OsStr::new("-f"),
        OsStr::new("-d"),
        OsStr::new(RANDOM_DIGEST),
        oci.as_ref(),
        OsStr::new("test"),
        OsStr::new(&puzzlefs_mountpoint),
    ]);

    assert!(mount_output
        .unwrap_err()
        .to_string()
        .contains("invalid fs_verity data: fsverity mismatch"));

    // test that we can mount with the right digest
    puzzlefs([
        OsStr::new("mount"),
        OsStr::new("-d"),
        OsStr::new(digest),
        oci.as_ref(),
        OsStr::new("test"),
        OsStr::new(&puzzlefs_mountpoint),
    ])?;

    fuser_umount(puzzlefs_mountpoint)?;

    Ok(())
}
