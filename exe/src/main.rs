extern crate clap;

use std::fs;
use std::io;
use std::path::Path;

use clap::Clap;
use signal_hook::consts::TERM_SIGNALS;
use signal_hook::iterator::exfiltrator::SignalOnly;
use signal_hook::iterator::SignalsInfo;

use builder::build_initial_rootfs;
use format::Result;
use oci::Image;
use reader::{mount, InodeMode, PuzzleFS, WalkPuzzleFS};

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

fn main() -> Result<()> {
    let opts: Opts = Opts::parse();
    match opts.subcmd {
        SubCommand::Build(b) => {
            let rootfs = Path::new(&b.rootfs);
            let oci_dir = Path::new(&b.oci_dir);
            let image = Image::new(oci_dir)?;
            let desc = build_initial_rootfs(rootfs, &image)?;
            image.add_tag(b.tag, desc)
        }
        SubCommand::Mount(m) => {
            // TODO: add --background option?
            let oci_dir = Path::new(&m.oci_dir);
            let image = Image::new(oci_dir)?;
            let mountpoint = Path::new(&m.mountpoint);
            let _bg = mount(&image, &m.tag, mountpoint)?;
            let mut signals = SignalsInfo::<SignalOnly>::new(TERM_SIGNALS);
            for s in &mut signals {
                eprintln!("got signal {:?}, exiting puzzlefs fuse mount", s);
            }
            // we can return, which will ->drop() _bg and kill the thread.
            Ok(())
        }
        SubCommand::Extract(e) => {
            let oci_dir = Path::new(&e.oci_dir);
            let image = Image::new(oci_dir)?;
            let dir = Path::new(&e.extract_dir);
            fs::create_dir(dir)?;
            let mut pfs = PuzzleFS::open(&image, &e.tag)?;
            let mut walker = WalkPuzzleFS::walk(&mut pfs)?;
            walker.try_for_each(|de| -> Result<()> {
                let dir_entry = de?;
                let path = dir.join(&dir_entry.path);
                match dir_entry.inode.mode {
                    InodeMode::File { .. } => {
                        let mut reader = dir_entry.open()?;
                        let mut f = fs::File::create(path)?;
                        io::copy(&mut reader, &mut f)?;
                    }
                    InodeMode::Dir { .. } => fs::create_dir(path)?,
                    InodeMode::Other => todo!(),
                };
                Ok(())
            })?;
            Ok(())
        }
    }
}
