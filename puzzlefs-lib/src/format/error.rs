use std::backtrace::Backtrace;
use std::io;
use std::os::raw::c_int;

use nix::errno::Errno;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum WireFormatError {
    #[error("cannot turn local ref into a digest")]
    LocalRefError(Backtrace),
    #[error("cannot seek to other blob")]
    SeekOtherError(Backtrace),
    #[error("invalid serialized data")]
    InvalidSerializedData(Backtrace),
    #[error("invalid image schema: {0}")]
    InvalidImageSchema(i32, Backtrace),
    #[error("invalid image version: {0}")]
    InvalidImageVersion(String, Backtrace),
    #[error("invalid fs_verity data: {0}")]
    InvalidFsVerityData(String, Backtrace),
    #[error("missing manifest: {0}")]
    MissingManifest(String, Backtrace),
    #[error("missing PuzzleFS rootfs")]
    MissingRootfs(Backtrace),
    #[error("fs error: {0}")]
    IOError(#[from] io::Error, Backtrace),
    #[error("deserialization error (capnp): {0}")]
    CapnpError(#[from] capnp::Error, Backtrace),
    #[error("numeric conversion error: {0}")]
    FromIntError(#[from] std::num::TryFromIntError, Backtrace),
    #[error("deserialization error (json): {0}")]
    JSONError(#[from] serde_json::Error, Backtrace),
    #[error("TryFromSlice error: {0}")]
    FromSliceError(#[from] std::array::TryFromSliceError, Backtrace),
    #[error("hex error: {0}")]
    HexError(#[from] hex::FromHexError, Backtrace),
    #[error("Oci error: {0}")]
    OciError(#[from] ocidir::oci_spec::OciSpecError, Backtrace),
    #[error("Oci dir error: {0}")]
    OciDirError(#[from] ocidir::Error, Backtrace),
}

impl WireFormatError {
    pub fn to_errno(&self) -> c_int {
        match self {
            WireFormatError::LocalRefError(..) => Errno::EINVAL as c_int,
            WireFormatError::SeekOtherError(..) => Errno::ESPIPE as c_int,
            WireFormatError::InvalidSerializedData(..) => Errno::EINVAL as c_int,
            WireFormatError::InvalidImageSchema(..) => Errno::EINVAL as c_int,
            WireFormatError::InvalidImageVersion(..) => Errno::EINVAL as c_int,
            WireFormatError::InvalidFsVerityData(..) => Errno::EINVAL as c_int,
            WireFormatError::MissingManifest(..) => Errno::EINVAL as c_int,
            WireFormatError::MissingRootfs(..) => Errno::EINVAL as c_int,
            WireFormatError::IOError(ioe, ..) => {
                ioe.raw_os_error().unwrap_or(Errno::EINVAL as i32) as c_int
            }
            WireFormatError::CapnpError(..) => Errno::EINVAL as c_int,
            WireFormatError::JSONError(..) => Errno::EINVAL as c_int,
            WireFormatError::HexError(..) => Errno::EINVAL as c_int,
            WireFormatError::FromIntError(..) => Errno::EINVAL as c_int,
            WireFormatError::FromSliceError(..) => Errno::EINVAL as c_int,
            WireFormatError::OciError(..) => Errno::EINVAL as c_int,
            WireFormatError::OciDirError(..) => Errno::EINVAL as c_int,
        }
    }

    pub fn from_errno(errno: Errno) -> Self {
        Self::IOError(
            io::Error::from_raw_os_error(errno as i32),
            Backtrace::capture(),
        )
    }
}

pub type Result<T> = std::result::Result<T, WireFormatError>;
