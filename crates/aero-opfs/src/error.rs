/// Canonical disk error type used by `aero-opfs`.
///
/// This is a re-export of [`aero_storage::DiskError`], so downstream crates only need to
/// handle one disk error enum across native and wasm/browser backends.
pub use aero_storage::DiskError;

/// Convenience alias for `aero_storage::Result`.
///
/// This exists for backwards compatibility with older `aero-opfs` APIs that had a separate
/// `DiskError` type.
pub type DiskResult<T> = aero_storage::Result<T>;
