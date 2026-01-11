use aero_audio::hda::HdaPciDevice;
use aero_devices::pci::profile::HDA_ICH6;

fn read_u8(dev: &HdaPciDevice, offset: u64) -> u8 {
    dev.config_read(offset, 1) as u8
}

fn read_u16(dev: &HdaPciDevice, offset: u64) -> u16 {
    dev.config_read(offset, 2) as u16
}

fn read_u32(dev: &HdaPciDevice, offset: u64) -> u32 {
    dev.config_read(offset, 4)
}

#[test]
fn hda_pci_config_matches_canonical_profile() {
    let dev = HdaPciDevice::new();

    assert_eq!(read_u16(&dev, 0x00), HDA_ICH6.vendor_id);
    assert_eq!(read_u16(&dev, 0x02), HDA_ICH6.device_id);
    assert_eq!(read_u8(&dev, 0x08), HDA_ICH6.revision_id);

    assert_eq!(read_u8(&dev, 0x09), HDA_ICH6.class.prog_if);
    assert_eq!(read_u8(&dev, 0x0a), HDA_ICH6.class.sub_class);
    assert_eq!(read_u8(&dev, 0x0b), HDA_ICH6.class.base_class);

    assert_eq!(read_u8(&dev, 0x0e), HDA_ICH6.header_type);
    assert_eq!(read_u16(&dev, 0x2c), HDA_ICH6.subsystem_vendor_id);
    assert_eq!(read_u16(&dev, 0x2e), HDA_ICH6.subsystem_id);

    let expected_pin = HDA_ICH6
        .interrupt_pin
        .map(|pin| pin.to_config_u8())
        .unwrap_or(0);
    assert_eq!(read_u8(&dev, 0x3d), expected_pin);

    // BAR0 size probing should report the HDA MMIO size.
    let mut dev = dev;
    dev.config_write(0x10, 4, 0xffff_ffff);
    assert_eq!(
        read_u32(&dev, 0x10),
        !(HdaPciDevice::MMIO_BAR_SIZE - 1) & 0xffff_fff0
    );

    dev.config_write(0x10, 4, 0xdead_beef);
    assert_eq!(read_u32(&dev, 0x10), 0xdead_bee0);
}

