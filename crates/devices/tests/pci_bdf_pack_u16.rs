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

        // Conversion impls are thin wrappers over the canonical helpers.
        assert_eq!(u16::from(bdf), packed);
        assert_eq!(PciBdf::from(packed), bdf);
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

#[test]
fn pci_bdf_pack_u16_matches_pci_cfg_addr_bdf_bit_layout() {
    // PCI config mechanism #1 address (0xCF8) layout:
    // - bit 31: enable
    // - bits 16..=23: bus
    // - bits 11..=15: device
    // - bits 8..=10: function
    // - bits 2..=7: register offset (dword aligned)
    // - bits 0..=1: must be 0
    //
    // `PciBdf::pack_u16()` is defined as the BDF portion of cfg_addr after shifting right by 8:
    // `(cfg_addr >> 8) & 0xFFFF`.
    fn cfg_addr(bdf: PciBdf, offset: u8) -> u32 {
        0x8000_0000
            | (u32::from(bdf.bus) << 16)
            | (u32::from(bdf.device) << 11)
            | (u32::from(bdf.function) << 8)
            | (u32::from(offset) & 0xFC)
    }

    let samples = [
        PciBdf::new(0, 0, 0),
        PciBdf::new(0, 1, 1),
        PciBdf::new(0, 2, 0),
        PciBdf::new(1, 2, 3),
        PciBdf::new(255, 31, 7),
    ];

    for bdf in samples {
        for &offset in &[0x00, 0x04, 0xFC] {
            let addr = cfg_addr(bdf, offset);
            let packed_from_cfg = ((addr >> 8) & 0xFFFF) as u16;
            assert_eq!(
                packed_from_cfg,
                bdf.pack_u16(),
                "packed BDF must match cfg_addr layout for {bdf:?} offset=0x{offset:02x}"
            );
        }
    }
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
