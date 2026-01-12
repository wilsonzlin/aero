use aero_snapshot::DeviceId;

#[test]
fn usb_device_id_has_stable_name() {
    assert_eq!(DeviceId::USB.name(), Some("USB"));

    let display = format!("{}", DeviceId::USB);
    assert!(
        display.contains("USB("),
        "DeviceId::USB Display should include name, got: {display}"
    );
}

