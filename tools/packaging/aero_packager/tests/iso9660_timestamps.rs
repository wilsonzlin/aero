use std::fs;
use std::path::PathBuf;

const SECTOR_SIZE: usize = 2048;
const SYSTEM_AREA_SECTORS: usize = 16;

fn device_contract_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
        .join("docs")
        .join("windows-device-contract.json")
}

fn pvd_root_dir_timestamp_7(iso_bytes: &[u8]) -> [u8; 7] {
    // The Primary Volume Descriptor is always immediately after the System Area.
    let pvd_off = SYSTEM_AREA_SECTORS * SECTOR_SIZE;
    // Root directory record is at 156..190 within the PVD; timestamp at 18..25 within the record.
    let ts_off = pvd_off + 156 + 18;
    iso_bytes[ts_off..ts_off + 7]
        .try_into()
        .expect("PVD root record timestamp bytes")
}

#[test]
fn iso_directory_record_timestamps_clamp_too_early_epoch_to_1900() -> anyhow::Result<()> {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");

    let early_epoch = time::OffsetDateTime::new_utc(
        time::Date::from_calendar_date(1800, time::Month::January, 1)?,
        time::Time::MIDNIGHT,
    )
    .unix_timestamp();

    let out_dir = tempfile::tempdir()?;
    let config = aero_packager::PackageConfig {
        drivers_dir: testdata.join("drivers"),
        guest_tools_dir: testdata.join("guest-tools"),
        windows_device_contract_path: device_contract_path(),
        out_dir: out_dir.path().to_path_buf(),
        spec_path: testdata.join("spec.json"),
        version: "0.0.0".to_string(),
        build_id: "test".to_string(),
        volume_id: "AERO_GUEST_TOOLS".to_string(),
        signing_policy: aero_packager::SigningPolicy::Test,
        source_date_epoch: early_epoch,
    };

    let outputs = aero_packager::package_guest_tools(&config)?;
    let iso_bytes = fs::read(&outputs.iso_path)?;

    assert_eq!(
        pvd_root_dir_timestamp_7(&iso_bytes),
        [0, 1, 1, 0, 0, 0, 0],
        "expected timestamps to clamp to 1900-01-01 00:00:00"
    );

    Ok(())
}

#[test]
fn iso_directory_record_timestamps_clamp_too_late_epoch_to_2155() -> anyhow::Result<()> {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");

    let out_dir = tempfile::tempdir()?;
    let config = aero_packager::PackageConfig {
        drivers_dir: testdata.join("drivers"),
        guest_tools_dir: testdata.join("guest-tools"),
        windows_device_contract_path: device_contract_path(),
        out_dir: out_dir.path().to_path_buf(),
        spec_path: testdata.join("spec.json"),
        version: "0.0.0".to_string(),
        build_id: "test".to_string(),
        volume_id: "AERO_GUEST_TOOLS".to_string(),
        signing_policy: aero_packager::SigningPolicy::Test,
        // Intentionally use an epoch far beyond what `time` can represent; ISO generation should
        // still succeed by clamping to the ISO9660 representable year range (1900..=2155).
        source_date_epoch: i64::MAX,
    };

    let outputs = aero_packager::package_guest_tools(&config)?;
    let iso_bytes = fs::read(&outputs.iso_path)?;

    assert_eq!(
        pvd_root_dir_timestamp_7(&iso_bytes),
        [255, 12, 31, 23, 59, 59, 0],
        "expected timestamps to clamp to 2155-12-31 23:59:59"
    );

    Ok(())
}

