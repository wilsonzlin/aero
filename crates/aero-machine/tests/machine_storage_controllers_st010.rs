#![cfg(not(target_arch = "wasm32"))]

//! ST-010-style storage controller integration tests for `aero_machine::Machine`.
//!
//! These are intentionally "host-driven" (no guest code) and validate the machine wiring:
//! - PCI config ports + BAR assignment
//! - MMIO/I/O access through the machine buses
//! - DMA into guest RAM
//! - interrupt routing:
//!   - AHCI via PCI INTx -> PciIntxRouter -> GSI12 -> PIC IRQ12
//!   - PIIX3 IDE via ISA IRQ14/15 (not PCI INTx)

use aero_devices::pci::{profile, PciBdf};
use aero_machine::{Machine, MachineConfig};
use aero_platform::interrupts::PlatformInterruptMode;
use aero_snapshot as snapshot;
use aero_storage::{MemBackend, RawDisk, VirtualDisk as _, SECTOR_SIZE};
use pretty_assertions::assert_eq;
use std::io::{Cursor, Read};

use aero_devices_storage::pci_ide::{PRIMARY_PORTS, SECONDARY_PORTS};
use aero_io_snapshot::io::state::IoSnapshot as _;
use aero_io_snapshot::io::storage::dskc::DiskControllersSnapshot;

fn machine_cfg_ahci_only() -> MachineConfig {
    MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_ahci: true,
        enable_ide: false,
        // Keep deterministic and focused.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: true,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    }
}

fn machine_cfg_ide_only() -> MachineConfig {
    MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_ahci: false,
        enable_ide: true,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: true,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    }
}

fn enable_a20(m: &mut Machine) {
    // Fast A20 gate at port 0x92: bit1 enables A20.
    m.io_write(0x92, 1, 0x02);
}

fn cfg_addr(bdf: PciBdf, offset: u16) -> u32 {
    0x8000_0000
        | (u32::from(bdf.bus) << 16)
        | (u32::from(bdf.device & 0x1F) << 11)
        | (u32::from(bdf.function & 0x07) << 8)
        | (u32::from(offset) & 0xFC)
}

fn cfg_read(m: &mut Machine, bdf: PciBdf, offset: u16, size: u8) -> u32 {
    m.io_write(0xCF8, 4, cfg_addr(bdf, offset));
    m.io_read(0xCFC + (offset & 3), size)
}

fn cfg_write(m: &mut Machine, bdf: PciBdf, offset: u16, size: u8, value: u32) {
    m.io_write(0xCF8, 4, cfg_addr(bdf, offset));
    m.io_write(0xCFC + (offset & 3), size, value);
}

fn setup_pic_for_irq(m: &mut Machine, irq: u8) {
    let interrupts = m.platform_interrupts().expect("pc platform enabled");
    let mut ints = interrupts.borrow_mut();
    ints.set_mode(PlatformInterruptMode::LegacyPic);
    ints.pic_mut().set_offsets(0x20, 0x28);
    // Mask everything for determinism.
    for i in 0..16 {
        ints.pic_mut().set_masked(i, true);
    }
    // Unmask cascade + the IRQ under test.
    ints.pic_mut().set_masked(2, false);
    ints.pic_mut().set_masked(irq, false);
}

