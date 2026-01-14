#![cfg(not(target_arch = "wasm32"))]

use std::io::{Cursor, Read, Seek, SeekFrom};

use aero_devices::a20_gate::A20_GATE_PORT;
use aero_devices::pci::profile::{AHCI_ABAR_CFG_OFFSET, SATA_AHCI_ICH9};
use aero_devices::pci::{PciBdf, PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT};
use aero_devices_storage::ata::ATA_CMD_READ_DMA_EXT;
use aero_io_snapshot::io::storage::state::DiskControllersSnapshot;
use aero_machine::{Machine, MachineConfig};
use aero_snapshot as snapshot;
use aero_snapshot::io_snapshot_bridge::apply_io_snapshot_to_device;
use aero_storage::{MemBackend, RawDisk, VirtualDisk as _, SECTOR_SIZE};
use pretty_assertions::assert_eq;

const HBA_GHC: u64 = 0x04;
const HBA_VS: u64 = 0x10;
const PORT_BASE: u64 = 0x100;
const PORT_CLB: u64 = 0x00;
const PORT_CLBU: u64 = 0x04;
const PORT_FB: u64 = 0x08;
const PORT_FBU: u64 = 0x0C;
const PORT_IS: u64 = 0x10;
const PORT_IE: u64 = 0x14;
const PORT_CMD: u64 = 0x18;
const PORT_CI: u64 = 0x38;

const GHC_IE: u32 = 1 << 1;
const GHC_AE: u32 = 1 << 31;
const PORT_CMD_ST: u32 = 1 << 0;
const PORT_CMD_FRE: u32 = 1 << 4;
const PORT_IS_DHRS: u32 = 1 << 0;

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

fn snapshot_devices(bytes: &[u8]) -> Vec<snapshot::DeviceState> {
    let index = snapshot::inspect_snapshot(&mut Cursor::new(bytes)).unwrap();
    let devices_section = index
        .sections
        .iter()
        .find(|s| s.id == snapshot::SectionId::DEVICES)
        .expect("missing DEVICES section");

    let mut cursor = Cursor::new(bytes);
    cursor
        .seek(SeekFrom::Start(devices_section.offset))
        .unwrap();
    let mut r = cursor.take(devices_section.len);

    let count = {
        let mut buf = [0u8; 4];
        r.read_exact(&mut buf).unwrap();
        u32::from_le_bytes(buf) as usize
    };

    let mut devices = Vec::with_capacity(count);
    for _ in 0..count {
        devices.push(
            snapshot::DeviceState::decode(&mut r, snapshot::limits::MAX_DEVICE_ENTRY_LEN).unwrap(),
        );
    }
    devices
}

