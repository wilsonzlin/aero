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

#[cfg(test)]
mod tests {
    use super::{DiskError, DiskResult};

    /// Guardrail to ensure `aero-opfs` continues to expose the *canonical* disk error type from
    /// `aero-storage`, rather than drifting back to a separate enum.
    #[test]
    fn disk_error_is_reexported_from_aero_storage() {
        fn takes_storage_error(_: aero_storage::DiskError) {}
        fn takes_storage_result(_: aero_storage::Result<()>) {}

        let err = DiskError::QuotaExceeded;
        takes_storage_error(err);

        let res: DiskResult<()> = Ok(());
        takes_storage_result(res);
    }
}