#[test]
fn st010_machine_ahci_read_dma_ext_and_irq12_routing() {
    let mut m = Machine::new(machine_cfg_ahci_only()).unwrap();
    enable_a20(&mut m);

    // Keep the CPU from acknowledging interrupts so we can observe PIC pending state.
    m.cpu_mut().set_rflags(0x2); // IF=0
    m.cpu_mut().halted = true;

    // Observe AHCI INTx via PIC IRQ12.
    setup_pic_for_irq(&mut m, 12);

    // Attach a small in-memory disk with a marker at LBA 4.
    let mut disk = RawDisk::create(MemBackend::new(), 8 * SECTOR_SIZE as u64).unwrap();
    disk.write_at(4 * SECTOR_SIZE as u64, &[9, 8, 7, 6])
        .unwrap();
    m.attach_ahci_disk_port0(Box::new(disk)).unwrap();

    let bdf = profile::SATA_AHCI_ICH9.bdf;

    // BAR5 assignment (ABAR).
    let abar_cfg_off = u16::from(profile::AHCI_ABAR_CFG_OFFSET);
    let bar5 = cfg_read(&mut m, bdf, abar_cfg_off, 4);
    assert_eq!(bar5 & 0x1, 0, "BAR5 must be MMIO");
    let bar5_base = u64::from(bar5 & 0xFFFF_FFF0);
    assert_ne!(bar5_base, 0);
    assert_eq!(bar5_base % profile::AHCI_ABAR_SIZE, 0);

    // Enable MMIO decoding and bus mastering (DMA).
    let mut cmd = cfg_read(&mut m, bdf, 0x04, 2) as u16;
    cmd |= 0x0006; // MEM + BUSMASTER
    cfg_write(&mut m, bdf, 0x04, 2, u32::from(cmd));

    // Sanity-check that MMIO is live (AHCI VS register).
    assert_eq!(m.read_physical_u32(bar5_base + 0x10), 0x0001_0300);

    // Guest memory layout.
    let clb = 0x10000u64;
    let fb = 0x11000u64;
    let ctba = 0x12000u64;
    let data_buf = 0x13000u64;

    // AHCI register programming (port 0).
    const HBA_GHC: u64 = 0x04;
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
    const PORT_IS_DHRS: u32 = 1 << 0;
    const PORT_CMD_ST: u32 = 1 << 0;
    const PORT_CMD_FRE: u32 = 1 << 4;

    m.write_physical_u32(bar5_base + HBA_GHC, GHC_IE | GHC_AE);
    m.write_physical_u32(bar5_base + PORT_BASE + PORT_CLB, clb as u32);
    m.write_physical_u32(bar5_base + PORT_BASE + PORT_CLBU, 0);
    m.write_physical_u32(bar5_base + PORT_BASE + PORT_FB, fb as u32);
    m.write_physical_u32(bar5_base + PORT_BASE + PORT_FBU, 0);
    m.write_physical_u32(bar5_base + PORT_BASE + PORT_IE, PORT_IS_DHRS);
    m.write_physical_u32(bar5_base + PORT_BASE + PORT_CMD, PORT_CMD_ST | PORT_CMD_FRE);

    // Build a single-slot command list: READ DMA EXT (LBA=4, 1 sector).
    const ATA_CMD_READ_DMA_EXT: u8 = aero_devices_storage::ata::ATA_CMD_READ_DMA_EXT;

    let cfl = 5u32; // 20 bytes / 4
    let prdtl = 1u32;
    let header_flags = cfl | (prdtl << 16);
    m.write_physical_u32(clb, header_flags);
    m.write_physical_u32(clb + 4, 0); // PRDBC
    m.write_physical_u32(clb + 8, ctba as u32);
    m.write_physical_u32(clb + 12, 0);

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
    m.write_physical(ctba, &cfis);

    // PRDT entry 0.
    let prd = ctba + 0x80;
    m.write_physical_u32(prd, data_buf as u32);
    m.write_physical_u32(prd + 4, 0);
    m.write_physical_u32(prd + 8, 0);
    // DBC is stored as byte_count-1; set IOC (bit31) for realism.
    m.write_physical_u32(prd + 12, (SECTOR_SIZE as u32 - 1) | (1 << 31));

    // Clear any prior interrupt state and issue the command.
    m.write_physical_u32(bar5_base + PORT_BASE + PORT_IS, PORT_IS_DHRS);
    m.write_physical_u32(bar5_base + PORT_BASE + PORT_CI, 1);

    // Tick until DMA completes and the interrupt is asserted.
    for _ in 0..10 {
        let _ = m.run_slice(1);
        if m.read_physical_bytes(data_buf, 4) == vec![9, 8, 7, 6] {
            break;
        }
    }

    assert_eq!(m.read_physical_bytes(data_buf, 4), vec![9, 8, 7, 6]);

    let interrupts = m.platform_interrupts().unwrap();
    let pending_vec = interrupts.borrow_mut().pic_mut().get_pending_vector();
    assert_eq!(pending_vec, Some(0x2C)); // 0x28 + (IRQ12-8)
}