#[test]
fn machine_snapshot_roundtrip_preserves_ahci_inflight_dma_command_and_allows_resume() {
    const RAM_SIZE: u64 = 2 * 1024 * 1024;
    let cfg = MachineConfig {
        ram_size_bytes: RAM_SIZE,
        enable_pc_platform: true,
        enable_ahci: true,
        // Keep this test focused on AHCI + snapshot/restore.
        enable_serial: false,
        enable_i8042: false,
        enable_reset_ctrl: false,
        ..Default::default()
    };

    let mut src = Machine::new(cfg.clone()).unwrap();

    // Enable A20 before touching high MMIO addresses.
    src.io_write(A20_GATE_PORT, 1, 0x02);

    // Attach a small in-memory disk with a known marker at LBA 4.
    let mut disk = RawDisk::create(MemBackend::new(), 8 * SECTOR_SIZE as u64).unwrap();
    disk.write_at(4 * SECTOR_SIZE as u64, &[9, 8, 7, 6])
        .unwrap();
    src.attach_ahci_disk_port0(Box::new(disk)).unwrap();

    let bdf = SATA_AHCI_ICH9.bdf;
    let bar5_base: u64 = 0xE200_0000;

    // Program PCI config: BAR5 (ABAR), COMMAND.MEM + COMMAND.BME.
    write_cfg_u32(&mut src, bdf, AHCI_ABAR_CFG_OFFSET, bar5_base as u32);
    write_cfg_u16(&mut src, bdf, 0x04, 0x0006);

    // Sanity-check that MMIO is live (AHCI VS register).
    assert_eq!(src.read_physical_u32(bar5_base + HBA_VS), 0x0001_0300);

    // Guest memory layout for command list + FIS receive area + command table + DMA buffer.
    let clb = 0x1000u64;
    let fb = 0x2000u64;
    let ctba = 0x3000u64;
    let data_buf = 0x4000u64;

    // Program the AHCI registers (port 0).
    src.write_physical_u32(bar5_base + HBA_GHC, GHC_IE | GHC_AE);
    src.write_physical_u32(bar5_base + PORT_BASE + PORT_CLB, clb as u32);
    src.write_physical_u32(bar5_base + PORT_BASE + PORT_CLBU, 0);
    src.write_physical_u32(bar5_base + PORT_BASE + PORT_FB, fb as u32);
    src.write_physical_u32(bar5_base + PORT_BASE + PORT_FBU, 0);
    src.write_physical_u32(bar5_base + PORT_BASE + PORT_IE, PORT_IS_DHRS);
    src.write_physical_u32(bar5_base + PORT_BASE + PORT_CMD, PORT_CMD_ST | PORT_CMD_FRE);

    // Build a single-slot command list entry: READ DMA EXT (LBA=4, 1 sector).
    let cfl = 5u32; // 20 bytes / 4
    let prdtl = 1u32;
    let header_flags = cfl | (prdtl << 16);
    src.write_physical_u32(clb, header_flags);
    src.write_physical_u32(clb + 4, 0); // PRDBC
    src.write_physical_u32(clb + 8, ctba as u32);
    src.write_physical_u32(clb + 12, 0);

    let mut cfis = [0u8; 64];
    cfis[0] = 0x27;
    cfis[1] = 0x80;
    cfis[2] = ATA_CMD_READ_DMA_EXT;
    cfis[7] = 0x40; // LBA mode
    let lba: u64 = 4;
    cfis[4] = (lba & 0xFF) as u8;
    cfis[5] = ((lba >> 8) & 0xFF) as u8;
    cfis[6] = ((lba >> 16) & 0xFF) as u8;
    cfis[8] = ((lba >> 24) & 0xFF) as u8;
    cfis[9] = ((lba >> 32) & 0xFF) as u8;
    cfis[10] = ((lba >> 40) & 0xFF) as u8;
    cfis[12] = 1;
    src.write_physical(ctba, &cfis);

    // PRDT entry 0.
    let prd = ctba + 0x80;
    src.write_physical_u32(prd, data_buf as u32);
    src.write_physical_u32(prd + 4, 0);
    src.write_physical_u32(prd + 8, 0);
    src.write_physical_u32(prd + 12, (SECTOR_SIZE as u32 - 1) | (1 << 31));

    // Clear any prior interrupt state and issue the command.
    src.write_physical_u32(bar5_base + PORT_BASE + PORT_IS, PORT_IS_DHRS);
    src.write_physical_u32(bar5_base + PORT_BASE + PORT_CI, 1);

    // Ensure the command is still "in flight" (not completed) at the moment we take the snapshot.
    assert_eq!(src.read_physical_u32(bar5_base + PORT_BASE + PORT_CI), 1);
    assert_ne!(src.read_physical_bytes(data_buf, 4), vec![9, 8, 7, 6]);

    let snap = src.take_snapshot_full().unwrap();

    // Validate the snapshot stores the canonical DISK_CONTROLLER wrapper (`DSKC`).
    let devices = snapshot_devices(&snap);
    let disk_entries: Vec<_> = devices
        .iter()
        .filter(|d| d.id == snapshot::DeviceId::DISK_CONTROLLER)
        .collect();
    assert_eq!(
        disk_entries.len(),
        1,
        "expected exactly one DISK_CONTROLLER entry"
    );
    let disk_state = disk_entries[0];
    assert_eq!(
        disk_state.data.get(8..12).unwrap_or(&[]),
        b"DSKC",
        "expected DISK_CONTROLLER entry to contain a DSKC wrapper"
    );
    let mut wrapper = DiskControllersSnapshot::default();
    apply_io_snapshot_to_device(disk_state, &mut wrapper).unwrap();
    assert!(
        wrapper
            .controllers()
            .contains_key(&SATA_AHCI_ICH9.bdf.pack_u16()),
        "expected DSKC wrapper to include an entry for the AHCI controller BDF"
    );

    let mut restored = Machine::new(cfg).unwrap();
    restored.restore_snapshot_bytes(&snap).unwrap();

    // Host contract: controller restore drops attached disks; reattach after restoring state.
    let mut disk = RawDisk::create(MemBackend::new(), 8 * SECTOR_SIZE as u64).unwrap();
    disk.write_at(4 * SECTOR_SIZE as u64, &[9, 8, 7, 6])
        .unwrap();
    restored.attach_ahci_disk_port0(Box::new(disk)).unwrap();

    // Resume device processing and verify the DMA completes.
    for _ in 0..16 {
        restored.process_ahci();
        if restored.read_physical_u32(bar5_base + PORT_BASE + PORT_CI) == 0 {
            break;
        }
    }
    assert_eq!(
        restored.read_physical_u32(bar5_base + PORT_BASE + PORT_CI),
        0
    );

    assert_eq!(restored.read_physical_bytes(data_buf, 4), vec![9, 8, 7, 6]);
}
