extern crate clap;

use std::path::Path;

use clap::Clap;

use builder::build_initial_rootfs;
use oci::Image;

#[derive(Clap)]
#[clap(version = "0.1.0", author = "Tycho Andersen <tycho@tycho.pizza>")]
struct Opts {
    #[clap(subcommand)]
    subcmd: SubCommand,
}

#[derive(Clap)]
enum SubCommand {
    Build(Build),
}

#[derive(Clap)]
struct Build {
    rootfs: String,
    oci_dir: String,
    tag: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let opts: Opts = Opts::parse();
    match opts.subcmd {
        SubCommand::Build(b) => {
            let rootfs = Path::new(&b.rootfs);
            let oci_dir = Path::new(&b.oci_dir);
            let image = Image::new(oci_dir)?;
            let desc = build_initial_rootfs(rootfs, &image)?;
            image.add_tag(b.tag, desc)
        }
    }
}
