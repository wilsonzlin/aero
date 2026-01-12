use aero_devices::pci::{profile, PciBdf};

#[test]
fn pci_bdf_pack_unpack_u16_roundtrip() {
    let samples = [
        PciBdf::new(0, 0, 0),
        PciBdf::new(0, 2, 0),
        PciBdf::new(1, 2, 3),
        PciBdf::new(255, 31, 7),
    ];

    for bdf in samples {
        let packed = bdf.pack_u16();
        let unpacked = PciBdf::unpack_u16(packed);
        assert_eq!(unpacked, bdf);
    }
}

#[test]
fn pci_bdf_pack_u16_matches_profile_constants() {
    // AHCI at 00:02.0
    assert_eq!(profile::SATA_AHCI_ICH9.bdf, PciBdf::new(0, 2, 0));
    assert_eq!(profile::SATA_AHCI_ICH9.bdf.pack_u16(), 0x0010);

    // PIIX3 IDE at 00:01.1 (multi-function)
    assert_eq!(profile::IDE_PIIX3.bdf, PciBdf::new(0, 1, 1));
    assert_eq!(profile::IDE_PIIX3.bdf.pack_u16(), 0x0009);
}

#[cfg(debug_assertions)]
#[test]
#[should_panic]
fn pci_bdf_pack_u16_rejects_invalid_device_in_debug() {
    // Valid PCI device numbers are 0-31.
    let _ = PciBdf::new(0, 32, 0).pack_u16();
}

#[cfg(debug_assertions)]
#[test]
#[should_panic]
fn pci_bdf_pack_u16_rejects_invalid_function_in_debug() {
    // Valid PCI function numbers are 0-7.
    let _ = PciBdf::new(0, 0, 8).pack_u16();
}

