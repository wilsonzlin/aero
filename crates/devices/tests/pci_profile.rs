use std::collections::HashSet;

use aero_devices::pci::capabilities::PCI_CAP_ID_VENDOR_SPECIFIC;
use aero_devices::pci::msi::PCI_CAP_ID_MSI;
use aero_devices::pci::msix::PCI_CAP_ID_MSIX;
use aero_devices::pci::profile::*;
use aero_devices::pci::{MsiCapability, PciBarDefinition, PciBdf, PciInterruptPin};
use aero_devices::usb::xhci::XhciPciDevice;
use aero_protocol::aerogpu::aerogpu_pci as protocol_pci;

#[test]
fn canonical_ids_and_class_codes() {
    assert_eq!(ISA_PIIX3.vendor_id, 0x8086);
    assert_eq!(ISA_PIIX3.device_id, 0x7000);
    assert_eq!(ISA_PIIX3.class.as_u32(), 0x060100);
    assert_eq!(ISA_PIIX3.revision_id, 0);
    assert_eq!(ISA_PIIX3.header_type, 0x80);
    assert_eq!(ISA_PIIX3.subsystem_vendor_id, 0);
    assert_eq!(ISA_PIIX3.subsystem_id, 0);
    assert_eq!(ISA_PIIX3.interrupt_pin, None);

    assert_eq!(IDE_PIIX3.vendor_id, 0x8086);
    assert_eq!(IDE_PIIX3.device_id, 0x7010);
    assert_eq!(IDE_PIIX3.class.as_u32(), 0x01018a);
    assert_eq!(IDE_PIIX3.revision_id, 0);
    assert_eq!(IDE_PIIX3.header_type, 0x00);
    assert_eq!(IDE_PIIX3.subsystem_vendor_id, 0);
    assert_eq!(IDE_PIIX3.subsystem_id, 0);
    assert_eq!(IDE_PIIX3.interrupt_pin, Some(PciInterruptPin::IntA));

    assert_eq!(USB_UHCI_PIIX3.vendor_id, 0x8086);
    assert_eq!(USB_UHCI_PIIX3.device_id, 0x7020);
    assert_eq!(USB_UHCI_PIIX3.class.as_u32(), 0x0c0300);
    assert_eq!(USB_UHCI_PIIX3.revision_id, 0);
    assert_eq!(USB_UHCI_PIIX3.header_type, 0x00);
    assert_eq!(USB_UHCI_PIIX3.subsystem_vendor_id, 0);
    assert_eq!(USB_UHCI_PIIX3.subsystem_id, 0);
    assert_eq!(USB_UHCI_PIIX3.interrupt_pin, Some(PciInterruptPin::IntA));

    assert_eq!(USB_EHCI_ICH9.vendor_id, 0x8086);
    assert_eq!(USB_EHCI_ICH9.device_id, PCI_DEVICE_ID_INTEL_ICH9_EHCI);
    assert_eq!(USB_EHCI_ICH9.class.as_u32(), 0x0c0320);

    assert_eq!(SATA_AHCI_ICH9.vendor_id, 0x8086);
    assert_eq!(SATA_AHCI_ICH9.device_id, 0x2922);
    assert_eq!(SATA_AHCI_ICH9.class.as_u32(), 0x010601);
    assert_eq!(SATA_AHCI_ICH9.revision_id, 0);
    assert_eq!(SATA_AHCI_ICH9.header_type, 0x00);
    assert_eq!(SATA_AHCI_ICH9.subsystem_vendor_id, 0);
    assert_eq!(SATA_AHCI_ICH9.subsystem_id, 0);
    assert_eq!(SATA_AHCI_ICH9.interrupt_pin, Some(PciInterruptPin::IntA));

    // AHCI ABAR (HBA registers) must stay consistent with the canonical PCI profile.
    assert_eq!(SATA_AHCI_ICH9.bars.len(), 1);
    assert_eq!(SATA_AHCI_ICH9.bars[0].index, AHCI_ABAR_BAR_INDEX);
    assert_eq!(AHCI_ABAR_CFG_OFFSET, 0x10 + 4 * AHCI_ABAR_BAR_INDEX);
    assert_eq!(SATA_AHCI_ICH9.bars[0].size, AHCI_ABAR_SIZE);
    assert_eq!(u64::from(AHCI_ABAR_SIZE_U32), AHCI_ABAR_SIZE);

    assert_eq!(NVME_CONTROLLER.vendor_id, 0x1b36);
    assert_eq!(NVME_CONTROLLER.device_id, 0x0010);
    assert_eq!(NVME_CONTROLLER.class.as_u32(), 0x010802);
    assert_eq!(NVME_CONTROLLER.revision_id, 0);
    assert_eq!(NVME_CONTROLLER.header_type, 0x00);
    assert_eq!(NVME_CONTROLLER.subsystem_vendor_id, 0);
    assert_eq!(NVME_CONTROLLER.subsystem_id, 0);
    assert_eq!(NVME_CONTROLLER.interrupt_pin, Some(PciInterruptPin::IntA));

    assert_eq!(HDA_ICH6.vendor_id, 0x8086);
    assert_eq!(HDA_ICH6.device_id, 0x2668);
    assert_eq!(HDA_ICH6.class.as_u32(), 0x040300);
    assert_eq!(HDA_ICH6.revision_id, 1);
    assert_eq!(HDA_ICH6.header_type, 0x00);
    assert_eq!(HDA_ICH6.subsystem_vendor_id, 0x8086);
    assert_eq!(HDA_ICH6.subsystem_id, 0x2668);
    assert_eq!(HDA_ICH6.interrupt_pin, Some(PciInterruptPin::IntA));

    assert_eq!(NIC_E1000_82540EM.vendor_id, 0x8086);
    assert_eq!(NIC_E1000_82540EM.device_id, 0x100e);
    assert_eq!(NIC_E1000_82540EM.class.as_u32(), 0x020000);
    assert_eq!(NIC_E1000_82540EM.revision_id, 0);
    assert_eq!(NIC_E1000_82540EM.header_type, 0x00);
    assert_eq!(NIC_E1000_82540EM.subsystem_vendor_id, 0x8086);
    assert_eq!(NIC_E1000_82540EM.subsystem_id, 0x100e);
    assert_eq!(NIC_E1000_82540EM.interrupt_pin, Some(PciInterruptPin::IntA));

    assert_eq!(NIC_RTL8139.vendor_id, 0x10ec);
    assert_eq!(NIC_RTL8139.device_id, 0x8139);
    assert_eq!(NIC_RTL8139.class.as_u32(), 0x020000);

    assert_eq!(AEROGPU.vendor_id, 0xA3A0);
    assert_eq!(AEROGPU.device_id, 0x0001);
    assert_eq!(AEROGPU.class.as_u32(), 0x030000);

    // BAR layout for AeroGPU (per `docs/16-aerogpu-vga-vesa-compat.md`):
    // - BAR0: 64KiB non-prefetchable MMIO registers
    // - BAR1: prefetchable MMIO VRAM aperture
    assert_eq!(AEROGPU.bars.len(), 2);
    assert_eq!(AEROGPU.bars[0].index, AEROGPU_BAR0_INDEX);
    assert_eq!(AEROGPU.bars[0].kind, PciBarKind::Mem32);
    assert_eq!(AEROGPU.bars[0].size, AEROGPU_BAR0_SIZE);
    assert!(!AEROGPU.bars[0].prefetchable);

    assert_eq!(AEROGPU.bars[1].index, AEROGPU_BAR1_VRAM_INDEX);
    assert_eq!(AEROGPU.bars[1].kind, PciBarKind::Mem32);
    assert_eq!(AEROGPU.bars[1].size, AEROGPU_VRAM_SIZE);
    assert!(AEROGPU.bars[1].prefetchable);

    assert_eq!(VIRTIO_NET.class.as_u32(), 0x020000);
    assert_eq!(VIRTIO_BLK.class.as_u32(), 0x010000);
    assert_eq!(VIRTIO_INPUT_KEYBOARD.class.as_u32(), 0x098000);
    assert_eq!(VIRTIO_INPUT_MOUSE.class.as_u32(), 0x098000);
    assert_eq!(VIRTIO_INPUT_TABLET.class.as_u32(), 0x098000);
    assert_eq!(VIRTIO_SND.class.as_u32(), 0x040100);

    // AERO-W7-VIRTIO v1: virtio-input is exposed as keyboard + mouse functions.
    assert_eq!(VIRTIO_INPUT_KEYBOARD.subsystem_id, 0x0010);
    assert_eq!(VIRTIO_INPUT_MOUSE.subsystem_id, 0x0011);
    assert_eq!(VIRTIO_INPUT_TABLET.subsystem_id, 0x0012);
}

