use std::collections::HashSet;

use aero_devices::pci::capabilities::PCI_CAP_ID_VENDOR_SPECIFIC;
use aero_devices::pci::profile::*;
use aero_devices::pci::PciBdf;

#[test]
fn canonical_ids_and_class_codes() {
    assert_eq!(ISA_PIIX3.vendor_id, 0x8086);
    assert_eq!(ISA_PIIX3.device_id, 0x7000);
    assert_eq!(ISA_PIIX3.class.as_u32(), 0x060100);

    assert_eq!(IDE_PIIX3.vendor_id, 0x8086);
    assert_eq!(IDE_PIIX3.device_id, 0x7010);
    assert_eq!(IDE_PIIX3.class.as_u32(), 0x01018a);

    assert_eq!(USB_UHCI_PIIX3.vendor_id, 0x8086);
    assert_eq!(USB_UHCI_PIIX3.device_id, 0x7020);
    assert_eq!(USB_UHCI_PIIX3.class.as_u32(), 0x0c0300);

    assert_eq!(SATA_AHCI_ICH9.vendor_id, 0x8086);
    assert_eq!(SATA_AHCI_ICH9.device_id, 0x2922);
    assert_eq!(SATA_AHCI_ICH9.class.as_u32(), 0x010601);

    // Lock in AHCI ABAR contract details (BAR5, 8KiB MMIO window) so the guest-visible PCI profile
    // cannot drift during refactors.
    assert_eq!(AHCI_ABAR_BAR_INDEX, 5);
    assert_eq!(AHCI_ABAR_CFG_OFFSET, 0x24);
    assert_eq!(AHCI_ABAR_SIZE_U32, 0x2000);
    assert_eq!(AHCI_ABAR_SIZE, 0x2000);

    // AHCI ABAR constants should stay consistent with the canonical PCI profile.
    assert_eq!(SATA_AHCI_ICH9.bars.len(), 1);
    assert_eq!(SATA_AHCI_ICH9.bars[0].index, AHCI_ABAR_BAR_INDEX);
    assert_eq!(SATA_AHCI_ICH9.bars[0].size, AHCI_ABAR_SIZE);
    assert_eq!(u64::from(AHCI_ABAR_SIZE_U32), AHCI_ABAR_SIZE);

    assert_eq!(NVME_CONTROLLER.vendor_id, 0x1b36);
    assert_eq!(NVME_CONTROLLER.device_id, 0x0010);
    assert_eq!(NVME_CONTROLLER.class.as_u32(), 0x010802);

    assert_eq!(HDA_ICH6.vendor_id, 0x8086);
    assert_eq!(HDA_ICH6.device_id, 0x2668);
    assert_eq!(HDA_ICH6.class.as_u32(), 0x040300);

    assert_eq!(NIC_E1000_82540EM.vendor_id, 0x8086);
    assert_eq!(NIC_E1000_82540EM.device_id, 0x100e);
    assert_eq!(NIC_E1000_82540EM.class.as_u32(), 0x020000);

    assert_eq!(NIC_RTL8139.vendor_id, 0x10ec);
    assert_eq!(NIC_RTL8139.device_id, 0x8139);
    assert_eq!(NIC_RTL8139.class.as_u32(), 0x020000);

    assert_eq!(AEROGPU.vendor_id, 0xA3A0);
    assert_eq!(AEROGPU.device_id, 0x0001);
    assert_eq!(AEROGPU.class.as_u32(), 0x030000);

    assert_eq!(VIRTIO_NET.class.as_u32(), 0x020000);
    assert_eq!(VIRTIO_BLK.class.as_u32(), 0x010000);
    assert_eq!(VIRTIO_INPUT_KEYBOARD.class.as_u32(), 0x098000);
    assert_eq!(VIRTIO_INPUT_MOUSE.class.as_u32(), 0x098000);
    assert_eq!(VIRTIO_SND.class.as_u32(), 0x040100);

    // AERO-W7-VIRTIO v1: virtio-input is exposed as keyboard + mouse functions.
    assert_eq!(VIRTIO_INPUT_KEYBOARD.subsystem_id, 0x0010);
    assert_eq!(VIRTIO_INPUT_MOUSE.subsystem_id, 0x0011);
}

#[test]
fn canonical_bdfs_are_stable() {
    // BDFs are not part of Windows driver binding (which is primarily VID/DID/class), but the
    // canonical machine and driver/packaging docs assume stable device numbering for predictable
    // guest enumeration and debugging.
    //
    // In particular, `00:07.0` is reserved for the AeroGPU (A3A0:0001) device contract.
    assert_eq!(AEROGPU.bdf, PciBdf::new(0, 0x07, 0));
}

#[test]
fn virtio_ids_include_transitional_and_modern_variants() {
    assert_eq!(PCI_VENDOR_ID_VIRTIO, 0x1af4);

    assert_eq!(PCI_DEVICE_ID_VIRTIO_NET_TRANSITIONAL, 0x1000);
    assert_eq!(PCI_DEVICE_ID_VIRTIO_NET_MODERN, 0x1041);

    assert_eq!(PCI_DEVICE_ID_VIRTIO_BLK_TRANSITIONAL, 0x1001);
    assert_eq!(PCI_DEVICE_ID_VIRTIO_BLK_MODERN, 0x1042);

    assert_eq!(PCI_DEVICE_ID_VIRTIO_INPUT_TRANSITIONAL, 0x1011);
    assert_eq!(PCI_DEVICE_ID_VIRTIO_INPUT_MODERN, 0x1052);
    assert_eq!(PCI_DEVICE_ID_VIRTIO_SND_TRANSITIONAL, 0x1018);
    assert_eq!(PCI_DEVICE_ID_VIRTIO_SND_MODERN, 0x1059);
}

