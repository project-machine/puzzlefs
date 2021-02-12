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
}

impl FSError {
    pub fn to_errno(&self) -> c_int {
        match self {
            FSError::IO(ioe) => ioe.raw_os_error().unwrap_or(Errno::EINVAL as i32) as c_int,
            FSError::WireFormat(wfe) => wfe.to_errno(),
        }
    }

    pub fn from_errno(errno: Errno) -> FSError {
        FSError::IO(io::Error::from_raw_os_error(errno as i32))
    }
}

pub type FSResult<T> = std::result::Result<T, FSError>;
