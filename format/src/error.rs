extern crate serde_cbor;
extern crate serde_json;

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
    #[error("no value present")]
    ValueMissing(Backtrace),
    #[error("invalid image schema: {0}")]
    InvalidImageSchema(i32, Backtrace),
    #[error("invalid image version: {0}")]
    InvalidImageVersion(String, Backtrace),
    #[error("fs error: {0}")]
    IOError(#[from] io::Error, Backtrace),
    #[error("deserialization error (cbor): {0}")]
    CBORError(#[from] serde_cbor::Error, Backtrace),
    #[error("deserialization error (json): {0}")]
    JSONError(#[from] serde_json::Error, Backtrace),
}

impl WireFormatError {
    pub fn to_errno(&self) -> c_int {
        match self {
            WireFormatError::LocalRefError(..) => Errno::EINVAL as c_int,
            WireFormatError::SeekOtherError(..) => Errno::ESPIPE as c_int,
            WireFormatError::ValueMissing(..) => Errno::ENOENT as c_int,
            WireFormatError::InvalidImageSchema(..) => Errno::EINVAL as c_int,
            WireFormatError::InvalidImageVersion(..) => Errno::EINVAL as c_int,
            WireFormatError::IOError(ioe, ..) => {
                ioe.raw_os_error().unwrap_or(Errno::EINVAL as i32) as c_int
            }
            WireFormatError::CBORError(..) => Errno::EINVAL as c_int,
            WireFormatError::JSONError(..) => Errno::EINVAL as c_int,
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
