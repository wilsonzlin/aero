#![cfg(not(target_arch = "wasm32"))]
//! Regression test: `Machine::reset()` must preserve attached storage media backends.
//!
//! A VM "reset" should reset controller register state (deterministic power-on) but keep host-side
//! disks/ISOs attached. This test attaches:
//! - an AHCI ATA disk (port 0)
//! - an IDE secondary-master ATAPI CDROM
//!
//! Then verifies both remain readable after a `Machine::reset()`.

use aero_devices::pci::profile::{AHCI_ABAR_CFG_OFFSET, SATA_AHCI_ICH9};
use aero_devices::pci::{PciBdf, PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT};
use aero_devices_storage::ata::AtaDrive;
use aero_devices_storage::atapi::AtapiCdrom;
use aero_devices_storage::pci_ide::SECONDARY_PORTS;
use aero_machine::{Machine, MachineConfig};
use aero_storage::{MemBackend, RawDisk, VirtualDisk, SECTOR_SIZE};
use pretty_assertions::assert_eq;

fn cfg_addr(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    0x8000_0000
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | (offset as u32 & 0xFC)
}

fn write_cfg_u16(m: &mut Machine, bdf: PciBdf, offset: u8, value: u16) {
    m.io_write(
        PCI_CFG_ADDR_PORT,
        4,
        cfg_addr(bdf.bus, bdf.device, bdf.function, offset),
    );
    m.io_write(PCI_CFG_DATA_PORT, 2, u32::from(value));
}

fn write_cfg_u32(m: &mut Machine, bdf: PciBdf, offset: u8, value: u32) {
    m.io_write(
        PCI_CFG_ADDR_PORT,
        4,
        cfg_addr(bdf.bus, bdf.device, bdf.function, offset),
    );
    m.io_write(PCI_CFG_DATA_PORT, 4, value);
}

const HBA_GHC: u64 = 0x04;
const HBA_VS: u64 = 0x10;

const PORT_BASE: u64 = 0x100;
const PORT_REG_CLB: u64 = 0x00;
const PORT_REG_CLBU: u64 = 0x04;
const PORT_REG_FB: u64 = 0x08;
const PORT_REG_FBU: u64 = 0x0C;
const PORT_REG_IS: u64 = 0x10;
const PORT_REG_IE: u64 = 0x14;
const PORT_REG_CMD: u64 = 0x18;
const PORT_REG_CI: u64 = 0x38;

const GHC_IE: u32 = 1 << 1;
const GHC_AE: u32 = 1 << 31;

const PORT_CMD_ST: u32 = 1 << 0;
const PORT_CMD_FRE: u32 = 1 << 4;
const PORT_IS_DHRS: u32 = 1 << 0;

fn write_cmd_header(m: &mut Machine, clb: u64, slot: usize, ctba: u64, prdtl: u16) {
    let cfl = 5u32;
    let flags = cfl | ((prdtl as u32) << 16);
    let addr = clb + (slot as u64) * 32;
    m.write_physical_u32(addr, flags);
    m.write_physical_u32(addr + 4, 0); // PRDBC
    m.write_physical_u32(addr + 8, ctba as u32);
    m.write_physical_u32(addr + 12, (ctba >> 32) as u32);
}

fn write_prdt(m: &mut Machine, ctba: u64, entry: usize, dba: u64, dbc: u32) {
    let addr = ctba + 0x80 + (entry as u64) * 16;
    m.write_physical_u32(addr, dba as u32);
    m.write_physical_u32(addr + 4, (dba >> 32) as u32);
    m.write_physical_u32(addr + 8, 0);
    // DBC stores byte_count-1 in bits 0..21.
    m.write_physical_u32(addr + 12, (dbc - 1) & 0x003F_FFFF);
}

