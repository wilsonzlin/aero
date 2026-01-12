use aero_snapshot::DeviceId;

#[test]
fn usb_device_id_has_stable_name() {
    assert_eq!(
        DeviceId::USB.0, 12u32,
        "USB DeviceId number changed; must remain stable"
    );
    assert_eq!(DeviceId::USB.name(), Some("USB"));
    assert_eq!(format!("{}", DeviceId::USB), "USB(12)");
}

#[test]
fn platform_device_ids_have_stable_names_and_numbers() {
    let cases = [
        (DeviceId::I8042, 13u32, "I8042"),
        (DeviceId::PCI_CFG, 14u32, "PCI_CFG"),
        (DeviceId::PCI_INTX, 15u32, "PCI_INTX"),
        (DeviceId::ACPI_PM, 16u32, "ACPI_PM"),
        (DeviceId::HPET, 17u32, "HPET"),
        (DeviceId::HDA, 18u32, "HDA"),
        (DeviceId::E1000, 19u32, "E1000"),
        (DeviceId::NET_STACK, 20u32, "NET_STACK"),
        (DeviceId::PLATFORM_INTERRUPTS, 21u32, "PLATFORM_INTERRUPTS"),
    ];

    for (id, expected_num, expected_name) in cases {
        assert_eq!(
            id.0, expected_num,
            "{expected_name} DeviceId number changed; must remain stable"
        );
        assert_eq!(id.name(), Some(expected_name));
        assert_eq!(format!("{id}"), format!("{expected_name}({expected_num})"));
    }
}

