#![cfg(unix)]

use std::fs;

#[test]
fn packaging_fails_fast_on_symlink_in_guest_tools_licenses() -> anyhow::Result<()> {
    use std::os::unix::fs::symlink;

    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");

    let drivers_dir = testdata.join("drivers");
    let guest_tools_src = testdata.join("guest-tools");
    let spec_path = testdata.join("spec.json");

    let guest_tools_tmp = tempfile::tempdir()?;
    copy_dir_all(&guest_tools_src, guest_tools_tmp.path())?;

    // Create a license file symlink; symlinks must not be silently skipped since that can lead
    // to confusing/unsafe packaging outcomes.
    let licenses_dir = guest_tools_tmp.path().join("licenses");
    fs::create_dir_all(&licenses_dir)?;
    let real = licenses_dir.join("real.txt");
    fs::write(&real, b"real license\n")?;
    let link = licenses_dir.join("link.txt");
    symlink(&real, &link)?;

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
    assert!(msg.contains("symlink"), "unexpected error: {msg}");
    assert!(
        msg.contains(&link.display().to_string()),
        "expected error to include full symlink path {}; got: {msg}",
        link.display()
    );
    assert!(
        msg.contains("guest_tools/licenses"),
        "unexpected error: {msg}"
    );
    assert!(
        msg.contains("replace the symlink with a real file or remove it"),
        "unexpected error (missing remediation): {msg}"
    );

    Ok(())
}

#[test]
fn packaging_fails_fast_on_symlink_in_guest_tools_tools() -> anyhow::Result<()> {
    use std::os::unix::fs::symlink;

    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");

    let drivers_dir = testdata.join("drivers");
    let guest_tools_src = testdata.join("guest-tools");
    let spec_path = testdata.join("spec.json");

    let guest_tools_tmp = tempfile::tempdir()?;
    copy_dir_all(&guest_tools_src, guest_tools_tmp.path())?;

    // Create a tools payload symlink; symlinks must not be silently skipped since that can lead
    // to confusing/unsafe packaging outcomes (and can be used to smuggle unintended host files).
    let tools_dir = guest_tools_tmp.path().join("tools");
    fs::create_dir_all(&tools_dir)?;
    let real = tools_dir.join("real.exe");
    fs::write(&real, b"real tool\n")?;
    let link = tools_dir.join("link.exe");
    symlink(&real, &link)?;

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
    assert!(msg.contains("symlink"), "unexpected error: {msg}");
    assert!(
        msg.contains(&link.display().to_string()),
        "expected error to include full symlink path {}; got: {msg}",
        link.display()
    );
    assert!(
        msg.contains("guest_tools/tools"),
        "unexpected error: {msg}"
    );
    assert!(
        msg.contains("replace the symlink with a real file or remove it"),
        "unexpected error (missing remediation): {msg}"
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

fn device_contract_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
        .join("docs")
        .join("windows-device-contract.json")
}
