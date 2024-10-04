use clap::{Args, Parser, Subcommand};
use daemonize::Daemonize;
use env_logger::Env;
use libmount::mountinfo;
use libmount::Overlay;
use log::{error, info, LevelFilter};
use nix::mount::umount;
use nix::unistd::Uid;
use os_pipe::{PipeReader, PipeWriter};
use puzzlefs_lib::{
    builder::{add_rootfs_delta, build_initial_rootfs, enable_fs_verity},
    compression::{Noop, Zstd},
    extractor::extract_rootfs,
    fsverity_helpers::get_fs_verity_digest,
    oci::Image,
    reader::{fuse::PipeDescriptor, mount, spawn_mount},
};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::fs::OpenOptions;
use std::io::prelude::*;
use std::path::{Path, PathBuf};
use std::process::exit;
use std::sync::Arc;
use syslog::{BasicLogger, Facility, Formatter3164};

#[derive(Parser)]
#[command(author, version, about)]
struct Opts {
    #[command(subcommand)]
    subcmd: SubCommand,
}

#[derive(Subcommand)]
enum SubCommand {
    Build(Build),
    Mount(Mount),
    Umount(Umount),
    Extract(Extract),
    EnableFsVerity(FsVerity),
}

#[derive(Args)]
struct Build {
    rootfs: String,
    oci_dir: String,
    #[arg(short, long, value_name = "base-layer")]
    base_layer: Option<String>,
    #[arg(short, long, value_name = "compressed")]
    compression: bool,
}

#[derive(Args)]
struct Mount {
    oci_dir: String,
    mountpoint: String,
    #[arg(short, long)]
    foreground: bool,
    #[arg(short, long, value_name = "init-pipe")]
    init_pipe: Option<String>,
    #[arg(short, value_delimiter = ',')]
    options: Option<Vec<String>>,
    #[arg(short, long, value_name = "fs verity root digest")]
    digest: Option<String>,
    #[arg(short, long, conflicts_with = "foreground")]
    writable: bool,
    #[arg(short, long, conflicts_with = "foreground")]
    persist: Option<String>,
}

#[derive(Args)]
struct Umount {
    mountpoint: String,
}

#[derive(Args)]
struct Extract {
    oci_dir: String,
    extract_dir: String,
}

#[derive(Args)]
struct FsVerity {
    oci_dir: String,
    root_hash: String,
}

// set default log level when RUST_LOG environment variable is not set
fn init_logging(log_level: &str) {
    env_logger::Builder::from_env(Env::default().default_filter_or(log_level)).init();
}

