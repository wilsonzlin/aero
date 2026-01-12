#![cfg(not(target_arch = "wasm32"))]

use aero_devices::pci::profile::IDE_PIIX3;
use aero_devices::pci::{PciBdf, PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT};
use aero_machine::{Machine, MachineConfig, SharedDisk};
use aero_storage::{MemBackend, RawDisk, VirtualDisk as _, SECTOR_SIZE};
use pretty_assertions::{assert_eq, assert_ne};

fn cfg_addr(bdf: PciBdf, offset: u8) -> u32 {
    0x8000_0000
        | ((bdf.bus as u32) << 16)
        | ((bdf.device as u32) << 11)
        | ((bdf.function as u32) << 8)
        | (offset as u32 & 0xFC)
}

fn write_cfg_u16(m: &mut Machine, bdf: PciBdf, offset: u8, value: u16) {
    m.io_write(PCI_CFG_ADDR_PORT, 4, cfg_addr(bdf, offset));
    m.io_write(PCI_CFG_DATA_PORT, 2, u32::from(value));
}

fn read_cfg_u32(m: &mut Machine, bdf: PciBdf, offset: u8) -> u32 {
    m.io_write(PCI_CFG_ADDR_PORT, 4, cfg_addr(bdf, offset));
    m.io_read(PCI_CFG_DATA_PORT, 4)
}

