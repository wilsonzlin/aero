use aero_opfs::{DiskError, DiskResult};

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

