use sha2::Digest as _;
use std::collections::BTreeSet;
use std::fs;
use std::io::Read;

#[test]
fn package_outputs_are_reproducible_and_contain_expected_files() -> anyhow::Result<()> {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");

    let drivers_dir = testdata.join("drivers");
    let guest_tools_dir = testdata.join("guest-tools");
    let spec_path = testdata.join("spec.json");

    let out1 = tempfile::tempdir()?;
    let out2 = tempfile::tempdir()?;

    let config1 = aero_packager::PackageConfig {
        drivers_dir: drivers_dir.clone(),
        guest_tools_dir: guest_tools_dir.clone(),
        out_dir: out1.path().to_path_buf(),
        spec_path: spec_path.clone(),
        version: "1.2.3".to_string(),
        build_id: "test".to_string(),
        volume_id: "AERO_GUEST_TOOLS".to_string(),
        source_date_epoch: 0,
    };
    let config2 = aero_packager::PackageConfig {
        out_dir: out2.path().to_path_buf(),
        ..config1.clone()
    };

    let outputs1 = aero_packager::package_guest_tools(&config1)?;
    let outputs2 = aero_packager::package_guest_tools(&config2)?;

    // Deterministic outputs: byte-identical ISO/zip/manifest.
    assert_eq!(fs::read(&outputs1.iso_path)?, fs::read(&outputs2.iso_path)?);
    assert_eq!(fs::read(&outputs1.zip_path)?, fs::read(&outputs2.zip_path)?);
    assert_eq!(
        fs::read(&outputs1.manifest_path)?,
        fs::read(&outputs2.manifest_path)?
    );

    // Optional: ensure we accept common input arch directory names (`x64` instead of `amd64`)
    // while still emitting `drivers/amd64/...` paths inside the ISO/zip.
    let drivers_x64_tmp = tempfile::tempdir()?;
    copy_dir_all(&drivers_dir, drivers_x64_tmp.path())?;
    fs::rename(
        drivers_x64_tmp.path().join("amd64"),
        drivers_x64_tmp.path().join("x64"),
    )?;

    let out3 = tempfile::tempdir()?;
    let config3 = aero_packager::PackageConfig {
        drivers_dir: drivers_x64_tmp.path().to_path_buf(),
        out_dir: out3.path().to_path_buf(),
        ..config1.clone()
    };
    let outputs3 = aero_packager::package_guest_tools(&config3)?;
    assert_eq!(fs::read(&outputs1.iso_path)?, fs::read(&outputs3.iso_path)?);
    assert_eq!(fs::read(&outputs1.zip_path)?, fs::read(&outputs3.zip_path)?);
    assert_eq!(
        fs::read(&outputs1.manifest_path)?,
        fs::read(&outputs3.manifest_path)?
    );

    // Legacy spec schema (`required_drivers`) should still work and produce identical output.
    let legacy_spec_dir = tempfile::tempdir()?;
    let legacy_spec_path = legacy_spec_dir.path().join("spec.json");
    let legacy_spec = serde_json::json!({
        "required_drivers": [
            {
                "name": "testdrv",
                "expected_hardware_ids": [r"PCI\\VEN_1234&DEV_5678"],
            }
        ]
    });
    fs::write(&legacy_spec_path, serde_json::to_vec_pretty(&legacy_spec)?)?;

    let out4 = tempfile::tempdir()?;
    let config4 = aero_packager::PackageConfig {
        out_dir: out4.path().to_path_buf(),
        spec_path: legacy_spec_path,
        ..config1.clone()
    };
    let outputs4 = aero_packager::package_guest_tools(&config4)?;
    assert_eq!(fs::read(&outputs1.iso_path)?, fs::read(&outputs4.iso_path)?);
    assert_eq!(fs::read(&outputs1.zip_path)?, fs::read(&outputs4.zip_path)?);
    assert_eq!(
        fs::read(&outputs1.manifest_path)?,
        fs::read(&outputs4.manifest_path)?
    );

    // Verify ISO contains expected tree (via Joliet directory records).
    let iso_bytes = fs::read(&outputs1.iso_path)?;
    let tree = aero_packager::read_joliet_tree(&iso_bytes)?;
    let iso_entries = aero_packager::read_joliet_file_entries(&iso_bytes)?;
    for required in [
        "setup.cmd",
        "uninstall.cmd",
        "verify.cmd",
        "verify.ps1",
        "README.md",
        "THIRD_PARTY_NOTICES.md",
        "licenses/virtio-win/LICENSE.txt",
        "licenses/virtio-win/driver-pack-manifest.json",
        "config/devices.cmd",
        "manifest.json",
        "config/README.md",
        "config/devices.cmd",
        "certs/test.cer",
        "drivers/x86/testdrv/test.inf",
        "drivers/x86/testdrv/test.sys",
        "drivers/x86/testdrv/test.cat",
        "drivers/x86/testdrv/test.dll",
        "drivers/x86/testdrv/WdfCoInstaller01009.dll",
        "drivers/amd64/testdrv/test.inf",
        "drivers/amd64/testdrv/test.sys",
        "drivers/amd64/testdrv/test.cat",
        "drivers/amd64/testdrv/test.dll",
        "drivers/amd64/testdrv/WdfCoInstaller01009.dll",
    ] {
        assert!(
            tree.contains(required),
            "ISO is missing required file: {required}"
        );
    }

    // Zip should contain the exact same files.
    let zip_file = fs::File::open(&outputs1.zip_path)?;
    let mut zip = zip::ZipArchive::new(zip_file)?;
    let mut zip_paths = BTreeSet::new();
    for i in 0..zip.len() {
        let entry = zip.by_index(i)?;
        if entry.is_dir() {
            continue;
        }
        zip_paths.insert(entry.name().to_string());
    }
    assert_eq!(zip_paths, tree.paths);

    // Verify the ISO file extents match the zip file bytes (guards against wrong sector offsets).
    for entry in &iso_entries {
        let start = entry.extent_sector as usize * 2048;
        let end = start + entry.size as usize;
        let iso_payload = &iso_bytes[start..end];

        let mut zf = zip.by_name(&entry.path)?;
        let mut zip_payload = Vec::new();
        zf.read_to_end(&mut zip_payload)?;

        assert_eq!(
            iso_payload,
            zip_payload.as_slice(),
            "ISO/zip payload mismatch for {}",
            entry.path
        );
    }

    // Verify manifest hashes match the packaged bytes (via the zip).
    let manifest_bytes = fs::read(&outputs1.manifest_path)?;
    let manifest: aero_packager::Manifest = serde_json::from_slice(&manifest_bytes)?;
    assert_eq!(manifest.package.version, "1.2.3");
    assert_eq!(manifest.package.build_id, "test");
    assert_eq!(manifest.package.source_date_epoch, 0);

    let mut manifest_paths = BTreeSet::new();
    for entry in &manifest.files {
        assert_ne!(entry.path, "manifest.json");
        manifest_paths.insert(entry.path.clone());

        let mut zf = zip.by_name(&entry.path)?;
        let mut buf = Vec::new();
        zf.read_to_end(&mut buf)?;

        let mut h = sha2::Sha256::new();
        h.update(&buf);
        let sha = hex::encode(h.finalize());
        assert_eq!(sha, entry.sha256, "sha mismatch for {}", entry.path);
        assert_eq!(
            buf.len() as u64,
            entry.size,
            "size mismatch for {}",
            entry.path
        );
    }

    // Zip includes manifest.json in addition to the files hashed within it.
    assert!(zip_paths.contains("manifest.json"));
    assert_eq!(manifest_paths.len() + 1, zip_paths.len());
    assert_eq!(
        manifest_paths
            .into_iter()
            .chain(std::iter::once("manifest.json".to_string()))
            .collect::<BTreeSet<_>>(),
        zip_paths
    );

    Ok(())
}

