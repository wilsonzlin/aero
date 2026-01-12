use std::io;

use thiserror::Error;

pub type Result<T> = std::result::Result<T, SnapshotError>;

#[derive(Debug, Error)]
pub enum SnapshotError {
    #[error("io error: {0}")]
    Io(#[from] io::Error),

    #[error("out of memory allocating {len} bytes")]
    OutOfMemory { len: usize },

    #[error("invalid snapshot magic")]
    InvalidMagic,

    #[error("unsupported snapshot version {0}")]
    UnsupportedVersion(u16),

    #[error("invalid endianness tag {0}")]
    InvalidEndianness(u8),

    #[error("corrupt snapshot: {0}")]
    Corrupt(&'static str),

    #[error(
        "dirty page size mismatch (SaveOptions.ram.page_size={options} bytes, SnapshotSource::dirty_page_size()={dirty_page_size} bytes)"
    )]
    DirtyPageSizeMismatch { options: u32, dirty_page_size: u32 },

    #[error("guest RAM size mismatch (expected {expected} bytes, found {found} bytes)")]
    RamLenMismatch { expected: u64, found: u64 },

    #[error("lz4 decompression failed: {0}")]
    Lz4Decompress(#[from] lz4_flex::block::DecompressError),

    #[error("utf-8 decoding failed: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),
}
