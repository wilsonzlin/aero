use sha2::Digest as _;
use std::collections::BTreeSet;
use std::fs;
use std::io::Read;

#[test]
fn package_outputs_are_reproducible_and_contain_expected_files() -> anyhow::Result<()> {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");

    // The repo intentionally avoids tracking real driver binaries. For packager tests, create a
    // private copy of `testdata/drivers` and inject a dummy DLL so we can verify that optional
    // user-mode payloads are preserved in the output ISO/zip.
    let drivers_src = testdata.join("drivers");
    let drivers_tmp = tempfile::tempdir()?;
    copy_dir_all(&drivers_src, drivers_tmp.path())?;
    fs::write(
        drivers_tmp.path().join("x86/testdrv/test.dll"),
        b"dummy dll (x86)\n",
    )?;
    fs::write(
        drivers_tmp.path().join("amd64/testdrv/test.dll"),
        b"dummy dll (amd64)\n",
    )?;

    let drivers_dir = drivers_tmp.path().to_path_buf();
    let guest_tools_dir = testdata.join("guest-tools");
    let spec_path = testdata.join("spec.json");
    let windows_device_contract_path = device_contract_path();

    let out1 = tempfile::tempdir()?;
    let out2 = tempfile::tempdir()?;

    let config1 = aero_packager::PackageConfig {
        drivers_dir: drivers_dir.clone(),
        guest_tools_dir: guest_tools_dir.clone(),
        windows_device_contract_path: windows_device_contract_path.clone(),
        out_dir: out1.path().to_path_buf(),
        spec_path: spec_path.clone(),
        version: "1.2.3".to_string(),
        build_id: "test".to_string(),
        volume_id: "AERO_GUEST_TOOLS".to_string(),
        signing_policy: aero_packager::SigningPolicy::Test,
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

    // Legacy spec schema (`required_drivers`) should still work. Note that `manifest.json`
    // records the SHA-256 of the exact spec bytes used as an input, so the overall media will
    // differ when the spec JSON differs (even if it is semantically equivalent).
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
    let manifest4_bytes = fs::read(&outputs4.manifest_path)?;
    let manifest4: aero_packager::Manifest = serde_json::from_slice(&manifest4_bytes)?;

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
        "certs/README.md",
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
    assert_eq!(manifest.schema_version, 4);
    assert_eq!(manifest.package.version, "1.2.3");
    assert_eq!(manifest.package.build_id, "test");
    assert_eq!(manifest.package.source_date_epoch, 0);
    assert_eq!(manifest.signing_policy, aero_packager::SigningPolicy::Test);
    assert!(manifest.certs_required);

    // Compute expected canonical hashes for provenance checks below.
    let spec_sha256 = canonical_json_sha256_hex(&fs::read(&spec_path)?)?;
    let contract_sha256 = canonical_json_sha256_hex(&fs::read(&windows_device_contract_path)?)?;

    let provenance = manifest
        .provenance
        .as_ref()
        .expect("manifest should include provenance");
    assert_eq!(
        provenance.packaging_spec_path,
        spec_path.to_string_lossy().to_string()
    );
    assert_eq!(
        provenance.windows_device_contract_path,
        windows_device_contract_path.to_string_lossy().to_string()
    );
    assert_eq!(provenance.packaging_spec_sha256, spec_sha256);
    assert_eq!(provenance.windows_device_contract_sha256, contract_sha256);

    // Input provenance should be present.
    let inputs = manifest.inputs.as_ref().expect("manifest.inputs");
    let spec_input = inputs
        .packaging_spec
        .as_ref()
        .expect("manifest.inputs.packaging_spec");
    assert_eq!(spec_input.path, "spec.json");
    let contract_input = inputs
        .windows_device_contract
        .as_ref()
        .expect("manifest.inputs.windows_device_contract");
    assert_eq!(contract_input.path, "windows-device-contract.json");
    assert_eq!(
        contract_input.contract_name,
        "aero-windows-pci-device-contract"
    );
    assert!(!contract_input.contract_version.trim().is_empty());
    assert_eq!(contract_input.schema_version, 1);
    assert_eq!(
        inputs
            .aero_packager_version
            .as_deref()
            .expect("manifest.inputs.aero_packager_version"),
        env!("CARGO_PKG_VERSION")
    );

    // Verify input hashes are correct.
    assert_eq!(spec_input.sha256, spec_sha256);

    assert_eq!(contract_input.sha256, contract_sha256);

    // Legacy spec schema should still produce the exact same packaged file list/hashes, even
    // though the overall media differs due to `manifest.inputs.packaging_spec.sha256`.
    assert_eq!(manifest4.schema_version, 4);
    assert_eq!(manifest4.files, manifest.files);
    let legacy_inputs = manifest4.inputs.as_ref().expect("manifest4.inputs");
    let legacy_spec_input = legacy_inputs
        .packaging_spec
        .as_ref()
        .expect("manifest4.inputs.packaging_spec");
    assert_eq!(legacy_spec_input.path, "spec.json");
    assert_ne!(legacy_spec_input.sha256, spec_sha256);
    let legacy_spec_bytes = fs::read(&config4.spec_path)?;
    let legacy_spec_sha256 = canonical_json_sha256_hex(&legacy_spec_bytes)?;
    assert_eq!(legacy_spec_input.sha256, legacy_spec_sha256);

    let legacy_contract_input = legacy_inputs
        .windows_device_contract
        .as_ref()
        .expect("manifest4.inputs.windows_device_contract");
    assert_eq!(legacy_contract_input.sha256, contract_sha256);

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
fn package_outputs_include_optional_tools_dir_and_exclude_build_artifacts() -> anyhow::Result<()> {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");
    let drivers_dir = testdata.join("drivers");
    let spec_path = testdata.join("spec.json");

    let guest_tools_src = testdata.join("guest-tools");
    let guest_tools_tmp = tempfile::tempdir()?;
    copy_dir_all(&guest_tools_src, guest_tools_tmp.path())?;

    // Optional guest-side tools should be packaged under `tools/...` when present.
    fs::create_dir_all(guest_tools_tmp.path().join("tools/x86"))?;
    fs::create_dir_all(guest_tools_tmp.path().join("tools/amd64"))?;
    fs::write(
        guest_tools_tmp.path().join("tools/x86/foo.exe"),
        b"dummy tool (x86)\n",
    )?;
    fs::write(
        guest_tools_tmp.path().join("tools/amd64/foo.exe"),
        b"dummy tool (amd64)\n",
    )?;
    // Common build artifacts should be excluded by default.
    fs::write(
        guest_tools_tmp.path().join("tools/amd64/foo.pdb"),
        b"dummy pdb\n",
    )?;

    let out1 = tempfile::tempdir()?;
    let out2 = tempfile::tempdir()?;

    let config1 = aero_packager::PackageConfig {
        drivers_dir: drivers_dir.clone(),
        guest_tools_dir: guest_tools_tmp.path().to_path_buf(),
        windows_device_contract_path: device_contract_path(),
        out_dir: out1.path().to_path_buf(),
        spec_path: spec_path.clone(),
        version: "1.2.3".to_string(),
        build_id: "test".to_string(),
        volume_id: "AERO_GUEST_TOOLS".to_string(),
        signing_policy: aero_packager::SigningPolicy::Test,
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

    let iso_bytes = fs::read(&outputs1.iso_path)?;
    let tree = aero_packager::read_joliet_tree(&iso_bytes)?;
    for required in ["tools/x86/foo.exe", "tools/amd64/foo.exe"] {
        assert!(
            tree.contains(required),
            "ISO is missing required tools file: {required}"
        );
    }
    assert!(
        !tree.contains("tools/amd64/foo.pdb"),
        "unexpected build artifact packaged: tools/amd64/foo.pdb"
    );

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
    assert!(zip_paths.contains("tools/x86/foo.exe"));
    assert!(zip_paths.contains("tools/amd64/foo.exe"));
    assert!(!zip_paths.contains("tools/amd64/foo.pdb"));

    Ok(())
}

#[test]
fn aero_virtio_spec_packages_expected_drivers() -> anyhow::Result<()> {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");

    // The repo intentionally avoids tracking real driver binaries. For this fixture, inject
    // stub `.sys` files so we can validate spec/packaging behaviour without shipping actual
    // driver payloads in git.
    let drivers_src = testdata.join("drivers-aero-virtio");
    let drivers_tmp = tempfile::tempdir()?;
    copy_dir_all(&drivers_src, drivers_tmp.path())?;
    for arch in ["x86", "amd64"] {
        for drv in ["aero_virtio_blk", "aero_virtio_net"] {
            let dir = drivers_tmp.path().join(arch).join(drv);
            fs::write(dir.join(format!("{drv}.sys")), b"dummy sys\n")?;
        }
    }
    let drivers_dir = drivers_tmp.path().to_path_buf();
    let guest_tools_dir = testdata.join("guest-tools");
    let spec_path = repo_root
        .join("..")
        .join("specs")
        .join("win7-aero-virtio.json");

    let out = tempfile::tempdir()?;
    let config = aero_packager::PackageConfig {
        drivers_dir,
        guest_tools_dir,
        windows_device_contract_path: device_contract_path(),
        out_dir: out.path().to_path_buf(),
        spec_path,
        version: "1.2.3".to_string(),
        build_id: "test".to_string(),
        volume_id: "AERO_GUEST_TOOLS".to_string(),
        signing_policy: aero_packager::SigningPolicy::Test,
        source_date_epoch: 0,
    };

    let outputs = aero_packager::package_guest_tools(&config)?;

    let iso_bytes = fs::read(&outputs.iso_path)?;
    let tree = aero_packager::read_joliet_tree(&iso_bytes)?;
    for required in [
        "drivers/x86/aero_virtio_blk/aero_virtio_blk.inf",
        "drivers/x86/aero_virtio_blk/aero_virtio_blk.sys",
        "drivers/x86/aero_virtio_blk/aero_virtio_blk.cat",
        "drivers/x86/aero_virtio_net/aero_virtio_net.inf",
        "drivers/x86/aero_virtio_net/aero_virtio_net.sys",
        "drivers/x86/aero_virtio_net/aero_virtio_net.cat",
        "drivers/amd64/aero_virtio_blk/aero_virtio_blk.inf",
        "drivers/amd64/aero_virtio_blk/aero_virtio_blk.sys",
        "drivers/amd64/aero_virtio_blk/aero_virtio_blk.cat",
        "drivers/amd64/aero_virtio_net/aero_virtio_net.inf",
        "drivers/amd64/aero_virtio_net/aero_virtio_net.sys",
        "drivers/amd64/aero_virtio_net/aero_virtio_net.cat",
    ] {
        assert!(
            tree.contains(required),
            "ISO is missing required file: {required}"
        );
    }

    Ok(())
}

#[test]
fn aerogpu_only_spec_packages_without_virtio_drivers() -> anyhow::Result<()> {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");

    let guest_tools_dir = testdata.join("guest-tools");
    let spec_path = repo_root
        .join("..")
        .join("specs")
        .join("win7-aerogpu-only.json");

    let drivers_tmp = tempfile::tempdir()?;
    let drivers_dir = drivers_tmp.path();

    // Minimal driver payload: only aerogpu (x86 + amd64).
    for arch in ["x86", "amd64"] {
        write_stub_pci_driver(
            &drivers_dir.join(arch).join("aerogpu"),
            "aerogpu_dx11",
            "aerogpu",
            // Must match the canonical device contract (`AERO_GPU_HWIDS`).
            r"PCI\VEN_A3A0&DEV_0001",
        )?;
    }

    let out = tempfile::tempdir()?;
    let config = aero_packager::PackageConfig {
        drivers_dir: drivers_dir.to_path_buf(),
        guest_tools_dir,
        windows_device_contract_path: device_contract_path(),
        out_dir: out.path().to_path_buf(),
        spec_path,
        version: "0.0.0".to_string(),
        build_id: "test".to_string(),
        volume_id: "AERO_GUEST_TOOLS".to_string(),
        signing_policy: aero_packager::SigningPolicy::Test,
        source_date_epoch: 0,
    };

    let outputs = aero_packager::package_guest_tools(&config)?;
    let iso_bytes = fs::read(&outputs.iso_path)?;
    let tree = aero_packager::read_joliet_tree(&iso_bytes)?;

    for required in [
        "drivers/x86/aerogpu/aerogpu_dx11.inf",
        "drivers/x86/aerogpu/aerogpu_dx11.sys",
        "drivers/x86/aerogpu/aerogpu_dx11.cat",
        "drivers/amd64/aerogpu/aerogpu_dx11.inf",
        "drivers/amd64/aerogpu/aerogpu_dx11.sys",
        "drivers/amd64/aerogpu/aerogpu_dx11.cat",
    ] {
        assert!(
            tree.contains(required),
            "ISO is missing required file: {required}"
        );
    }

    assert!(
        !tree
            .paths
            .iter()
            .any(|p| p.starts_with("drivers/x86/virtio-") || p.starts_with("drivers/amd64/virtio-")),
        "unexpected virtio driver payload present in AeroGPU-only spec output"
    );

    Ok(())
}

#[test]
fn win7_aero_guest_tools_spec_rejects_transitional_virtio_ids_in_infs() -> anyhow::Result<()> {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");

    let guest_tools_dir = testdata.join("guest-tools");
    let spec_path = repo_root
        .join("..")
        .join("specs")
        .join("win7-aero-guest-tools.json");

    let drivers_tmp = tempfile::tempdir()?;
    let drivers_dir = drivers_tmp.path();

    for arch in ["x86", "amd64"] {
        write_stub_pci_driver(
            &drivers_dir.join(arch).join("aerogpu"),
            "aerogpu_dx11",
            "aerogpu",
            r"PCI\VEN_A3A0&DEV_0001",
        )?;
        write_stub_pci_driver(
            &drivers_dir.join(arch).join("virtio-blk"),
            "aero_virtio_blk",
            "aero_virtio_blk",
            // Modern-only virtio-blk ID (AERO-W7-VIRTIO v1).
            r"PCI\VEN_1AF4&DEV_1042&REV_01",
        )?;
        write_stub_pci_driver(
            &drivers_dir.join(arch).join("virtio-net"),
            "aero_virtio_net",
            "aero_virtio_net",
            r"PCI\VEN_1AF4&DEV_1041&REV_01",
        )?;
        write_stub_pci_driver(
            &drivers_dir.join(arch).join("virtio-input"),
            "aero_virtio_input",
            "aero_virtio_input",
            r"PCI\VEN_1AF4&DEV_1052&REV_01",
        )?;
    }

    let out_ok = tempfile::tempdir()?;
    let config_ok = aero_packager::PackageConfig {
        drivers_dir: drivers_dir.to_path_buf(),
        guest_tools_dir: guest_tools_dir.clone(),
        windows_device_contract_path: device_contract_path(),
        out_dir: out_ok.path().to_path_buf(),
        spec_path: spec_path.clone(),
        version: "0.0.0".to_string(),
        build_id: "test".to_string(),
        volume_id: "AERO_GUEST_TOOLS".to_string(),
        signing_policy: aero_packager::SigningPolicy::Test,
        source_date_epoch: 0,
    };

    // Sanity: should package successfully when all driver INFs contain the modern virtio IDs.
    aero_packager::package_guest_tools(&config_ok)?;

    // Now regress virtio-blk to transitional IDs only and ensure packaging fails when using the
    // default Aero Guest Tools spec (`win7-aero-guest-tools.json`), which is contract-v1 strict.
    let drivers_bad_tmp = tempfile::tempdir()?;
    let drivers_bad_dir = drivers_bad_tmp.path();
    for arch in ["x86", "amd64"] {
        write_stub_pci_driver(
            &drivers_bad_dir.join(arch).join("aerogpu"),
            "aerogpu_dx11",
            "aerogpu",
            r"PCI\VEN_A3A0&DEV_0001",
        )?;
        write_stub_pci_driver(
            &drivers_bad_dir.join(arch).join("virtio-blk"),
            "aero_virtio_blk",
            "aero_virtio_blk",
            // Transitional virtio-blk ID (must not satisfy the spec).
            r"PCI\VEN_1AF4&DEV_1001&REV_01",
        )?;
        write_stub_pci_driver(
            &drivers_bad_dir.join(arch).join("virtio-net"),
            "aero_virtio_net",
            "aero_virtio_net",
            r"PCI\VEN_1AF4&DEV_1041&REV_01",
        )?;
        write_stub_pci_driver(
            &drivers_bad_dir.join(arch).join("virtio-input"),
            "aero_virtio_input",
            "aero_virtio_input",
            r"PCI\VEN_1AF4&DEV_1052&REV_01",
        )?;
    }

    let out_bad = tempfile::tempdir()?;
    let config_bad = aero_packager::PackageConfig {
        drivers_dir: drivers_bad_dir.to_path_buf(),
        out_dir: out_bad.path().to_path_buf(),
        ..config_ok
    };

    let err = aero_packager::package_guest_tools(&config_bad).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("virtio-blk") && msg.contains("DEV_1042"),
        "unexpected error: {msg}"
    );

    Ok(())
}

#[test]
fn drivers_and_required_drivers_merge_case_insensitively() -> anyhow::Result<()> {
    let spec_dir = tempfile::tempdir()?;
    let spec_path = spec_dir.path().join("spec.json");
    let spec = serde_json::json!({
        "drivers": [
            {
                "name": "testdrv",
                "required": false,
                "expected_hardware_ids": [],
            }
        ],
        "required_drivers": [
            {
                "name": "TESTDRV",
                "expected_hardware_ids": [r"PCI\\VEN_1234&DEV_5678"],
            }
        ]
    });
    fs::write(&spec_path, serde_json::to_vec_pretty(&spec)?)?;

    let loaded = aero_packager::PackagingSpec::load(&spec_path)?;
    assert_eq!(loaded.drivers.len(), 1);
    assert_eq!(loaded.drivers[0].name, "testdrv");
    assert!(loaded.drivers[0].required);
    assert_eq!(
        loaded.drivers[0].expected_hardware_ids,
        vec![r"PCI\\VEN_1234&DEV_5678"]
    );

    Ok(())
}

#[test]
fn aerogpu_driver_name_alias_is_normalized() -> anyhow::Result<()> {
    let spec_dir = tempfile::tempdir()?;
    let spec_path = spec_dir.path().join("spec.json");
    let spec = serde_json::json!({
        "drivers": [
            {
                "name": "aero-gpu",
                "required": true,
                "expected_hardware_ids": [],
            }
        ]
    });
    fs::write(&spec_path, serde_json::to_vec_pretty(&spec)?)?;

    let loaded = aero_packager::PackagingSpec::load(&spec_path)?;
    assert_eq!(loaded.drivers.len(), 1);
    assert_eq!(loaded.drivers[0].name, "aerogpu");

    Ok(())
}

#[test]
fn aerogpu_driver_directory_alias_is_accepted() -> anyhow::Result<()> {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");
    let guest_tools_dir = testdata.join("guest-tools");

    let drivers_tmp = tempfile::tempdir()?;
    for arch in ["x86", "amd64"] {
        let driver_dir = drivers_tmp.path().join(arch).join("aero-gpu");
        fs::create_dir_all(&driver_dir)?;
        fs::write(driver_dir.join("aerogpu.inf"), b"PCI\\VEN_A3A0&DEV_0001\n")?;
        fs::write(driver_dir.join("aerogpu.sys"), b"sys\n")?;
        fs::write(driver_dir.join("aerogpu.cat"), b"cat\n")?;
    }

    let spec_dir = tempfile::tempdir()?;
    let spec_path = spec_dir.path().join("spec.json");
    let spec = serde_json::json!({
        "drivers": [
            {
                "name": "aerogpu",
                "required": true,
                "expected_hardware_ids": [r"PCI\\VEN_A3A0&DEV_0001"],
            }
        ]
    });
    fs::write(&spec_path, serde_json::to_vec_pretty(&spec)?)?;

    let out = tempfile::tempdir()?;
    let config = aero_packager::PackageConfig {
        drivers_dir: drivers_tmp.path().to_path_buf(),
        guest_tools_dir: guest_tools_dir.clone(),
        windows_device_contract_path: device_contract_path(),
        out_dir: out.path().to_path_buf(),
        spec_path,
        version: "0.0.0".to_string(),
        build_id: "test".to_string(),
        volume_id: "AERO_GUEST_TOOLS".to_string(),
        signing_policy: aero_packager::SigningPolicy::Test,
        source_date_epoch: 0,
    };

    let outputs = aero_packager::package_guest_tools(&config)?;
    let iso_bytes = fs::read(&outputs.iso_path)?;
    let tree = aero_packager::read_joliet_tree(&iso_bytes)?;

    // Even when the input directory uses the legacy dashed name, we should emit the canonical
    // `drivers/<arch>/aerogpu/` path in the packaged output.
    assert!(tree.contains("drivers/x86/aerogpu/aerogpu.inf"));
    assert!(tree.contains("drivers/amd64/aerogpu/aerogpu.inf"));

    Ok(())
}

#[test]
fn aerogpu_driver_directory_alias_conflict_is_rejected() -> anyhow::Result<()> {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");
    let guest_tools_dir = testdata.join("guest-tools");

    let drivers_tmp = tempfile::tempdir()?;
    for arch in ["x86", "amd64"] {
        fs::create_dir_all(drivers_tmp.path().join(arch).join("aerogpu"))?;
        fs::create_dir_all(drivers_tmp.path().join(arch).join("aero-gpu"))?;
    }

    let spec_dir = tempfile::tempdir()?;
    let spec_path = spec_dir.path().join("spec.json");
    let spec = serde_json::json!({
        "drivers": [
            {
                "name": "aerogpu",
                "required": true,
                "expected_hardware_ids": [r"PCI\\VEN_A3A0&DEV_0001"],
            }
        ]
    });
    fs::write(&spec_path, serde_json::to_vec_pretty(&spec)?)?;

    let out = tempfile::tempdir()?;
    let config = aero_packager::PackageConfig {
        drivers_dir: drivers_tmp.path().to_path_buf(),
        guest_tools_dir: guest_tools_dir.clone(),
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
        msg.contains("multiple driver directories found for aerogpu"),
        "unexpected error: {msg}"
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
        windows_device_contract_path: device_contract_path(),
        out_dir: out.path().to_path_buf(),
        spec_path,
        version: "0.0.0".to_string(),
        build_id: "test".to_string(),
        volume_id: "AERO_GUEST_TOOLS".to_string(),
        signing_policy: aero_packager::SigningPolicy::Test,
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
fn signing_policy_controls_certificate_requirements() -> anyhow::Result<()> {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");
    let drivers_dir = testdata.join("drivers");
    let guest_tools_dir = testdata.join("guest-tools");
    let spec_path = testdata.join("spec.json");

    let guest_tools_tmp = tempfile::tempdir()?;
    copy_dir_all(&guest_tools_dir, guest_tools_tmp.path())?;

    // Remove certificate artifacts while keeping certs/README.md.
    let certs_dir = guest_tools_tmp.path().join("certs");
    for entry in fs::read_dir(&certs_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let path = entry.path();
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if matches!(ext.as_str(), "cer" | "crt" | "p7b") {
            fs::remove_file(path)?;
        }
    }

    let out_tmp = tempfile::tempdir()?;
    let base = aero_packager::PackageConfig {
        drivers_dir,
        guest_tools_dir: guest_tools_tmp.path().to_path_buf(),
        windows_device_contract_path: device_contract_path(),
        out_dir: out_tmp.path().to_path_buf(),
        spec_path,
        version: "0.0.0".to_string(),
        build_id: "test".to_string(),
        volume_id: "AERO_GUEST_TOOLS".to_string(),
        signing_policy: aero_packager::SigningPolicy::Test,
        source_date_epoch: 0,
    };

    // test: requires at least one cert file.
    let err = aero_packager::package_guest_tools(&base).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("contains no certificate files"),
        "unexpected error: {msg}"
    );

    // production/none: allow empty certs dir.
    for policy in [
        aero_packager::SigningPolicy::Production,
        aero_packager::SigningPolicy::None,
    ] {
        let out_dir = tempfile::tempdir()?;
        let config = aero_packager::PackageConfig {
            out_dir: out_dir.path().to_path_buf(),
            signing_policy: policy,
            ..base.clone()
        };

        let outputs = aero_packager::package_guest_tools(&config)?;

        let manifest_bytes = fs::read(&outputs.manifest_path)?;
        let manifest: aero_packager::Manifest = serde_json::from_slice(&manifest_bytes)?;
        assert_eq!(manifest.signing_policy, policy);
        assert!(!manifest.certs_required);

        let iso_bytes = fs::read(&outputs.iso_path)?;
        let tree = aero_packager::read_joliet_tree(&iso_bytes)?;
        assert!(
            tree.contains("certs/README.md"),
            "expected certs/README.md to be packaged"
        );
        assert!(
            !tree.contains("certs/test.cer"),
            "unexpected certificate file packaged under production/none policy"
        );
    }

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
            b"[Version]\nPCI\\VEN_BEEF&DEV_CAFE\n",
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

    for (rel_path, ext) in [
        ("x86/testdrv/test.pfx", "pfx"),
        ("x86/testdrv/secret.p12", "p12"),
        // Hidden file should still be rejected (even though it would normally be excluded).
        ("x86/testdrv/.hidden.key", "key"),
        // Hidden directory should still be scanned for key material.
        ("x86/testdrv/.secrets/secret.pem", "pem"),
    ] {
        let drivers_tmp = tempfile::tempdir()?;
        copy_dir_all(&drivers_dir, drivers_tmp.path())?;
        let dst = drivers_tmp.path().join(rel_path);
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&dst, b"dummy secret")?;

        let out_dir = tempfile::tempdir()?;
        let config = aero_packager::PackageConfig {
            drivers_dir: drivers_tmp.path().to_path_buf(),
            guest_tools_dir: guest_tools_dir.clone(),
            windows_device_contract_path: device_contract_path(),
            out_dir: out_dir.path().to_path_buf(),
            spec_path: spec_path.clone(),
            version: "0.0.0".to_string(),
            build_id: "test".to_string(),
            volume_id: "AERO_GUEST_TOOLS".to_string(),
            signing_policy: aero_packager::SigningPolicy::Test,
            source_date_epoch: 0,
        };

        let err = aero_packager::package_guest_tools(&config).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("refusing to package private key material"),
            "unexpected error: {msg}"
        );
        let file_name = std::path::Path::new(rel_path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(rel_path);
        assert!(
            msg.contains(file_name),
            "expected offending path in error for .{ext}: {msg}"
        );
    }
    Ok(())
}

#[test]
fn package_rejects_private_key_materials_in_licenses_dir() -> anyhow::Result<()> {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");
    let spec_path = testdata.join("spec.json");

    let drivers_dir = testdata.join("drivers");
    let guest_tools_dir = testdata.join("guest-tools");

    let guest_tools_tmp = tempfile::tempdir()?;
    copy_dir_all(&guest_tools_dir, guest_tools_tmp.path())?;
    let licenses_dir = guest_tools_tmp.path().join("licenses").join("virtio-win");
    fs::create_dir_all(&licenses_dir)?;
    fs::write(licenses_dir.join("secret.pfx"), b"dummy pfx")?;

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
        msg.contains("private key material in licenses directory"),
        "unexpected error: {msg}"
    );
    Ok(())
}

#[test]
fn package_rejects_private_key_materials_in_config_dir() -> anyhow::Result<()> {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");
    let spec_path = testdata.join("spec.json");

    let drivers_dir = testdata.join("drivers");
    let guest_tools_dir = testdata.join("guest-tools");

    let guest_tools_tmp = tempfile::tempdir()?;
    copy_dir_all(&guest_tools_dir, guest_tools_tmp.path())?;
    fs::write(
        guest_tools_tmp.path().join("config").join("secret.pfx"),
        b"dummy pfx",
    )?;

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
        msg.contains("private key material in config directory"),
        "unexpected error: {msg}"
    );
    Ok(())
}

