use std::fs;
use std::path::{Path, PathBuf};

#[test]
fn packaging_fails_when_inf_catalogfile_is_missing() -> anyhow::Result<()> {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");
    let drivers_src = testdata.join("drivers");
    let guest_tools_dir = testdata.join("guest-tools");
    let spec_path = testdata.join("spec.json");

    let drivers_tmp = tempfile::tempdir()?;
    copy_dir_all(&drivers_src, drivers_tmp.path())?;

    // Add a CatalogFile directive pointing at a missing file. The directory still contains a
    // different `.cat` to satisfy the baseline "at least one .cat exists" validation.
    for arch in ["x86", "amd64"] {
        let inf_path = drivers_tmp.path().join(format!("{arch}/testdrv/test.inf"));
        let original = fs::read_to_string(&inf_path)?;

        let mut out = Vec::<String>::new();
        let mut inserted = false;
        for line in original.lines() {
            out.push(line.to_string());
            if !inserted && line.trim().eq_ignore_ascii_case(r#"Signature="$Windows NT$""#) {
                out.push("CatalogFile = missing.cat".to_string());
                inserted = true;
            }
        }
        if !inserted {
            out.push("CatalogFile = missing.cat".to_string());
        }
        fs::write(&inf_path, out.join("\n") + "\n")?;
    }

    let out_dir = tempfile::tempdir()?;
    let config = aero_packager::PackageConfig {
        drivers_dir: drivers_tmp.path().to_path_buf(),
        guest_tools_dir,
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
    assert!(msg.contains("missing.cat"), "unexpected error: {msg}");
    Ok(())
}

#[test]
fn packaging_fails_when_inf_servicebinary_is_missing() -> anyhow::Result<()> {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");
    let drivers_src = testdata.join("drivers");
    let guest_tools_dir = testdata.join("guest-tools");
    let spec_path = testdata.join("spec.json");

    let drivers_tmp = tempfile::tempdir()?;
    copy_dir_all(&drivers_src, drivers_tmp.path())?;

    // Add a ServiceBinary directive pointing at a missing SYS. The directory still contains a
    // different `.sys` to satisfy the baseline "at least one .sys exists" validation.
    for arch in ["x86", "amd64"] {
        let inf_path = drivers_tmp.path().join(format!("{arch}/testdrv/test.inf"));
        let original = fs::read_to_string(&inf_path)?;
        let mut out = original.trim_end_matches('\n').to_string();
        out.push_str("\n\n[ServiceInstall]\nServiceBinary = %12%\\missing.sys\n");
        fs::write(&inf_path, out)?;
    }

    let out_dir = tempfile::tempdir()?;
    let config = aero_packager::PackageConfig {
        drivers_dir: drivers_tmp.path().to_path_buf(),
        guest_tools_dir,
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
    assert!(msg.contains("missing.sys"), "unexpected error: {msg}");
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