#[test]
fn st010_machine_ide_primary_ata_dma_and_irq14_routing() {
    let mut m = Machine::new(machine_cfg_ide_only()).unwrap();

    // Keep the CPU from acknowledging interrupts so we can observe PIC pending state.
    m.cpu_mut().set_rflags(0x2); // IF=0
    m.cpu_mut().halted = true;

    setup_pic_for_irq(&mut m, 14);

    // Attach a small ATA disk on IDE primary master (optional topology slot).
    let mut disk = RawDisk::create(MemBackend::new(), SECTOR_SIZE as u64).unwrap();
    let mut sector0 = [0u8; SECTOR_SIZE];
    for (i, b) in sector0.iter_mut().enumerate().take(32) {
        *b = (i as u8).wrapping_add(1);
    }
    disk.write_at(0, &sector0).unwrap();
    m.attach_ide_primary_master_disk(Box::new(disk)).unwrap();

    let bdf = profile::IDE_PIIX3.bdf;

    // Enable I/O decoding and bus mastering (DMA).
    let mut cmd = cfg_read(&mut m, bdf, 0x04, 2) as u16;
    cmd |= 0x0005; // IO + BUSMASTER
    cfg_write(&mut m, bdf, 0x04, 2, u32::from(cmd));

    // Bus Master IDE BAR4.
    let bar4 = cfg_read(&mut m, bdf, 0x20, 4);
    assert_eq!(bar4 & 0x1, 0x1, "BAR4 must be I/O");
    let bm_base = (bar4 & 0xFFFF_FFFC) as u16;
    assert_ne!(bm_base, 0);

    // PRD table and DMA buffer in guest RAM.
    let prd_addr = 0x2000u64;
    let dma_buf = 0x3000u64;
    m.write_physical_u32(prd_addr, dma_buf as u32);
    m.write_physical_u16(prd_addr + 4, SECTOR_SIZE as u16);
    m.write_physical_u16(prd_addr + 6, 0x8000); // EOT

    // Clear BMIDE status (IRQ/error) and program PRD pointer (primary channel).
    m.io_write(bm_base + 2, 1, 0x06);
    m.io_write(bm_base + 4, 4, prd_addr as u32);

    // READ DMA (LBA 0, 1 sector).
    let pri_cmd = PRIMARY_PORTS.cmd_base;
    m.io_write(pri_cmd + 6, 1, 0xE0); // master, LBA
    m.io_write(pri_cmd + 2, 1, 1); // sector count
    m.io_write(pri_cmd + 3, 1, 0); // LBA0
    m.io_write(pri_cmd + 4, 1, 0); // LBA1
    m.io_write(pri_cmd + 5, 1, 0); // LBA2
    m.io_write(pri_cmd + 7, 1, 0xC8); // READ DMA

    m.io_write(bm_base, 1, 0x09); // start + direction=read

    // Tick once; the device model completes DMA synchronously in `tick`.
    let _ = m.run_slice(1);

    let out = m.read_physical_bytes(dma_buf, 32);
    let expected: Vec<u8> = (1u8..=32).collect();
    assert_eq!(out, expected);

    let interrupts = m.platform_interrupts().unwrap();
    let pending_vec = interrupts.borrow_mut().pic_mut().get_pending_vector();
    assert_eq!(pending_vec, Some(0x2E)); // 0x28 + (IRQ14-8)
}

fn wait_drq(m: &mut Machine, cmd_base: u16) {
    for _ in 0..1000 {
        let st = m.io_read(cmd_base + 7, 1) as u8;
        if (st & 0x80) == 0 && (st & 0x08) != 0 {
            return;
        }
    }
    panic!("timeout waiting for DRQ on IDE port {cmd_base:#x}");
}