#[test]
fn package_rejects_private_key_materials_in_certs_dir() -> anyhow::Result<()> {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");
    let spec_path = testdata.join("spec.json");

    let drivers_dir = testdata.join("drivers");
    let guest_tools_dir = testdata.join("guest-tools");

    let guest_tools_tmp = tempfile::tempdir()?;
    copy_dir_all(&guest_tools_dir, guest_tools_tmp.path())?;
    fs::write(
        guest_tools_tmp.path().join("certs").join("secret.pfx"),
        b"dummy pfx",
    )?;

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
        msg.contains("private key material in certs directory"),
        "unexpected error: {msg}"
    );
    Ok(())
}

#[test]
fn package_rejects_private_key_materials_in_tools_dir() -> anyhow::Result<()> {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");
    let spec_path = testdata.join("spec.json");

    let drivers_dir = testdata.join("drivers");
    let guest_tools_dir = testdata.join("guest-tools");

    let guest_tools_tmp = tempfile::tempdir()?;
    copy_dir_all(&guest_tools_dir, guest_tools_tmp.path())?;
    fs::create_dir_all(guest_tools_tmp.path().join("tools"))?;
    fs::write(
        guest_tools_tmp.path().join("tools").join("secret.pfx"),
        b"dummy pfx",
    )?;

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
        msg.contains("refusing to package private key material") && msg.contains("secret.pfx"),
        "unexpected error: {msg}"
    );
    Ok(())
}

