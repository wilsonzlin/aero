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

#[test]
fn i8042_device_id_has_stable_name() {
    assert_eq!(DeviceId::I8042.name(), Some("I8042"));

    let display = format!("{}", DeviceId::I8042);
    assert!(
        display.contains("I8042("),
        "DeviceId::I8042 Display should include name, got: {display}"
    );
}