#[test]
fn optional_drivers_are_skipped_when_missing_and_stray_driver_dirs_are_ignored(
) -> anyhow::Result<()> {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");

    let drivers_dir = testdata.join("drivers");
    let guest_tools_dir = testdata.join("guest-tools");

    let drivers_tmp = tempfile::tempdir()?;
    copy_dir_all(&drivers_dir, drivers_tmp.path())?;

    // Add an incomplete stray driver directory that should be ignored because it
    // isn't declared in the spec. If the packager accidentally validates or
    // includes it, packaging would fail.
    for arch in ["x86", "amd64"] {
        let stray_dir = drivers_tmp.path().join(arch).join("stray");
        fs::create_dir_all(&stray_dir)?;
        fs::write(stray_dir.join("stray.inf"), b"; stray\n")?;
    }

    let spec_dir = tempfile::tempdir()?;
    let spec_path = spec_dir.path().join("spec.json");
    let spec = serde_json::json!({
        "drivers": [
            {
                "name": "testdrv",
                "required": true,
                "expected_hardware_ids": [r"PCI\\VEN_1234&DEV_5678"],
            },
            {
                "name": "optdrv",
                "required": false,
                "expected_hardware_ids": [r"PCI\\VEN_BEEF&DEV_CAFE"],
            },
        ]
    });
    fs::write(&spec_path, serde_json::to_vec_pretty(&spec)?)?;

    let out = tempfile::tempdir()?;
    let config = aero_packager::PackageConfig {
        drivers_dir: drivers_tmp.path().to_path_buf(),
        guest_tools_dir: guest_tools_dir.clone(),
        out_dir: out.path().to_path_buf(),
        spec_path,
        version: "0.0.0".to_string(),
        build_id: "test".to_string(),
        volume_id: "AERO_GUEST_TOOLS".to_string(),
        source_date_epoch: 0,
    };

    let outputs = aero_packager::package_guest_tools(&config)?;
    let iso_bytes = fs::read(&outputs.iso_path)?;
    let tree = aero_packager::read_joliet_tree(&iso_bytes)?;

    assert!(tree.contains("drivers/x86/testdrv/test.inf"));
    assert!(tree.contains("drivers/amd64/testdrv/test.inf"));

    assert!(
        !tree
            .paths
            .iter()
            .any(|p| p.starts_with("drivers/x86/stray/")),
        "stray driver unexpectedly packaged for x86"
    );
    assert!(
        !tree
            .paths
            .iter()
            .any(|p| p.starts_with("drivers/amd64/stray/")),
        "stray driver unexpectedly packaged for amd64"
    );

    assert!(
        !tree
            .paths
            .iter()
            .any(|p| p.starts_with("drivers/x86/optdrv/")),
        "missing optional driver unexpectedly packaged for x86"
    );
    assert!(
        !tree
            .paths
            .iter()
            .any(|p| p.starts_with("drivers/amd64/optdrv/")),
        "missing optional driver unexpectedly packaged for amd64"
    );

    Ok(())
}

