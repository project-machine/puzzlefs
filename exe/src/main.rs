use builder::{add_rootfs_delta, build_initial_rootfs};
use clap::{Args, Parser, Subcommand};
use daemonize::Daemonize;
use env_logger::Env;
use extractor::extract_rootfs;
use log::LevelFilter;
use oci::Image;
use reader::{mount, spawn_mount};
use std::fs;
use std::path::Path;
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
}

#[derive(Args)]
struct Build {
    rootfs: String,
    oci_dir: String,
    tag: String,
    #[arg(short, long, value_name = "base-layer")]
    base_layer: Option<String>,
}

#[derive(Args)]
struct Mount {
    oci_dir: String,
    tag: String,
    mountpoint: String,
    #[arg(short, long)]
    foreground: bool,
}

#[derive(Args)]
struct Extract {
    oci_dir: String,
    tag: String,
    extract_dir: String,
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

fn main() -> anyhow::Result<()> {
    let opts: Opts = Opts::parse();
    match opts.subcmd {
        SubCommand::Build(b) => {
            let rootfs = Path::new(&b.rootfs);
            let oci_dir = Path::new(&b.oci_dir);
            let image = Image::new(oci_dir)?;
            let desc = match b.base_layer {
                Some(base_layer) => add_rootfs_delta(rootfs, image.clone(), &base_layer)?,
                None => build_initial_rootfs(rootfs, &image)?,
            };
            image.add_tag(b.tag, desc).map_err(|e| e.into())
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
            let image = Image::new(&oci_dir)?;
            let mountpoint = Path::new(&m.mountpoint);
            let mountpoint = fs::canonicalize(mountpoint)?;

            if m.foreground {
                let (send, recv) = std::sync::mpsc::channel();
                let send_ctrlc = send.clone();

                ctrlc::set_handler(move || {
                    println!("puzzlefs unmounted");
                    send_ctrlc.send(()).unwrap();
                })
                .unwrap();

                let fuse_thread_finished = send;
                let _guard = spawn_mount(image, &m.tag, &mountpoint, Some(fuse_thread_finished))?;
                // This blocks until either ctrl-c is pressed or the filesystem is unmounted
                let () = recv.recv().unwrap();
            } else {
                let daemonize = Daemonize::new();

                match daemonize.start() {
                    Ok(_) => {
                        mount(image, &m.tag, &mountpoint)?;
                    }
                    Err(e) => eprintln!("Error, {e}"),
                }
            }

            Ok(())
        }
        SubCommand::Extract(e) => {
            init_logging("info");
            extract_rootfs(&e.oci_dir, &e.tag, &e.extract_dir)
        }
    }
}