fn atapi_send_packet(
    m: &mut Machine,
    cmd_base: u16,
    features: u8,
    pkt: &[u8; 12],
    byte_count: u16,
) {
    m.io_write(cmd_base + 1, 1, u32::from(features));
    m.io_write(cmd_base + 4, 1, u32::from(byte_count & 0xFF));
    m.io_write(cmd_base + 5, 1, u32::from(byte_count >> 8));
    m.io_write(cmd_base + 7, 1, 0xA0); // PACKET

    wait_drq(m, cmd_base);
    for i in 0..6 {
        let w = u16::from_le_bytes([pkt[i * 2], pkt[i * 2 + 1]]);
        m.io_write(cmd_base, 2, u32::from(w));
    }
}

#[test]
fn st010_machine_ide_atapi_dma_and_irq15_routing() {
    let mut m = Machine::new(machine_cfg_ide_only()).unwrap();

    // Keep the CPU from acknowledging interrupts so we can observe PIC pending state.
    m.cpu_mut().set_rflags(0x2); // IF=0
    m.cpu_mut().halted = true;

    setup_pic_for_irq(&mut m, 15);

    // Attach an ATAPI CD-ROM backend (secondary master) with recognizable bytes at LBA 0.
    let mut iso = RawDisk::create(MemBackend::new(), 2048).unwrap();
    iso.write_at(0, b"DMATEST!").unwrap();
    m.attach_ide_secondary_master_iso(Box::new(iso)).unwrap();

    let bdf = profile::IDE_PIIX3.bdf;

    // Enable I/O decoding and bus mastering (DMA).
    let mut cmd = cfg_read(&mut m, bdf, 0x04, 2) as u16;
    cmd |= 0x0005; // IO + BUSMASTER
    cfg_write(&mut m, bdf, 0x04, 2, u32::from(cmd));

    let bar4 = cfg_read(&mut m, bdf, 0x20, 4);
    assert_eq!(bar4 & 0x1, 0x1, "BAR4 must be I/O");
    let bm_base = (bar4 & 0xFFFF_FFFC) as u16;

    // Select secondary master.
    let sec_cmd = SECONDARY_PORTS.cmd_base;
    m.io_write(sec_cmd + 6, 1, 0xA0);

    // Clear initial UNIT ATTENTION (media change) by issuing TEST UNIT READY then REQUEST SENSE.
    // (Do not call `run_slice` during this phase so no stale IRQ is propagated into the PIC.)
    let tur = [0u8; 12];
    atapi_send_packet(&mut m, sec_cmd, 0, &tur, 0);
    let _ = m.io_read(sec_cmd + 7, 1); // clear IRQ (device-side)

    let mut req_sense = [0u8; 12];
    req_sense[0] = 0x03;
    req_sense[4] = 18;
    atapi_send_packet(&mut m, sec_cmd, 0, &req_sense, 18);
    wait_drq(&mut m, sec_cmd);
    for _ in 0..(18 / 2) {
        let _ = m.io_read(sec_cmd, 2);
    }
    let _ = m.io_read(sec_cmd + 7, 1); // clear IRQ

    // PRD table and DMA buffer in guest RAM.
    let prd_addr = 0x2000u64;
    let dma_buf = 0x3000u64;
    m.write_physical_u32(prd_addr, dma_buf as u32);
    m.write_physical_u16(prd_addr + 4, 2048);
    m.write_physical_u16(prd_addr + 6, 0x8000); // EOT

    // Program secondary PRD pointer (BMIDE base + 8 + 4) and clear BMIDE status.
    m.io_write(bm_base + 8 + 2, 1, 0x06);
    m.io_write(bm_base + 8 + 4, 4, prd_addr as u32);

    // READ(10) for LBA=0, blocks=1 with DMA enabled (FEATURES bit0).
    let mut read10 = [0u8; 12];
    read10[0] = 0x28;
    read10[2..6].copy_from_slice(&0u32.to_be_bytes());
    read10[7..9].copy_from_slice(&1u16.to_be_bytes());
    atapi_send_packet(&mut m, sec_cmd, 0x01, &read10, 2048);

    m.io_write(bm_base + 8, 1, 0x09); // start + direction=read

    let _ = m.run_slice(1);

    let out = m.read_physical_bytes(dma_buf, 8);
    assert_eq!(out.as_slice(), b"DMATEST!");

    let interrupts = m.platform_interrupts().unwrap();
    let pending_vec = interrupts.borrow_mut().pic_mut().get_pending_vector();
    assert_eq!(pending_vec, Some(0x2F)); // 0x28 + (IRQ15-8)
}

