use aero_devices::pci::profile::NIC_E1000_82540EM;
use aero_net_e1000::E1000Device;

fn read_u8(dev: &E1000Device, offset: u16) -> u8 {
    dev.pci_config_read(offset, 1) as u8
}

fn read_u16(dev: &E1000Device, offset: u16) -> u16 {
    dev.pci_config_read(offset, 2) as u16
}

#[test]
fn e1000_pci_config_matches_canonical_profile_identity_fields() {
    let dev = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);

    assert_eq!(read_u16(&dev, 0x00), NIC_E1000_82540EM.vendor_id);
    assert_eq!(read_u16(&dev, 0x02), NIC_E1000_82540EM.device_id);
    assert_eq!(read_u8(&dev, 0x08), NIC_E1000_82540EM.revision_id);

    assert_eq!(read_u8(&dev, 0x09), NIC_E1000_82540EM.class.prog_if);
    assert_eq!(read_u8(&dev, 0x0a), NIC_E1000_82540EM.class.sub_class);
    assert_eq!(read_u8(&dev, 0x0b), NIC_E1000_82540EM.class.base_class);

    assert_eq!(read_u8(&dev, 0x0e), NIC_E1000_82540EM.header_type);
    assert_eq!(read_u16(&dev, 0x2c), NIC_E1000_82540EM.subsystem_vendor_id);
    assert_eq!(read_u16(&dev, 0x2e), NIC_E1000_82540EM.subsystem_id);

    let expected_pin = NIC_E1000_82540EM
        .interrupt_pin
        .map(|pin| pin.to_config_u8())
        .unwrap_or(0);
    assert_eq!(read_u8(&dev, 0x3d), expected_pin);
}
