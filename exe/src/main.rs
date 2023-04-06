use builder::{add_rootfs_delta, build_initial_rootfs, enable_fs_verity};
use clap::{Args, Parser, Subcommand};
use compression::{Noop, Zstd};
use daemonize::Daemonize;
use env_logger::Env;
use extractor::extract_rootfs;
use fsverity_helpers::get_fs_verity_digest;
use log::{error, info, LevelFilter};
use oci::Image;
use os_pipe::{PipeReader, PipeWriter};
use reader::fuse::PipeDescriptor;
use reader::{mount, spawn_mount};
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
    Extract(Extract),
    EnableFsVerity(FsVerity),
}

#[derive(Args)]
struct Build {
    rootfs: String,
    oci_dir: String,
    tag: String,
    #[arg(short, long, value_name = "base-layer")]
    base_layer: Option<String>,
    #[arg(short, long, value_name = "compressed")]
    compression: bool,
}

#[derive(Args)]
struct Mount {
    oci_dir: String,
    tag: String,
    mountpoint: String,
    #[arg(short, long)]
    foreground: bool,
    #[arg(short, long, value_name = "init-pipe")]
    init_pipe: Option<String>,
    #[arg(short, value_delimiter = ',')]
    options: Option<Vec<String>>,
    #[arg(short, long, value_name = "fs verity root digest")]
    digest: Option<String>,
}

#[derive(Args)]
struct Extract {
    oci_dir: String,
    tag: String,
    extract_dir: String,
}

#[derive(Args)]
struct FsVerity {
    oci_dir: String,
    tag: String,
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

fn mount_background(
    image: Image,
    tag: &str,
    mountpoint: &Path,
    options: Option<Vec<String>>,
    manifest_verity: Option<Vec<u8>>,
    mut recv: PipeReader,
    init_notify: &mut PipeWriter,
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

fn main() -> anyhow::Result<()> {
    let opts: Opts = Opts::parse();
    match opts.subcmd {
        SubCommand::Build(b) => {
            let rootfs = Path::new(&b.rootfs);
            let oci_dir = Path::new(&b.oci_dir);
            let image = Image::new(oci_dir)?;
            let new_image = match b.base_layer {
                Some(base_layer) => {
                    let (desc, image) = if b.compression {
                        add_rootfs_delta::<Zstd>(rootfs, image, &base_layer)?
                    } else {
                        add_rootfs_delta::<Noop>(rootfs, image, &base_layer)?
                    };
                    image.add_tag(&b.tag, desc)?;
                    image
                }
                None => {
                    let desc = if b.compression {
                        build_initial_rootfs::<Zstd>(rootfs, &image)?
                    } else {
                        build_initial_rootfs::<Noop>(rootfs, &image)?
                    };
                    image.add_tag(&b.tag, desc)?;
                    Arc::new(image)
                }
            };
            let mut manifest_fd = new_image.get_image_manifest_fd(&b.tag)?;
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

            let oci_dir = Path::new(&m.oci_dir);
            let oci_dir = fs::canonicalize(oci_dir)?;
            let image = Image::open(&oci_dir)?;
            let mountpoint = Path::new(&m.mountpoint);
            let mountpoint = fs::canonicalize(mountpoint)?;

            let manifest_verity = m.digest.map(hex::decode).transpose()?;

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
                    &m.tag,
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
                    &m.tag,
                    &mountpoint,
                    m.options,
                    manifest_verity,
                    recv,
                    &mut init_notify,
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
        SubCommand::Extract(e) => {
            init_logging("info");
            extract_rootfs(&e.oci_dir, &e.tag, &e.extract_dir)
        }
        SubCommand::EnableFsVerity(v) => {
            let oci_dir = Path::new(&v.oci_dir);
            let oci_dir = fs::canonicalize(oci_dir)?;
            let image = Image::open(&oci_dir)?;
            enable_fs_verity(image, &v.tag, &v.root_hash)?;
            Ok(())
        }
    }
}