#[test]
fn debug_symbols_are_excluded_from_packaged_driver_dirs() -> anyhow::Result<()> {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");
    let spec_path = testdata.join("spec.json");
    let drivers_dir = testdata.join("drivers");
    let guest_tools_dir = testdata.join("guest-tools");

    // Baseline package.
    let out_base = tempfile::tempdir()?;
    let config_base = aero_packager::PackageConfig {
        drivers_dir: drivers_dir.clone(),
        guest_tools_dir: guest_tools_dir.clone(),
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

    // Add dummy build artifacts to the driver directory and ensure they are not packaged.
    let drivers_tmp = tempfile::tempdir()?;
    copy_dir_all(&drivers_dir, drivers_tmp.path())?;
    for arch in ["x86", "amd64"] {
        for (name, contents) in [
            ("test.pdb", b"dummy pdb".as_slice()),
            ("test.exp", b"dummy exp".as_slice()),
            ("test.ilk", b"dummy ilk".as_slice()),
            ("test.tlog", b"dummy tlog".as_slice()),
            ("test.log", b"dummy log".as_slice()),
        ] {
            fs::write(
                drivers_tmp.path().join(format!("{arch}/testdrv/{name}")),
                contents,
            )?;
        }
    }

    let out_pdb = tempfile::tempdir()?;
    let config_pdb = aero_packager::PackageConfig {
        drivers_dir: drivers_tmp.path().to_path_buf(),
        out_dir: out_pdb.path().to_path_buf(),
        ..config_base
    };
    let outputs_pdb = aero_packager::package_guest_tools(&config_pdb)?;
    let iso_pdb = fs::read(&outputs_pdb.iso_path)?;
    let tree = aero_packager::read_joliet_tree(&iso_pdb)?;

    for unexpected in [
        "drivers/x86/testdrv/test.pdb",
        "drivers/amd64/testdrv/test.pdb",
        "drivers/x86/testdrv/test.exp",
        "drivers/amd64/testdrv/test.exp",
        "drivers/x86/testdrv/test.ilk",
        "drivers/amd64/testdrv/test.ilk",
        "drivers/x86/testdrv/test.tlog",
        "drivers/amd64/testdrv/test.tlog",
        "drivers/x86/testdrv/test.log",
        "drivers/amd64/testdrv/test.log",
    ] {
        assert!(
            !tree.contains(unexpected),
            "unexpected file packaged: {unexpected}"
        );
    }

    // Excluded debug symbols should not affect deterministic outputs.
    assert_eq!(iso_base, iso_pdb);
    assert_eq!(zip_base, fs::read(&outputs_pdb.zip_path)?);
    assert_eq!(manifest_base, fs::read(&outputs_pdb.manifest_path)?);

    Ok(())
}

#[test]
fn driver_dll_extensions_are_handled_case_insensitively() -> anyhow::Result<()> {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");
    let spec_path = testdata.join("spec.json");
    let guest_tools_dir = testdata.join("guest-tools");

    // Copy drivers, then rename `test.dll` to an uppercase extension. The packager should
    // still include it and the INF reference validation should still pass.
    let drivers_tmp = tempfile::tempdir()?;
    copy_dir_all(&testdata.join("drivers"), drivers_tmp.path())?;
    for arch in ["x86", "amd64"] {
        fs::rename(
            drivers_tmp.path().join(format!("{arch}/testdrv/test.dll")),
            drivers_tmp.path().join(format!("{arch}/testdrv/test.DLL")),
        )?;
    }

    let out = tempfile::tempdir()?;
    let config = aero_packager::PackageConfig {
        drivers_dir: drivers_tmp.path().to_path_buf(),
        guest_tools_dir: guest_tools_dir.clone(),
        windows_device_contract_path: device_contract_path(),
        out_dir: out.path().to_path_buf(),
        spec_path,
        version: "0.0.0".to_string(),
        build_id: "test".to_string(),
        volume_id: "AERO_GUEST_TOOLS".to_string(),
        signing_policy: aero_packager::SigningPolicy::Test,
        source_date_epoch: 0,
    };
    let outputs = aero_packager::package_guest_tools(&config)?;
    let iso_bytes = fs::read(&outputs.iso_path)?;
    let tree = aero_packager::read_joliet_tree(&iso_bytes)?;

    assert!(tree.contains("drivers/x86/testdrv/test.DLL"));
    assert!(tree.contains("drivers/amd64/testdrv/test.DLL"));
    assert!(!tree.contains("drivers/x86/testdrv/test.dll"));
    assert!(!tree.contains("drivers/amd64/testdrv/test.dll"));

    Ok(())
}

#[test]
fn copyinf_directives_are_validated() -> anyhow::Result<()> {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");
    let spec_path = testdata.join("spec.json");
    let guest_tools_dir = testdata.join("guest-tools");

    // Copy drivers, then mutate the test INF to reference a missing INF via CopyINF.
    let drivers_tmp = tempfile::tempdir()?;
    copy_dir_all(&testdata.join("drivers"), drivers_tmp.path())?;

    let inf_path = drivers_tmp.path().join("x86/testdrv/test.inf");
    let original = fs::read_to_string(&inf_path)?;
    let mut out_lines = Vec::new();
    for line in original.lines() {
        out_lines.push(line.to_string());
        if line
            .trim()
            .eq_ignore_ascii_case("CopyFiles=DriverCopyFiles,CoInstaller_CopyFiles")
        {
            out_lines.push("CopyINF=missing.inf".to_string());
        }
    }
    fs::write(inf_path, out_lines.join("\n") + "\n")?;

    let out = tempfile::tempdir()?;
    let config = aero_packager::PackageConfig {
        drivers_dir: drivers_tmp.path().to_path_buf(),
        guest_tools_dir: guest_tools_dir.clone(),
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
        msg.contains("missing.inf") && msg.contains("INF referenced files are missing"),
        "unexpected error: {msg}"
    );

    Ok(())
}

#[test]
fn utf8_bom_infs_are_parsed_for_reference_validation() -> anyhow::Result<()> {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");
    let spec_path = testdata.join("spec.json");
    let guest_tools_dir = testdata.join("guest-tools");

    let drivers_tmp = tempfile::tempdir()?;
    copy_dir_all(&testdata.join("drivers"), drivers_tmp.path())?;

    // Rewrite the INF so it begins with a UTF-8 BOM immediately before the first section
    // header. Without BOM stripping, section parsing would fail and referenced-file validation
    // would miss missing payloads.
    let inf_path = drivers_tmp.path().join("x86/testdrv/test.inf");
    let inf_text = fs::read_to_string(&inf_path)?;
    let body_start = inf_text
        .find('[')
        .expect("fixture INF should contain a section header");
    let body = &inf_text[body_start..];
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
    bytes.extend_from_slice(body.as_bytes());
    fs::write(&inf_path, bytes)?;

    // Remove a referenced payload to ensure the parser actually runs.
    fs::remove_file(drivers_tmp.path().join("x86/testdrv/test.dll"))?;

    let out = tempfile::tempdir()?;
    let config = aero_packager::PackageConfig {
        drivers_dir: drivers_tmp.path().to_path_buf(),
        guest_tools_dir: guest_tools_dir.clone(),
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
        msg.contains("test.dll") && msg.contains("INF referenced files are missing"),
        "unexpected error: {msg}"
    );

    Ok(())
}

#[test]
fn utf16le_bom_infs_are_parsed_for_reference_validation() -> anyhow::Result<()> {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");
    let spec_path = testdata.join("spec.json");
    let guest_tools_dir = testdata.join("guest-tools");

    let drivers_tmp = tempfile::tempdir()?;
    copy_dir_all(&testdata.join("drivers"), drivers_tmp.path())?;

    // Rewrite the INF as UTF-16LE with a BOM. Many real-world driver packages ship
    // UTF-16LE INFs and we still want referenced-file validation to work.
    let inf_path = drivers_tmp.path().join("x86/testdrv/test.inf");
    let inf_text = fs::read_to_string(&inf_path)?;

    let mut bytes = Vec::new();
    bytes.extend_from_slice(&[0xFF, 0xFE]);
    for unit in inf_text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    fs::write(&inf_path, bytes)?;

    // Remove a referenced payload to ensure the parser actually runs.
    fs::remove_file(drivers_tmp.path().join("x86/testdrv/test.dll"))?;

    let out = tempfile::tempdir()?;
    let config = aero_packager::PackageConfig {
        drivers_dir: drivers_tmp.path().to_path_buf(),
        guest_tools_dir: guest_tools_dir.clone(),
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
        msg.contains("test.dll") && msg.contains("INF referenced files are missing"),
        "unexpected error: {msg}"
    );

    Ok(())
}

#[test]
fn utf16le_no_bom_infs_are_accepted() -> anyhow::Result<()> {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");
    let spec_path = testdata.join("spec.json");
    let guest_tools_dir = testdata.join("guest-tools");

    let drivers_tmp = tempfile::tempdir()?;
    copy_dir_all(&testdata.join("drivers"), drivers_tmp.path())?;

    // Rewrite the INF as UTF-16LE *without* a BOM. Some real-world driver packages ship
    // BOM-less UTF-16 INFs and we still want HWID validation to work.
    for arch in ["x86", "amd64"] {
        let inf_path = drivers_tmp.path().join(format!("{arch}/testdrv/test.inf"));
        let inf_text = fs::read_to_string(&inf_path)?;

        let mut bytes = Vec::new();
        for unit in inf_text.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        fs::write(&inf_path, bytes)?;
    }

    let out = tempfile::tempdir()?;
    let config = aero_packager::PackageConfig {
        drivers_dir: drivers_tmp.path().to_path_buf(),
        guest_tools_dir: guest_tools_dir.clone(),
        windows_device_contract_path: device_contract_path(),
        out_dir: out.path().to_path_buf(),
        spec_path,
        version: "0.0.0".to_string(),
        build_id: "test".to_string(),
        volume_id: "AERO_GUEST_TOOLS".to_string(),
        signing_policy: aero_packager::SigningPolicy::Test,
        source_date_epoch: 0,
    };

    // Should succeed; HWID validation must still work.
    aero_packager::package_guest_tools(&config)?;

    Ok(())
}

#[test]
fn catalogfile_directives_are_validated() -> anyhow::Result<()> {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");
    let spec_path = testdata.join("spec.json");
    let guest_tools_dir = testdata.join("guest-tools");

    let drivers_tmp = tempfile::tempdir()?;
    copy_dir_all(&testdata.join("drivers"), drivers_tmp.path())?;

    // Mutate each INF to reference a missing catalog file. Ensure we fail even if another `.cat`
    // exists in the driver directory.
    for (arch, suffix) in [("x86", "NTx86"), ("amd64", "NTamd64")] {
        let inf_path = drivers_tmp.path().join(format!("{arch}/testdrv/test.inf"));
        let original = fs::read_to_string(&inf_path)?;
        let mut out_lines = Vec::new();
        for line in original.lines() {
            out_lines.push(line.to_string());
            if line.trim().eq_ignore_ascii_case("Signature=\"$Windows NT$\"") {
                out_lines.push(format!("CatalogFile.{suffix}=missing.cat"));
            }
        }
        fs::write(inf_path, out_lines.join("\n") + "\n")?;
    }

    let out = tempfile::tempdir()?;
    let config = aero_packager::PackageConfig {
        drivers_dir: drivers_tmp.path().to_path_buf(),
        guest_tools_dir: guest_tools_dir.clone(),
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
        msg.contains("missing.cat") && msg.contains("INF referenced files are missing"),
        "unexpected error: {msg}"
    );

    Ok(())
}

#[test]
fn servicebinary_directives_are_validated() -> anyhow::Result<()> {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");
    let spec_path = testdata.join("spec.json");
    let guest_tools_dir = testdata.join("guest-tools");

    let drivers_tmp = tempfile::tempdir()?;
    copy_dir_all(&testdata.join("drivers"), drivers_tmp.path())?;

    // Add a minimal service install section referencing a missing `*.sys` via `ServiceBinary`.
    for arch in ["x86", "amd64"] {
        let inf_path = drivers_tmp.path().join(format!("{arch}/testdrv/test.inf"));
        let mut original = fs::read_to_string(&inf_path)?;
        original.push_str(
            concat!(
                "\n",
                "[Install.Services]\n",
                // Add an inline comment to ensure comment stripping works.
                "AddService=TestSvc,0x00000002,TestSvc_Inst ; test comment\n",
                "\n",
                "[TestSvc_Inst]\n",
                "ServiceBinary=\\SystemRoot\\system32\\drivers\\missing.sys\n",
            ),
        );
        fs::write(inf_path, original)?;
    }

    let out = tempfile::tempdir()?;
    let config = aero_packager::PackageConfig {
        drivers_dir: drivers_tmp.path().to_path_buf(),
        guest_tools_dir: guest_tools_dir.clone(),
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
        msg.contains("missing.sys") && msg.contains("INF referenced files are missing"),
        "unexpected error: {msg}"
    );

    Ok(())
}

#[test]
fn wdfcoinstaller_mentioned_only_in_comment_does_not_require_payload() -> anyhow::Result<()> {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");
    let spec_path = testdata.join("spec.json");
    let guest_tools_dir = testdata.join("guest-tools");

    let drivers_tmp = tempfile::tempdir()?;
    copy_dir_all(&testdata.join("drivers"), drivers_tmp.path())?;

    // Remove explicit references to the KMDF coinstaller DLL from the INF, but add a comment
    // mentioning WdfCoInstaller. The packager should ignore comment-only mentions and not
    // require a WdfCoInstaller*.dll payload.
    for arch in ["x86", "amd64"] {
        let inf_path = drivers_tmp.path().join(format!("{arch}/testdrv/test.inf"));
        let original = fs::read_to_string(&inf_path)?;
        let mut lines = Vec::new();
        for line in original.lines() {
            if line.contains("WdfCoInstaller01009.dll") {
                continue;
            }
            lines.push(line.to_string());
        }
        lines.insert(
            1,
            "; WdfCoInstaller is not required on this platform".to_string(),
        );
        fs::write(inf_path, lines.join("\n") + "\n")?;

        fs::remove_file(
            drivers_tmp
                .path()
                .join(format!("{arch}/testdrv/WdfCoInstaller01009.dll")),
        )?;
    }

    let out = tempfile::tempdir()?;
    let config = aero_packager::PackageConfig {
        drivers_dir: drivers_tmp.path().to_path_buf(),
        guest_tools_dir: guest_tools_dir.clone(),
        windows_device_contract_path: device_contract_path(),
        out_dir: out.path().to_path_buf(),
        spec_path,
        version: "0.0.0".to_string(),
        build_id: "test".to_string(),
        volume_id: "AERO_GUEST_TOOLS".to_string(),
        signing_policy: aero_packager::SigningPolicy::Test,
        source_date_epoch: 0,
    };

    let outputs = aero_packager::package_guest_tools(&config)?;
    let iso_bytes = fs::read(&outputs.iso_path)?;
    let tree = aero_packager::read_joliet_tree(&iso_bytes)?;
    assert!(!tree.contains("drivers/x86/testdrv/WdfCoInstaller01009.dll"));
    assert!(!tree.contains("drivers/amd64/testdrv/WdfCoInstaller01009.dll"));

    Ok(())
}

#[test]
fn expected_hardware_id_patterns_in_comments_do_not_satisfy_spec() -> anyhow::Result<()> {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");
    let guest_tools_dir = testdata.join("guest-tools");

    let drivers_tmp = tempfile::tempdir()?;
    for arch in ["x86", "amd64"] {
        let driver_dir = drivers_tmp.path().join(arch).join("commenthwid");
        fs::create_dir_all(&driver_dir)?;
        fs::write(
            driver_dir.join("commenthwid.inf"),
            concat!(
                "; PCI\\VEN_1234&DEV_5678\n",
                "[Version]\n",
                "Signature=\"$Windows NT$\"\n",
                "\n",
                "[Manufacturer]\n",
                "%Mfg%=Mfg,NTx86,NTamd64\n",
                "\n",
                "[Mfg.NTx86]\n",
                "%Dev%=Install, PCI\\VEN_1234&DEV_9999\n",
                "\n",
                "[Mfg.NTamd64]\n",
                "%Dev%=Install, PCI\\VEN_1234&DEV_9999\n",
                "\n",
                "[Strings]\n",
                "Mfg=\"Aero\"\n",
                "Dev=\"Test\"\n",
            ),
        )?;
        fs::write(driver_dir.join("commenthwid.sys"), b"dummy sys\n")?;
        fs::write(driver_dir.join("commenthwid.cat"), b"dummy cat\n")?;
    }

    let spec_dir = tempfile::tempdir()?;
    let spec_path = spec_dir.path().join("spec.json");
    let spec = serde_json::json!({
        "drivers": [
            {
                "name": "commenthwid",
                "required": true,
                "expected_hardware_ids": [r"PCI\\VEN_1234&DEV_5678"],
            }
        ]
    });
    fs::write(&spec_path, serde_json::to_vec_pretty(&spec)?)?;

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
    assert!(
        msg.contains("missing expected hardware ID pattern")
            && msg.contains("PCI\\\\VEN_1234&DEV_5678"),
        "unexpected error: {msg}"
    );

    Ok(())
}

#[test]
fn copyfiles_section_names_with_dots_are_treated_as_sections() -> anyhow::Result<()> {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");
    let spec_path = testdata.join("spec.json");
    let guest_tools_dir = testdata.join("guest-tools");

    let drivers_tmp = tempfile::tempdir()?;
    copy_dir_all(&testdata.join("drivers"), drivers_tmp.path())?;

    // Real-world INFs often use CopyFiles section names with `.NT*` suffixes. Ensure we don't
    // misinterpret those as file references.
    for arch in ["x86", "amd64"] {
        let inf_path = drivers_tmp.path().join(format!("{arch}/testdrv/test.inf"));
        let original = fs::read_to_string(&inf_path)?;
        let mut out = Vec::new();
        for line in original.lines() {
            if line
                .trim()
                .eq_ignore_ascii_case("CopyFiles=DriverCopyFiles,CoInstaller_CopyFiles")
            {
                out.push("CopyFiles=DriverCopyFiles.NT,CoInstaller_CopyFiles".to_string());
                continue;
            }
            if line.trim().eq_ignore_ascii_case("[DriverCopyFiles]") {
                out.push("[DriverCopyFiles.NT]".to_string());
                continue;
            }
            out.push(line.to_string());
        }
        fs::write(inf_path, out.join("\n") + "\n")?;
    }

    let out_dir = tempfile::tempdir()?;
    let config = aero_packager::PackageConfig {
        drivers_dir: drivers_tmp.path().to_path_buf(),
        guest_tools_dir: guest_tools_dir.clone(),
        windows_device_contract_path: device_contract_path(),
        out_dir: out_dir.path().to_path_buf(),
        spec_path,
        version: "0.0.0".to_string(),
        build_id: "test".to_string(),
        volume_id: "AERO_GUEST_TOOLS".to_string(),
        signing_policy: aero_packager::SigningPolicy::Test,
        source_date_epoch: 0,
    };

    // Should succeed; the `.NT` section name must not be treated as a missing file.
    aero_packager::package_guest_tools(&config)?;

    Ok(())
}

#[test]
fn windows_shell_metadata_files_are_excluded_from_driver_dirs() -> anyhow::Result<()> {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");
    let spec_path = testdata.join("spec.json");
    let drivers_dir = testdata.join("drivers");
    let guest_tools_dir = testdata.join("guest-tools");

    let out_base = tempfile::tempdir()?;
    let config_base = aero_packager::PackageConfig {
        drivers_dir: drivers_dir.clone(),
        guest_tools_dir: guest_tools_dir.clone(),
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

    let drivers_tmp = tempfile::tempdir()?;
    copy_dir_all(&drivers_dir, drivers_tmp.path())?;
    for arch in ["x86", "amd64"] {
        fs::write(
            drivers_tmp.path().join(format!("{arch}/testdrv/Thumbs.db")),
            b"dummy thumbs",
        )?;
        fs::write(
            drivers_tmp
                .path()
                .join(format!("{arch}/testdrv/desktop.ini")),
            b"dummy ini",
        )?;
    }

    let out = tempfile::tempdir()?;
    let config = aero_packager::PackageConfig {
        drivers_dir: drivers_tmp.path().to_path_buf(),
        out_dir: out.path().to_path_buf(),
        ..config_base
    };
    let outputs = aero_packager::package_guest_tools(&config)?;
    let iso_bytes = fs::read(&outputs.iso_path)?;
    let tree = aero_packager::read_joliet_tree(&iso_bytes)?;

    for unexpected in [
        "drivers/x86/testdrv/Thumbs.db",
        "drivers/amd64/testdrv/Thumbs.db",
        "drivers/x86/testdrv/desktop.ini",
        "drivers/amd64/testdrv/desktop.ini",
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

#[test]
fn os_metadata_files_are_excluded_from_config_and_licenses_dirs() -> anyhow::Result<()> {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");
    let spec_path = testdata.join("spec.json");
    let drivers_dir = testdata.join("drivers");
    let guest_tools_src = testdata.join("guest-tools");

    let out_base = tempfile::tempdir()?;
    let config_base = aero_packager::PackageConfig {
        drivers_dir: drivers_dir.clone(),
        guest_tools_dir: guest_tools_src.clone(),
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
    copy_dir_all(&guest_tools_src, guest_tools_tmp.path())?;
    fs::write(
        guest_tools_tmp.path().join("config/Thumbs.db"),
        b"dummy thumbs",
    )?;
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

    let zip_file = fs::File::open(&outputs.zip_path)?;
    let mut zip = zip::ZipArchive::new(zip_file)?;
    let mut zip_paths = BTreeSet::new();
    for i in 0..zip.len() {
        let entry = zip.by_index(i)?;
        if entry.is_dir() {
            continue;
        }
        zip_paths.insert(entry.name().to_string());
    }

    for unexpected in ["config/Thumbs.db", "licenses/__MACOSX/junk.txt"] {
        assert!(
            !tree.contains(unexpected),
            "unexpected file packaged in ISO: {unexpected}"
        );
        assert!(
            !zip_paths.contains(unexpected),
            "unexpected file packaged in zip: {unexpected}"
        );
    }

    // Excluding metadata files should keep outputs stable.
    assert_eq!(iso_base, iso_bytes);
    assert_eq!(zip_base, fs::read(&outputs.zip_path)?);
    assert_eq!(manifest_base, fs::read(&outputs.manifest_path)?);

    Ok(())
}

#[test]
fn guest_tools_tools_dir_is_packaged_filtered_and_reproducible() -> anyhow::Result<()> {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");
    let spec_path = testdata.join("spec.json");
    let drivers_dir = testdata.join("drivers");
    let guest_tools_src = testdata.join("guest-tools");

    let guest_tools_tmp = tempfile::tempdir()?;
    copy_dir_all(&guest_tools_src, guest_tools_tmp.path())?;

    // Inject a dummy guest-side tool payload. `.pdb` should be excluded by default.
    let tools_dir = guest_tools_tmp.path().join("tools");
    fs::create_dir_all(&tools_dir)?;
    fs::write(tools_dir.join("dummy.exe"), b"dummy exe\n")?;
    fs::write(tools_dir.join("dummy.pdb"), b"dummy pdb\n")?;

    let out1 = tempfile::tempdir()?;
    let out2 = tempfile::tempdir()?;

    let config1 = aero_packager::PackageConfig {
        drivers_dir,
        guest_tools_dir: guest_tools_tmp.path().to_path_buf(),
        windows_device_contract_path: device_contract_path(),
        out_dir: out1.path().to_path_buf(),
        spec_path,
        version: "1.2.3".to_string(),
        build_id: "test".to_string(),
        volume_id: "AERO_GUEST_TOOLS".to_string(),
        signing_policy: aero_packager::SigningPolicy::Test,
        source_date_epoch: 0,
    };
    let config2 = aero_packager::PackageConfig {
        out_dir: out2.path().to_path_buf(),
        ..config1.clone()
    };

    let outputs1 = aero_packager::package_guest_tools(&config1)?;
    let outputs2 = aero_packager::package_guest_tools(&config2)?;

    // Deterministic outputs, even when optional guest tools are present.
    assert_eq!(fs::read(&outputs1.iso_path)?, fs::read(&outputs2.iso_path)?);
    assert_eq!(fs::read(&outputs1.zip_path)?, fs::read(&outputs2.zip_path)?);
    assert_eq!(
        fs::read(&outputs1.manifest_path)?,
        fs::read(&outputs2.manifest_path)?
    );

    let iso_bytes = fs::read(&outputs1.iso_path)?;
    let tree = aero_packager::read_joliet_tree(&iso_bytes)?;
    assert!(tree.contains("tools/dummy.exe"));
    assert!(!tree.contains("tools/dummy.pdb"));

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
    assert!(zip_paths.contains("tools/dummy.exe"));
    assert!(!zip_paths.contains("tools/dummy.pdb"));

    Ok(())
}

#[test]
fn default_excluded_driver_extensions_are_excluded_from_driver_dirs() -> anyhow::Result<()> {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");
    let spec_path = testdata.join("spec.json");
    let drivers_dir = testdata.join("drivers");
    let guest_tools_dir = testdata.join("guest-tools");

    let out_base = tempfile::tempdir()?;
    let config_base = aero_packager::PackageConfig {
        drivers_dir: drivers_dir.clone(),
        guest_tools_dir: guest_tools_dir.clone(),
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

    let excluded_exts = ["map", "dbg", "cod", "tmp", "lastbuildstate", "idb"];

    let drivers_tmp = tempfile::tempdir()?;
    copy_dir_all(&drivers_dir, drivers_tmp.path())?;
    for arch in ["x86", "amd64"] {
        for ext in excluded_exts {
            fs::write(
                drivers_tmp
                    .path()
                    .join(format!("{arch}/testdrv/ignored.{ext}")),
                format!("dummy {ext}\n"),
            )?;
        }
    }

    let out = tempfile::tempdir()?;
    let config = aero_packager::PackageConfig {
        drivers_dir: drivers_tmp.path().to_path_buf(),
        out_dir: out.path().to_path_buf(),
        ..config_base
    };
    let outputs = aero_packager::package_guest_tools(&config)?;
    let iso_bytes = fs::read(&outputs.iso_path)?;
    let tree = aero_packager::read_joliet_tree(&iso_bytes)?;

    for arch in ["x86", "amd64"] {
        for ext in excluded_exts {
            let unexpected = format!("drivers/{arch}/testdrv/ignored.{ext}");
            assert!(
                !tree.contains(&unexpected),
                "unexpected file packaged: {unexpected}"
            );
        }
    }

    // Excluding build artifacts should keep outputs stable.
    assert_eq!(iso_base, iso_bytes);
    assert_eq!(zip_base, fs::read(&outputs.zip_path)?);
    assert_eq!(manifest_base, fs::read(&outputs.manifest_path)?);

    Ok(())
}

#[test]
fn allowlisted_default_excluded_driver_extensions_are_included() -> anyhow::Result<()> {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");
    let drivers_dir = testdata.join("drivers");
    let guest_tools_dir = testdata.join("guest-tools");

    let excluded_exts = ["map", "dbg", "cod", "tmp", "lastbuildstate", "idb"];

    let drivers_tmp = tempfile::tempdir()?;
    copy_dir_all(&drivers_dir, drivers_tmp.path())?;
    for arch in ["x86", "amd64"] {
        for ext in excluded_exts {
            fs::write(
                drivers_tmp.path().join(format!("{arch}/testdrv/keep.{ext}")),
                format!("dummy {ext}\n"),
            )?;
        }
    }

    let spec_dir = tempfile::tempdir()?;
    let spec_path = spec_dir.path().join("spec.json");
    let spec = serde_json::json!({
        "drivers": [
            {
                "name": "testdrv",
                "required": true,
                "expected_hardware_ids_from_devices_cmd_var": "AERO_TESTDRV_HWIDS",
                "allow_extensions": [".map", "dbg", "cod", "tmp", "lastbuildstate", "idb"],
            }
        ]
    });
    fs::write(&spec_path, serde_json::to_vec_pretty(&spec)?)?;

    let out = tempfile::tempdir()?;
    let config = aero_packager::PackageConfig {
        drivers_dir: drivers_tmp.path().to_path_buf(),
        guest_tools_dir,
        windows_device_contract_path: device_contract_path(),
        out_dir: out.path().to_path_buf(),
        spec_path,
        version: "1.2.3".to_string(),
        build_id: "test".to_string(),
        volume_id: "AERO_GUEST_TOOLS".to_string(),
        signing_policy: aero_packager::SigningPolicy::Test,
        source_date_epoch: 0,
    };

    let outputs = aero_packager::package_guest_tools(&config)?;
    let iso_bytes = fs::read(&outputs.iso_path)?;
    let tree = aero_packager::read_joliet_tree(&iso_bytes)?;

    for arch in ["x86", "amd64"] {
        for ext in excluded_exts {
            let required = format!("drivers/{arch}/testdrv/keep.{ext}");
            assert!(tree.contains(&required), "expected file missing: {required}");
        }
    }

    Ok(())
}

#[test]
fn package_outputs_allow_empty_certs_when_signing_policy_none() -> anyhow::Result<()> {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");
    let spec_path = testdata.join("spec.json");

    let drivers_dir = testdata.join("drivers");
    let guest_tools_dir = testdata.join("guest-tools-no-certs");

    let out1 = tempfile::tempdir()?;
    let out2 = tempfile::tempdir()?;

    let config1 = aero_packager::PackageConfig {
        drivers_dir: drivers_dir.clone(),
        guest_tools_dir: guest_tools_dir.clone(),
        windows_device_contract_path: device_contract_path(),
        out_dir: out1.path().to_path_buf(),
        spec_path: spec_path.clone(),
        version: "1.2.3".to_string(),
        build_id: "test".to_string(),
        volume_id: "AERO_GUEST_TOOLS".to_string(),
        signing_policy: aero_packager::SigningPolicy::None,
        source_date_epoch: 0,
    };
    let config2 = aero_packager::PackageConfig {
        out_dir: out2.path().to_path_buf(),
        ..config1.clone()
    };

    let outputs1 = aero_packager::package_guest_tools(&config1)?;
    let outputs2 = aero_packager::package_guest_tools(&config2)?;

    // Deterministic outputs even when certs are omitted.
    assert_eq!(fs::read(&outputs1.iso_path)?, fs::read(&outputs2.iso_path)?);
    assert_eq!(fs::read(&outputs1.zip_path)?, fs::read(&outputs2.zip_path)?);
    assert_eq!(
        fs::read(&outputs1.manifest_path)?,
        fs::read(&outputs2.manifest_path)?
    );

    let manifest_bytes = fs::read(&outputs1.manifest_path)?;
    let manifest: aero_packager::Manifest = serde_json::from_slice(&manifest_bytes)?;
    assert_eq!(manifest.signing_policy, aero_packager::SigningPolicy::None);
    assert!(!manifest.certs_required);

    let iso_bytes = fs::read(&outputs1.iso_path)?;
    let tree = aero_packager::read_joliet_tree(&iso_bytes)?;
    assert!(
        !tree
            .paths
            .iter()
            .any(|p| p.starts_with("certs/") && !p.ends_with("README.md")),
        "expected no certificates in ISO when signing_policy=none"
    );

    Ok(())
}

#[test]
fn package_outputs_allow_missing_certs_dir_when_signing_policy_none() -> anyhow::Result<()> {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");
    let spec_path = testdata.join("spec.json");

    let drivers_dir = testdata.join("drivers");
    let guest_tools_src = testdata.join("guest-tools-no-certs");

    let guest_tools_tmp = tempfile::tempdir()?;
    copy_dir_all(&guest_tools_src, guest_tools_tmp.path())?;
    let certs_dir = guest_tools_tmp.path().join("certs");
    if certs_dir.exists() {
        fs::remove_dir_all(&certs_dir)?;
    }

    let out = tempfile::tempdir()?;
    let config = aero_packager::PackageConfig {
        drivers_dir,
        guest_tools_dir: guest_tools_tmp.path().to_path_buf(),
        windows_device_contract_path: device_contract_path(),
        out_dir: out.path().to_path_buf(),
        spec_path,
        version: "1.2.3".to_string(),
        build_id: "test".to_string(),
        volume_id: "AERO_GUEST_TOOLS".to_string(),
        signing_policy: aero_packager::SigningPolicy::None,
        source_date_epoch: 0,
    };

    let outputs = aero_packager::package_guest_tools(&config)?;
    let iso_bytes = fs::read(&outputs.iso_path)?;
    let tree = aero_packager::read_joliet_tree(&iso_bytes)?;
    assert!(
        !tree.paths.iter().any(|p| p.starts_with("certs/")),
        "expected no cert files when certs/ directory is missing"
    );

    Ok(())
}

#[test]
fn packaging_fails_when_certs_present_for_production_or_none() -> anyhow::Result<()> {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");
    let spec_path = testdata.join("spec.json");
    let drivers_dir = testdata.join("drivers");
    let guest_tools_src = testdata.join("guest-tools-no-certs");

    let guest_tools_tmp = tempfile::tempdir()?;
    copy_dir_all(&guest_tools_src, guest_tools_tmp.path())?;
    fs::write(
        guest_tools_tmp.path().join("certs").join("foo.cer"),
        b"dummy cert\n",
    )?;

    for signing_policy in [
        aero_packager::SigningPolicy::Production,
        aero_packager::SigningPolicy::None,
    ] {
        let out = tempfile::tempdir()?;
        let config = aero_packager::PackageConfig {
            drivers_dir: drivers_dir.clone(),
            guest_tools_dir: guest_tools_tmp.path().to_path_buf(),
            windows_device_contract_path: device_contract_path(),
            out_dir: out.path().to_path_buf(),
            spec_path: spec_path.clone(),
            version: "1.2.3".to_string(),
            build_id: "test".to_string(),
            volume_id: "AERO_GUEST_TOOLS".to_string(),
            signing_policy,
            source_date_epoch: 0,
        };

        let err = aero_packager::package_guest_tools(&config).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("refusing to package certificate")
                && msg.contains("--signing-policy test")
                && msg.contains("foo.cer"),
            "unexpected error for signing_policy={signing_policy}: {msg}"
        );
    }

    // Sanity: the exact same tree should be accepted for test signing policies.
    let out = tempfile::tempdir()?;
    let config = aero_packager::PackageConfig {
        drivers_dir,
        guest_tools_dir: guest_tools_tmp.path().to_path_buf(),
        windows_device_contract_path: device_contract_path(),
        out_dir: out.path().to_path_buf(),
        spec_path,
        version: "1.2.3".to_string(),
        build_id: "test".to_string(),
        volume_id: "AERO_GUEST_TOOLS".to_string(),
        signing_policy: aero_packager::SigningPolicy::Test,
        source_date_epoch: 0,
    };
    aero_packager::package_guest_tools(&config)?;

    Ok(())
}

#[test]
fn packaging_fails_when_signing_policy_requires_certs_but_none_present() -> anyhow::Result<()> {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");
    let drivers_dir = testdata.join("drivers");
    let guest_tools_dir = testdata.join("guest-tools-no-certs");
    let spec_path = testdata.join("spec.json");

    let out = tempfile::tempdir()?;
    let config = aero_packager::PackageConfig {
        drivers_dir,
        guest_tools_dir,
        windows_device_contract_path: device_contract_path(),
        out_dir: out.path().to_path_buf(),
        spec_path,
        version: "1.2.3".to_string(),
        build_id: "test".to_string(),
        volume_id: "AERO_GUEST_TOOLS".to_string(),
        signing_policy: aero_packager::SigningPolicy::Test,
        source_date_epoch: 0,
    };

    let err = aero_packager::package_guest_tools(&config).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("contains no certificate files"),
        "unexpected error: {msg}"
    );
    Ok(())
}

#[test]
fn duplicate_driver_names_in_spec_are_rejected() -> anyhow::Result<()> {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");

    let drivers_dir = testdata.join("drivers");
    let guest_tools_dir = testdata.join("guest-tools");

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
                // Duplicate (case-insensitive) name should be rejected.
                "name": "TESTDRV",
                "required": true,
                "expected_hardware_ids": [],
            },
        ]
    });
    fs::write(&spec_path, serde_json::to_vec_pretty(&spec)?)?;

    let out = tempfile::tempdir()?;
    let config = aero_packager::PackageConfig {
        drivers_dir,
        guest_tools_dir,
        windows_device_contract_path: device_contract_path(),
        out_dir: out.path().to_path_buf(),
        spec_path,
        version: "0.0.0".to_string(),
        build_id: "test".to_string(),
        volume_id: "AERO_GUEST_TOOLS".to_string(),
        source_date_epoch: 0,
        signing_policy: aero_packager::SigningPolicy::Test,
    };

    let err = aero_packager::package_guest_tools(&config).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("lists the same driver multiple times"),
        "unexpected error: {msg}"
    );

    Ok(())
}

