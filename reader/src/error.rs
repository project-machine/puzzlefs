use std::io;
use std::os::raw::c_int;

use nix::errno::Errno;
use thiserror::Error;

use format::WireFormatError;

#[derive(Error, Debug)]
pub enum FSError {
    #[error("fs error")]
    IO(#[from] io::Error),

    #[error("error unpacking metadata")]
    WireFormat(#[from] WireFormatError),

    // TODO: let's get rid of Box<dyn std::error::Error>, it was a lazy way to propagate errors,
    // but erasing types is painful here, since we have to render it as EINVAL everywhere, which
    // might look weird to FUSE users.
    #[error("generic error")]
    Generic(#[from] Box<dyn std::error::Error>),
}

impl FSError {
    pub fn to_errno(&self) -> c_int {
        match self {
            FSError::IO(ioe) => ioe.raw_os_error().unwrap_or(Errno::EINVAL as i32) as c_int,
            FSError::WireFormat(wfe) => wfe.to_errno(),
            FSError::Generic(_) => Errno::EINVAL as c_int,
        }
    }

    pub fn from_errno(errno: Errno) -> FSError {
        FSError::IO(io::Error::from_raw_os_error(errno as i32))
    }
}

pub type FSResult<T> = std::result::Result<T, FSError>;
