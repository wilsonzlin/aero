use std::fs;
use std::path::{Path, PathBuf};

#[test]
fn production_and_none_signing_policy_reject_certificate_payloads() -> anyhow::Result<()> {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");

    let drivers_dir = testdata.join("drivers");
    let guest_tools_dir = testdata.join("guest-tools"); // contains certs/test.cer
    let spec_path = testdata.join("spec.json");

    for signing_policy in [
        aero_packager::SigningPolicy::Production,
        aero_packager::SigningPolicy::None,
    ] {
        let out = tempfile::tempdir()?;
        let config = aero_packager::PackageConfig {
            drivers_dir: drivers_dir.clone(),
            guest_tools_dir: guest_tools_dir.clone(),
            windows_device_contract_path: device_contract_path(),
            out_dir: out.path().to_path_buf(),
            spec_path: spec_path.clone(),
            version: "0.0.0".to_string(),
            build_id: "test".to_string(),
            volume_id: "AERO_GUEST_TOOLS".to_string(),
            signing_policy,
            source_date_epoch: 0,
        };

        let err = aero_packager::package_guest_tools(&config).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("must not ship test certificates") && msg.contains("signing_policy="),
            "unexpected error: {msg}"
        );
    }

    Ok(())
}

#[cfg(unix)]
#[test]
fn symlinks_in_input_trees_cause_packaging_failure() -> anyhow::Result<()> {
    use std::os::unix::fs::symlink;

    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");

    let drivers_tmp = tempfile::tempdir()?;
    copy_dir_all(&testdata.join("drivers"), drivers_tmp.path())?;
    let link_path = drivers_tmp.path().join("x86/testdrv/link.txt");
    symlink("test.inf", &link_path)?;

    let out = tempfile::tempdir()?;
    let config = aero_packager::PackageConfig {
        drivers_dir: drivers_tmp.path().to_path_buf(),
        guest_tools_dir: testdata.join("guest-tools-no-certs"),
        windows_device_contract_path: device_contract_path(),
        out_dir: out.path().to_path_buf(),
        spec_path: testdata.join("spec.json"),
        version: "0.0.0".to_string(),
        build_id: "test".to_string(),
        volume_id: "AERO_GUEST_TOOLS".to_string(),
        signing_policy: aero_packager::SigningPolicy::None,
        source_date_epoch: 0,
    };

    let err = aero_packager::package_guest_tools(&config).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("symlink") && msg.contains("link.txt"),
        "unexpected error: {msg}"
    );
    Ok(())
}

#[test]
fn additional_secret_key_extensions_are_rejected() -> anyhow::Result<()> {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");

    let guest_tools_tmp = tempfile::tempdir()?;
    copy_dir_all(&testdata.join("guest-tools-no-certs"), guest_tools_tmp.path())?;
    fs::write(
        guest_tools_tmp.path().join("config/secret.p12"),
        b"dummy secret key material\n",
    )?;

    let out = tempfile::tempdir()?;
    let config = aero_packager::PackageConfig {
        drivers_dir: testdata.join("drivers"),
        guest_tools_dir: guest_tools_tmp.path().to_path_buf(),
        windows_device_contract_path: device_contract_path(),
        out_dir: out.path().to_path_buf(),
        spec_path: testdata.join("spec.json"),
        version: "0.0.0".to_string(),
        build_id: "test".to_string(),
        volume_id: "AERO_GUEST_TOOLS".to_string(),
        signing_policy: aero_packager::SigningPolicy::None,
        source_date_epoch: 0,
    };

    let err = aero_packager::package_guest_tools(&config).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("private key material") && msg.contains("secret.p12"),
        "unexpected error: {msg}"
    );
    Ok(())
}

#[test]
fn guest_tools_tools_must_be_directory_if_present() -> anyhow::Result<()> {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");
    let spec_path = testdata.join("spec.json");

    let drivers_dir = testdata.join("drivers");

    let guest_tools_tmp = tempfile::tempdir()?;
    copy_dir_all(&testdata.join("guest-tools-no-certs"), guest_tools_tmp.path())?;

    // `guest-tools/tools/` is optional. However, if present, it must be a real directory (not a
    // file/symlink) so we have deterministic, safe packaging semantics.
    fs::write(guest_tools_tmp.path().join("tools"), b"not a directory\n")?;

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
        signing_policy: aero_packager::SigningPolicy::None,
        source_date_epoch: 0,
    };

    let err = aero_packager::package_guest_tools(&config).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("tools") && msg.contains("not a directory"),
        "unexpected error: {msg}"
    );

    Ok(())
}

