use nix::unistd::close;
use std::fs;
use std::io::prelude::*;
use std::path::Path;

use clap::{Args, Parser, Subcommand};
use std::os::unix::io::AsRawFd;
use std::thread;

use builder::build_initial_rootfs;
use daemonize::Daemonize;
use env_logger::Env;
use extractor::extract_rootfs;
use log::{info, LevelFilter};
use oci::Image;
use reader::spawn_mount;
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

fn init_syslog() -> std::io::Result<()> {
    let formatter = Formatter3164 {
        facility: Facility::LOG_USER,
        hostname: None,
        process: "puzzlefs".into(),
        pid: 0,
    };

    let logger = match syslog::unix(formatter) {
        Err(e) => {
            println!("impossible to connect to syslog: {:?}", e);
            return Err(std::io::Error::last_os_error());
        }
        Ok(logger) => logger,
    };
    log::set_boxed_logger(Box::new(BasicLogger::new(logger)))
        .map(|()| log::set_max_level(LevelFilter::Info))
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
            let desc = build_initial_rootfs(rootfs, &image)?;
            image.add_tag(b.tag, desc).map_err(|e| e.into())
        }
        SubCommand::Mount(m) => {
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
                init_syslog()?;
                let (mut reader, writer) = os_pipe::pipe()?;
                let daemonize = Daemonize::new()
                    .stdout(writer.as_raw_fd())
                    .stderr(writer.as_raw_fd());

                match daemonize.start() {
                    Ok(_) => {
                        // writer needs to be dropped so it doesn't keep the pipe open
                        drop(writer);
                        // this thread receives messages from stdout/stderr and sends them to syslog
                        let thread_handle = thread::spawn(move || {
                            let mut output = [0; 4096];
                            let mut syslog_line = String::new();
                            loop {
                                let nr_bytes_read = reader.read(&mut output);
                                let nr_bytes_read = match nr_bytes_read {
                                    Ok(r) => r,
                                    Err(e) => {
                                        info!("Error {e}");
                                        break;
                                    }
                                };
                                if nr_bytes_read > 0 {
                                    if let Ok(str) = std::str::from_utf8(&output[0..nr_bytes_read])
                                    {
                                        // line buffering, send messages to syslog line by line
                                        for c in str.chars() {
                                            if c == '\n' {
                                                info!("{syslog_line}");
                                                syslog_line.clear();
                                            } else {
                                                syslog_line.push(c);
                                            }
                                        }
                                    }
                                } else {
                                    break;
                                }
                            }
                        });

                        let (fuse_thread_finished, recv) = std::sync::mpsc::channel();
                        let guard =
                            spawn_mount(image, &m.tag, &mountpoint, Some(fuse_thread_finished));
                        match guard {
                            Ok(_res) => {
                                // This blocks until the filesystem is unmounted
                                let () = recv.recv().unwrap();
                            }
                            Err(e) => {
                                info!("cannot mount filesystem: {e}");
                            }
                        };

                        // Closing the file descriptors will cause the syslog processing thread to finish
                        close(std::io::stdout().as_raw_fd()).expect("cannot close stdout");
                        close(std::io::stderr().as_raw_fd()).expect("cannot close stderr");
                        thread_handle.join().expect("Cannot join thread");
                    }
                    Err(e) => eprintln!("Error, {}", e),
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
