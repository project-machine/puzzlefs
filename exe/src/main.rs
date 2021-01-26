extern crate clap;

use std::path::Path;

use clap::Clap;

use builder::build_initial_rootfs;
use format::Rootfs;
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
}

fn main() -> Result<(), std::io::Error> {
    let opts: Opts = Opts::parse();
    match opts.subcmd {
        SubCommand::Build(b) => {
            let rootfs = Path::new(&b.rootfs);
            let oci_dir = Path::new(&b.oci_dir);
            let image = Image::new(oci_dir)?;
            let _ = build_initial_rootfs(rootfs, &image).map_err::<Rootfs, _>(|e| {
                println!("build rootfs failed: {}", e);
                std::process::exit(1);
            });
            Ok(())
        }
    }
}