#[test]
fn windows_shell_metadata_files_are_excluded_from_config_and_licenses() -> anyhow::Result<()> {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");
    let spec_path = testdata.join("spec.json");
    let drivers_dir = testdata.join("drivers");

    let out_base = tempfile::tempdir()?;
    let config_base = aero_packager::PackageConfig {
        drivers_dir: drivers_dir.clone(),
        guest_tools_dir: testdata.join("guest-tools"),
        windows_device_contract_path: device_contract_path(),
        out_dir: out_base.path().to_path_buf(),
        spec_path: spec_path.clone(),
        version: "1.2.3".to_string(),
        build_id: "test".to_string(),
        volume_id: "AERO_GUEST_TOOLS".to_string(),
        signing_policy: aero_packager::SigningPolicy::Test,
        source_date_epoch: 0,
    };
    let outputs_base = aero_packager::package_guest_tools(&config_base)?;
    let iso_base = fs::read(&outputs_base.iso_path)?;
    let zip_base = fs::read(&outputs_base.zip_path)?;
    let manifest_base = fs::read(&outputs_base.manifest_path)?;

    let guest_tools_tmp = tempfile::tempdir()?;
    copy_dir_all(&testdata.join("guest-tools"), guest_tools_tmp.path())?;

    // Add deterministic-breaking OS metadata artifacts that must be ignored.
    fs::write(guest_tools_tmp.path().join("config/Thumbs.db"), b"thumbs")?;
    fs::write(guest_tools_tmp.path().join("config/desktop.ini"), b"ini")?;
    fs::create_dir_all(guest_tools_tmp.path().join("config/__MACOSX"))?;
    fs::write(
        guest_tools_tmp.path().join("config/__MACOSX/junk.txt"),
        b"junk",
    )?;

    fs::write(guest_tools_tmp.path().join("licenses/Thumbs.db"), b"thumbs")?;
    fs::write(guest_tools_tmp.path().join("licenses/desktop.ini"), b"ini")?;
    fs::create_dir_all(guest_tools_tmp.path().join("licenses/__MACOSX"))?;
    fs::write(
        guest_tools_tmp.path().join("licenses/__MACOSX/junk.txt"),
        b"junk",
    )?;

    let out = tempfile::tempdir()?;
    let config = aero_packager::PackageConfig {
        guest_tools_dir: guest_tools_tmp.path().to_path_buf(),
        out_dir: out.path().to_path_buf(),
        ..config_base
    };
    let outputs = aero_packager::package_guest_tools(&config)?;
    let iso_bytes = fs::read(&outputs.iso_path)?;
    let tree = aero_packager::read_joliet_tree(&iso_bytes)?;

    for unexpected in [
        "config/Thumbs.db",
        "config/desktop.ini",
        "config/__MACOSX/junk.txt",
        "licenses/Thumbs.db",
        "licenses/desktop.ini",
        "licenses/__MACOSX/junk.txt",
    ] {
        assert!(
            !tree.contains(unexpected),
            "unexpected file packaged: {unexpected}"
        );
    }

    // Excluding metadata files should keep outputs stable.
    assert_eq!(iso_base, iso_bytes);
    assert_eq!(zip_base, fs::read(&outputs.zip_path)?);
    assert_eq!(manifest_base, fs::read(&outputs.manifest_path)?);

    Ok(())
}

#[cfg(not(windows))]
#[test]
fn case_insensitive_path_collisions_are_rejected() -> anyhow::Result<()> {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");

    let guest_tools_tmp = tempfile::tempdir()?;
    copy_dir_all(&testdata.join("guest-tools-no-certs"), guest_tools_tmp.path())?;

    let config_dir = guest_tools_tmp.path().join("config");
    fs::write(config_dir.join("Case.txt"), b"a")?;
    fs::write(config_dir.join("case.txt"), b"b")?;

    // Some hosts (notably default macOS) use case-insensitive filesystems and cannot represent the
    // collision fixture. If we can't create both entries distinctly, skip the assertion.
    let mut entries = std::collections::BTreeSet::<String>::new();
    for entry in fs::read_dir(&config_dir)? {
        let entry = entry?;
        entries.insert(entry.file_name().to_string_lossy().to_string());
    }
    if !(entries.contains("Case.txt") && entries.contains("case.txt")) {
        return Ok(());
    }

    let out = tempfile::tempdir()?;
    let config = aero_packager::PackageConfig {
        drivers_dir: testdata.join("drivers"),
        guest_tools_dir: guest_tools_tmp.path().to_path_buf(),
        windows_device_contract_path: device_contract_path(),
        out_dir: out.path().to_path_buf(),
        spec_path: testdata.join("spec.json"),
        version: "0.0.0".to_string(),
        build_id: "test".to_string(),
        volume_id: "AERO_GUEST_TOOLS".to_string(),
        signing_policy: aero_packager::SigningPolicy::None,
        source_date_epoch: 0,
    };

    let err = aero_packager::package_guest_tools(&config).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("case-insensitive path collision"),
        "unexpected error: {msg}"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn non_utf8_paths_are_rejected() -> anyhow::Result<()> {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");

    let guest_tools_tmp = tempfile::tempdir()?;
    copy_dir_all(&testdata.join("guest-tools-no-certs"), guest_tools_tmp.path())?;

    let bad_name = OsString::from_vec(vec![0x66, 0x6f, 0x80, 0x2e, 0x74, 0x78, 0x74]); // fo\x80.txt
    let bad_path = guest_tools_tmp.path().join("config").join(bad_name);
    fs::write(&bad_path, b"bad\n")?;

    let out = tempfile::tempdir()?;
    let config = aero_packager::PackageConfig {
        drivers_dir: testdata.join("drivers"),
        guest_tools_dir: guest_tools_tmp.path().to_path_buf(),
        windows_device_contract_path: device_contract_path(),
        out_dir: out.path().to_path_buf(),
        spec_path: testdata.join("spec.json"),
        version: "0.0.0".to_string(),
        build_id: "test".to_string(),
        volume_id: "AERO_GUEST_TOOLS".to_string(),
        signing_policy: aero_packager::SigningPolicy::None,
        source_date_epoch: 0,
    };

    let err = aero_packager::package_guest_tools(&config).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.to_ascii_lowercase().contains("utf8"),
        "unexpected error: {msg}"
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