#[test]
fn optional_drivers_must_be_present_on_all_arches_when_strict_flag_is_enabled() -> anyhow::Result<()> {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");
    let guest_tools_dir = testdata.join("guest-tools");

    let spec_dir = tempfile::tempdir()?;
    let spec_path = spec_dir.path().join("spec.json");
    let spec = serde_json::json!({
        "require_optional_drivers_on_all_arches": true,
        "drivers": [
            {
                "name": "optdrv",
                "required": false,
                "expected_hardware_ids": [r"PCI\\VEN_1234&DEV_5678"],
            },
        ],
    });
    fs::write(&spec_path, serde_json::to_vec_pretty(&spec)?)?;

    let drivers_tmp = tempfile::tempdir()?;
    write_stub_pci_driver(
        &drivers_tmp.path().join("x86").join("optdrv"),
        "optdrv",
        "optdrv",
        r"PCI\VEN_1234&DEV_5678",
    )?;
    // Ensure the amd64 arch root exists but does not contain the optional driver directory.
    fs::create_dir_all(drivers_tmp.path().join("amd64"))?;

    let out = tempfile::tempdir()?;
    let config = aero_packager::PackageConfig {
        drivers_dir: drivers_tmp.path().to_path_buf(),
        guest_tools_dir,
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
        msg.contains("optional driver directory is present for x86 but missing for amd64")
            && msg.contains("optdrv")
            && msg.contains("require_optional_drivers_on_all_arches=true"),
        "unexpected error: {msg}"
    );

    Ok(())
}

