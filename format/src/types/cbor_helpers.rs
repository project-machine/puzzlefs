use crate::{Result, WireFormatError};
use std::backtrace::Backtrace;
use std::io::Read;

pub const fn cbor_size_of_list_header(size: usize) -> usize {
    match size {
        0..=23 => 1,
        24..=255 => 2,
        256..=65535 => 3,
        65536..=4294967295 => 4,
        _ => 8,
    }
}

fn parse_u8(mut reader: impl Read) -> Result<u8> {
    let mut buf = [0; 1];
    reader.read_exact(&mut buf)?;
    Ok(u8::from_be_bytes(buf))
}

fn parse_u16(mut reader: impl Read) -> Result<u16> {
    let mut buf = [0; 2];
    reader.read_exact(&mut buf)?;
    Ok(u16::from_be_bytes(buf))
}

fn parse_u32(mut reader: impl Read) -> Result<u32> {
    let mut buf = [0; 4];
    reader.read_exact(&mut buf)?;
    Ok(u32::from_be_bytes(buf))
}

fn parse_u64(mut reader: impl Read) -> Result<u64> {
    let mut buf = [0; 8];
    reader.read_exact(&mut buf)?;
    Ok(u64::from_be_bytes(buf))
}

pub fn cbor_get_array_size<R: Read>(mut reader: R) -> Result<u64> {
    let mut buf = [0; 1];
    reader.read_exact(&mut buf)?;

    match buf[0] {
        0x80..=0x97 => Ok((buf[0] - 0x80) as u64),
        0x98 => parse_u8(reader).map(u64::from),
        0x99 => parse_u16(reader).map(u64::from),
        0x9a => parse_u32(reader).map(u64::from),
        0x9b => parse_u64(reader).map(u64::from),
        _ => Err(WireFormatError::ValueMissing(Backtrace::capture())),
    }
}
