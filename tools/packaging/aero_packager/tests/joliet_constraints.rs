use std::fs;
use std::path::{Path, PathBuf};

#[test]
fn packaging_rejects_non_bmp_characters_in_joliet_identifiers() -> anyhow::Result<()> {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");

    let drivers_dir = testdata.join("drivers");
    let guest_tools_src = testdata.join("guest-tools");
    let spec_path = testdata.join("spec.json");

    let guest_tools_tmp = tempfile::tempdir()?;
    copy_dir_all(&guest_tools_src, guest_tools_tmp.path())?;

    // Emoji are non-BMP and require surrogate pairs in UTF-16; Joliet uses UCS-2 (BMP-only).
    let bad_name = "emoji-😀.txt";
    let bad_path = guest_tools_tmp.path().join("config").join(bad_name);
    if let Err(err) = fs::write(&bad_path, b"bad\n") {
        eprintln!(
            "skipping test: failed to create file with non-BMP name {}: {err}",
            bad_path.display()
        );
        return Ok(());
    }

    let out_dir = tempfile::tempdir()?;
    let config = aero_packager::PackageConfig {
        drivers_dir,
        guest_tools_dir: guest_tools_tmp.path().to_path_buf(),
        windows_device_contract_path: device_contract_path(),
        out_dir: out_dir.path().to_path_buf(),
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
        msg.contains("Joliet") && msg.contains("UCS-2"),
        "unexpected error: {msg}"
    );
    assert!(
        msg.contains("config/emoji-"),
        "expected error to include full path, got: {msg}"
    );
    Ok(())
}

#[test]
fn packaging_rejects_joliet_level3_identifiers_longer_than_64_chars() -> anyhow::Result<()> {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");

    let drivers_dir = testdata.join("drivers");
    let guest_tools_src = testdata.join("guest-tools");
    let spec_path = testdata.join("spec.json");

    let guest_tools_tmp = tempfile::tempdir()?;
    copy_dir_all(&guest_tools_src, guest_tools_tmp.path())?;

    let long_name = format!("{}.txt", "a".repeat(65));
    let bad_path = guest_tools_tmp.path().join("config").join(&long_name);
    if let Err(err) = fs::write(&bad_path, b"bad\n") {
        eprintln!(
            "skipping test: failed to create file with long name {}: {err}",
            bad_path.display()
        );
        return Ok(());
    }

    let out_dir = tempfile::tempdir()?;
    let config = aero_packager::PackageConfig {
        drivers_dir,
        guest_tools_dir: guest_tools_tmp.path().to_path_buf(),
        windows_device_contract_path: device_contract_path(),
        out_dir: out_dir.path().to_path_buf(),
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
        msg.contains("Joliet") && msg.contains("64"),
        "unexpected error: {msg}"
    );
    assert!(
        msg.contains(&long_name),
        "expected error to include full path, got: {msg}"
    );
    Ok(())
}

fn copy_dir_all(src: &Path, dst: &Path) -> anyhow::Result<()> {
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

