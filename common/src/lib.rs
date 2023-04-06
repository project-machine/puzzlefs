// Quoting from https://github.com/ronomon/deduplication
// An average chunk size of 64 KB is recommended for optimal end-to-end deduplication and compression efficiency
pub const MIN_CHUNK_SIZE: u32 = 16 * 1024;
pub const AVG_CHUNK_SIZE: u32 = 64 * 1024;
pub const MAX_CHUNK_SIZE: u32 = 256 * 1024;
