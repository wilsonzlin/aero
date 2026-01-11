use aero_devices::pci::profile::{
    PciDeviceProfile, PCI_VENDOR_ID_VIRTIO, VIRTIO_BLK, VIRTIO_NET, VIRTIO_SND,
};

fn parse_hex_u16(value: &str) -> u16 {
    let value = value
        .trim()
        .strip_prefix("0x")
        .or_else(|| value.trim().strip_prefix("0X"))
        .unwrap_or(value.trim());
    u16::from_str_radix(value, 16).expect("invalid u16 hex string")
}

fn find_contract_device<'a>(
    devices: &'a [serde_json::Value],
    name: &str,
) -> &'a serde_json::Value {
    devices
        .iter()
        .find(|d| d.get("device").and_then(|v| v.as_str()) == Some(name))
        .unwrap_or_else(|| panic!("windows-device-contract.json missing device entry {name:?}"))
}

fn assert_has_pattern(patterns: &[String], expected: &str) {
    assert!(
        patterns.iter().any(|p| p == expected),
        "windows-device-contract.json missing hardware_id_patterns entry {expected:?}.\n\
         Found:\n{patterns:#?}"
    );
}

fn parse_subsys(pattern: &str) -> Option<(u16, u16)> {
    // Format: PCI\VEN_1AF4&DEV_1059&SUBSYS_00191AF4 (subsys device ID first, then subsystem vendor).
    let idx = pattern.to_ascii_uppercase().find("&SUBSYS_")?;
    let start = idx + "&SUBSYS_".len();
    let hex = pattern.get(start..start + 8)?;
    let subsys_device = parse_hex_u16(&hex[0..4]);
    let subsys_vendor = parse_hex_u16(&hex[4..8]);
    Some((subsys_vendor, subsys_device))
}

fn assert_contract_matches_profile(profile: PciDeviceProfile, contract: &serde_json::Value) {
    let pci_vendor_id = contract
        .get("pci_vendor_id")
        .and_then(|v| v.as_str())
        .expect("device entry missing pci_vendor_id");
    let pci_device_id = contract
        .get("pci_device_id")
        .and_then(|v| v.as_str())
        .expect("device entry missing pci_device_id");

    assert_eq!(parse_hex_u16(pci_vendor_id), profile.vendor_id, "{}", profile.name);
    assert_eq!(parse_hex_u16(pci_device_id), profile.device_id, "{}", profile.name);

    let patterns: Vec<String> = contract
        .get("hardware_id_patterns")
        .and_then(|v| v.as_array())
        .expect("device entry missing hardware_id_patterns")
        .iter()
        .map(|v| v.as_str().expect("hardware_id_patterns must be strings").to_string())
        .collect();

    let expected_ven_dev =
        format!("PCI\\VEN_{:04X}&DEV_{:04X}", profile.vendor_id, profile.device_id);
    assert_has_pattern(&patterns, &expected_ven_dev);

    // If a subsystem-qualified pattern is present, it must match the canonical profile.
    if let Some((subsys_vendor, subsys_device)) = patterns.iter().find_map(|p| parse_subsys(p)) {
        assert_eq!(subsys_vendor, profile.subsystem_vendor_id, "{}", profile.name);
        assert_eq!(subsys_device, profile.subsystem_id, "{}", profile.name);
    } else {
        panic!(
            "expected at least one SUBSYS-qualified HWID pattern for {}",
            profile.name
        );
    }
}

#[test]
fn windows_device_contract_virtio_snd_matches_pci_profile() {
    let contract: serde_json::Value =
        serde_json::from_str(include_str!("../../../docs/windows-device-contract.json"))
            .expect("parse windows-device-contract.json");

    let devices = contract
        .get("devices")
        .and_then(|v| v.as_array())
        .expect("windows-device-contract.json missing devices array");

    let snd = find_contract_device(devices, "virtio-snd");

    assert_contract_matches_profile(VIRTIO_SND, snd);

    // Additional sanity checks for the virtio-snd driver binding contract.
    assert_eq!(VIRTIO_SND.vendor_id, PCI_VENDOR_ID_VIRTIO);
    assert_eq!(
        snd.get("driver_service_name").and_then(|v| v.as_str()),
        Some("aeroviosnd")
    );
    assert_eq!(
        snd.get("inf_name").and_then(|v| v.as_str()),
        Some("aero-virtio-snd.inf")
    );
    assert_eq!(snd.get("virtio_device_type").and_then(|v| v.as_u64()), Some(25));
}

#[test]
fn windows_device_contract_virtio_blk_matches_pci_profile() {
    let contract: serde_json::Value =
        serde_json::from_str(include_str!("../../../docs/windows-device-contract.json"))
            .expect("parse windows-device-contract.json");

    let devices = contract
        .get("devices")
        .and_then(|v| v.as_array())
        .expect("windows-device-contract.json missing devices array");

    let blk = find_contract_device(devices, "virtio-blk");

    assert_contract_matches_profile(VIRTIO_BLK, blk);

    assert_eq!(VIRTIO_BLK.vendor_id, PCI_VENDOR_ID_VIRTIO);
    assert_eq!(
        blk.get("driver_service_name").and_then(|v| v.as_str()),
        Some("aerovblk")
    );
    assert_eq!(
        blk.get("inf_name").and_then(|v| v.as_str()),
        Some("aerovblk.inf")
    );
    assert_eq!(blk.get("virtio_device_type").and_then(|v| v.as_u64()), Some(2));
}

#[test]
fn windows_device_contract_virtio_net_matches_pci_profile() {
    let contract: serde_json::Value =
        serde_json::from_str(include_str!("../../../docs/windows-device-contract.json"))
            .expect("parse windows-device-contract.json");

    let devices = contract
        .get("devices")
        .and_then(|v| v.as_array())
        .expect("windows-device-contract.json missing devices array");

    let net = find_contract_device(devices, "virtio-net");

    assert_contract_matches_profile(VIRTIO_NET, net);

    assert_eq!(VIRTIO_NET.vendor_id, PCI_VENDOR_ID_VIRTIO);
    assert_eq!(
        net.get("driver_service_name").and_then(|v| v.as_str()),
        Some("aerovnet")
    );
    assert_eq!(
        net.get("inf_name").and_then(|v| v.as_str()),
        Some("aerovnet.inf")
    );
    assert_eq!(net.get("virtio_device_type").and_then(|v| v.as_u64()), Some(1));
}