#[test]
fn driver_names_with_whitespace_are_rejected() -> anyhow::Result<()> {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");

    let drivers_dir = testdata.join("drivers");
    let guest_tools_dir = testdata.join("guest-tools");

    let spec_dir = tempfile::tempdir()?;
    let spec_path = spec_dir.path().join("spec.json");
    let spec = serde_json::json!({
        "drivers": [
            {
                "name": "testdrv ",
                "required": true,
                "expected_hardware_ids": [r"PCI\\VEN_1234&DEV_5678"],
            },
        ]
    });
    fs::write(&spec_path, serde_json::to_vec_pretty(&spec)?)?;

    let out = tempfile::tempdir()?;
    let config = aero_packager::PackageConfig {
        drivers_dir,
        guest_tools_dir,
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
        msg.contains("leading/trailing whitespace"),
        "unexpected error: {msg}"
    );

    Ok(())
}

#[test]
fn empty_driver_list_is_rejected() -> anyhow::Result<()> {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");

    let drivers_dir = testdata.join("drivers");
    let guest_tools_dir = testdata.join("guest-tools");

    let spec_dir = tempfile::tempdir()?;
    let spec_path = spec_dir.path().join("spec.json");
    let spec = serde_json::json!({ "drivers": [] });
    fs::write(&spec_path, serde_json::to_vec_pretty(&spec)?)?;

    let out = tempfile::tempdir()?;
    let config = aero_packager::PackageConfig {
        drivers_dir,
        guest_tools_dir,
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
        msg.contains("contains no drivers"),
        "unexpected error: {msg}"
    );

    Ok(())
}

