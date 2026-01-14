use aero_devices::pci::profile::NVME_CONTROLLER;
use aero_devices::pci::PciDevice as _;
use aero_devices_nvme::{NvmeController, NvmePciDevice};

fn read_u8(dev: &mut NvmePciDevice, offset: u16) -> u8 {
    dev.config_mut().read(offset, 1) as u8
}

fn read_u16(dev: &mut NvmePciDevice, offset: u16) -> u16 {
    dev.config_mut().read(offset, 2) as u16
}

fn read_u32(dev: &mut NvmePciDevice, offset: u16) -> u32 {
    dev.config_mut().read(offset, 4)
}

#[test]
fn nvme_pci_config_matches_canonical_profile_and_bar0_probing() {
    let mut dev = NvmePciDevice::default();

    assert_eq!(read_u16(&mut dev, 0x00), NVME_CONTROLLER.vendor_id);
    assert_eq!(read_u16(&mut dev, 0x02), NVME_CONTROLLER.device_id);
    assert_eq!(read_u8(&mut dev, 0x08), NVME_CONTROLLER.revision_id);

    assert_eq!(read_u8(&mut dev, 0x09), NVME_CONTROLLER.class.prog_if);
    assert_eq!(read_u8(&mut dev, 0x0a), NVME_CONTROLLER.class.sub_class);
    assert_eq!(read_u8(&mut dev, 0x0b), NVME_CONTROLLER.class.base_class);

    assert_eq!(read_u8(&mut dev, 0x0e), NVME_CONTROLLER.header_type);
    assert_eq!(
        read_u16(&mut dev, 0x2c),
        NVME_CONTROLLER.subsystem_vendor_id
    );
    assert_eq!(read_u16(&mut dev, 0x2e), NVME_CONTROLLER.subsystem_id);

    let expected_pin = NVME_CONTROLLER
        .interrupt_pin
        .map(|pin| pin.to_config_u8())
        .unwrap_or(0);
    assert_eq!(read_u8(&mut dev, 0x3d), expected_pin);

    // BAR0 is a 64-bit MMIO BAR.
    assert_eq!(read_u32(&mut dev, 0x10) & 0x7, 0x4);

    // BAR0 size probing should report the implemented BAR size and keep the 64-bit indicator.
    dev.config_mut().write(0x10, 4, 0xffff_ffff);
    let mask_lo = dev.config_mut().read(0x10, 4);
    let mask_hi = dev.config_mut().read(0x14, 4);
    let size = NvmeController::bar0_len() as u32;
    assert_eq!(mask_lo, (!(size - 1) & 0xffff_fff0) | 0x4);
    assert_eq!(mask_hi, 0xffff_ffff);

    // BAR base writes must be masked by the BAR size alignment and preserve the read-only type bits.
    let flags = mask_lo & 0xf;
    let addr_mask = mask_lo & !0xf;
    dev.config_mut().write(0x10, 4, 0xdead_bee0);
    assert_eq!(
        dev.config_mut().read(0x10, 4),
        (0xdead_bee0 & addr_mask) | flags
    );
    assert_eq!(dev.config_mut().read(0x14, 4), 0);
}