#[test]
fn aerogpu_profile_matches_protocol_constants() {
    // Keep the canonical PCI profile in sync with the driver-visible ABI constants
    // (`drivers/aerogpu/protocol/aerogpu_pci.h`, mirrored via `aero-protocol`).
    assert_eq!(PCI_VENDOR_ID_AERO, protocol_pci::AEROGPU_PCI_VENDOR_ID);
    assert_eq!(
        PCI_DEVICE_ID_AERO_AEROGPU,
        protocol_pci::AEROGPU_PCI_DEVICE_ID
    );
    assert_eq!(
        AEROGPU.subsystem_vendor_id,
        protocol_pci::AEROGPU_PCI_SUBSYSTEM_VENDOR_ID
    );
    assert_eq!(AEROGPU.subsystem_id, protocol_pci::AEROGPU_PCI_SUBSYSTEM_ID);
    assert_eq!(
        AEROGPU.class.base_class,
        protocol_pci::AEROGPU_PCI_CLASS_CODE_DISPLAY_CONTROLLER
    );
    assert_eq!(
        AEROGPU.class.sub_class,
        protocol_pci::AEROGPU_PCI_SUBCLASS_VGA_COMPATIBLE
    );
    assert_eq!(AEROGPU.class.prog_if, protocol_pci::AEROGPU_PCI_PROG_IF);

    assert_eq!(
        u32::from(AEROGPU_BAR0_INDEX),
        protocol_pci::AEROGPU_PCI_BAR0_INDEX
    );
    assert_eq!(
        AEROGPU_BAR0_SIZE,
        u64::from(protocol_pci::AEROGPU_PCI_BAR0_SIZE_BYTES)
    );

    assert_eq!(
        u32::from(AEROGPU_BAR1_VRAM_INDEX),
        protocol_pci::AEROGPU_PCI_BAR1_INDEX
    );
    assert_eq!(
        AEROGPU_VRAM_SIZE,
        u64::from(protocol_pci::AEROGPU_PCI_BAR1_SIZE_BYTES)
    );
}

