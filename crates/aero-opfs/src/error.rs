pub use aero_storage::DiskError;

/// Convenience alias for `aero_storage::Result`.
///
/// This exists for backwards compatibility with older `aero-opfs` APIs that had a separate
/// `DiskError` type.
pub type DiskResult<T> = aero_storage::Result<T>;
