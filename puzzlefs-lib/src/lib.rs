#![feature(error_generic_member_access)]
#![feature(seek_stream_len)]
#[macro_use]
extern crate anyhow;

pub mod builder;
mod common;
pub mod compression;
pub mod extractor;
mod format;
pub mod fsverity_helpers;
pub mod oci;
pub mod reader;

#[allow(clippy::needless_lifetimes)]
pub mod metadata_capnp {
    include!(concat!(env!("OUT_DIR"), "/metadata_capnp.rs"));
}