#[test]
fn canonical_bdfs_are_stable() {
    // BDFs are not part of Windows driver binding (which is primarily VID/DID/class), but the
    // canonical machine and driver/packaging docs assume stable device numbering for predictable
    // guest enumeration and debugging.
    //
    // In particular:
    // - `00:07.0` is reserved for the AeroGPU (A3A0:0001) device contract.
    // - `00:0c.0` is reserved for the historical Bochs/QEMU VGA PCI stub identity (1234:1111).
    // - `00:12.0` is reserved for the canonical EHCI (USB2) controller.
    assert_eq!(AEROGPU.bdf, PciBdf::new(0, 0x07, 0));
    assert_eq!(VGA_TRANSITIONAL_STUB.bdf, PciBdf::new(0, 0x0c, 0));
    assert_eq!(USB_EHCI_ICH9.bdf, PciBdf::new(0, 0x12, 0));
}

#[test]
fn aerogpu_bar_offsets_and_flags_are_stable() {
    let mut cfg = AEROGPU.build_config_space();

    // BAR0 must remain at config offset 0x10.
    let bar0 = cfg.read(0x10, 4);
    assert_eq!(bar0 & 0x1, 0, "BAR0 must be MMIO (bit0=0)");
    assert_eq!(bar0 & 0x8, 0, "BAR0 must be non-prefetchable (bit3=0)");

    // BAR1 must remain at config offset 0x14.
    let bar1 = cfg.read(0x14, 4);
    assert_eq!(bar1 & 0x1, 0, "BAR1 must be MMIO (bit0=0)");
    assert_eq!(
        bar1 & 0x8,
        0x8,
        "BAR1 must be prefetchable (bit3=1) for VRAM aperture"
    );
}

