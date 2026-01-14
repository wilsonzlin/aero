use std::fs;
use std::path::PathBuf;

#[test]
fn packaging_rejects_windows_reserved_device_names_in_paths() -> anyhow::Result<()> {
    // Windows cannot create these paths in the first place.
    if cfg!(windows) {
        return Ok(());
    }

    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");

    let drivers_dir = testdata.join("drivers");
    let guest_tools_src = testdata.join("guest-tools");
    let spec_path = testdata.join("spec.json");

    let guest_tools_tmp = tempfile::tempdir()?;
    copy_dir_all(&guest_tools_src, guest_tools_tmp.path())?;
    fs::write(guest_tools_tmp.path().join("licenses/con.txt"), b"bad\n")?;

    let out = tempfile::tempdir()?;
    let config = aero_packager::PackageConfig {
        drivers_dir,
        guest_tools_dir: guest_tools_tmp.path().to_path_buf(),
        windows_device_contract_path: device_contract_path(),
        out_dir: out.path().to_path_buf(),
        spec_path,
        version: "0.0.0".to_string(),
        build_id: "test".to_string(),
        volume_id: "AERO_GUEST_TOOLS".to_string(),
        signing_policy: aero_packager::SigningPolicy::Test,
        source_date_epoch: 0,
    };

    let err = aero_packager::package_guest_tools(&config).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("con.txt"),
        "unexpected error: {msg}"
    );
    Ok(())
}

#[test]
fn packaging_rejects_windows_invalid_characters_in_paths() -> anyhow::Result<()> {
    // Windows cannot create these paths in the first place.
    if cfg!(windows) {
        return Ok(());
    }

    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");

    let drivers_dir = testdata.join("drivers");
    let guest_tools_src = testdata.join("guest-tools");
    let spec_path = testdata.join("spec.json");

    let guest_tools_tmp = tempfile::tempdir()?;
    copy_dir_all(&guest_tools_src, guest_tools_tmp.path())?;
    fs::write(guest_tools_tmp.path().join("licenses/foo?.txt"), b"bad\n")?;

    let out = tempfile::tempdir()?;
    let config = aero_packager::PackageConfig {
        drivers_dir,
        guest_tools_dir: guest_tools_tmp.path().to_path_buf(),
        windows_device_contract_path: device_contract_path(),
        out_dir: out.path().to_path_buf(),
        spec_path,
        version: "0.0.0".to_string(),
        build_id: "test".to_string(),
        volume_id: "AERO_GUEST_TOOLS".to_string(),
        signing_policy: aero_packager::SigningPolicy::Test,
        source_date_epoch: 0,
    };

    let err = aero_packager::package_guest_tools(&config).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("foo?.txt"),
        "unexpected error: {msg}"
    );
    Ok(())
}

fn copy_dir_all(src: &std::path::Path, dst: &std::path::Path) -> anyhow::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let dst_path = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&entry.path(), &dst_path)?;
        } else {
            fs::copy(entry.path(), dst_path)?;
        }
    }
    Ok(())
}

fn device_contract_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
        .join("docs")
        .join("windows-device-contract.json")
}
