use aero_devices::pci::profile::{
    AHCI_ABAR_CFG_OFFSET, AHCI_ABAR_SIZE_U32, IDE_PIIX3, SATA_AHCI_ICH9, USB_UHCI_PIIX3,
    NVME_CONTROLLER,
};
use aero_devices::pci::PciDevice as CanonPciDevice;
use aero_devices::pci::{PciIntxRouter, PciIntxRouterConfig};

#[cfg(feature = "legacy-audio")]
use aero_devices::pci::profile::HDA_ICH6;

#[cfg(feature = "legacy-audio")]
use emulator::io::audio::hda::HdaPciDevice;
use emulator::io::pci::PciDevice as EmuPciDevice;
use emulator::io::storage::disk::MemDisk;
use emulator::io::storage::ide::IdeController;
use emulator::io::storage::nvme::{NvmeController, NvmePciDevice};
use emulator::io::usb::uhci::{UhciController, UhciPciDevice};

fn assert_basic_identity(
    mut read: impl FnMut(u16, usize) -> u32,
    profile: aero_devices::pci::profile::PciDeviceProfile,
) {
    assert_eq!(read(0x00, 2) as u16, profile.vendor_id, "{}", profile.name);
    assert_eq!(read(0x02, 2) as u16, profile.device_id, "{}", profile.name);
    assert_eq!(read(0x08, 1) as u8, profile.revision_id, "{}", profile.name);

    assert_eq!(
        read(0x09, 1) as u8,
        profile.class.prog_if,
        "{}",
        profile.name
    );
    assert_eq!(
        read(0x0a, 1) as u8,
        profile.class.sub_class,
        "{}",
        profile.name
    );
    assert_eq!(
        read(0x0b, 1) as u8,
        profile.class.base_class,
        "{}",
        profile.name
    );

    assert_eq!(
        read(0x0e, 1) as u8,
        profile.header_type,
        "{}",
        profile.name
    );

    assert_eq!(
        read(0x2c, 2) as u16,
        profile.subsystem_vendor_id,
        "{}",
        profile.name
    );
    assert_eq!(
        read(0x2e, 2) as u16,
        profile.subsystem_id,
        "{}",
        profile.name
    );

    let expected_pin = profile
        .interrupt_pin
        .map(|pin| pin.to_config_u8())
        .unwrap_or(0);
    assert_eq!(read(0x3d, 1) as u8, expected_pin, "{}", profile.name);
}

#[test]
fn uhci_pci_config_matches_canonical_profile() {
    let uhci = UhciPciDevice::new(UhciController::new(), 0);
    assert_basic_identity(|off, size| uhci.config_read(off, size), USB_UHCI_PIIX3);

    let router = PciIntxRouter::new(PciIntxRouterConfig::default());
    let expected_pin = USB_UHCI_PIIX3
        .interrupt_pin
        .expect("profile should provide interrupt pin");
    let expected_gsi = router.gsi_for_intx(USB_UHCI_PIIX3.bdf, expected_pin);
    assert_eq!(uhci.config_read(0x3c, 1) as u8, u8::try_from(expected_gsi).unwrap());

    // UHCI uses BAR4 (I/O) at 0x20.
    assert_eq!(uhci.config_read(0x20, 4) & 0x1, 0x1);

    let mut uhci = uhci;
    uhci.config_write(0x20, 4, 0xffff_ffff);
    let mask = uhci.config_read(0x20, 4);
    assert_eq!(mask, (!(0x20u32 - 1) & 0xffff_fffc) | 0x1);

    uhci.config_write(0x20, 4, 0x1235);
    assert_eq!(uhci.io_base, 0x1220);
    assert_eq!(uhci.config_read(0x20, 4), 0x1221);
}

#[test]
fn ahci_pci_config_matches_canonical_profile() {
    let mut dev = aero_devices_storage::pci_ahci::AhciPciDevice::new(1);
    assert_basic_identity(|off, size| dev.config_mut().read(off, size), SATA_AHCI_ICH9);

    // BAR5 probe must report the implemented ABAR size.
    let abar_cfg_off: u16 = AHCI_ABAR_CFG_OFFSET as u16;
    dev.config_mut().write(abar_cfg_off, 4, 0xffff_ffff);
    let mask = dev.config_mut().read(abar_cfg_off, 4);
    assert_eq!(mask, !(AHCI_ABAR_SIZE_U32 - 1) & 0xffff_fff0);

    // BAR bases must be masked by the BAR size alignment (not just 16 bytes).
    dev.config_mut().write(abar_cfg_off, 4, 0xdead_beef);
    assert_eq!(dev.config_mut().read(abar_cfg_off, 4), 0xdead_beef & mask);
}

#[test]
#[cfg(feature = "legacy-audio")]
fn hda_pci_config_matches_canonical_profile() {
    let dev = HdaPciDevice::new(emulator::io::audio::hda::HdaController::new(), 0xfebf_0000);
    assert_basic_identity(|off, size| dev.config_read(off, size), HDA_ICH6);

    let mut dev = dev;
    dev.config_write(0x10, 4, 0xffff_ffff);
    let mask = dev.config_read(0x10, 4);
    assert_eq!(
        mask,
        !(HdaPciDevice::MMIO_BAR_SIZE - 1) & 0xffff_fff0,
        "hda BAR0 size probe mismatch"
    );

    // BAR bases must be masked by the BAR size alignment (not just 16 bytes).
    dev.config_write(0x10, 4, 0xdead_beef);
    assert_eq!(dev.config_read(0x10, 4), 0xdead_beef & mask);
}

#[test]
fn nvme_pci_config_matches_canonical_profile() {
    let disk = Box::new(MemDisk::new(16));
    let dev = NvmePciDevice::new(disk, 0xfebf_0000);
    assert_basic_identity(|off, size| dev.config_read(off, size), NVME_CONTROLLER);

    // BAR0 is a 64-bit MMIO BAR.
    assert_eq!(dev.config_read(0x10, 4) & 0x7, 0x4);

    let mut dev = dev;
    dev.config_write(0x10, 4, 0xffff_ffff);
    let mask_lo = dev.config_read(0x10, 4);
    let mask_hi = dev.config_read(0x14, 4);
    assert_eq!(
        mask_lo,
        (!(NvmeController::BAR0_SIZE as u32 - 1) & 0xffff_fff0) | 0x4
    );
    assert_eq!(mask_hi, 0xffff_ffff);

    // BAR base writes must be masked to the BAR size and preserve the read-only 64-bit indicator
    // bit (0x4) regardless of the guest-written value.
    let bar0_flags = mask_lo & 0xf;
    let bar0_addr_mask = mask_lo & !0xf;
    dev.config_write(0x10, 4, 0xdead_bee0);
    assert_eq!(
        dev.config_read(0x10, 4),
        (0xdead_bee0 & bar0_addr_mask) | bar0_flags
    );
    assert_eq!(dev.config_read(0x14, 4), 0);
}

#[test]
fn ide_pci_config_matches_canonical_profile() {
    let dev = IdeController::new(0xC000);
    assert_basic_identity(|off, size| dev.config_read(off, size), IDE_PIIX3);
}