#[test]
fn bdfs_are_unique_in_canonical_profile() {
    let mut bdfs = HashSet::new();
    for profile in CANONICAL_IO_DEVICES {
        assert!(bdfs.insert(profile.bdf), "duplicate BDF: {:?}", profile.bdf);
    }
}

#[test]
fn config_space_builder_matches_profile() {
    for profile in CANONICAL_IO_DEVICES {
        let mut cfg = profile.build_config_space();

        assert_eq!(cfg.read(0x00, 2) as u16, profile.vendor_id);
        assert_eq!(cfg.read(0x02, 2) as u16, profile.device_id);
        assert_eq!(cfg.read(0x08, 1) as u8, profile.revision_id);
        assert_eq!(cfg.read(0x09, 1) as u8, profile.class.prog_if);
        assert_eq!(cfg.read(0x0a, 1) as u8, profile.class.sub_class);
        assert_eq!(cfg.read(0x0b, 1) as u8, profile.class.base_class);
        assert_eq!(cfg.read(0x0e, 1) as u8, profile.header_type);

        assert_eq!(cfg.read(0x2c, 2) as u16, profile.subsystem_vendor_id);
        assert_eq!(cfg.read(0x2e, 2) as u16, profile.subsystem_id);

        let expected_pin = profile.interrupt_pin.map(|p| p.to_config_u8()).unwrap_or(0);
        assert_eq!(cfg.read(0x3d, 1) as u8, expected_pin);
    }
}

#[test]
fn virtio_config_space_exposes_vendor_specific_capabilities() {
    let mut cfg = VIRTIO_NET.build_config_space();
    let cap_ptr = cfg.read(0x34, 1) as u8;
    assert_eq!(cap_ptr, 0x40);

    let caps = cfg.capability_list();
    assert_eq!(caps.len(), 4);
    assert!(caps.iter().all(|c| c.id == PCI_CAP_ID_VENDOR_SPECIFIC));
    assert_eq!(caps[0].offset, 0x40);
    assert_eq!(caps[1].offset, 0x50);
    assert_eq!(caps[2].offset, 0x64);
    assert_eq!(caps[3].offset, 0x74);

    let payload = |cfg: &mut aero_devices::pci::PciConfigSpace, off: u16| -> Vec<u8> {
        (0..cfg.read(off + 2, 1) as u8)
            .skip(2)
            .map(|i| cfg.read(off + i as u16, 1) as u8)
            .collect()
    };

    assert_eq!(payload(&mut cfg, 0x40), VIRTIO_CAP_COMMON.to_vec());
    assert_eq!(payload(&mut cfg, 0x50), VIRTIO_CAP_NOTIFY.to_vec());
    assert_eq!(payload(&mut cfg, 0x64), VIRTIO_CAP_ISR.to_vec());
    assert_eq!(payload(&mut cfg, 0x74), VIRTIO_CAP_DEVICE.to_vec());
}

#[test]
fn virtio_bar0_is_64bit_mmio() {
    let mut cfg = VIRTIO_NET.build_config_space();
    let bar0 = cfg.read(0x10, 4);
    assert_eq!(bar0 & 0x1, 0, "BAR0 must be MMIO (bit0=0)");
    assert_eq!(
        bar0 & 0x6,
        0x4,
        "BAR0 must be a 64-bit MMIO BAR (bits2:1=0b10)"
    );
    assert_eq!(
        cfg.read(0x14, 4),
        0,
        "BAR1 (high dword of BAR0) should start at 0"
    );
}

#[test]
fn virtio_bar0_probe_returns_expected_size_mask() {
    let mut cfg = VIRTIO_NET.build_config_space();

    // Standard PCI BAR size probing: write all 1s, then read back the size mask.
    cfg.write(0x10, 4, 0xffff_ffff);
    cfg.write(0x14, 4, 0xffff_ffff);

    // VIRTIO_BARS defines BAR0 as a 64-bit MMIO BAR of size 0x4000, non-prefetchable.
    // That should probe as:
    // - low dword:  mask 0xffff_c000 + 64-bit type bits2:1=0b10 (0x4) => 0xffff_c004
    // - high dword: 0xffff_ffff (since size < 4GiB)
    assert_eq!(cfg.read(0x10, 4), 0xffff_c004);
    assert_eq!(cfg.read(0x14, 4), 0xffff_ffff);
}

#[test]
fn pci_dump_includes_canonical_devices() {
    let dump = pci_dump(CANONICAL_IO_DEVICES);
    for profile in [IDE_PIIX3, SATA_AHCI_ICH9, NVME_CONTROLLER, VIRTIO_NET] {
        let prefix = format!(
            "{:02x}:{:02x}.{} {:04x}:{:04x}",
            profile.bdf.bus,
            profile.bdf.device,
            profile.bdf.function,
            profile.vendor_id,
            profile.device_id
        );
        assert!(dump.contains(&prefix), "pci_dump missing {prefix}");
    }
}
