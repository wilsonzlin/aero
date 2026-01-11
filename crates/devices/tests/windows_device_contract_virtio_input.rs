use std::fs;
use std::path::PathBuf;

use aero_devices::pci::profile::{VIRTIO_INPUT_KEYBOARD, VIRTIO_INPUT_MOUSE};

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
        content
            .to_ascii_uppercase()
            .contains(&needle.to_ascii_uppercase()),
        "{} is out of sync: expected to contain {needle:?}",
        path.display()
    );
}

#[test]
fn virtio_input_pci_ids_match_windows_device_contract() {
    let contract_path = repo_root().join("docs/windows-device-contract.json");
    let contract_text = fs::read_to_string(&contract_path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", contract_path.display()));
    let contract_json: serde_json::Value = serde_json::from_str(&contract_text)
        .unwrap_or_else(|err| panic!("failed to parse {}: {err}", contract_path.display()));

    let devices = contract_json
        .get("devices")
        .and_then(|value| value.as_array())
        .unwrap_or_else(|| {
            panic!(
                "{}: expected top-level `devices` array",
                contract_path.display()
            )
        });

    let virtio_input = devices
        .iter()
        .find(|device| device.get("device").and_then(|v| v.as_str()) == Some("virtio-input"))
        .unwrap_or_else(|| {
            panic!(
                "{}: missing device entry for `virtio-input`",
                contract_path.display()
            )
        });

    let vendor_id = parse_hex_u16(require_str(virtio_input, "pci_vendor_id"));
    let device_id = parse_hex_u16(require_str(virtio_input, "pci_device_id"));

    assert_eq!(
        vendor_id, VIRTIO_INPUT_KEYBOARD.vendor_id,
        "{}: virtio-input pci_vendor_id drift: contract is {vendor_id:04X}, emulator profile is {:04X}",
        contract_path.display(),
        VIRTIO_INPUT_KEYBOARD.vendor_id
    );
    assert_eq!(
        device_id, VIRTIO_INPUT_KEYBOARD.device_id,
        "{}: virtio-input pci_device_id drift: contract is {device_id:04X}, emulator profile is {:04X}",
        contract_path.display(),
        VIRTIO_INPUT_KEYBOARD.device_id
    );
    assert_eq!(
        (
            VIRTIO_INPUT_KEYBOARD.vendor_id,
            VIRTIO_INPUT_KEYBOARD.device_id
        ),
        (VIRTIO_INPUT_MOUSE.vendor_id, VIRTIO_INPUT_MOUSE.device_id),
        "virtio-input profiles must share vendor/device IDs"
    );

    let patterns: Vec<String> = require_array(virtio_input, "hardware_id_patterns")
        .iter()
        .map(|value| {
            value
                .as_str()
                .unwrap_or_else(|| panic!("hardware_id_patterns entries must be strings"))
                .to_string()
        })
        .collect();

    let expected_hwid_short = format!("PCI\\VEN_{vendor_id:04X}&DEV_{device_id:04X}");
    let expected_hwid_short_rev = format!("PCI\\VEN_{vendor_id:04X}&DEV_{device_id:04X}&REV_01");
    let expected_hwid_keyboard_subsys = format!(
        "PCI\\VEN_{vendor_id:04X}&DEV_{device_id:04X}&SUBSYS_{subsys_id:04X}{subsys_vendor:04X}",
        subsys_vendor = VIRTIO_INPUT_KEYBOARD.subsystem_vendor_id,
        subsys_id = VIRTIO_INPUT_KEYBOARD.subsystem_id
    );
    let expected_hwid_keyboard_subsys_rev = format!(
        "PCI\\VEN_{vendor_id:04X}&DEV_{device_id:04X}&SUBSYS_{subsys_id:04X}{subsys_vendor:04X}&REV_01",
        subsys_vendor = VIRTIO_INPUT_KEYBOARD.subsystem_vendor_id,
        subsys_id = VIRTIO_INPUT_KEYBOARD.subsystem_id
    );
    let expected_hwid_mouse_subsys = format!(
        "PCI\\VEN_{vendor_id:04X}&DEV_{device_id:04X}&SUBSYS_{subsys_id:04X}{subsys_vendor:04X}",
        subsys_vendor = VIRTIO_INPUT_MOUSE.subsystem_vendor_id,
        subsys_id = VIRTIO_INPUT_MOUSE.subsystem_id
    );
    let expected_hwid_mouse_subsys_rev = format!(
        "PCI\\VEN_{vendor_id:04X}&DEV_{device_id:04X}&SUBSYS_{subsys_id:04X}{subsys_vendor:04X}&REV_01",
        subsys_vendor = VIRTIO_INPUT_MOUSE.subsystem_vendor_id,
        subsys_id = VIRTIO_INPUT_MOUSE.subsystem_id
    );

    assert!(
        patterns
            .iter()
            .any(|value| value.eq_ignore_ascii_case(&expected_hwid_short)),
        "{}: virtio-input hardware_id_patterns missing {expected_hwid_short:?}. Found: {patterns:?}",
        contract_path.display()
    );
    assert!(
        patterns
            .iter()
            .any(|value| value.eq_ignore_ascii_case(&expected_hwid_short_rev)),
        "{}: virtio-input hardware_id_patterns missing {expected_hwid_short_rev:?}. Found: {patterns:?}",
        contract_path.display()
    );
    assert!(
        patterns
            .iter()
            .any(|value| value.eq_ignore_ascii_case(&expected_hwid_keyboard_subsys)),
        "{}: virtio-input hardware_id_patterns missing {expected_hwid_keyboard_subsys:?}. Found: {patterns:?}",
        contract_path.display()
    );
    assert!(
        patterns
            .iter()
            .any(|value| value.eq_ignore_ascii_case(&expected_hwid_keyboard_subsys_rev)),
        "{}: virtio-input hardware_id_patterns missing {expected_hwid_keyboard_subsys_rev:?}. Found: {patterns:?}",
        contract_path.display()
    );
    assert!(
        patterns
            .iter()
            .any(|value| value.eq_ignore_ascii_case(&expected_hwid_mouse_subsys)),
        "{}: virtio-input hardware_id_patterns missing {expected_hwid_mouse_subsys:?}. Found: {patterns:?}",
        contract_path.display()
    );
    assert!(
        patterns
            .iter()
            .any(|value| value.eq_ignore_ascii_case(&expected_hwid_mouse_subsys_rev)),
        "{}: virtio-input hardware_id_patterns missing {expected_hwid_mouse_subsys_rev:?}. Found: {patterns:?}",
        contract_path.display()
    );

    let service_name = require_str(virtio_input, "driver_service_name");
    let virtio_device_type = virtio_input
        .get("virtio_device_type")
        .and_then(|value| value.as_u64())
        .unwrap_or_else(|| panic!("virtio_device_type must be a number"));
    assert_eq!(
        virtio_device_type,
        u64::from(VIRTIO_DEVICE_TYPE_INPUT),
        "{}: virtio-input virtio_device_type drift: contract is {virtio_device_type}, expected {}",
        contract_path.display(),
        VIRTIO_DEVICE_TYPE_INPUT
    );

    // Cross-check a few other repo-owned “consumers” so driver binding / tooling doesn't drift.
    let root = repo_root();
    assert_file_contains_case_insensitive(
        &root.join("guest-tools/config/devices.cmd"),
        &expected_hwid_short,
    );
    assert_file_contains_case_insensitive(
        &root.join("guest-tools/config/devices.cmd"),
        &expected_hwid_short_rev,
    );
    assert_file_contains_case_insensitive(
        &root.join("guest-tools/config/devices.cmd"),
        &expected_hwid_keyboard_subsys,
    );
    assert_file_contains_case_insensitive(
        &root.join("guest-tools/config/devices.cmd"),
        &expected_hwid_keyboard_subsys_rev,
    );
    assert_file_contains_case_insensitive(
        &root.join("guest-tools/config/devices.cmd"),
        &expected_hwid_mouse_subsys,
    );
    assert_file_contains_case_insensitive(
        &root.join("guest-tools/config/devices.cmd"),
        &expected_hwid_mouse_subsys_rev,
    );

    // Contract JSON specifies the canonical INF filename; it must exist in-tree and match the same HWID.
    let inf_name = require_str(virtio_input, "inf_name");
    let inf_path = root
        .join("drivers/windows/virtio-input")
        .join(inf_name);
    assert!(
        inf_path.is_file(),
        "{}: virtio-input INF referenced by windows-device-contract.json is missing: {}",
        contract_path.display(),
        inf_path.display()
    );
    assert_file_contains_case_insensitive(&inf_path, &expected_hwid_short);
    // Some INFs omit spaces around `=`, so accept both spellings.
    let addservice_with_space = format!("AddService = {service_name}");
    let addservice_no_space = format!("AddService={service_name}");
    let inf_text = fs::read_to_string(&inf_path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", inf_path.display()));
    let inf_upper = inf_text.to_ascii_uppercase();
    assert!(
        inf_upper.contains(&addservice_with_space.to_ascii_uppercase())
            || inf_upper.contains(&addservice_no_space.to_ascii_uppercase()),
        "{} is out of sync: expected to contain AddService directive for {service_name:?} ({addservice_with_space:?} or {addservice_no_space:?})",
        inf_path.display()
    );
}

const VIRTIO_DEVICE_TYPE_INPUT: u16 = 18;