#[test]
fn manifest_input_hashes_are_formatting_insensitive() -> anyhow::Result<()> {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");

    // Match the main packaging test: inject a dummy DLL so we can ensure non-empty user-mode
    // payloads are preserved.
    let drivers_src = testdata.join("drivers");
    let drivers_tmp = tempfile::tempdir()?;
    copy_dir_all(&drivers_src, drivers_tmp.path())?;
    fs::write(
        drivers_tmp.path().join("x86/testdrv/test.dll"),
        b"dummy dll (x86)\n",
    )?;
    fs::write(
        drivers_tmp.path().join("amd64/testdrv/test.dll"),
        b"dummy dll (amd64)\n",
    )?;

    let guest_tools_dir = testdata.join("guest-tools");

    // Two semantically-identical specs with different formatting/key ordering.
    let spec_a = r#"{
  "drivers": [
    {
      "name": "testdrv",
      "required": true,
      "expected_hardware_ids_from_devices_cmd_var": "AERO_TESTDRV_HWIDS"
    }
  ]
}"#;
    let spec_b = r#"{"drivers":[{"expected_hardware_ids_from_devices_cmd_var":"AERO_TESTDRV_HWIDS","required":true,"name":"testdrv"}]}"#;

    // Two semantically-identical contracts with different formatting/key ordering.
    let contract_a = r#"{
  "schema_version": 1,
  "contract_name": "aero-windows-pci-device-contract",
  "contract_version": "0.0.0",
  "devices": [
    {
      "device": "virtio-blk",
      "pci_vendor_id": "0x1AF4",
      "pci_device_id": "0x1042",
      "hardware_id_patterns": ["PCI\\VEN_1AF4&DEV_1042&REV_01"],
      "driver_service_name": "aero_virtio_blk",
      "inf_name": "aero_virtio_blk.inf",
      "virtio_device_type": 2
    },
    {
      "device": "virtio-net",
      "pci_vendor_id": "0x1AF4",
      "pci_device_id": "0x1041",
      "hardware_id_patterns": ["PCI\\VEN_1AF4&DEV_1041&REV_01"],
      "driver_service_name": "aero_virtio_net",
      "inf_name": "aero_virtio_net.inf",
      "virtio_device_type": 1
    },
    {
      "device": "virtio-input",
      "pci_vendor_id": "0x1AF4",
      "pci_device_id": "0x1052",
      "hardware_id_patterns": ["PCI\\VEN_1AF4&DEV_1052&REV_01"],
      "driver_service_name": "aero_virtio_input",
      "inf_name": "aero_virtio_input.inf",
      "virtio_device_type": 18
    },
    {
      "device": "virtio-snd",
      "pci_vendor_id": "0x1AF4",
      "pci_device_id": "0x1059",
      "hardware_id_patterns": ["PCI\\VEN_1AF4&DEV_1059&REV_01"],
      "driver_service_name": "aero_virtio_snd",
      "inf_name": "aero_virtio_snd.inf",
      "virtio_device_type": 25
    },
    {
      "device": "aero-gpu",
      "pci_vendor_id": "0xA3A0",
      "pci_device_id": "0x0001",
      "hardware_id_patterns": ["PCI\\VEN_A3A0&DEV_0001"],
      "driver_service_name": "aerogpu",
      "inf_name": "aerogpu.inf"
    }
  ]
}"#;
    let contract_b = r#"{
  "devices": [
    {
      "inf_name": "aero_virtio_blk.inf",
      "virtio_device_type": 2,
      "pci_device_id": "0x1042",
      "pci_vendor_id": "0x1AF4",
      "device": "virtio-blk",
      "driver_service_name": "aero_virtio_blk",
      "hardware_id_patterns": [
        "PCI\\VEN_1AF4&DEV_1042&REV_01"
      ]
    },
    {
      "hardware_id_patterns": ["PCI\\VEN_1AF4&DEV_1041&REV_01"],
      "driver_service_name": "aero_virtio_net",
      "device": "virtio-net",
      "pci_vendor_id": "0x1AF4",
      "pci_device_id": "0x1041",
      "inf_name": "aero_virtio_net.inf",
      "virtio_device_type": 1
    },
    {
      "device": "virtio-input",
      "driver_service_name": "aero_virtio_input",
      "pci_vendor_id": "0x1AF4",
      "pci_device_id": "0x1052",
      "hardware_id_patterns": ["PCI\\VEN_1AF4&DEV_1052&REV_01"],
      "inf_name": "aero_virtio_input.inf",
      "virtio_device_type": 18
    },
    {
      "pci_device_id": "0x1059",
      "pci_vendor_id": "0x1AF4",
      "driver_service_name": "aero_virtio_snd",
      "hardware_id_patterns": ["PCI\\VEN_1AF4&DEV_1059&REV_01"],
      "inf_name": "aero_virtio_snd.inf",
      "device": "virtio-snd",
      "virtio_device_type": 25
    },
    {
      "pci_vendor_id": "0xA3A0",
      "pci_device_id": "0x0001",
      "driver_service_name": "aerogpu",
      "inf_name": "aerogpu.inf",
      "hardware_id_patterns": ["PCI\\VEN_A3A0&DEV_0001"],
      "device": "aero-gpu"
    }
  ],
  "contract_version": "0.0.0",
  "schema_version": 1,
  "contract_name": "aero-windows-pci-device-contract"
}"#;

    // Use the same spec/contract paths across both packaging runs; only the JSON formatting changes.
    // This keeps the package outputs stable even when the packager records the source file paths in
    // the manifest provenance.
    let spec_dir = tempfile::tempdir()?;
    let spec_path = spec_dir.path().join("spec.json");
    fs::write(&spec_path, spec_a)?;

    let contract_dir = tempfile::tempdir()?;
    let contract_path = contract_dir.path().join("windows-device-contract.json");
    fs::write(&contract_path, contract_a)?;

    let out1 = tempfile::tempdir()?;
    let out2 = tempfile::tempdir()?;

    let config1 = aero_packager::PackageConfig {
        drivers_dir: drivers_tmp.path().to_path_buf(),
        guest_tools_dir: guest_tools_dir.clone(),
        windows_device_contract_path: contract_path,
        out_dir: out1.path().to_path_buf(),
        spec_path: spec_path,
        version: "1.2.3".to_string(),
        build_id: "test".to_string(),
        volume_id: "AERO_GUEST_TOOLS".to_string(),
        signing_policy: aero_packager::SigningPolicy::Test,
        source_date_epoch: 0,
    };
    let config2 = aero_packager::PackageConfig {
        out_dir: out2.path().to_path_buf(),
        ..config1.clone()
    };

    let outputs1 = aero_packager::package_guest_tools(&config1)?;

    fs::write(&config1.spec_path, spec_b)?;
    fs::write(&config1.windows_device_contract_path, contract_b)?;
    let outputs2 = aero_packager::package_guest_tools(&config2)?;

    // If JSON canonicalization is working, the input hashes (and thus the entire package outputs)
    // should be byte-identical even though the input JSON formatting differs.
    let manifest1: aero_packager::Manifest =
        serde_json::from_slice(&fs::read(&outputs1.manifest_path)?)?;
    let manifest2: aero_packager::Manifest =
        serde_json::from_slice(&fs::read(&outputs2.manifest_path)?)?;
    let inputs1 = manifest1.inputs.as_ref().expect("manifest1.inputs");
    let inputs2 = manifest2.inputs.as_ref().expect("manifest2.inputs");
    assert_eq!(
        inputs1
            .packaging_spec
            .as_ref()
            .expect("manifest1.inputs.packaging_spec")
            .sha256,
        inputs2
            .packaging_spec
            .as_ref()
            .expect("manifest2.inputs.packaging_spec")
            .sha256
    );
    assert_eq!(
        inputs1
            .windows_device_contract
            .as_ref()
            .expect("manifest1.inputs.windows_device_contract")
            .sha256,
        inputs2
            .windows_device_contract
            .as_ref()
            .expect("manifest2.inputs.windows_device_contract")
            .sha256
    );

    assert_eq!(fs::read(&outputs1.manifest_path)?, fs::read(&outputs2.manifest_path)?);
    assert_eq!(fs::read(&outputs1.iso_path)?, fs::read(&outputs2.iso_path)?);
    assert_eq!(fs::read(&outputs1.zip_path)?, fs::read(&outputs2.zip_path)?);

    Ok(())
}

