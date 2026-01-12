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

#[test]
fn canonical_platform_device_ids_have_stable_names() {
    for (id, name) in [
        (DeviceId::PCI_CFG, "PCI_CFG"),
        (DeviceId::PCI_INTX, "PCI_INTX"),
        (DeviceId::ACPI_PM, "ACPI_PM"),
        (DeviceId::HPET, "HPET"),
    ] {
        assert_eq!(id.name(), Some(name));
        let display = format!("{id}");
        assert!(
            display.contains(&format!("{name}(")),
            "DeviceId::{name} Display should include name, got: {display}"
        );
    }
}