#[test]
fn aerogpu_bar_probe_returns_expected_size_masks() {
    let mut cfg = AEROGPU.build_config_space();

    // Standard PCI BAR size probing: write all 1s, then read back the size mask.
    cfg.write(0x10, 4, 0xffff_ffff);
    cfg.write(0x14, 4, 0xffff_ffff);

    // BAR0: 64KiB MMIO32, non-prefetchable.
    assert_eq!(
        cfg.read(0x10, 4),
        (!(AEROGPU_BAR0_SIZE as u32 - 1) & 0xffff_fff0),
        "AeroGPU BAR0 size probe mismatch"
    );

    // BAR1: VRAM aperture, prefetchable.
    assert_eq!(
        cfg.read(0x14, 4),
        (!(AEROGPU_VRAM_SIZE as u32 - 1) & 0xffff_fff0) | 0x8,
        "AeroGPU BAR1 size probe mismatch"
    );
}

const _: () = {
    // VBE uses a linear framebuffer inside BAR1, with a fixed offset to keep the first 256KiB
    // reserved for legacy VGA planar memory (4 × 64KiB planes).
    //
    // See `docs/16-aerogpu-vga-vesa-compat.md` (VBE_LFB_OFFSET = 0x40000 /
    // AEROGPU_PCI_BAR1_VBE_LFB_OFFSET_BYTES).
    const VBE_LFB_OFFSET: u64 = protocol_pci::AEROGPU_PCI_BAR1_VBE_LFB_OFFSET_BYTES as u64;

    // Keep BAR1 large enough to hold at least one 32bpp 4K-class framebuffer after the offset.
    // Use 4096x2160 to cover DCI 4K as well as UHD.
    const WIDTH: u64 = 4096;
    const HEIGHT: u64 = 2160;
    const BYTES_PER_PIXEL: u64 = 4;
    const LFB_BYTES: u64 = WIDTH * HEIGHT * BYTES_PER_PIXEL;

    const {
        assert!(
            AEROGPU_VRAM_SIZE >= VBE_LFB_OFFSET + LFB_BYTES,
            "AEROGPU_VRAM_SIZE too small for VBE linear framebuffer"
        );
    }
};

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
    assert!(
        !bdfs.contains(&VGA_TRANSITIONAL_STUB.bdf),
        "canonical IO device profile must not occupy reserved VGA transitional stub BDF {:?}",
        VGA_TRANSITIONAL_STUB.bdf
    );
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
    assert_eq!(caps.len(), 5);
    assert!(caps[..4].iter().all(|c| c.id == PCI_CAP_ID_VENDOR_SPECIFIC));
    assert_eq!(caps[4].id, PCI_CAP_ID_MSIX);
    assert_eq!(caps[0].offset, 0x40);
    assert_eq!(caps[1].offset, 0x50);
    assert_eq!(caps[2].offset, 0x64);
    assert_eq!(caps[3].offset, 0x74);
    assert_eq!(caps[4].offset, 0x84);

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
fn virtio_msix_capability_table_size_and_offsets_are_stable() {
    // Each virtio-pci device uses MSI-X with one vector per virtqueue + one config vector.
    //
    // Keep these values stable so:
    // - profiles remain accurate, and
    // - platform/device integrations can rely on a fixed BAR0 layout for MSI-X table/PBA.
    //
    // Table starts at BAR0+0x3100 (after the 0x3000..=0x30ff device-specific window).
    // PBA immediately follows the table and is 8-byte aligned (BAR0-backed, bir=0).
    let cases = [
        // virtio-net: 2 queues (rx/tx) + config vector = 3.
        (VIRTIO_NET, 3u16, 0x3130u32),
        // virtio-blk: 1 queue + config vector = 2.
        (VIRTIO_BLK, 2u16, 0x3120u32),
        // virtio-input: 2 queues (event/status) + config vector = 3.
        (VIRTIO_INPUT_KEYBOARD, 3u16, 0x3130u32),
        (VIRTIO_INPUT_MOUSE, 3u16, 0x3130u32),
        (VIRTIO_INPUT_TABLET, 3u16, 0x3130u32),
        // virtio-snd: 4 queues + config vector = 5.
        (VIRTIO_SND, 5u16, 0x3150u32),
    ];

    for (profile, table_size, pba_offset) in cases {
        let mut cfg = profile.build_config_space();
        let msix_off = cfg
            .find_capability(PCI_CAP_ID_MSIX)
            .expect("virtio profile should expose MSI-X capability") as u16;

        // The virtio profiles install 4 vendor-specific caps before MSI-X; keep the MSI-X cap
        // offset stable so capability ordering/packing doesn't drift.
        assert_eq!(
            msix_off, 0x84,
            "unexpected MSI-X capability offset for {}",
            profile.name
        );

        let msix_ctrl = cfg.read(msix_off + 0x02, 2) as u16;
        // Table size is encoded as N-1 in bits 0..=10.
        assert_eq!(
            msix_ctrl & 0x07ff,
            table_size - 1,
            "unexpected MSI-X table_size for {}",
            profile.name
        );

        let table = cfg.read(msix_off + 0x04, 4);
        assert_eq!(
            table, VIRTIO_MSIX_TABLE_BAR0_OFFSET,
            "unexpected MSI-X table offset for {}",
            profile.name
        );

        let pba = cfg.read(msix_off + 0x08, 4);
        assert_eq!(
            pba, pba_offset,
            "unexpected MSI-X PBA offset for {}",
            profile.name
        );
    }
}

