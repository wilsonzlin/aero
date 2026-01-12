use aero_audio::hda_pci::HdaPciDevice;
use aero_devices::pci::profile::HDA_ICH6;
use aero_devices::pci::{
    PciBarDefinition, PciDevice, PciInterruptPin, PciIntxRouter, PciIntxRouterConfig,
};

fn read_u8(dev: &mut HdaPciDevice, offset: u16) -> u8 {
    dev.config_mut().read(offset, 1) as u8
}

fn read_u16(dev: &mut HdaPciDevice, offset: u16) -> u16 {
    dev.config_mut().read(offset, 2) as u16
}

fn read_u32(dev: &mut HdaPciDevice, offset: u16) -> u32 {
    dev.config_mut().read(offset, 4)
}

#[test]
fn hda_pci_config_matches_canonical_profile() {
    let mut dev = HdaPciDevice::new();

    assert_eq!(read_u16(&mut dev, 0x00), HDA_ICH6.vendor_id);
    assert_eq!(read_u16(&mut dev, 0x02), HDA_ICH6.device_id);
    assert_eq!(read_u8(&mut dev, 0x08), HDA_ICH6.revision_id);

    assert_eq!(read_u8(&mut dev, 0x09), HDA_ICH6.class.prog_if);
    assert_eq!(read_u8(&mut dev, 0x0a), HDA_ICH6.class.sub_class);
    assert_eq!(read_u8(&mut dev, 0x0b), HDA_ICH6.class.base_class);

    assert_eq!(read_u8(&mut dev, 0x0e), HDA_ICH6.header_type);
    assert_eq!(read_u16(&mut dev, 0x2c), HDA_ICH6.subsystem_vendor_id);
    assert_eq!(read_u16(&mut dev, 0x2e), HDA_ICH6.subsystem_id);

    let expected_pin = HDA_ICH6
        .interrupt_pin
        .map(|pin| pin.to_config_u8())
        .unwrap_or(0);
    assert_eq!(read_u8(&mut dev, 0x3d), expected_pin);

    assert_eq!(
        dev.config().bar_definition(0),
        Some(PciBarDefinition::Mmio32 {
            size: 0x4000,
            prefetchable: false,
        })
    );

    // INTx line/pin should be configurable by the platform's router.
    let router = PciIntxRouter::new(PciIntxRouterConfig {
        pirq_to_gsi: [5, 6, 7, 8],
    });
    router.configure_device_intx(HDA_ICH6.bdf, Some(PciInterruptPin::IntA), dev.config_mut());
    let expected_gsi = router.gsi_for_intx(HDA_ICH6.bdf, PciInterruptPin::IntA);
    assert_eq!(read_u8(&mut dev, 0x3c), u8::try_from(expected_gsi).unwrap());
    assert_eq!(read_u8(&mut dev, 0x3d), 1);

    // BAR0 size probing should report the HDA MMIO size.
    dev.config_mut().write(0x10, 4, 0xffff_ffff);
    assert_eq!(
        read_u32(&mut dev, 0x10),
        !(HdaPciDevice::MMIO_BAR_SIZE - 1) & 0xffff_fff0
    );

    dev.config_mut().write(0x10, 4, 0xdead_beef);
    // BAR address writes are masked both for the low flag bits and for the BAR's required
    // alignment (which is based on its size).
    assert_eq!(
        read_u32(&mut dev, 0x10),
        0xdead_beef & !(HdaPciDevice::MMIO_BAR_SIZE - 1) & 0xffff_fff0
    );
}