#[test]
fn machine_snapshot_roundtrip_preserves_ide_inflight_dma_command_and_allows_resume() {
    const RAM_SIZE: u64 = 2 * 1024 * 1024;
    let cfg = MachineConfig {
        ram_size_bytes: RAM_SIZE,
        enable_pc_platform: true,
        enable_ide: true,
        // Keep this test focused on IDE + snapshot/restore.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    };

    let mut src = Machine::new(cfg.clone()).unwrap();

    // Attach a small disk with a known marker at LBA 0.
    let mut disk = RawDisk::create(MemBackend::new(), 8 * SECTOR_SIZE as u64).unwrap();
    disk.write_at(0, &[9, 8, 7, 6]).unwrap();
    src.attach_ide_primary_master_disk(Box::new(disk)).unwrap();

    let bdf = IDE_PIIX3.bdf;

    // Enable PCI I/O decode + bus mastering.
    write_cfg_u16(&mut src, bdf, 0x04, 0x0005);

    // Resolve the BMIDE BAR4 base assigned by BIOS POST.
    let bar4_raw = read_cfg_u32(&mut src, bdf, 0x20);
    let bm_base = (bar4_raw & 0xFFFF_FFFC) as u16;
    assert_ne!(bm_base, 0, "expected BMIDE BAR4 base to be programmed");

    // Guest physical addresses for PRD table + DMA buffer.
    let prd_addr = 0x1000u64;
    let data_buf = 0x2000u64;

    // PRD entry: 512 bytes, end-of-table.
    src.write_physical_u32(prd_addr, data_buf as u32);
    src.write_physical_u16(prd_addr + 4, SECTOR_SIZE as u16);
    src.write_physical_u16(prd_addr + 6, 0x8000);

    // Clear destination buffer so the DMA result is observable.
    src.write_physical(data_buf, &[0u8; 4]);

    // Program BMIDE and start the engine (direction=to-memory).
    src.io_write(bm_base + 4, 4, prd_addr as u32);
    src.io_write(bm_base + 2, 1, 0x06); // clear error/irq bits (defensive)
    src.io_write(bm_base, 1, 0x09); // start + direction=to-memory

    // Issue ATA READ DMA (0xC8) for LBA 0, count 1, primary master.
    src.io_write(0x1F2, 1, 1);
    src.io_write(0x1F3, 1, 0);
    src.io_write(0x1F4, 1, 0);
    src.io_write(0x1F5, 1, 0);
    src.io_write(0x1F6, 1, 0xE0);
    src.io_write(0x1F7, 1, 0xC8);

    // Ensure the DMA has not run yet (we have not ticked the controller).
    assert_eq!(src.read_physical_bytes(data_buf, 4), vec![0, 0, 0, 0]);
    assert_eq!(
        src.io_read(bm_base + 2, 1) as u8 & 0x04,
        0,
        "BMIDE IRQ bit should not be set before DMA execution"
    );

    let snap = src.take_snapshot_full().unwrap();

    let mut restored = Machine::new(cfg).unwrap();
    restored.restore_snapshot_bytes(&snap).unwrap();

    // Host contract: controller restore drops attached disks; reattach after restoring state.
    let mut disk = RawDisk::create(MemBackend::new(), 8 * SECTOR_SIZE as u64).unwrap();
    disk.write_at(0, &[9, 8, 7, 6]).unwrap();
    restored
        .attach_ide_primary_master_disk(Box::new(disk))
        .unwrap();

    let bm_base2 = (read_cfg_u32(&mut restored, bdf, 0x20) & 0xFFFF_FFFC) as u16;
    assert_eq!(
        bm_base2, bm_base,
        "BMIDE BAR4 base should survive snapshot/restore"
    );

    // Resume IDE processing and verify the DMA completes.
    restored.process_ide();

    assert_eq!(restored.read_physical_bytes(data_buf, 4), vec![9, 8, 7, 6]);
    assert_eq!(
        restored.io_read(bm_base + 2, 1) as u8 & 0x04,
        0x04,
        "BMIDE IRQ bit should be set after DMA completion"
    );
}

#[test]
fn machine_snapshot_roundtrip_preserves_ide_inflight_dma_write_and_allows_resume() {
    const RAM_SIZE: u64 = 2 * 1024 * 1024;
    let cfg = MachineConfig {
        ram_size_bytes: RAM_SIZE,
        enable_pc_platform: true,
        enable_ide: true,
        // Keep this test focused on IDE + snapshot/restore.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    };

    let mut src = Machine::new(cfg.clone()).unwrap();

    let disk = SharedDisk::from_bytes(vec![0u8; 8 * SECTOR_SIZE]).unwrap();
    // Seed LBA 1 so we can observe the DMA write commit.
    {
        let mut seed = vec![0u8; SECTOR_SIZE];
        seed[0..4].copy_from_slice(b"OLD!");
        disk.clone().write_sectors(1, &seed).unwrap();
    }
    src.attach_ide_primary_master_disk(Box::new(disk.clone()))
        .unwrap();

    let bdf = IDE_PIIX3.bdf;

    // Enable PCI I/O decode + bus mastering.
    write_cfg_u16(&mut src, bdf, 0x04, 0x0005);

    // Resolve the BMIDE BAR4 base assigned by BIOS POST.
    let bar4_raw = read_cfg_u32(&mut src, bdf, 0x20);
    let bm_base = (bar4_raw & 0xFFFF_FFFC) as u16;
    assert_ne!(bm_base, 0, "expected BMIDE BAR4 base to be programmed");

    // Guest physical addresses for PRD table + DMA buffer.
    let prd_addr = 0x1000u64;
    let data_buf = 0x2000u64;

    // Prepare a single-entry PRD table (512 bytes, EOT).
    src.write_physical_u32(prd_addr, data_buf as u32);
    src.write_physical_u16(prd_addr + 4, SECTOR_SIZE as u16);
    src.write_physical_u16(prd_addr + 6, 0x8000);

    // Fill the guest buffer with a deterministic pattern to write to LBA 1.
    let mut pattern = vec![0u8; SECTOR_SIZE];
    pattern[0..8].copy_from_slice(b"SNAPWRIT");
    for (i, b) in pattern.iter_mut().enumerate().skip(8) {
        *b = (i as u8).wrapping_mul(7).wrapping_add(0x3D);
    }
    src.write_physical(data_buf, &pattern);

    // Program BMIDE and start the engine (direction=from-memory for ATA writes).
    src.io_write(bm_base + 4, 4, prd_addr as u32);
    src.io_write(bm_base + 2, 1, 0x06); // clear error/irq bits (defensive)
    src.io_write(bm_base, 1, 0x01); // start

    // Issue ATA WRITE DMA (0xCA) for LBA 1, count 1, primary master.
    src.io_write(0x1F2, 1, 1);
    src.io_write(0x1F3, 1, 1);
    src.io_write(0x1F4, 1, 0);
    src.io_write(0x1F5, 1, 0);
    src.io_write(0x1F6, 1, 0xE0);
    src.io_write(0x1F7, 1, 0xCA);

    // Ensure the DMA has not run/committed yet (we have not ticked the controller).
    {
        let mut out = vec![0u8; SECTOR_SIZE];
        disk.clone().read_sectors(1, &mut out).unwrap();
        assert_ne!(
            out.as_slice(),
            pattern.as_slice(),
            "disk should not be modified until DMA executes"
        );
    }

    let snap = src.take_snapshot_full().unwrap();
    drop(src);

    let mut restored = Machine::new(cfg).unwrap();
    restored.restore_snapshot_bytes(&snap).unwrap();

    // Host contract: controller restore drops attached disks; reattach after restoring state.
    restored
        .attach_ide_primary_master_disk(Box::new(disk.clone()))
        .unwrap();

    let bm_base2 = (read_cfg_u32(&mut restored, bdf, 0x20) & 0xFFFF_FFFC) as u16;
    assert_eq!(
        bm_base2, bm_base,
        "BMIDE BAR4 base should survive snapshot/restore"
    );

    // Resume IDE processing and verify the DMA executes + commits.
    restored.process_ide();

    let mut out = vec![0u8; SECTOR_SIZE];
    disk.clone().read_sectors(1, &mut out).unwrap();
    assert_eq!(out.as_slice(), pattern.as_slice());
    assert_eq!(
        restored.io_read(bm_base + 2, 1) as u8 & 0x04,
        0x04,
        "BMIDE IRQ bit should be set after DMA completion"
    );
}
