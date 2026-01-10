use aero_devices::pci::profile::{HDA_ICH6, NIC_E1000_82540EM, SATA_AHCI_ICH9, USB_UHCI_PIIX3};

use emulator::io::audio::hda::HdaPciDevice;
use emulator::io::net::e1000_aero::{E1000Device, E1000PciDevice};
use emulator::io::pci::PciDevice;
use emulator::io::storage::ahci::{AhciController, AhciPciDevice};
use emulator::io::storage::disk::MemDisk;
use emulator::io::usb::uhci::{UhciController, UhciPciDevice};

fn read_u8(dev: &dyn PciDevice, offset: u16) -> u8 {
    dev.config_read(offset, 1) as u8
}

fn read_u16(dev: &dyn PciDevice, offset: u16) -> u16 {
    dev.config_read(offset, 2) as u16
}

fn read_u32(dev: &dyn PciDevice, offset: u16) -> u32 {
    dev.config_read(offset, 4)
}

fn assert_basic_identity(
    dev: &dyn PciDevice,
    profile: aero_devices::pci::profile::PciDeviceProfile,
) {
    assert_eq!(read_u16(dev, 0x00), profile.vendor_id, "{}", profile.name);
    assert_eq!(read_u16(dev, 0x02), profile.device_id, "{}", profile.name);
    assert_eq!(read_u8(dev, 0x08), profile.revision_id, "{}", profile.name);

    assert_eq!(
        read_u8(dev, 0x09),
        profile.class.prog_if,
        "{}",
        profile.name
    );
    assert_eq!(
        read_u8(dev, 0x0a),
        profile.class.sub_class,
        "{}",
        profile.name
    );
    assert_eq!(
        read_u8(dev, 0x0b),
        profile.class.base_class,
        "{}",
        profile.name
    );

    assert_eq!(read_u8(dev, 0x0e), profile.header_type, "{}", profile.name);

    assert_eq!(
        read_u16(dev, 0x2c),
        profile.subsystem_vendor_id,
        "{}",
        profile.name
    );
    assert_eq!(
        read_u16(dev, 0x2e),
        profile.subsystem_id,
        "{}",
        profile.name
    );

    let expected_pin = profile
        .interrupt_pin
        .map(|pin| pin.to_config_u8())
        .unwrap_or(0);
    assert_eq!(read_u8(dev, 0x3d), expected_pin, "{}", profile.name);
}

#[test]
fn uhci_pci_config_matches_canonical_profile() {
    let uhci = UhciPciDevice::new(UhciController::new(), 0);
    assert_basic_identity(&uhci, USB_UHCI_PIIX3);

    // UHCI uses BAR4 (I/O) at 0x20.
    assert_eq!(read_u32(&uhci, 0x20) & 0x1, 0x1);
}

#[test]
fn ahci_pci_config_matches_canonical_profile() {
    let disk = Box::new(MemDisk::new(16));
    let dev = AhciPciDevice::new(AhciController::new(disk), 0xfebf_0000);
    assert_basic_identity(&dev, SATA_AHCI_ICH9);

    // BAR5 probe must report the implemented ABAR size.
    let mut dev = dev;
    dev.config_write(0x24, 4, 0xffff_ffff);
    let mask = dev.config_read(0x24, 4);
    assert_eq!(mask, !(AhciController::ABAR_SIZE as u32 - 1) & 0xffff_fff0);
}

#[test]
fn hda_pci_config_matches_canonical_profile() {
    let dev = HdaPciDevice::new(emulator::io::audio::hda::HdaController::new(), 0xfebf_0000);
    assert_basic_identity(&dev, HDA_ICH6);

    let mut dev = dev;
    dev.config_write(0x10, 4, 0xffff_ffff);
    let mask = dev.config_read(0x10, 4);
    assert_eq!(
        mask,
        !(HdaPciDevice::MMIO_BAR_SIZE - 1) & 0xffff_fff0,
        "hda BAR0 size probe mismatch"
    );
}

#[test]
fn e1000_pci_config_matches_canonical_profile() {
    let dev = E1000PciDevice::new(E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]));
    assert_basic_identity(&dev, NIC_E1000_82540EM);
}
