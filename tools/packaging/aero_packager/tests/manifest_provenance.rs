use std::fs;
use std::path::PathBuf;

fn reverse_object_key_order(v: serde_json::Value) -> serde_json::Value {
    match v {
        serde_json::Value::Object(map) => {
            let mut entries: Vec<(String, serde_json::Value)> = map.into_iter().collect();
            entries.reverse();
            let mut out = serde_json::Map::new();
            for (k, v) in entries {
                out.insert(k, reverse_object_key_order(v));
            }
            serde_json::Value::Object(out)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.into_iter().map(reverse_object_key_order).collect())
        }
        other => other,
    }
}

#[test]
fn provenance_hashes_are_stable_for_json_key_order_and_whitespace_changes() -> anyhow::Result<()> {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");

    let drivers_dir = testdata.join("drivers");
    let guest_tools_dir = testdata.join("guest-tools");

    let tmp = tempfile::tempdir()?;
    let spec_path = tmp.path().join("spec.json");
    let contract_path = tmp.path().join("contract.json");

    let spec = serde_json::json!({
        "drivers": [
            {
                "name": "testdrv",
                "required": true,
                "expected_hardware_ids_from_devices_cmd_var": "AERO_TESTDRV_HWIDS",
            }
        ]
    });
    let contract = serde_json::json!({
        "schema_version": 1,
        "contract_name": "test-contract",
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
                "device": "virtio-snd",
                "pci_vendor_id": "0x1AF4",
                "pci_device_id": "0x1059",
                "hardware_id_patterns": ["PCI\\VEN_1AF4&DEV_1059&REV_01"],
                "driver_service_name": "aero_virtio_snd",
                "inf_name": "aero_virtio_snd.inf",
                "virtio_device_type": 25
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
                "device": "aero-gpu",
                "pci_vendor_id": "0xA3A0",
                "pci_device_id": "0x0001",
                "hardware_id_patterns": ["PCI\\VEN_A3A0&DEV_0001"],
                "driver_service_name": "aerogpu",
                "inf_name": "aerogpu.inf"
            }
        ]
    });

    fs::write(&spec_path, serde_json::to_vec_pretty(&spec)?)?;
    fs::write(&contract_path, serde_json::to_vec_pretty(&contract)?)?;

    let out1 = tempfile::tempdir()?;
    let config1 = aero_packager::PackageConfig {
        drivers_dir: drivers_dir.clone(),
        guest_tools_dir: guest_tools_dir.clone(),
        windows_device_contract_path: contract_path.clone(),
        out_dir: out1.path().to_path_buf(),
        spec_path: spec_path.clone(),
        version: "1.2.3".to_string(),
        build_id: "test".to_string(),
        volume_id: "AERO_GUEST_TOOLS".to_string(),
        signing_policy: aero_packager::SigningPolicy::Test,
        source_date_epoch: 0,
    };
    let outputs1 = aero_packager::package_guest_tools(&config1)?;
    let manifest1: aero_packager::Manifest = serde_json::from_slice(&fs::read(&outputs1.manifest_path)?)?;
    let prov1 = manifest1.provenance.as_ref().expect("provenance should be present");

    // Rewrite both JSON inputs with different formatting and key order.
    let spec2 = reverse_object_key_order(spec);
    let contract2 = reverse_object_key_order(contract);
    let spec_bytes_before = fs::read(&spec_path)?;
    let contract_bytes_before = fs::read(&contract_path)?;
    fs::write(&spec_path, serde_json::to_vec(&spec2)?)?;
    fs::write(&contract_path, serde_json::to_vec(&contract2)?)?;
    assert_ne!(spec_bytes_before, fs::read(&spec_path)?);
    assert_ne!(contract_bytes_before, fs::read(&contract_path)?);

    let out2 = tempfile::tempdir()?;
    let config2 = aero_packager::PackageConfig {
        out_dir: out2.path().to_path_buf(),
        ..config1.clone()
    };
    let outputs2 = aero_packager::package_guest_tools(&config2)?;
    let manifest2: aero_packager::Manifest = serde_json::from_slice(&fs::read(&outputs2.manifest_path)?)?;
    let prov2 = manifest2.provenance.as_ref().expect("provenance should be present");

    // Hashes should be stable for the same logical JSON.
    assert_eq!(prov1.packaging_spec_sha256, prov2.packaging_spec_sha256);
    assert_eq!(
        prov1.windows_device_contract_sha256,
        prov2.windows_device_contract_sha256
    );

    // Output media should also be byte-identical because the recorded provenance hashes did not
    // change.
    assert_eq!(fs::read(&outputs1.iso_path)?, fs::read(&outputs2.iso_path)?);
    assert_eq!(fs::read(&outputs1.zip_path)?, fs::read(&outputs2.zip_path)?);
    assert_eq!(
        fs::read(&outputs1.manifest_path)?,
        fs::read(&outputs2.manifest_path)?
    );

    Ok(())
}