#[test]
fn ahci_config_space_exposes_msi_capability() {
    let mut cfg = SATA_AHCI_ICH9.build_config_space();
    let cap_ptr = cfg.read(0x34, 1) as u8;
    assert_eq!(cap_ptr, 0x40);

    let caps = cfg.capability_list();
    assert_eq!(caps.len(), 1);
    assert_eq!(caps[0].id, PCI_CAP_ID_MSI);
    assert_eq!(caps[0].offset, 0x40);

    let msi_off = cfg.find_capability(PCI_CAP_ID_MSI).unwrap() as u16;
    assert_eq!(cfg.read(msi_off + 1, 1) as u8, 0);

    let msi_ctrl = cfg.read(msi_off + 0x02, 2) as u16;
    assert_eq!(msi_ctrl & 0x0001, 0, "MSI should start disabled");
    assert_ne!(msi_ctrl & (1 << 7), 0, "AHCI MSI should be 64-bit");
    assert_ne!(
        msi_ctrl & (1 << 8),
        0,
        "AHCI MSI should advertise per-vector mask/pending registers"
    );
}

#[test]
fn ahci_msi_registers_are_read_write_and_update_capability_state() {
    let mut cfg = SATA_AHCI_ICH9.build_config_space();

    let msi_off = cfg.find_capability(PCI_CAP_ID_MSI).unwrap() as u16;

    // Program message address/data and enable bit.
    cfg.write(msi_off + 0x04, 4, 0xfee0_0000);
    cfg.write(msi_off + 0x08, 4, 0);
    cfg.write(msi_off + 0x0c, 2, 0x0045);
    let ctrl = cfg.read(msi_off + 0x02, 2) as u16;
    cfg.write(msi_off + 0x02, 2, u32::from(ctrl | 0x0001));

    let msi = cfg
        .capability::<MsiCapability>()
        .expect("missing MSI capability");
    assert!(msi.enabled());
    assert_eq!(msi.message_address(), 0xfee0_0000);
    assert_eq!(msi.message_data(), 0x0045);
}

#[test]
fn nvme_config_space_exposes_msi_and_msix_capabilities() {
    let mut cfg = NVME_CONTROLLER.build_config_space();
    let cap_ptr = cfg.read(0x34, 1) as u8;
    assert_eq!(cap_ptr, 0x40);

    let caps = cfg.capability_list();
    assert_eq!(caps.len(), 2);
    assert_eq!(caps[0].id, PCI_CAP_ID_MSI);
    assert_eq!(caps[1].id, PCI_CAP_ID_MSIX);
    assert_eq!(caps[0].offset, 0x40);
    assert_eq!(caps[1].offset, 0x58);

    let msi_off = cfg.find_capability(PCI_CAP_ID_MSI).unwrap() as u16;
    let msix_off = cfg.find_capability(PCI_CAP_ID_MSIX).unwrap() as u16;
    assert_eq!(cfg.read(msi_off + 1, 1) as u8, msix_off as u8);

    let msi_ctrl = cfg.read(msi_off + 0x02, 2) as u16;
    assert_eq!(msi_ctrl & 0x0001, 0, "MSI should start disabled");
    assert_ne!(msi_ctrl & (1 << 7), 0, "NVMe MSI should be 64-bit");
    assert_ne!(
        msi_ctrl & (1 << 8),
        0,
        "NVMe MSI should advertise per-vector mask/pending registers"
    );

    let msix_ctrl = cfg.read(msix_off + 0x02, 2) as u16;
    // Table size is encoded as N-1 in bits 0..=10; NVMe exposes one entry.
    assert_eq!(msix_ctrl & 0x07ff, 0);
    assert_eq!(msix_ctrl & (1 << 15), 0, "MSI-X should start disabled");

    let table = cfg.read(msix_off + 0x04, 4);
    assert_eq!(table, 0x3000);
    let pba = cfg.read(msix_off + 0x08, 4);
    assert_eq!(pba, 0x3010);
}

