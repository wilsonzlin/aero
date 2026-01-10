use thiserror::Error;

pub type Result<T> = std::result::Result<T, DiskError>;

#[derive(Debug, Error)]
pub enum DiskError {
    #[error("unaligned buffer length {len} (expected multiple of {alignment})")]
    UnalignedLength { len: usize, alignment: usize },

    #[error("out of bounds: offset={offset} len={len} capacity={capacity}")]
    OutOfBounds {
        offset: u64,
        len: usize,
        capacity: u64,
    },

    #[error("integer overflow while computing byte offsets")]
    OffsetOverflow,

    #[error("invalid sparse header: {0}")]
    InvalidSparseHeader(&'static str),

    #[error("invalid configuration: {0}")]
    InvalidConfig(&'static str),

    #[error("corrupt sparse image: {0}")]
    CorruptSparseImage(&'static str),

    #[error("io error: {0}")]
    Io(&'static str),
}
