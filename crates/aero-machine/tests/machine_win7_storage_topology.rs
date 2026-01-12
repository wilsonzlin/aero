#![cfg(not(target_arch = "wasm32"))]

//! Guards the canonical Windows 7 storage PCI topology for `aero_machine::Machine` against drift.
//!
//! If you update any of these values, also update:
//! - `docs/05-storage-topology-win7.md`
//! - `crates/devices/tests/win7_storage_topology.rs`
//! - `crates/aero-pc-platform/tests/pc_platform_win7_storage.rs`

use aero_devices::pci::profile::{IDE_PIIX3, ISA_PIIX3, NVME_CONTROLLER, SATA_AHCI_ICH9};
use aero_machine::{Machine, MachineConfig};
use pretty_assertions::assert_eq;

fn cfg_addr(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    // PCI config mechanism #1: 0x8000_0000 | bus<<16 | dev<<11 | fn<<8 | (offset & 0xFC)
    0x8000_0000
        | (u32::from(bus) << 16)
        | (u32::from(device & 0x1F) << 11)
        | (u32::from(function & 0x07) << 8)
        | (u32::from(offset) & 0xFC)
}

fn read_cfg_u32(m: &mut Machine, bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    m.io_write(0xCF8, 4, cfg_addr(bus, device, function, offset));
    m.io_read(0xCFC, 4)
}

#[test]
fn machine_win7_storage_has_ahci_and_ide_on_canonical_bdfs() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_ahci: true,
        enable_ide: true,
        // Keep the machine deterministic for PCI topology assertions.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    // AHCI at 00:02.0
    {
        let bdf = SATA_AHCI_ICH9.bdf;
        let id = read_cfg_u32(&mut m, bdf.bus, bdf.device, bdf.function, 0x00);
        assert_eq!(id & 0xffff, u32::from(SATA_AHCI_ICH9.vendor_id));
        assert_eq!((id >> 16) & 0xffff, u32::from(SATA_AHCI_ICH9.device_id));

        let int_line = read_cfg_u32(&mut m, bdf.bus, bdf.device, bdf.function, 0x3C) & 0xFF;
        assert_eq!(int_line, 12);
    }

    // IDE at 00:01.1
    {
        let bdf = IDE_PIIX3.bdf;
        let id = read_cfg_u32(&mut m, bdf.bus, bdf.device, bdf.function, 0x00);
        assert_eq!(id & 0xffff, u32::from(IDE_PIIX3.vendor_id));
        assert_eq!((id >> 16) & 0xffff, u32::from(IDE_PIIX3.device_id));

        let int_line = read_cfg_u32(&mut m, bdf.bus, bdf.device, bdf.function, 0x3C) & 0xFF;
        assert_eq!(int_line, 11);
    }

    // ISA bridge function at 00:01.0 should exist when IDE is enabled, with the multi-function
    // bit set (header type bit 7) so OSes enumerate function 1.
    {
        let bdf = ISA_PIIX3.bdf;
        let id = read_cfg_u32(&mut m, bdf.bus, bdf.device, bdf.function, 0x00);
        assert_eq!(id & 0xffff, u32::from(ISA_PIIX3.vendor_id));
        assert_eq!((id >> 16) & 0xffff, u32::from(ISA_PIIX3.device_id));

        let header = read_cfg_u32(&mut m, bdf.bus, bdf.device, bdf.function, 0x0C);
        let header_type = ((header >> 16) & 0xFF) as u8;
        assert_eq!(header_type & 0x80, 0x80);
    }

    // NVMe at 00:03.0 is optional and is off by default for Win7 (no inbox NVMe driver).
    {
        let bdf = NVME_CONTROLLER.bdf;
        let id = read_cfg_u32(&mut m, bdf.bus, bdf.device, bdf.function, 0x00);
        assert_eq!(id, 0xffff_ffff);
    }
}