fn write_cfis_read_dma_ext(m: &mut Machine, ctba: u64, lba: u64, count: u16) {
    let mut cfis = [0u8; 64];
    cfis[0] = 0x27; // FIS type: Register H2D
    cfis[1] = 0x80; // Command
    cfis[2] = 0x25; // READ DMA EXT
    cfis[7] = 0x40; // LBA mode

    cfis[4] = (lba & 0xFF) as u8;
    cfis[5] = ((lba >> 8) & 0xFF) as u8;
    cfis[6] = ((lba >> 16) & 0xFF) as u8;
    cfis[8] = ((lba >> 24) & 0xFF) as u8;
    cfis[9] = ((lba >> 32) & 0xFF) as u8;
    cfis[10] = ((lba >> 40) & 0xFF) as u8;

    cfis[12] = (count & 0xFF) as u8;
    cfis[13] = (count >> 8) as u8;

    m.write_physical(ctba, &cfis);
}

fn ahci_read_sector0(m: &mut Machine) -> [u8; SECTOR_SIZE] {
    let bdf = SATA_AHCI_ICH9.bdf;
    let bar5_base: u64 = 0xE200_0000;

    // Reprogram BAR5 within the platform's PCI MMIO window (deterministic address).
    write_cfg_u32(m, bdf, AHCI_ABAR_CFG_OFFSET, bar5_base as u32);

    // Enable memory decoding + bus mastering (required for DMA processing).
    write_cfg_u16(m, bdf, 0x04, 0x0006);

    // Sanity-check that MMIO is live (AHCI VS register).
    let vs = m.read_physical_u32(bar5_base + HBA_VS);
    assert_eq!(vs, 0x0001_0300);

    // Command list / FIS / command table / read buffer.
    let clb = 0x10_000u64;
    let fb = 0x11_000u64;
    let ctba = 0x12_000u64;
    let read_buf = 0x13_000u64;

    // Program port 0 bases.
    m.write_physical_u32(bar5_base + PORT_BASE + PORT_REG_CLB, clb as u32);
    m.write_physical_u32(bar5_base + PORT_BASE + PORT_REG_CLBU, (clb >> 32) as u32);
    m.write_physical_u32(bar5_base + PORT_BASE + PORT_REG_FB, fb as u32);
    m.write_physical_u32(bar5_base + PORT_BASE + PORT_REG_FBU, (fb >> 32) as u32);

    // Enable AHCI + interrupts (interrupts are not strictly needed for the read, but enable them
    // so we also exercise IRQ clearing paths).
    m.write_physical_u32(bar5_base + HBA_GHC, GHC_AE | GHC_IE);
    m.write_physical_u32(bar5_base + PORT_BASE + PORT_REG_IE, PORT_IS_DHRS);
    m.write_physical_u32(
        bar5_base + PORT_BASE + PORT_REG_CMD,
        PORT_CMD_ST | PORT_CMD_FRE,
    );

    // READ DMA EXT (LBA 0, 1 sector).
    write_cmd_header(m, clb, 0, ctba, 1);
    write_cfis_read_dma_ext(m, ctba, 0, 1);
    write_prdt(m, ctba, 0, read_buf, SECTOR_SIZE as u32);
    m.write_physical_u32(bar5_base + PORT_BASE + PORT_REG_CI, 1);

    // Let the controller make progress (DMA).
    for _ in 0..8 {
        m.process_ahci();
        let ci = m.read_physical_u32(bar5_base + PORT_BASE + PORT_REG_CI);
        if ci == 0 {
            break;
        }
    }

    let mut out = [0u8; SECTOR_SIZE];
    out.copy_from_slice(&m.read_physical_bytes(read_buf, SECTOR_SIZE));

    // Clear the completion interrupt so INTx is not left asserted.
    m.write_physical_u32(bar5_base + PORT_BASE + PORT_REG_IS, PORT_IS_DHRS);

    out
}

fn send_atapi_packet(m: &mut Machine, base: u16, features: u8, pkt: &[u8; 12], byte_count: u16) {
    m.io_write(base + 1, 1, features as u32);
    m.io_write(base + 4, 1, (byte_count & 0xFF) as u32);
    m.io_write(base + 5, 1, (byte_count >> 8) as u32);
    m.io_write(base + 7, 1, 0xA0); // PACKET
    for i in 0..6 {
        let w = u16::from_le_bytes([pkt[i * 2], pkt[i * 2 + 1]]);
        m.io_write(base, 2, w as u32);
    }
}