fn init_syslog(log_level: &str) -> std::io::Result<()> {
    let formatter = Formatter3164 {
        facility: Facility::LOG_USER,
        hostname: None,
        process: "puzzlefs".into(),
        pid: 0,
    };

    let logger = match syslog::unix(formatter) {
        Err(e) => {
            println!("impossible to connect to syslog: {e:?}");
            return Err(std::io::Error::last_os_error());
        }
        Ok(logger) => logger,
    };
    log::set_boxed_logger(Box::new(BasicLogger::new(logger)))
        .map(|()| {
            log::set_max_level(match log_level {
                "off" => LevelFilter::Off,
                "error" => LevelFilter::Error,
                "warn" => LevelFilter::Warn,
                "info" => LevelFilter::Info,
                "debug" => LevelFilter::Debug,
                "trace" => LevelFilter::Trace,
                _ => panic!("unexpected log level"),
            })
        })
        .unwrap();
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn mount_background(
    image: Image,
    tag: &str,
    mountpoint: &Path,
    options: Option<Vec<String>>,
    manifest_verity: Option<Vec<u8>>,
    mut recv: PipeReader,
    init_notify: &PipeWriter,
    parent_action: impl FnOnce() -> anyhow::Result<()> + 'static,
) -> anyhow::Result<()> {
    let daemonize = Daemonize::new().exit_action(move || {
        let mut read_buffer = [0];
        if let Err(e) = recv.read_exact(&mut read_buffer) {
            info!("error reading from pipe {e}")
        } else if read_buffer[0] == b'f' {
            // in case of failure, 'f' is written into the pipe
            // we explicitly exit with an error code, otherwise exit(0) is done by daemonize
            exit(1);
        }
        if let Err(e) = parent_action() {
            error!("parent_action error {e}");
        }
    });

    match daemonize.start() {
        Ok(_) => {
            mount(
                image,
                tag,
                mountpoint,
                &options.unwrap_or_default()[..],
                Some(PipeDescriptor::UnnamedPipe(init_notify.try_clone()?)),
                manifest_verity.as_deref(),
            )?;
        }
        Err(e) => {
            return Err(e.into());
        }
    };
    Ok(())
}

fn parse_oci_dir(oci_dir: &str) -> anyhow::Result<(&str, &str)> {
    let components: Vec<&str> = oci_dir.split_terminator(":").collect();
    if components.len() != 2 {
        anyhow::bail!("Expected oci_dir in the following format <oci_dir>:<tag> ")
    }

    Ok((components[0], components[1]))
}

fn get_mount_type(mountpoint: &str) -> anyhow::Result<OsString> {
    let contents = fs::read_to_string("/proc/self/mountinfo")?;
    let mut parser = mountinfo::Parser::new(contents.as_bytes());
    let mount_info = parser.find(|mount_info| {
        mount_info
            .as_ref()
            .map(|mount_info| mount_info.mount_point == OsStr::new(mountpoint))
            .unwrap_or(false)
    });
    let mount_info = mount_info
        .ok_or_else(|| anyhow::anyhow!("cannot find mountpoint in /proc/self/mountpoints"))??;
    Ok(mount_info.fstype.into_owned())
}

fn main() -> anyhow::Result<()> {
    let opts: Opts = Opts::parse();
    match opts.subcmd {
        SubCommand::Build(b) => {
            let rootfs = Path::new(&b.rootfs);
            let (oci_dir, tag) = parse_oci_dir(&b.oci_dir)?;
            let oci_dir = Path::new(oci_dir);
            let image = Image::new(oci_dir)?;
            let new_image = match b.base_layer {
                Some(base_layer) => {
                    let (_desc, image) = if b.compression {
                        add_rootfs_delta::<Zstd>(rootfs, image, tag, &base_layer)?
                    } else {
                        add_rootfs_delta::<Noop>(rootfs, image, tag, &base_layer)?
                    };
                    image
                }
                None => {
                    if b.compression {
                        build_initial_rootfs::<Zstd>(rootfs, &image, tag)?
                    } else {
                        build_initial_rootfs::<Noop>(rootfs, &image, tag)?
                    };
                    Arc::new(image)
                }
            };
            let mut manifest_fd = new_image.get_image_manifest_fd(tag)?;
            let mut read_buffer = Vec::new();
            manifest_fd.read_to_end(&mut read_buffer)?;
            let manifest_digest = get_fs_verity_digest(&read_buffer)?;
            println!(
                "puzzlefs image manifest digest: {}",
                hex::encode(manifest_digest)
            );
            Ok(())
        }
        SubCommand::Mount(m) => {
            let log_level = "info";
            if m.foreground {
                init_logging(log_level);
            } else {
                init_syslog(log_level)?;
            }

            if (m.writable || m.persist.is_some()) && !Uid::effective().is_root() {
                anyhow::bail!("Writable mounts can only be created by the root user!")
            }

            let (oci_dir, tag) = parse_oci_dir(&m.oci_dir)?;
            let oci_dir = Path::new(oci_dir);
            let oci_dir = fs::canonicalize(oci_dir)?;
            let image = Image::open(&oci_dir)?;
            let mountpoint = Path::new(&m.mountpoint);
            let mountpoint = fs::canonicalize(mountpoint)?;

            let manifest_verity = m.digest.map(hex::decode).transpose()?;

            if m.writable || m.persist.is_some() {
                // We only support background mounts with the writable|persist flag
                let (recv, mut init_notify) = os_pipe::pipe()?;
                let pfs_mountpoint = mountpoint.join("ro");
                fs::create_dir_all(&pfs_mountpoint)?;

                if let Err(e) = mount_background(
                    image,
                    tag,
                    &pfs_mountpoint.clone(),
                    m.options,
                    manifest_verity,
                    recv,
                    &init_notify,
                    move || {
                        let ovl_workdir = mountpoint.join("work");
                        fs::create_dir_all(&ovl_workdir)?;
                        let ovl_upperdir = match m.persist {
                            None => mountpoint.join("upper"),
                            Some(upperdir) => Path::new(&upperdir).to_path_buf(),
                        };
                        fs::create_dir_all(&ovl_upperdir)?;
                        let overlay = Overlay::writable(
                            [pfs_mountpoint.as_path()].into_iter(),
                            ovl_upperdir,
                            ovl_workdir,
                            &mountpoint,
                        );
                        overlay.mount().map_err(|e| anyhow::anyhow!("{e}"))
                    },
                ) {
                    if let Err(e) = init_notify.write_all(b"f") {
                        error!("puzzlefs will hang because we couldn't write to pipe, {e}");
                    }
                    error!("mount_background failed: {e}");
                    return Err(e);
                }
                return Ok(());
            }

            if m.foreground {
                let (send, recv) = std::sync::mpsc::channel();
                let send_ctrlc = send.clone();

                ctrlc::set_handler(move || {
                    println!("puzzlefs unmounted");
                    send_ctrlc.send(()).unwrap();
                })
                .unwrap();

                let fuse_thread_finished = send;
                let named_pipe = m.init_pipe.map(PathBuf::from);
                let result = spawn_mount(
                    image,
                    tag,
                    &mountpoint,
                    &m.options.unwrap_or_default(),
                    named_pipe.clone().map(PipeDescriptor::NamedPipe),
                    Some(fuse_thread_finished),
                    manifest_verity.as_deref(),
                );
                if let Err(e) = result {
                    if let Some(pipe) = named_pipe {
                        let file = OpenOptions::new().write(true).open(&pipe);
                        match file {
                            Ok(mut file) => {
                                if let Err(e) = file.write_all(b"f") {
                                    error!("cannot write to pipe {}, {e}", pipe.display());
                                }
                            }
                            Err(e) => {
                                error!("cannot open pipe {}, {e}", pipe.display());
                            }
                        }
                    }
                    return Err(e.into());
                }

                // This blocks until either ctrl-c is pressed or the filesystem is unmounted
                let () = recv.recv().unwrap();
            } else {
                let (recv, mut init_notify) = os_pipe::pipe()?;

                if let Err(e) = mount_background(
                    image,
                    tag,
                    &mountpoint,
                    m.options,
                    manifest_verity,
                    recv,
                    &init_notify,
                    || Ok(()),
                ) {
                    if let Err(e) = init_notify.write_all(b"f") {
                        error!("puzzlefs will hang because we couldn't write to pipe, {e}");
                    }
                    error!("mount_background failed: {e}");
                    return Err(e);
                }
            }

            Ok(())
        }
        SubCommand::Umount(e) => {
            let mountpoint = Path::new(&e.mountpoint);
            let mount_type = get_mount_type(&e.mountpoint)?;
            match mount_type.to_str() {
                Some("overlay") => {
                    if !Uid::effective().is_root() {
                        anyhow::bail!("Overlay mounts can only be unmounted by the root user!")
                    }
                    umount(mountpoint)?;
                    // Now unmount the read-only puzzlefs mountpoint
                    let pfs_mountpoint = mountpoint.join("ro");
                    umount(pfs_mountpoint.as_os_str())?;
                    // TODO: Decide whether to remove the directories we've created. For the LXC
                    // case, we don't want to remove them because we want to persist state between
                    // multiple mounts. Should we add a --delete flag to unmount?
                    // let ovl_workdir = mountpoint.join("work");
                    // let ovl_upperdir = mountpoint.join("upper");
                    // std::fs::remove_dir_all(&pfs_mountpoint)?;
                    // std::fs::remove_dir_all(&ovl_workdir)?;
                    // std::fs::remove_dir_all(&ovl_upperdir)?;
                    return Ok(());
                }
                Some("fuse") => {
                    // We call "fusermount -u" because we don't have permissions to umount directly
                    // fusermount and umount binaries have the setuid bit set
                    let status = std::process::Command::new("fusermount")
                        .arg("-u")
                        .arg(&e.mountpoint)
                        .status()?;
                    if !status.success() {
                        anyhow::bail!(
                            "umount exited with status {}",
                            status
                                .code()
                                .map(|code| code.to_string())
                                .unwrap_or("terminated by signal".to_string())
                        );
                    }
                }
                _ => anyhow::bail!(
                    "Unknown mountpoint type {} for {}",
                    mount_type.to_str().unwrap_or("unknown mount type"),
                    &e.mountpoint
                ),
            }

            Ok(())
        }
        SubCommand::Extract(e) => {
            let (oci_dir, tag) = parse_oci_dir(&e.oci_dir)?;
            init_logging("info");
            extract_rootfs(oci_dir, tag, &e.extract_dir)
        }
        SubCommand::EnableFsVerity(v) => {
            let (oci_dir, tag) = parse_oci_dir(&v.oci_dir)?;
            let oci_dir = Path::new(oci_dir);
            let oci_dir = fs::canonicalize(oci_dir)?;
            let image = Image::open(&oci_dir)?;
            enable_fs_verity(image, tag, &v.root_hash)?;
            Ok(())
        }
    }
}
