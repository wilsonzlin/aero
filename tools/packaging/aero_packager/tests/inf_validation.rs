use std::fs;
use std::path::PathBuf;

fn device_contract_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
        .join("docs")
        .join("windows-device-contract.json")
}

fn utf16le_with_bom(s: &str) -> Vec<u8> {
    let mut out = vec![0xFF, 0xFE];
    for u in s.encode_utf16() {
        out.extend_from_slice(&u.to_le_bytes());
    }
    out
}

fn write_minimal_driver_tree(
    drivers_dir: &std::path::Path,
    inf_name: &str,
    inf_bytes: Vec<u8>,
) -> anyhow::Result<()> {
    for arch in ["x86", "amd64"] {
        let drv_dir = drivers_dir.join(arch).join("testdrv");
        fs::create_dir_all(&drv_dir)?;
        fs::write(drv_dir.join(inf_name), &inf_bytes)?;
        fs::write(drv_dir.join("test.sys"), b"dummy sys\n")?;
        fs::write(drv_dir.join("test.cat"), b"dummy cat\n")?;
    }
    Ok(())
}

#[test]
fn packaging_fails_when_expected_inf_file_is_missing() -> anyhow::Result<()> {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");

    let drivers_tmp = tempfile::tempdir()?;
    write_minimal_driver_tree(drivers_tmp.path(), "test.inf", b"dummy\n".to_vec())?;

    let spec_dir = tempfile::tempdir()?;
    let spec_path = spec_dir.path().join("spec.json");
    let spec = serde_json::json!({
        "drivers": [
            {
                "name": "testdrv",
                "required": true,
                "expected_inf_files": ["missing.inf"],
            }
        ]
    });
    fs::write(&spec_path, serde_json::to_vec_pretty(&spec)?)?;

    let out_dir = tempfile::tempdir()?;
    let config = aero_packager::PackageConfig {
        drivers_dir: drivers_tmp.path().to_path_buf(),
        guest_tools_dir: testdata.join("guest-tools-no-certs"),
        windows_device_contract_path: device_contract_path(),
        out_dir: out_dir.path().to_path_buf(),
        spec_path,
        version: "0.0.0".to_string(),
        build_id: "test".to_string(),
        volume_id: "AERO_GUEST_TOOLS".to_string(),
        signing_policy: aero_packager::SigningPolicy::None,
        source_date_epoch: 0,
    };

    let err = aero_packager::package_guest_tools(&config).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("testdrv")
            && msg.contains("expected_inf_files")
            && msg.contains("missing.inf")
            && msg.contains("x86"),
        "unexpected error: {msg}"
    );

    Ok(())
}

#[test]
fn packaging_fails_when_expected_addservice_is_missing_in_utf16le_inf() -> anyhow::Result<()> {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");

    let drivers_tmp = tempfile::tempdir()?;
    // Service name appears only in a comment; validation must ignore it.
    let inf_text = "; AddService = TestSvc\n";
    write_minimal_driver_tree(drivers_tmp.path(), "test.inf", utf16le_with_bom(inf_text))?;

    let spec_dir = tempfile::tempdir()?;
    let spec_path = spec_dir.path().join("spec.json");
    let spec = serde_json::json!({
        "drivers": [
            {
                "name": "testdrv",
                "required": true,
                "expected_inf_files": ["test.inf"],
                "expected_add_services": ["TestSvc"],
            }
        ]
    });
    fs::write(&spec_path, serde_json::to_vec_pretty(&spec)?)?;

    let out_dir = tempfile::tempdir()?;
    let config = aero_packager::PackageConfig {
        drivers_dir: drivers_tmp.path().to_path_buf(),
        guest_tools_dir: testdata.join("guest-tools-no-certs"),
        windows_device_contract_path: device_contract_path(),
        out_dir: out_dir.path().to_path_buf(),
        spec_path,
        version: "0.0.0".to_string(),
        build_id: "test".to_string(),
        volume_id: "AERO_GUEST_TOOLS".to_string(),
        signing_policy: aero_packager::SigningPolicy::None,
        source_date_epoch: 0,
    };

    let err = aero_packager::package_guest_tools(&config).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("testdrv")
            && msg.contains("AddService")
            && msg.contains("TestSvc")
            && msg.contains("test.inf"),
        "unexpected error: {msg}"
    );

    Ok(())
}

#[test]
fn packaging_succeeds_when_expected_addservice_is_present_in_utf16le_inf() -> anyhow::Result<()> {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");

    let drivers_tmp = tempfile::tempdir()?;
    let inf_text = "AddService = \"TestSvc\" , 0x00000002, Service_Inst ; trailing comment\n";
    write_minimal_driver_tree(drivers_tmp.path(), "test.inf", utf16le_with_bom(inf_text))?;

    let spec_dir = tempfile::tempdir()?;
    let spec_path = spec_dir.path().join("spec.json");
    let spec = serde_json::json!({
        "drivers": [
            {
                "name": "testdrv",
                "required": true,
                "expected_inf_files": ["test.inf"],
                "expected_add_services": ["testsvc"],
            }
        ]
    });
    fs::write(&spec_path, serde_json::to_vec_pretty(&spec)?)?;

    let out_dir = tempfile::tempdir()?;
    let config = aero_packager::PackageConfig {
        drivers_dir: drivers_tmp.path().to_path_buf(),
        guest_tools_dir: testdata.join("guest-tools-no-certs"),
        windows_device_contract_path: device_contract_path(),
        out_dir: out_dir.path().to_path_buf(),
        spec_path,
        version: "0.0.0".to_string(),
        build_id: "test".to_string(),
        volume_id: "AERO_GUEST_TOOLS".to_string(),
        signing_policy: aero_packager::SigningPolicy::None,
        source_date_epoch: 0,
    };

    aero_packager::package_guest_tools(&config)?;
    Ok(())
}