fn canonical_json_sha256_hex(bytes: &[u8]) -> anyhow::Result<String> {
    let value: serde_json::Value = serde_json::from_slice(bytes)?;
    let canonical = serde_json::to_vec(&value)?;
    let mut h = sha2::Sha256::new();
    h.update(&canonical);
    Ok(hex::encode(h.finalize()))
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

fn write_stub_pci_driver(
    dir: &std::path::Path,
    base_name: &str,
    service_name: &str,
    hwid: &str,
) -> anyhow::Result<()> {
    fs::create_dir_all(dir)?;
    fs::write(
        dir.join(format!("{base_name}.inf")),
        format!(
            concat!(
                "[Version]\n",
                "Signature=\"$Windows NT$\"\n",
                "\n",
                "[Manufacturer]\n",
                "%Mfg%=Models,NTx86,NTamd64\n",
                "\n",
                "[Models.NTx86]\n",
                "%Dev%=Install, {hwid}\n",
                "\n",
                "[Models.NTamd64]\n",
                "%Dev%=Install, {hwid}\n",
                "\n",
                "[Install]\n",
                "CopyFiles=CopyFilesSection\n",
                "AddService = {service_name}, 0x00000002, Service_Inst\n",
                "\n",
                "[CopyFilesSection]\n",
                "{base_name}.sys\n",
                "\n",
                "[Service_Inst]\n",
                "ServiceBinary=%12%\\\\{base_name}.sys\n",
                "\n",
                "[SourceDisksFiles]\n",
                "{base_name}.sys=1\n",
                "\n",
                "[Strings]\n",
                "Mfg=\"Aero\"\n",
                "Dev=\"Test\"\n",
            ),
            hwid = hwid,
            service_name = service_name,
            base_name = base_name
        ),
    )?;
    fs::write(dir.join(format!("{base_name}.sys")), b"dummy sys\n")?;
    fs::write(dir.join(format!("{base_name}.cat")), b"dummy cat\n")?;
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
