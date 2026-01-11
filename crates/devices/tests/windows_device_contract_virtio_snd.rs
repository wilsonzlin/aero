use std::fs;
use std::path::PathBuf;

use aero_devices::pci::profile::VIRTIO_SND;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn parse_hex_u16(input: &str) -> u16 {
    let trimmed = input.trim();
    let no_prefix = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);
    u16::from_str_radix(no_prefix, 16)
        .unwrap_or_else(|err| panic!("bad hex u16 literal {input:?}: {err}"))
}

fn require_str<'a>(value: &'a serde_json::Value, field: &str) -> &'a str {
    value
        .get(field)
        .unwrap_or_else(|| panic!("missing field {field}"))
        .as_str()
        .unwrap_or_else(|| panic!("{field} must be a string"))
}

fn require_array<'a>(value: &'a serde_json::Value, field: &str) -> &'a [serde_json::Value] {
    value
        .get(field)
        .unwrap_or_else(|| panic!("missing field {field}"))
        .as_array()
        .unwrap_or_else(|| panic!("{field} must be an array"))
}

fn assert_file_contains_case_insensitive(path: &std::path::Path, needle: &str) {
    let content = fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
    assert!(
        content.to_ascii_uppercase().contains(&needle.to_ascii_uppercase()),
        "{} is out of sync: expected to contain {needle:?}",
        path.display()
    );
}

#[test]
fn virtio_snd_pci_ids_match_windows_device_contract() {
    let contract_path = repo_root().join("docs/windows-device-contract.json");
    let contract_text = fs::read_to_string(&contract_path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", contract_path.display()));
    let contract_json: serde_json::Value = serde_json::from_str(&contract_text)
        .unwrap_or_else(|err| panic!("failed to parse {}: {err}", contract_path.display()));

    let devices = contract_json
        .get("devices")
        .and_then(|value| value.as_array())
        .unwrap_or_else(|| panic!("{}: expected top-level `devices` array", contract_path.display()));

    let virtio_snd = devices
        .iter()
        .find(|device| device.get("device").and_then(|v| v.as_str()) == Some("virtio-snd"))
        .unwrap_or_else(|| panic!("{}: missing device entry for `virtio-snd`", contract_path.display()));

    let vendor_id = parse_hex_u16(require_str(virtio_snd, "pci_vendor_id"));
    let device_id = parse_hex_u16(require_str(virtio_snd, "pci_device_id"));

    assert_eq!(
        vendor_id, VIRTIO_SND.vendor_id,
        "{}: virtio-snd pci_vendor_id drift: contract is {vendor_id:04X}, emulator profile is {:04X}",
        contract_path.display(),
        VIRTIO_SND.vendor_id
    );
    assert_eq!(
        device_id, VIRTIO_SND.device_id,
        "{}: virtio-snd pci_device_id drift: contract is {device_id:04X}, emulator profile is {:04X}",
        contract_path.display(),
        VIRTIO_SND.device_id
    );

    let patterns: Vec<String> = require_array(virtio_snd, "hardware_id_patterns")
        .iter()
        .map(|value| {
            value
                .as_str()
                .unwrap_or_else(|| panic!("hardware_id_patterns entries must be strings"))
                .to_string()
        })
        .collect();

    let expected_hwid_short = format!("PCI\\VEN_{vendor_id:04X}&DEV_{device_id:04X}");
    let expected_hwid_subsys = format!(
        "PCI\\VEN_{vendor_id:04X}&DEV_{device_id:04X}&SUBSYS_{subsys_id:04X}{subsys_vendor:04X}",
        subsys_vendor = VIRTIO_SND.subsystem_vendor_id,
        subsys_id = VIRTIO_SND.subsystem_id
    );

    assert!(
        patterns
            .iter()
            .any(|value| value.eq_ignore_ascii_case(&expected_hwid_short)),
        "{}: virtio-snd hardware_id_patterns missing {expected_hwid_short:?}. Found: {patterns:?}",
        contract_path.display()
    );

    assert!(
        patterns
            .iter()
            .any(|value| value.eq_ignore_ascii_case(&expected_hwid_subsys)),
        "{}: virtio-snd hardware_id_patterns missing {expected_hwid_subsys:?}. Found: {patterns:?}",
        contract_path.display()
    );

    // Cross-check a few other repo-owned “consumers” so driver binding / tooling doesn't drift.
    let root = repo_root();
    assert_file_contains_case_insensitive(
        &root.join("guest-tools/config/devices.cmd"),
        &expected_hwid_short,
    );

    // Contract JSON specifies the canonical INF filename; it must exist in-tree and match the same HWID.
    let inf_name = require_str(virtio_snd, "inf_name");
    let service_name = require_str(virtio_snd, "driver_service_name");
    let virtio_device_type = virtio_snd
        .get("virtio_device_type")
        .and_then(|value| value.as_u64())
        .unwrap_or_else(|| panic!("virtio_device_type must be a number"));

    assert_eq!(
        virtio_device_type,
        u64::from(VIRTIO_SND.subsystem_id),
        "{}: virtio-snd virtio_device_type drift: contract is {virtio_device_type}, emulator profile subsystem_id is {}",
        contract_path.display(),
        VIRTIO_SND.subsystem_id
    );

    let inf_path = root.join("drivers/windows7/virtio-snd/inf").join(inf_name);
    assert!(
        inf_path.is_file(),
        "{}: virtio-snd INF referenced by windows-device-contract.json is missing: {}",
        contract_path.display(),
        inf_path.display()
    );
    assert_file_contains_case_insensitive(&inf_path, &expected_hwid_short);
    assert_file_contains_case_insensitive(&inf_path, &format!("AddService = {service_name}"));
}