#[test]
fn xhci_config_space_exposes_msi_and_msix_capabilities() {
    let mut cfg = USB_XHCI_QEMU.build_config_space();
    let cap_ptr = cfg.read(0x34, 1) as u8;
    assert_eq!(cap_ptr, 0x40);

    let caps = cfg.capability_list();
    assert_eq!(caps.len(), 2);
    assert_eq!(caps[0].id, PCI_CAP_ID_MSI);
    assert_eq!(caps[1].id, PCI_CAP_ID_MSIX);
    assert_eq!(caps[0].offset, 0x40);
    assert_eq!(caps[1].offset, 0x58);

    let msi_off = cfg.find_capability(PCI_CAP_ID_MSI).unwrap() as u16;
    let msix_off = cfg.find_capability(PCI_CAP_ID_MSIX).unwrap() as u16;
    assert_eq!(cfg.read(msi_off + 1, 1) as u8, msix_off as u8);

    let msi_ctrl = cfg.read(msi_off + 0x02, 2) as u16;
    assert_eq!(msi_ctrl & 0x0001, 0, "MSI should start disabled");
    assert_ne!(msi_ctrl & (1 << 7), 0, "xHCI MSI should be 64-bit");
    assert_ne!(
        msi_ctrl & (1 << 8),
        0,
        "xHCI MSI should advertise per-vector mask/pending registers"
    );

    let msix_ctrl = cfg.read(msix_off + 0x02, 2) as u16;
    // Table size is encoded as N-1 in bits 0..=10; xHCI exposes one entry.
    assert_eq!(msix_ctrl & 0x07ff, 0);
    assert_eq!(msix_ctrl & (1 << 15), 0, "MSI-X should start disabled");

    let table = cfg.read(msix_off + 0x04, 4);
    assert_eq!(table, 0x8000);
    let pba = cfg.read(msix_off + 0x08, 4);
    assert_eq!(pba, 0x9000);
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
fn xhci_profile_class_code_and_bar0_definition() {
    assert_eq!(USB_XHCI_QEMU.class.as_u32(), 0x0c0330);
    assert_eq!(USB_XHCI_QEMU.vendor_id, PCI_VENDOR_ID_REDHAT_QEMU);
    assert_eq!(USB_XHCI_QEMU.device_id, PCI_DEVICE_ID_QEMU_XHCI);

    assert_eq!(USB_XHCI_QEMU.bars.len(), 1);
    assert_eq!(USB_XHCI_QEMU.bars[0].index, 0);
    assert_eq!(USB_XHCI_QEMU.bars[0].size, XHCI_MMIO_BAR_SIZE);
    assert_eq!(
        u32::try_from(XHCI_MMIO_BAR_SIZE).expect("xHCI BAR size should fit in u32"),
        XhciPciDevice::MMIO_BAR_SIZE,
        "xHCI PCI profile BAR0 size must match XhciPciDevice::MMIO_BAR_SIZE"
    );

    let cfg = USB_XHCI_QEMU.build_config_space();
    assert_eq!(
        cfg.bar_definition(0),
        Some(PciBarDefinition::Mmio32 {
            size: u32::try_from(XHCI_MMIO_BAR_SIZE).expect("xHCI BAR size should fit in u32"),
            prefetchable: false,
        })
    );
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
