use thiserror::Error;

pub type Result<T> = std::result::Result<T, DiskError>;

/// Unified error type for Aero disk/storage operations.
///
/// This enum is used by both native helpers (e.g. host-side streaming) and
/// browser/wasm32 backends (e.g. OPFS in `crates/aero-opfs`). As a result it
/// includes variants that map cleanly from browser storage failures such as quota
/// exhaustion or a backend being locked by another context.
///
/// Note: [`DiskError::Io`] intentionally stores a human-readable `String` rather
/// than `std::io::Error` so wasm32 implementations can surface errors originating
/// from JavaScript/DOM APIs without requiring a platform-specific error type.
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

    #[error("corrupt disk image: {0}")]
    CorruptImage(&'static str),

    #[error("unsupported disk image feature: {0}")]
    Unsupported(&'static str),

    #[error("invalid sparse header: {0}")]
    InvalidSparseHeader(&'static str),

    #[error("invalid configuration: {0}")]
    InvalidConfig(&'static str),

    #[error("corrupt sparse image: {0}")]
    CorruptSparseImage(&'static str),

    #[error("backend not supported: {0}")]
    NotSupported(String),

    #[error("storage quota exceeded")]
    QuotaExceeded,

    #[error("backend is in use")]
    InUse,

    #[error("invalid backend state: {0}")]
    InvalidState(String),

    #[error("backend unavailable")]
    BackendUnavailable,

    /// Generic I/O failure.
    ///
    /// This is a catch-all for errors that do not map to a more structured variant.
    #[error("io error: {0}")]
    Io(String),
}