fn ide_atapi_read_lba1(m: &mut Machine) -> Vec<u8> {
    let base = SECONDARY_PORTS.cmd_base;

    // Select secondary master.
    m.io_write(base + 6, 1, 0xA0);

    // Clear UNIT ATTENTION (if any): TEST UNIT READY then REQUEST SENSE.
    let tur = [0u8; 12];
    send_atapi_packet(m, base, 0, &tur, 0);
    let _ = m.io_read(base + 7, 1);

    let mut req_sense = [0u8; 12];
    req_sense[0] = 0x03;
    req_sense[4] = 18;
    send_atapi_packet(m, base, 0, &req_sense, 18);
    for _ in 0..(18 / 2) {
        let _ = m.io_read(base, 2);
    }

    // READ(10) for LBA=1, blocks=1.
    let mut read10 = [0u8; 12];
    read10[0] = 0x28;
    read10[2..6].copy_from_slice(&1u32.to_be_bytes());
    read10[7..9].copy_from_slice(&1u16.to_be_bytes());
    send_atapi_packet(m, base, 0, &read10, 2048);

    let mut out = vec![0u8; 2048];
    for i in 0..(2048 / 2) {
        let w = m.io_read(base, 2) as u16;
        out[i * 2..i * 2 + 2].copy_from_slice(&w.to_le_bytes());
    }
    out
}

#[test]
fn machine_reset_preserves_ahci_and_ide_media() {
    let mut cfg = MachineConfig::win7_storage_defaults(2 * 1024 * 1024);
    cfg.enable_serial = false;
    cfg.enable_i8042 = false;
    cfg.enable_a20_gate = false;
    cfg.enable_reset_ctrl = false;

    let mut m = Machine::new(cfg).expect("machine config should be valid");

    // Attach an AHCI disk with a recognizable boot sector.
    {
        let capacity = 8 * SECTOR_SIZE as u64;
        let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
        let mut sector0 = vec![0u8; SECTOR_SIZE];
        sector0[0..4].copy_from_slice(b"BOOT");
        sector0[510] = 0x55;
        sector0[511] = 0xAA;
        disk.write_sectors(0, &sector0).unwrap();
        m.attach_ahci_drive_port0(AtaDrive::new(Box::new(disk)).unwrap());
    }

    // Attach an IDE secondary-master ATAPI ISO with recognizable data at LBA 1.
    {
        let iso_capacity = (AtapiCdrom::SECTOR_SIZE * 2) as u64;
        let mut iso_disk = RawDisk::create(MemBackend::new(), iso_capacity).unwrap();
        iso_disk
            .write_at(AtapiCdrom::SECTOR_SIZE as u64, b"WORLD")
            .unwrap();
        let cd = AtapiCdrom::new_from_virtual_disk(Box::new(iso_disk)).unwrap();
        m.attach_ide_secondary_master_atapi(cd);
    }

    // Before reset: both devices should be readable.
    let ahci_before = ahci_read_sector0(&mut m);
    assert_eq!(&ahci_before[0..4], b"BOOT");
    assert_eq!(&ahci_before[510..512], &[0x55, 0xAA]);

    let atapi_before = ide_atapi_read_lba1(&mut m);
    assert_eq!(&atapi_before[0..5], b"WORLD");

    // Reset the whole machine (should NOT detach host backends).
    m.reset();

    // After reset: devices should still be readable without reattaching anything.
    let ahci_after = ahci_read_sector0(&mut m);
    assert_eq!(&ahci_after[0..4], b"BOOT");
    assert_eq!(&ahci_after[510..512], &[0x55, 0xAA]);

    let atapi_after = ide_atapi_read_lba1(&mut m);
    assert_eq!(&atapi_after[0..5], b"WORLD");
}
