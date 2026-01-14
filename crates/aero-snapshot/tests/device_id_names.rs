use aero_snapshot::DeviceId;

#[test]
fn usb_device_id_has_stable_name() {
    assert_eq!(
        DeviceId::USB.0,
        12u32,
        "USB DeviceId number changed; must remain stable"
    );
    assert_eq!(DeviceId::USB.name(), Some("USB"));
    assert_eq!(format!("{}", DeviceId::USB), "USB(12)");
}

#[test]
fn core_device_ids_have_stable_names_and_numbers() {
    let cases = [
        (DeviceId::PIC, 1u32, "PIC"),
        (DeviceId::APIC, 2u32, "APIC"),
        (DeviceId::PIT, 3u32, "PIT"),
        (DeviceId::RTC, 4u32, "RTC"),
        (DeviceId::PCI, 5u32, "PCI"),
        (DeviceId::DISK_CONTROLLER, 6u32, "DISK_CONTROLLER"),
        (DeviceId::VGA, 7u32, "VGA"),
        (DeviceId::SERIAL, 8u32, "SERIAL"),
        (DeviceId::CPU_INTERNAL, 9u32, "CPU_INTERNAL"),
        (DeviceId::BIOS, 10u32, "BIOS"),
        (DeviceId::MEMORY, 11u32, "MEMORY"),
        (DeviceId::USB, 12u32, "USB"),
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

#[test]
fn platform_device_ids_have_stable_names_and_numbers() {
    let cases = [
        (DeviceId::I8042, 13u32, "I8042"),
        (DeviceId::PCI_CFG, 14u32, "PCI_CFG"),
        (DeviceId::PCI_INTX_ROUTER, 15u32, "PCI_INTX_ROUTER"),
        (DeviceId::ACPI_PM, 16u32, "ACPI_PM"),
        (DeviceId::HPET, 17u32, "HPET"),
        (DeviceId::HDA, 18u32, "HDA"),
        (DeviceId::E1000, 19u32, "E1000"),
        (DeviceId::NET_STACK, 20u32, "NET_STACK"),
        (DeviceId::PLATFORM_INTERRUPTS, 21u32, "PLATFORM_INTERRUPTS"),
        (DeviceId::VIRTIO_SND, 22u32, "VIRTIO_SND"),
        (DeviceId::VIRTIO_NET, 23u32, "VIRTIO_NET"),
        (DeviceId::VIRTIO_INPUT, 24u32, "VIRTIO_INPUT"),
        (DeviceId::AEROGPU, 25u32, "AEROGPU"),
        (
            DeviceId::VIRTIO_INPUT_KEYBOARD,
            26u32,
            "VIRTIO_INPUT_KEYBOARD",
        ),
        (DeviceId::VIRTIO_INPUT_MOUSE, 27u32, "VIRTIO_INPUT_MOUSE"),
        (DeviceId::GPU_VRAM, 28u32, "GPU_VRAM"),
        (DeviceId::VIRTIO_INPUT_TABLET, 29u32, "VIRTIO_INPUT_TABLET"),
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

#[test]
fn pci_intx_alias_matches_pci_intx_router() {
    assert_eq!(
        DeviceId::PCI_INTX,
        DeviceId::PCI_INTX_ROUTER,
        "PCI_INTX alias must remain identical to PCI_INTX_ROUTER for backward compatibility"
    );
    // `name()` should still report the canonical name.
    assert_eq!(DeviceId::PCI_INTX.name(), Some("PCI_INTX_ROUTER"));
}
