use crate::format::{Result, WireFormatError, SHA256_BLOCK_SIZE};
use std::backtrace::Backtrace;
use std::io::Write;
use std::os::unix::io::AsRawFd;

pub use fs_verity::linux::fsverity_enable;
use fs_verity::linux::fsverity_measure;
use fs_verity::FsVeritySha256;
pub use fs_verity::InnerHashAlgorithm;
use sha2::Digest;

pub const FS_VERITY_BLOCK_SIZE_DEFAULT: usize = 4096;

pub fn get_fs_verity_digest(data: &[u8]) -> Result<[u8; SHA256_BLOCK_SIZE]> {
    let mut digest = FsVeritySha256::new();
    digest.write_all(data)?;
    let result = digest.finalize();
    Ok(result.into())
}

pub fn check_fs_verity(file: &cap_std::fs::File, expected: &[u8]) -> Result<()> {
    if expected.len() != SHA256_BLOCK_SIZE {
        return Err(WireFormatError::InvalidFsVerityData(
            format!(
                "fsverity invalid SHA256 hash length {}",
                hex::encode(expected),
            ),
            Backtrace::capture(),
        ));
    }
    let (_, measurement) = fsverity_measure(file.as_raw_fd())?;

    if *expected != measurement[..] {
        return Err(WireFormatError::InvalidFsVerityData(
            format!(
                "fsverity mismatch {}, expected {}",
                hex::encode(expected),
                hex::encode(measurement)
            ),
            Backtrace::capture(),
        ));
    }

    Ok(())
}
