use core::fmt;
use thiserror::Error;

/// Errors used by the *async streaming* storage backends (remote HTTP range fetch,
/// integrity checks, metadata persistence, etc.).
#[derive(Debug, Clone, Error)]
pub enum StorageError {
    #[error("remote server does not support HTTP Range requests")]
    RangeNotSupported,

    #[error("remote request failed: {0}")]
    Http(String),

    #[error("unexpected remote response: {0}")]
    Protocol(String),

    #[error("I/O error: {0}")]
    Io(String),

    #[error("integrity check failed for chunk {chunk_index}: expected {expected} got {actual}")]
    Integrity {
        chunk_index: u64,
        expected: String,
        actual: String,
    },

    #[error("operation cancelled")]
    Cancelled,

    #[error("out of bounds access: offset {offset} len {len} size {size}")]
    OutOfBounds { offset: u64, len: u64, size: u64 },
}

/// Result type used by the synchronous sector-addressable disk layer.
pub type DiskResult<T> = Result<T, DiskError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiskError {
    /// Legacy convenience variant used by some controllers/tests.
    ///
    /// Prefer `OutOfRange { .. }` where possible.
    OutOfBounds,
    /// Legacy convenience variant used by some controllers/tests.
    ///
    /// Prefer `UnalignedBuffer { .. }` where possible.
    InvalidBufferLength,
    /// Backend is unavailable in the current build/runtime (e.g. OPFS not present).
    NotSupported(String),
    /// Storage quota is exhausted.
    QuotaExceeded,
    /// Backend is currently locked by another context (e.g. another worker holding an OPFS handle).
    InUse,
    /// Backend is in an invalid state (closed, suspended, etc).
    InvalidState(String),
    /// A request referenced sectors beyond `capacity_sectors()`.
    OutOfRange {
        lba: u64,
        sectors: u64,
        capacity_sectors: u64,
    },
    /// A buffer length was not a multiple of the backend sector size.
    UnalignedBuffer { len: usize, sector_size: u32 },
    /// The backend is temporarily unavailable (e.g. disconnected remote / locked file handle).
    BackendUnavailable,
    /// Underlying I/O error from the storage implementation.
    Io(String),
    /// The on-disk image is corrupt or not a supported version.
    CorruptImage(&'static str),
    /// The operation is not supported by this backend.
    Unsupported(&'static str),
}

impl fmt::Display for DiskError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DiskError::OutOfBounds => write!(f, "out of bounds"),
            DiskError::InvalidBufferLength => write!(f, "invalid buffer length"),
            DiskError::NotSupported(msg) => write!(f, "not supported: {msg}"),
            DiskError::QuotaExceeded => write!(f, "quota exceeded"),
            DiskError::InUse => write!(f, "backend is in use"),
            DiskError::InvalidState(msg) => write!(f, "invalid state: {msg}"),
            DiskError::OutOfRange {
                lba,
                sectors,
                capacity_sectors,
            } => write!(
                f,
                "out of range: lba={lba} sectors={sectors} capacity_sectors={capacity_sectors}"
            ),
            DiskError::UnalignedBuffer { len, sector_size } => write!(
                f,
                "unaligned buffer: len={len} is not a multiple of sector_size={sector_size}"
            ),
            DiskError::BackendUnavailable => write!(f, "backend unavailable"),
            DiskError::Io(msg) => write!(f, "io error: {msg}"),
            DiskError::CorruptImage(msg) => write!(f, "corrupt image: {msg}"),
            DiskError::Unsupported(msg) => write!(f, "unsupported: {msg}"),
        }
    }
}

impl std::error::Error for DiskError {}