fn snapshot_devices(bytes: &[u8]) -> Vec<snapshot::DeviceState> {
    const FILE_HEADER_LEN: usize = 16;
    const SECTION_HEADER_LEN: usize = 16;

    let mut r = Cursor::new(bytes);
    let mut file_header = [0u8; FILE_HEADER_LEN];
    r.read_exact(&mut file_header).unwrap();

    while (r.position() as usize) < bytes.len() {
        let mut section_header = [0u8; SECTION_HEADER_LEN];
        if let Err(e) = r.read_exact(&mut section_header) {
            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                break;
            }
            panic!("failed to read section header: {e}");
        }

        let id = u32::from_le_bytes(section_header[0..4].try_into().unwrap());
        let len = u64::from_le_bytes(section_header[8..16].try_into().unwrap());

        let mut payload = vec![0u8; len as usize];
        r.read_exact(&mut payload).unwrap();

        if id != snapshot::SectionId::DEVICES.0 {
            continue;
        }

        let mut pr = Cursor::new(&payload);
        let mut count_bytes = [0u8; 4];
        pr.read_exact(&mut count_bytes).unwrap();
        let count = u32::from_le_bytes(count_bytes) as usize;

        let mut devices = Vec::with_capacity(count);
        for _ in 0..count {
            devices.push(snapshot::DeviceState::decode(&mut pr, 64 * 1024 * 1024).unwrap());
        }

        return devices;
    }

    panic!("snapshot did not contain a DEVICES section");
}

#[test]
fn st010_machine_snapshot_includes_disk_controller_dskc_and_restores() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_ahci: true,
        enable_ide: true,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg.clone()).unwrap();

    // Make the controller snapshots non-trivial (ports present, media present).
    let mut disk = RawDisk::create(MemBackend::new(), 8 * SECTOR_SIZE as u64).unwrap();
    disk.write_at(0, &[1, 2, 3, 4]).unwrap();
    m.attach_ahci_disk_port0(Box::new(disk)).unwrap();

    let mut iso = RawDisk::create(MemBackend::new(), 2048).unwrap();
    iso.write_at(0, b"HELLO").unwrap();
    m.attach_ide_secondary_master_iso(Box::new(iso)).unwrap();

    let snap = m.take_snapshot_full().unwrap();

    let devices = snapshot_devices(&snap);
    let disk_state = devices
        .iter()
        .find(|d| d.id == snapshot::DeviceId::DISK_CONTROLLER)
        .expect("snapshot missing DISK_CONTROLLER device entry");

    let mut dskc = DiskControllersSnapshot::default();
    dskc.load_state(&disk_state.data).unwrap();

    assert!(
        dskc.get(DiskControllersSnapshot::bdf_tag(0, 2, 0))
            .is_some(),
        "DSKC missing AHCI (00:02.0) entry"
    );
    assert!(
        dskc.get(DiskControllersSnapshot::bdf_tag(0, 1, 1))
            .is_some(),
        "DSKC missing IDE (00:01.1) entry"
    );

    // Restore into a fresh machine instance without reattaching disks.
    let mut restored = Machine::new(cfg).unwrap();
    restored.restore_snapshot_bytes(&snap).unwrap();
}
