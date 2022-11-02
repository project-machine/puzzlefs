use std::fs::File;
use std::path::Path;

use clap::Clap;

use builder::build_initial_rootfs;
use daemonize::Daemonize;
use extractor::extract_rootfs;
use oci::Image;
use reader::mount;

#[derive(Clap)]
#[clap(version = "0.1.0", author = "Tycho Andersen <tycho@tycho.pizza>")]
struct Opts {
    #[clap(subcommand)]
    subcmd: SubCommand,
}

#[derive(Clap)]
enum SubCommand {
    Build(Build),
    Mount(Mount),
    Extract(Extract),
}

#[derive(Clap)]
struct Build {
    rootfs: String,
    oci_dir: String,
    tag: String,
}

#[derive(Clap)]
struct Mount {
    oci_dir: String,
    tag: String,
    mountpoint: String,
}

#[derive(Clap)]
struct Extract {
    oci_dir: String,
    tag: String,
    extract_dir: String,
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
            // TODO: add --foreground option?
            let oci_dir = Path::new(&m.oci_dir);
            let image = Image::new(oci_dir)?;
            let mountpoint = Path::new(&m.mountpoint);
            let stdout = File::create("/tmp/puzzlefs.out")?;
            let stderr = File::create("/tmp/puzzlefs.err")?;
            let daemonize = Daemonize::new().stdout(stdout).stderr(stderr);

            match daemonize.start() {
                Ok(_) => mount(&image, &m.tag, mountpoint)?,
                Err(e) => eprintln!("Error, {}", e),
            }

            Ok(())
        }
        SubCommand::Extract(e) => extract_rootfs(&e.oci_dir, &e.tag, &e.extract_dir),
    }
}