#[test]
fn optional_drivers_are_validated_when_present() -> anyhow::Result<()> {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");

    let drivers_dir = testdata.join("drivers");
    let guest_tools_dir = testdata.join("guest-tools");

    let drivers_tmp = tempfile::tempdir()?;
    copy_dir_all(&drivers_dir, drivers_tmp.path())?;

    // Add an optional driver directory, but make it incomplete (missing .sys)
    // to ensure optional drivers are still validated when present.
    for arch in ["x86", "amd64"] {
        let opt_dir = drivers_tmp.path().join(arch).join("optdrv");
        fs::create_dir_all(&opt_dir)?;
        fs::write(
            opt_dir.join("opt.inf"),
            b"[Version]\n; PCI\\VEN_BEEF&DEV_CAFE\n",
        )?;
        fs::write(opt_dir.join("opt.cat"), b"dummy cat\n")?;
    }

    let spec_dir = tempfile::tempdir()?;
    let spec_path = spec_dir.path().join("spec.json");
    let spec = serde_json::json!({
        "drivers": [
            {
                "name": "testdrv",
                "required": true,
                "expected_hardware_ids": [r"PCI\\VEN_1234&DEV_5678"],
            },
            {
                "name": "optdrv",
                "required": false,
                "expected_hardware_ids": [r"PCI\\VEN_BEEF&DEV_CAFE"],
            },
        ]
    });
    fs::write(&spec_path, serde_json::to_vec_pretty(&spec)?)?;

    let out = tempfile::tempdir()?;
    let config = aero_packager::PackageConfig {
        drivers_dir: drivers_tmp.path().to_path_buf(),
        guest_tools_dir: guest_tools_dir.clone(),
        out_dir: out.path().to_path_buf(),
        spec_path,
        version: "0.0.0".to_string(),
        build_id: "test".to_string(),
        volume_id: "AERO_GUEST_TOOLS".to_string(),
        source_date_epoch: 0,
    };

    let err = aero_packager::package_guest_tools(&config).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("optdrv") && msg.contains("incomplete"),
        "unexpected error: {msg}"
    );

    // Now make the optional driver complete and ensure it is included.
    for arch in ["x86", "amd64"] {
        let opt_dir = drivers_tmp.path().join(arch).join("optdrv");
        fs::write(opt_dir.join("opt.sys"), b"dummy sys\n")?;
    }

    let out2 = tempfile::tempdir()?;
    let config2 = aero_packager::PackageConfig {
        out_dir: out2.path().to_path_buf(),
        ..config
    };

    let outputs = aero_packager::package_guest_tools(&config2)?;
    let iso_bytes = fs::read(&outputs.iso_path)?;
    let tree = aero_packager::read_joliet_tree(&iso_bytes)?;

    for required in [
        "drivers/x86/optdrv/opt.inf",
        "drivers/x86/optdrv/opt.sys",
        "drivers/x86/optdrv/opt.cat",
        "drivers/amd64/optdrv/opt.inf",
        "drivers/amd64/optdrv/opt.sys",
        "drivers/amd64/optdrv/opt.cat",
    ] {
        assert!(
            tree.contains(required),
            "ISO is missing expected optional driver file: {required}"
        );
    }

    Ok(())
}

#[test]
fn package_rejects_private_key_materials() -> anyhow::Result<()> {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");

    let drivers_dir = testdata.join("drivers");
    let guest_tools_dir = testdata.join("guest-tools");
    let spec_path = testdata.join("spec.json");

    let drivers_tmp = tempfile::tempdir()?;
    copy_dir_all(&drivers_dir, drivers_tmp.path())?;
    fs::write(
        drivers_tmp.path().join("x86/testdrv/test.pfx"),
        b"dummy pfx",
    )?;

    let out_dir = tempfile::tempdir()?;
    let config = aero_packager::PackageConfig {
        drivers_dir: drivers_tmp.path().to_path_buf(),
        guest_tools_dir,
        out_dir: out_dir.path().to_path_buf(),
        spec_path,
        version: "1.2.3".to_string(),
        build_id: "test".to_string(),
        volume_id: "AERO_GUEST_TOOLS".to_string(),
        source_date_epoch: 0,
    };

    let err = aero_packager::package_guest_tools(&config).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("refusing to package private key material"),
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
