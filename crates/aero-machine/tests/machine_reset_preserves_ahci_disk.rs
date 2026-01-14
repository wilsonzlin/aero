#![cfg(not(target_arch = "wasm32"))]

use aero_devices::a20_gate::A20_GATE_PORT;
use aero_devices::pci::profile::{AHCI_ABAR_CFG_OFFSET, SATA_AHCI_ICH9};
use aero_devices::pci::{PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT};
use aero_devices_storage::ata::ATA_CMD_READ_DMA_EXT;
use aero_machine::{Machine, MachineConfig};
use aero_storage::{MemBackend, RawDisk, VirtualDisk, SECTOR_SIZE};

fn cfg_addr(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    0x8000_0000
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | (offset as u32 & 0xFC)
}

fn write_cfg_u16(m: &mut Machine, bus: u8, device: u8, function: u8, offset: u8, value: u16) {
    m.io_write(
        PCI_CFG_ADDR_PORT,
        4,
        cfg_addr(bus, device, function, offset),
    );
    m.io_write(PCI_CFG_DATA_PORT, 2, u32::from(value));
}

fn write_cfg_u32(m: &mut Machine, bus: u8, device: u8, function: u8, offset: u8, value: u32) {
    m.io_write(
        PCI_CFG_ADDR_PORT,
        4,
        cfg_addr(bus, device, function, offset),
    );
    m.io_write(PCI_CFG_DATA_PORT, 4, value);
}

const HBA_GHC: u64 = 0x04;

const PORT_BASE: u64 = 0x100;

const PORT_REG_CLB: u64 = 0x00;
const PORT_REG_CLBU: u64 = 0x04;
const PORT_REG_FB: u64 = 0x08;
const PORT_REG_FBU: u64 = 0x0C;
const PORT_REG_CMD: u64 = 0x18;
const PORT_REG_CI: u64 = 0x38;

const GHC_AE: u32 = 1 << 31;
const PORT_CMD_ST: u32 = 1 << 0;
const PORT_CMD_FRE: u32 = 1 << 4;

fn write_cmd_header(m: &mut Machine, clb: u64, slot: usize, ctba: u64, prdtl: u16, write: bool) {
    let cfl = 5u32;
    let w = if write { 1u32 << 6 } else { 0 };
    let flags = cfl | w | ((prdtl as u32) << 16);
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
    // DBC field stores byte_count-1 in bits 0..21.
    m.write_physical_u32(addr + 12, (dbc - 1) & 0x003F_FFFF);
}

fn write_cfis(m: &mut Machine, ctba: u64, command: u8, lba: u64, count: u16) {
    let mut cfis = [0u8; 64];
    cfis[0] = 0x27;
    cfis[1] = 0x80;
    cfis[2] = command;
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

#[test]
fn machine_reset_preserves_ahci_disk_port0_backend() {
    const RAM_SIZE: u64 = 2 * 1024 * 1024;

    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: RAM_SIZE,
        enable_pc_platform: true,
        enable_ahci: true,
        // Keep this test focused on storage reset semantics.
        enable_serial: false,
        enable_i8042: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    // Enable A20 before touching high MMIO addresses.
    m.io_write(A20_GATE_PORT, 1, 0x02);

    // Attach a disk with a recognizable marker to AHCI port 0.
    let capacity = 8 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut sector0 = vec![0u8; SECTOR_SIZE];
    sector0[0..8].copy_from_slice(b"RST-AHCI");
    disk.write_sectors(0, &sector0).unwrap();
    m.attach_ahci_disk_port0(Box::new(disk)).unwrap();

    // Soft reset should preserve attached disks (it should reset controller state in-place).
    m.reset();

    // Reset clears A20 again; re-enable before touching high MMIO addresses.
    m.io_write(A20_GATE_PORT, 1, 0x02);

    // Program the AHCI controller and read back LBA0 via DMA.
    let bdf = SATA_AHCI_ICH9.bdf;
    let bar5_base: u64 = 0xE200_0000;

    // Reprogram BAR5 within the machine's PCI MMIO window (deterministic address).
    write_cfg_u32(
        &mut m,
        bdf.bus,
        bdf.device,
        bdf.function,
        AHCI_ABAR_CFG_OFFSET,
        bar5_base as u32,
    );

    // Enable memory decoding + bus mastering (required for DMA processing).
    write_cfg_u16(&mut m, bdf.bus, bdf.device, bdf.function, 0x04, 0x0006);

    let clb = 0x1000u64;
    let fb = 0x2000u64;
    let ctba = 0x3000u64;
    let read_buf = 0x4000u64;

    m.write_physical_u32(bar5_base + PORT_BASE + PORT_REG_CLB, clb as u32);
    m.write_physical_u32(bar5_base + PORT_BASE + PORT_REG_CLBU, (clb >> 32) as u32);
    m.write_physical_u32(bar5_base + PORT_BASE + PORT_REG_FB, fb as u32);
    m.write_physical_u32(bar5_base + PORT_BASE + PORT_REG_FBU, (fb >> 32) as u32);

    m.write_physical_u32(bar5_base + HBA_GHC, GHC_AE);
    m.write_physical_u32(
        bar5_base + PORT_BASE + PORT_REG_CMD,
        PORT_CMD_ST | PORT_CMD_FRE,
    );

    // READ DMA EXT for LBA 0 (sector 0).
    write_cmd_header(&mut m, clb, 0, ctba, 1, false);
    write_cfis(&mut m, ctba, ATA_CMD_READ_DMA_EXT, 0, 1);
    write_prdt(&mut m, ctba, 0, read_buf, SECTOR_SIZE as u32);
    m.write_physical_u32(bar5_base + PORT_BASE + PORT_REG_CI, 1);

    // Process the controller until the command completes.
    for _ in 0..32 {
        m.process_ahci();
        if m.read_physical_u32(bar5_base + PORT_BASE + PORT_REG_CI) == 0 {
            break;
        }
    }
    assert_eq!(
        m.read_physical_u32(bar5_base + PORT_BASE + PORT_REG_CI),
        0,
        "AHCI read did not complete (disk may have been detached/replaced by reset)"
    );

    let got = m.read_physical_bytes(read_buf, SECTOR_SIZE);
    assert_eq!(&got[0..8], b"RST-AHCI");
}
