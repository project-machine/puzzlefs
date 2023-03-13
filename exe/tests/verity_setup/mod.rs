use anyhow::bail;
use std::process::Command;
use std::str;

#[derive(Debug)]
pub struct VeritySetup {
    pub mountpoint: String,
    lo_device: String,
    backing_file: String,
}

impl VeritySetup {
    pub fn new() -> anyhow::Result<Self> {
        let output = Command::new("tests/setup_verity_device.sh").output()?;

        if !output.status.success() {
            bail!("tests/setup_fs_verity_device.sh failed!");
        }

        let output =
            str::from_utf8(&output.stdout).expect("Script output should not contain non-UTF8");
        let tokens;

        println!("output: {output}");

        for line in output.lines() {
            if line.starts_with("mounted ") {
                tokens = line.split_whitespace().collect::<Vec<_>>();
                // the script outputs something like:
                // mounted /dev/loop1 backed by /tmp/tmp.ACBmpxbuul at /tmp/tmp.pU1MTG0K70
                let setup = VeritySetup {
                    lo_device: String::from(tokens[1]),
                    backing_file: String::from(tokens[4]),
                    mountpoint: String::from(tokens[6]),
                };
                return Ok(setup);
            }
        }
        bail!("Didn't find backing_file, lo_device and mountpoint in script output")
    }
}

impl Drop for VeritySetup {
    fn drop(&mut self) {
        let status = Command::new("tests/cleanup_verity_device.sh")
            .arg(&self.mountpoint)
            .arg(&self.lo_device)
            .arg(&self.backing_file)
            .status();

        if let Err(e) = status {
            println!("Could not cleanup the verity setup {e}");
        }
    }
}
