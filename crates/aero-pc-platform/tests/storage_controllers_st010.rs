//! ST-010: Unified storage controller integration tests.
//!
//! These tests exercise the PcPlatform plumbing end-to-end:
//! - PCI enumeration + BAR assignment
//! - MMIO/I/O register semantics through the platform bus
//! - DMA into guest memory
//! - INTx interrupt routing via the platform PIC

mod helpers;

use aero_devices::pci::profile;
use aero_devices_storage::pci_ide::{PRIMARY_PORTS, SECONDARY_PORTS};
use aero_io_snapshot::io::state::IoSnapshot;
use aero_pc_platform::{PcPlatform, PcPlatformConfig};
use aero_storage::{MemBackend, RawDisk, VirtualDisk as _};
use memory::MemoryBus as _;

use helpers::*;

#[test]
fn st010_ahci_read_dma_ext_and_intx_routing() {
    let mut pc = PcPlatform::new_with_config(
        2 * 1024 * 1024,
        PcPlatformConfig {
            enable_ahci: true,
            ..Default::default()
        },
    );

    // Attach a small in-memory disk with a known marker at LBA 4.
    let mut disk = RawDisk::create(MemBackend::new(), 8 * aero_storage::SECTOR_SIZE as u64).unwrap();
    disk.write_at(4 * aero_storage::SECTOR_SIZE as u64, &[9, 8, 7, 6])
        .unwrap();
    pc.attach_ahci_disk_port0(Box::new(disk)).unwrap();

    let bdf = profile::SATA_AHCI_ICH9.bdf;

    // PCI enumeration.
    let id = pci_cfg_read_u32(&mut pc, bdf, 0x00);
    assert_eq!(id & 0xFFFF, u32::from(profile::SATA_AHCI_ICH9.vendor_id));
    assert_eq!(
        (id >> 16) & 0xFFFF,
        u32::from(profile::SATA_AHCI_ICH9.device_id)
    );

    // BAR assignment (BAR5).
    let bar5 = pci_read_bar(&mut pc, bdf, 5);
    assert_eq!(bar5.kind, BarKind::Mem32);
    assert_ne!(bar5.base, 0);
    assert_eq!(bar5.base % 0x2000, 0);

    // Interrupt Line should match the router-selected GSI (device 2 => PIRQ C => IRQ12).
    assert_eq!(pci_cfg_read_u8(&mut pc, bdf, 0x3C), 12);

    // Observe INTx via the legacy PIC (unmask cascade + IRQ12).
    unmask_pic_irq(&mut pc, 12);

    // Allow the controller to DMA (bus mastering).
    let mut cmd = pci_cfg_read_u16(&mut pc, bdf, 0x04);
    cmd |= 0x0006; // MEM + BUSMASTER
    pci_cfg_write_u16(&mut pc, bdf, 0x04, cmd);

    // Guest memory layout.
    let mut alloc = GuestAllocator::new(2 * 1024 * 1024, 0x1000);
    let clb = alloc.alloc_bytes(1024, 1024);
    let fb = alloc.alloc_bytes(256, 256);
    let ctba = alloc.alloc_bytes(256, 128);
    let data_buf = alloc.alloc_bytes(aero_storage::SECTOR_SIZE, 512);

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

    pc.memory.write_u32(bar5.base + HBA_GHC, GHC_IE | GHC_AE);
    pc.memory
        .write_u32(bar5.base + PORT_BASE + PORT_CLB, clb as u32);
    pc.memory.write_u32(bar5.base + PORT_BASE + PORT_CLBU, 0);
    pc.memory
        .write_u32(bar5.base + PORT_BASE + PORT_FB, fb as u32);
    pc.memory.write_u32(bar5.base + PORT_BASE + PORT_FBU, 0);
    pc.memory
        .write_u32(bar5.base + PORT_BASE + PORT_IE, PORT_IS_DHRS);
    pc.memory
        .write_u32(bar5.base + PORT_BASE + PORT_CMD, PORT_CMD_ST | PORT_CMD_FRE);

    // Build a single-slot command list: READ DMA EXT (LBA=4, 1 sector).
    const ATA_CMD_READ_DMA_EXT: u8 = aero_devices_storage::ata::ATA_CMD_READ_DMA_EXT;

    let cfl = 5u32; // 20 bytes / 4
    let prdtl = 1u32;
    let header_flags = cfl | (prdtl << 16);
    pc.memory.write_u32(clb, header_flags);
    pc.memory.write_u32(clb + 4, 0); // PRDBC
    pc.memory.write_u32(clb + 8, ctba as u32);
    pc.memory.write_u32(clb + 12, 0);

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
    mem_write(&mut pc, ctba, &cfis);

    // PRDT entry 0.
    let prd = ctba + 0x80;
    pc.memory.write_u32(prd, data_buf as u32);
    pc.memory.write_u32(prd + 4, 0);
    pc.memory.write_u32(prd + 8, 0);
    // DBC is stored as byte_count-1; set IOC (bit31) for realism.
    pc.memory.write_u32(
        prd + 12,
        ((aero_storage::SECTOR_SIZE as u32) - 1) | (1 << 31),
    );

    // Clear any prior interrupt state and issue the command.
    pc.memory
        .write_u32(bar5.base + PORT_BASE + PORT_IS, PORT_IS_DHRS);
    pc.memory.write_u32(bar5.base + PORT_BASE + PORT_CI, 1);

    pc.process_ahci();

    // Verify DMA landed in guest RAM.
    let mut out = [0u8; 4];
    mem_read(&mut pc, data_buf, &mut out);
    assert_eq!(out, [9, 8, 7, 6]);

    // Verify IRQ level and routing.
    assert!(pc.ahci.as_ref().unwrap().borrow().intx_level());
    pc.poll_pci_intx_lines();
    assert_eq!(pic_pending_irq(&pc), Some(12));
}

#[test]
fn st010_nvme_admin_identify_and_intx_routing() {
    let mut pc = PcPlatform::new_with_config(
        2 * 1024 * 1024,
        PcPlatformConfig {
            enable_nvme: true,
            ..Default::default()
        },
    );

    let bdf = profile::NVME_CONTROLLER.bdf;

    // PCI enumeration.
    let id = pci_cfg_read_u32(&mut pc, bdf, 0x00);
    assert_eq!(id & 0xFFFF, u32::from(profile::NVME_CONTROLLER.vendor_id));
    assert_eq!(
        (id >> 16) & 0xFFFF,
        u32::from(profile::NVME_CONTROLLER.device_id)
    );

    // BAR assignment (BAR0, mem64).
    let bar0 = pci_read_bar(&mut pc, bdf, 0);
    assert_eq!(bar0.kind, BarKind::Mem64);
    assert_ne!(bar0.base, 0);
    assert_eq!(bar0.base % 0x4000, 0);

    // Interrupt Line should match router-selected GSI (device 3 => PIRQ D => IRQ13).
    assert_eq!(pci_cfg_read_u8(&mut pc, bdf, 0x3C), 13);
    unmask_pic_irq(&mut pc, 13);

    // Enable bus mastering (DMA).
    let mut cmd = pci_cfg_read_u16(&mut pc, bdf, 0x04);
    cmd |= 0x0006; // MEM + BUSMASTER
    pci_cfg_write_u16(&mut pc, bdf, 0x04, cmd);

    // Guest memory: admin queues + identify buffer (all 4K-aligned).
    let mut alloc = GuestAllocator::new(2 * 1024 * 1024, 0x1000);
    let asq = alloc.alloc_bytes(4096, 4096);
    let acq = alloc.alloc_bytes(4096, 4096);
    let identify = alloc.alloc_bytes(4096, 4096);

    // Configure controller (AQA/ASQ/ACQ then CC.EN).
    // Note: AQA fields are 0-based; a value of 0 would create a 1-entry queue which cannot
    // represent a non-empty ring (tail wraps to 0). Use 2 entries instead.
    pc.memory.write_u32(bar0.base + 0x24, 0x0001_0001); // AQA: 2-entry SQ/CQ
    pc.memory.write_u64(bar0.base + 0x28, asq);
    pc.memory.write_u64(bar0.base + 0x30, acq);
    pc.memory.write_u32(bar0.base + 0x14, 1); // CC.EN

    // Build IDENTIFY command (CNS=1 identify controller) in ASQ[0].
    let cid: u16 = 1;
    let opc: u8 = 0x06;
    let dw0 = (opc as u32) | ((cid as u32) << 16);
    pc.memory.write_u32(asq, dw0);
    pc.memory.write_u32(asq + 4, 0); // NSID
                                      // PRP1/PRP2
    pc.memory.write_u64(asq + 24, identify);
    pc.memory.write_u64(asq + 32, 0);
    pc.memory.write_u32(asq + 40, 1); // CDW10: CNS=1

    // Ring SQ0 tail doorbell to 1.
    pc.memory.write_u32(bar0.base + 0x1000, 1);
    pc.process_nvme();

    // Completion queue entry 0 should be populated and the identify buffer should contain the controller ID.
    let cpl_cid = pc.memory.read_u16(acq + 12);
    let cpl_status = pc.memory.read_u16(acq + 14);
    assert_eq!(cpl_cid, cid);
    assert_eq!(cpl_status & 0xFFFE, 0, "status should report SUCCESS");

    let vid = pc.memory.read_u16(identify);
    assert_eq!(vid, 0x1b36);

    assert!(pc.nvme.as_ref().unwrap().borrow().irq_level());
    pc.poll_pci_intx_lines();
    assert_eq!(pic_pending_irq(&pc), Some(13));
}

#[test]
fn st010_virtio_blk_read_write_and_intx_routing() {
    let mut pc = PcPlatform::new_with_config(
        2 * 1024 * 1024,
        PcPlatformConfig {
            enable_virtio_blk: true,
            ..Default::default()
        },
    );

    let bdf = profile::VIRTIO_BLK.bdf;

    // PCI enumeration.
    let id = pci_cfg_read_u32(&mut pc, bdf, 0x00);
    assert_eq!(id & 0xFFFF, u32::from(profile::VIRTIO_BLK.vendor_id));
    assert_eq!(
        (id >> 16) & 0xFFFF,
        u32::from(profile::VIRTIO_BLK.device_id)
    );

    // BAR assignment (BAR0, mem64).
    let bar0 = pci_read_bar(&mut pc, bdf, 0);
    assert_eq!(bar0.kind, BarKind::Mem64);
    assert_ne!(bar0.base, 0);
    assert_eq!(bar0.base % 0x4000, 0);

    // Interrupt Line should match router-selected GSI (device 9 => PIRQ B => IRQ11).
    assert_eq!(pci_cfg_read_u8(&mut pc, bdf, 0x3C), 11);
    unmask_pic_irq(&mut pc, 11);

    // Enable bus mastering for DMA and MMIO decoding.
    let mut cmd = pci_cfg_read_u16(&mut pc, bdf, 0x04);
    cmd |= 0x0006; // MEM + BUSMASTER
    pci_cfg_write_u16(&mut pc, bdf, 0x04, cmd);

    // Allocate virtqueue + request buffers.
    let mut alloc = GuestAllocator::new(2 * 1024 * 1024, 0x1000);
    let queue_size: u16 = 128;

    let desc_addr = alloc.alloc_bytes((queue_size as usize) * 16, 16);
    let avail_addr = alloc.alloc_bytes(4 + (queue_size as usize) * 2, 2);
    let used_addr = alloc.alloc_bytes(4 + (queue_size as usize) * 8, 4);

    let req_hdr = alloc.alloc_bytes(16, 16);
    let data_buf = alloc.alloc_bytes(512, 512);
    let status_buf = alloc.alloc_bytes(1, 1);

    // Modern virtio-pci common config lives at BAR0 + 0x0000.
    const COMMON_BASE: u64 = 0x0000;
    const NOTIFY_BASE: u64 = 0x1000;
    const ISR_BASE: u64 = 0x2000;

    // Negotiate a minimal feature set (VERSION_1 only).
    const VIRTIO_F_VERSION_1: u64 = aero_virtio::pci::VIRTIO_F_VERSION_1;

    // status = ACKNOWLEDGE | DRIVER
    pc.memory.write_u8(
        bar0.base + COMMON_BASE + 0x14,
        aero_virtio::pci::VIRTIO_STATUS_ACKNOWLEDGE,
    );
    pc.memory.write_u8(
        bar0.base + COMMON_BASE + 0x14,
        aero_virtio::pci::VIRTIO_STATUS_ACKNOWLEDGE | aero_virtio::pci::VIRTIO_STATUS_DRIVER,
    );

    // driver_features (low then high 32 bits).
    pc.memory.write_u32(bar0.base + COMMON_BASE + 0x08, 0); // driver_feature_select=0
    pc.memory.write_u32(bar0.base + COMMON_BASE + 0x0c, 0); // low bits
    pc.memory.write_u32(bar0.base + COMMON_BASE + 0x08, 1); // driver_feature_select=1
    pc.memory.write_u32(
        bar0.base + COMMON_BASE + 0x0c,
        (VIRTIO_F_VERSION_1 >> 32) as u32,
    );

    // status |= FEATURES_OK (triggers negotiation).
    pc.memory.write_u8(
        bar0.base + COMMON_BASE + 0x14,
        aero_virtio::pci::VIRTIO_STATUS_ACKNOWLEDGE
            | aero_virtio::pci::VIRTIO_STATUS_DRIVER
            | aero_virtio::pci::VIRTIO_STATUS_FEATURES_OK,
    );

    // Configure queue 0.
    pc.memory.write_u16(bar0.base + COMMON_BASE + 0x16, 0); // queue_select
    pc.memory
        .write_u64(bar0.base + COMMON_BASE + 0x20, desc_addr);
    pc.memory
        .write_u64(bar0.base + COMMON_BASE + 0x28, avail_addr);
    pc.memory
        .write_u64(bar0.base + COMMON_BASE + 0x30, used_addr);
    pc.memory.write_u16(bar0.base + COMMON_BASE + 0x1c, 1); // queue_enable

    // status |= DRIVER_OK.
    pc.memory.write_u8(
        bar0.base + COMMON_BASE + 0x14,
        aero_virtio::pci::VIRTIO_STATUS_ACKNOWLEDGE
            | aero_virtio::pci::VIRTIO_STATUS_DRIVER
            | aero_virtio::pci::VIRTIO_STATUS_FEATURES_OK
            | aero_virtio::pci::VIRTIO_STATUS_DRIVER_OK,
    );

    fn write_desc(
        pc: &mut PcPlatform,
        desc_addr: u64,
        index: u16,
        addr: u64,
        len: u32,
        flags: u16,
        next: u16,
    ) {
        let base = desc_addr + u64::from(index) * 16;
        pc.memory.write_u64(base, addr);
        pc.memory.write_u32(base + 8, len);
        pc.memory.write_u16(base + 12, flags);
        pc.memory.write_u16(base + 14, next);
    }

    fn submit_avail(pc: &mut PcPlatform, avail_addr: u64, pos: u16, head: u16) {
        let ring_off = 4 + u64::from(pos) * 2;
        pc.memory.write_u16(avail_addr + ring_off, head);
        pc.memory.write_u16(avail_addr + 2, pos + 1);
    }

    // Common descriptor flags.
    const VIRTQ_DESC_F_NEXT: u16 = aero_virtio::queue::VIRTQ_DESC_F_NEXT;
    const VIRTQ_DESC_F_WRITE: u16 = aero_virtio::queue::VIRTQ_DESC_F_WRITE;

    // --- Request 0: WRITE sector 1 (OUT) ---
    let payload = [0xAAu8; 512];
    mem_write(&mut pc, data_buf, &payload);

    // virtio-blk request header: type=u32, reserved=u32, sector=u64
    pc.memory
        .write_u32(req_hdr, aero_virtio::devices::blk::VIRTIO_BLK_T_OUT);
    pc.memory.write_u32(req_hdr + 4, 0);
    pc.memory.write_u64(req_hdr + 8, 1);

    write_desc(&mut pc, desc_addr, 0, req_hdr, 16, VIRTQ_DESC_F_NEXT, 1);
    write_desc(&mut pc, desc_addr, 1, data_buf, 512, VIRTQ_DESC_F_NEXT, 2); // device reads data (no WRITE flag)
    write_desc(&mut pc, desc_addr, 2, status_buf, 1, VIRTQ_DESC_F_WRITE, 0);
    pc.memory.write_u8(status_buf, 0xFF);
    submit_avail(&mut pc, avail_addr, 0, 0);

    // Notify queue 0 (offset encodes queue index in modern transport).
    pc.memory.write_u32(bar0.base + NOTIFY_BASE, 0);
    pc.process_virtio_blk();

    assert_eq!(pc.memory.read_u16(used_addr + 2), 1);
    assert_eq!(
        pc.memory.read_u8(status_buf),
        aero_virtio::devices::blk::VIRTIO_BLK_S_OK
    );

    assert!(pc.virtio_blk.as_ref().unwrap().borrow().irq_level());
    pc.poll_pci_intx_lines();
    assert_eq!(pic_pending_irq(&pc), Some(11));

    // Clear virtio ISR (lowers legacy IRQ) and propagate the deassert through INTx routing so the
    // second request can trigger a fresh edge for the PIC.
    let _isr = pc.memory.read_u8(bar0.base + ISR_BASE);
    pc.poll_pci_intx_lines();

    // --- Request 1: READ sector 1 (IN) ---
    pc.memory
        .write_u32(req_hdr, aero_virtio::devices::blk::VIRTIO_BLK_T_IN);
    pc.memory.write_u32(req_hdr + 4, 0);
    pc.memory.write_u64(req_hdr + 8, 1);

    write_desc(&mut pc, desc_addr, 0, req_hdr, 16, VIRTQ_DESC_F_NEXT, 1);
    write_desc(
        &mut pc,
        desc_addr,
        1,
        data_buf,
        512,
        VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE,
        2,
    );
    write_desc(&mut pc, desc_addr, 2, status_buf, 1, VIRTQ_DESC_F_WRITE, 0);
    pc.memory.write_u8(status_buf, 0xFF);
    submit_avail(&mut pc, avail_addr, 1, 0);

    pc.memory.write_u32(bar0.base + NOTIFY_BASE, 0);
    pc.process_virtio_blk();

    assert_eq!(pc.memory.read_u16(used_addr + 2), 2);
    assert_eq!(
        pc.memory.read_u8(status_buf),
        aero_virtio::devices::blk::VIRTIO_BLK_S_OK
    );

    let mut readback = [0u8; 512];
    mem_read(&mut pc, data_buf, &mut readback);
    assert_eq!(readback, payload);
}

#[test]
fn st010_ide_pio_atapi_and_busmaster_dma() {
    let mut pc = PcPlatform::new_with_config(
        2 * 1024 * 1024,
        PcPlatformConfig {
            enable_ide: true,
            ..Default::default()
        },
    );

    let bdf = profile::IDE_PIIX3.bdf;

    // PCI enumeration (PIIX3 IDE).
    let id = pci_cfg_read_u32(&mut pc, bdf, 0x00);
    assert_eq!(id & 0xFFFF, u32::from(profile::IDE_PIIX3.vendor_id));
    assert_eq!((id >> 16) & 0xFFFF, u32::from(profile::IDE_PIIX3.device_id));

    // BAR assignment: Bus Master IDE is a relocatable I/O BAR (BAR4).
    let bar4 = pci_read_bar(&mut pc, bdf, 4);
    assert_eq!(bar4.kind, BarKind::Io);
    assert_ne!(bar4.base, 0);

    // Enable I/O decoding and bus mastering (DMA).
    let mut cmd = pci_cfg_read_u16(&mut pc, bdf, 0x04);
    cmd |= 0x0005; // IO + BUSMASTER
    pci_cfg_write_u16(&mut pc, bdf, 0x04, cmd);

    // Attach an ATA HDD (primary master) and an ATAPI CD-ROM (secondary master).
    let mut hdd = RawDisk::create(MemBackend::new(), 8 * aero_storage::SECTOR_SIZE as u64).unwrap();
    hdd.write_at(aero_storage::SECTOR_SIZE as u64, &[1, 2, 3, 4])
        .unwrap();
    let mut sector0 = [0u8; aero_storage::SECTOR_SIZE];
    for (i, b) in sector0.iter_mut().enumerate() {
        let half = (i >= 256) as u8;
        *b = (i as u8).wrapping_add(1 + half * 16);
    }
    hdd.write_at(0, &sector0).unwrap();
    pc.attach_ide_primary_master_disk(Box::new(hdd)).unwrap();

    let mut iso = RawDisk::create(MemBackend::new(), 4 * 2048).unwrap();
    iso.write_at(0, b"HELLO").unwrap();
    iso.write_at(2048, b"WORLD").unwrap();
    pc.attach_ide_secondary_master_iso(Box::new(iso)).unwrap();

    // Helper to wait for BSY=0 and DRQ=1.
    fn wait_drq(pc: &mut PcPlatform, cmd_base: u16) {
        for _ in 0..1000 {
            let st = pc.io.read(cmd_base + 7, 1) as u8;
            if (st & 0x80) == 0 && (st & 0x08) != 0 {
                return;
            }
        }
        panic!("timeout waiting for DRQ on IDE port {cmd_base:#x}");
    }

    // --- ATA PIO read from primary master (LBA=1) ---
    let pri_cmd = PRIMARY_PORTS.cmd_base;
    pc.io.write(pri_cmd + 6, 1, 0xE0); // master + LBA
    pc.io.write(pri_cmd + 2, 1, 1); // sector count
    pc.io.write(pri_cmd + 3, 1, 1); // LBA0
    pc.io.write(pri_cmd + 4, 1, 0);
    pc.io.write(pri_cmd + 5, 1, 0);
    pc.io.write(pri_cmd + 7, 1, 0x20); // READ SECTORS

    wait_drq(&mut pc, pri_cmd);
    let mut sector = [0u8; 512];
    for i in 0..256 {
        let w = pc.io.read(pri_cmd, 2) as u16;
        sector[i * 2..i * 2 + 2].copy_from_slice(&w.to_le_bytes());
    }
    assert_eq!(&sector[0..4], &[1, 2, 3, 4]);

    // --- ATAPI READ(10) from secondary master CD-ROM (LBA=1) ---
    let sec_cmd = SECONDARY_PORTS.cmd_base;
    pc.io.write(sec_cmd + 6, 1, 0xA0); // select master

    fn atapi_send_packet(pc: &mut PcPlatform, cmd_base: u16, pkt: &[u8; 12], byte_count: u16) {
        // FEATURES: PIO
        pc.io.write(cmd_base + 1, 1, 0);
        pc.io.write(cmd_base + 4, 1, (byte_count & 0xFF) as u32);
        pc.io.write(cmd_base + 5, 1, (byte_count >> 8) as u32);
        pc.io.write(cmd_base + 7, 1, 0xA0); // PACKET

        wait_drq(pc, cmd_base);

        for i in 0..6 {
            let w = u16::from_le_bytes([pkt[i * 2], pkt[i * 2 + 1]]);
            pc.io.write(cmd_base, 2, w as u32);
        }
    }

    // Clear initial UNIT ATTENTION (media change) by issuing TEST UNIT READY then REQUEST SENSE.
    let tur = [0u8; 12];
    atapi_send_packet(&mut pc, sec_cmd, &tur, 0);

    let mut req_sense = [0u8; 12];
    req_sense[0] = 0x03;
    req_sense[4] = 18;
    atapi_send_packet(&mut pc, sec_cmd, &req_sense, 18);
    wait_drq(&mut pc, sec_cmd);
    for _ in 0..9 {
        let _ = pc.io.read(sec_cmd, 2);
    }

    // READ(10) for LBA=1, blocks=1.
    let mut read10 = [0u8; 12];
    read10[0] = 0x28;
    read10[2..6].copy_from_slice(&1u32.to_be_bytes());
    read10[7..9].copy_from_slice(&1u16.to_be_bytes());
    atapi_send_packet(&mut pc, sec_cmd, &read10, 2048);
    wait_drq(&mut pc, sec_cmd);

    let mut cd = vec![0u8; 2048];
    for i in 0..(2048 / 2) {
        let w = pc.io.read(sec_cmd, 2) as u16;
        cd[i * 2..i * 2 + 2].copy_from_slice(&w.to_le_bytes());
    }
    assert_eq!(&cd[0..5], b"WORLD");

    // --- Bus Master IDE DMA PRD scatter/gather crossing a 64KiB boundary ---
    let bus_master_base = bar4.base as u16;

    // PRD table in guest RAM.
    let prd_addr = 0x2000u64;
    let buf0 = 0x0FF00u64; // 0xFF00..0x10000 (256 bytes)
    let buf1 = 0x10000u64; // 0x10000..0x10100 (256 bytes)

    pc.memory.write_u32(prd_addr, buf0 as u32);
    pc.memory.write_u16(prd_addr + 4, 256);
    pc.memory.write_u16(prd_addr + 6, 0x0000);
    pc.memory.write_u32(prd_addr + 8, buf1 as u32);
    pc.memory.write_u16(prd_addr + 12, 256);
    pc.memory.write_u16(prd_addr + 14, 0x8000); // EOT

    // Clear Bus Master status (IRQ/error).
    pc.io.write(bus_master_base + 2, 1, 0x06);

    // Program PRD pointer (primary channel) and start DMA.
    pc.io.write(bus_master_base + 4, 4, prd_addr as u32);

    // READ DMA (LBA 0, 1 sector).
    pc.io.write(pri_cmd + 6, 1, 0xE0);
    pc.io.write(pri_cmd + 2, 1, 1);
    pc.io.write(pri_cmd + 3, 1, 0);
    pc.io.write(pri_cmd + 4, 1, 0);
    pc.io.write(pri_cmd + 5, 1, 0);
    pc.io.write(pri_cmd + 7, 1, 0xC8);

    pc.io.write(bus_master_base, 1, 0x09); // start + direction=read
    pc.process_ide();

    let mut dma0 = [0u8; 16];
    let mut dma1 = [0u8; 16];
    mem_read(&mut pc, buf0, &mut dma0);
    mem_read(&mut pc, buf1, &mut dma1);
    assert_eq!(
        dma0,
        [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]
    );
    assert_eq!(
        dma1,
        [17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32]
    );
}

#[test]
fn st010_ahci_snapshot_roundtrip_preserves_intx_level() {
    let mut pc = PcPlatform::new_with_config(
        2 * 1024 * 1024,
        PcPlatformConfig {
            enable_ahci: true,
            enable_uhci: false,
            ..Default::default()
        },
    );

    // Attach a small in-memory disk with a known marker at LBA 4 so the port is "present" and
    // snapshot restore won't reset registers when we re-attach the backend.
    let mut disk = RawDisk::create(MemBackend::new(), 8 * aero_storage::SECTOR_SIZE as u64).unwrap();
    disk.write_at(4 * aero_storage::SECTOR_SIZE as u64, &[9, 8, 7, 6])
        .unwrap();
    pc.attach_ahci_disk_port0(Box::new(disk)).unwrap();

    let bdf = profile::SATA_AHCI_ICH9.bdf;
    let bar5 = pci_read_bar(&mut pc, bdf, 5);

    // Enable MMIO + bus mastering.
    let mut cmd = pci_cfg_read_u16(&mut pc, bdf, 0x04);
    cmd |= 0x0006; // MEM + BUSMASTER
    pci_cfg_write_u16(&mut pc, bdf, 0x04, cmd);

    // Observe INTx via the PIC.
    unmask_pic_irq(&mut pc, 12);

    // Minimal AHCI command (same programming model as the main AHCI test).
    let mut alloc = GuestAllocator::new(2 * 1024 * 1024, 0x1000);
    let clb = alloc.alloc_bytes(1024, 1024);
    let fb = alloc.alloc_bytes(256, 256);
    let ctba = alloc.alloc_bytes(256, 128);
    let data_buf = alloc.alloc_bytes(aero_storage::SECTOR_SIZE, 512);

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

    pc.memory.write_u32(bar5.base + HBA_GHC, GHC_IE | GHC_AE);
    pc.memory
        .write_u32(bar5.base + PORT_BASE + PORT_CLB, clb as u32);
    pc.memory.write_u32(bar5.base + PORT_BASE + PORT_CLBU, 0);
    pc.memory
        .write_u32(bar5.base + PORT_BASE + PORT_FB, fb as u32);
    pc.memory.write_u32(bar5.base + PORT_BASE + PORT_FBU, 0);
    pc.memory
        .write_u32(bar5.base + PORT_BASE + PORT_IE, PORT_IS_DHRS);
    pc.memory
        .write_u32(bar5.base + PORT_BASE + PORT_CMD, PORT_CMD_ST | PORT_CMD_FRE);

    const ATA_CMD_READ_DMA_EXT: u8 = aero_devices_storage::ata::ATA_CMD_READ_DMA_EXT;

    let cfl = 5u32;
    let prdtl = 1u32;
    pc.memory.write_u32(clb, cfl | (prdtl << 16));
    pc.memory.write_u32(clb + 4, 0);
    pc.memory.write_u32(clb + 8, ctba as u32);
    pc.memory.write_u32(clb + 12, 0);

    let mut cfis = [0u8; 64];
    cfis[0] = 0x27;
    cfis[1] = 0x80;
    cfis[2] = ATA_CMD_READ_DMA_EXT;
    cfis[7] = 0x40;
    let lba: u64 = 4;
    cfis[4] = (lba & 0xFF) as u8;
    cfis[5] = ((lba >> 8) & 0xFF) as u8;
    cfis[6] = ((lba >> 16) & 0xFF) as u8;
    cfis[8] = ((lba >> 24) & 0xFF) as u8;
    cfis[9] = ((lba >> 32) & 0xFF) as u8;
    cfis[10] = ((lba >> 40) & 0xFF) as u8;
    cfis[12] = 1;
    mem_write(&mut pc, ctba, &cfis);

    let prd = ctba + 0x80;
    pc.memory.write_u32(prd, data_buf as u32);
    pc.memory.write_u32(prd + 4, 0);
    pc.memory.write_u32(prd + 8, 0);
    pc.memory.write_u32(prd + 12, (aero_storage::SECTOR_SIZE as u32 - 1) | (1 << 31));

    pc.memory
        .write_u32(bar5.base + PORT_BASE + PORT_IS, PORT_IS_DHRS);
    pc.memory.write_u32(bar5.base + PORT_BASE + PORT_CI, 1);

    pc.process_ahci();
    pc.poll_pci_intx_lines();
    assert_eq!(pic_pending_irq(&pc), Some(12));

    let ahci_state = pc
        .ahci
        .as_ref()
        .expect("ahci enabled")
        .borrow()
        .save_state();

    // Restore into a fresh platform instance.
    let mut pc2 = PcPlatform::new_with_config(
        2 * 1024 * 1024,
        PcPlatformConfig {
            enable_ahci: true,
            enable_uhci: false,
            ..Default::default()
        },
    );
    pc2.ahci
        .as_ref()
        .expect("ahci enabled")
        .borrow_mut()
        .load_state(&ahci_state)
        .unwrap();

    // Re-attach the host disk backend post-restore (AHCI snapshots drop backends).
    let mut disk2 =
        RawDisk::create(MemBackend::new(), 8 * aero_storage::SECTOR_SIZE as u64).unwrap();
    disk2
        .write_at(4 * aero_storage::SECTOR_SIZE as u64, &[9, 8, 7, 6])
        .unwrap();
    pc2.attach_ahci_disk_port0(Box::new(disk2)).unwrap();

    unmask_pic_irq(&mut pc2, 12);
    let mut cmd2 = pci_cfg_read_u16(&mut pc2, bdf, 0x04);
    cmd2 |= 0x0006;
    pci_cfg_write_u16(&mut pc2, bdf, 0x04, cmd2);

    pc2.poll_pci_intx_lines();
    assert_eq!(
        pic_pending_irq(&pc2),
        Some(12),
        "AHCI INTx should remain asserted after snapshot/restore"
    );
}

#[test]
fn st010_nvme_snapshot_roundtrip_preserves_intx_level() {
    let mut pc = PcPlatform::new_with_config(
        2 * 1024 * 1024,
        PcPlatformConfig {
            enable_nvme: true,
            enable_ahci: false,
            enable_uhci: false,
            ..Default::default()
        },
    );

    let bdf = profile::NVME_CONTROLLER.bdf;
    let bar0 = pci_read_bar(&mut pc, bdf, 0);
    assert_eq!(bar0.kind, BarKind::Mem64);

    unmask_pic_irq(&mut pc, 13);

    let mut cmd = pci_cfg_read_u16(&mut pc, bdf, 0x04);
    cmd |= 0x0006; // MEM + BUSMASTER
    pci_cfg_write_u16(&mut pc, bdf, 0x04, cmd);

    let mut alloc = GuestAllocator::new(2 * 1024 * 1024, 0x1000);
    let asq = alloc.alloc_bytes(4096, 4096);
    let acq = alloc.alloc_bytes(4096, 4096);
    let identify = alloc.alloc_bytes(4096, 4096);

    pc.memory.write_u32(bar0.base + 0x24, 0x0001_0001);
    pc.memory.write_u64(bar0.base + 0x28, asq);
    pc.memory.write_u64(bar0.base + 0x30, acq);
    pc.memory.write_u32(bar0.base + 0x14, 1);

    let cid: u16 = 1;
    let dw0 = (0x06u32) | ((cid as u32) << 16); // IDENTIFY
    pc.memory.write_u32(asq, dw0);
    pc.memory.write_u64(asq + 24, identify);
    pc.memory.write_u32(asq + 40, 1); // CNS=1

    pc.memory.write_u32(bar0.base + 0x1000, 1);
    pc.process_nvme();

    pc.poll_pci_intx_lines();
    assert_eq!(pic_pending_irq(&pc), Some(13));

    let nvme_state = pc
        .nvme
        .as_ref()
        .expect("nvme enabled")
        .borrow()
        .save_state();

    let mut pc2 = PcPlatform::new_with_config(
        2 * 1024 * 1024,
        PcPlatformConfig {
            enable_nvme: true,
            enable_ahci: false,
            enable_uhci: false,
            ..Default::default()
        },
    );
    pc2.nvme
        .as_ref()
        .expect("nvme enabled")
        .borrow_mut()
        .load_state(&nvme_state)
        .unwrap();

    unmask_pic_irq(&mut pc2, 13);
    let mut cmd2 = pci_cfg_read_u16(&mut pc2, bdf, 0x04);
    cmd2 |= 0x0006;
    pci_cfg_write_u16(&mut pc2, bdf, 0x04, cmd2);

    pc2.poll_pci_intx_lines();
    assert_eq!(
        pic_pending_irq(&pc2),
        Some(13),
        "NVMe INTx should remain asserted after snapshot/restore"
    );
}

#[test]
fn st010_ide_snapshot_roundtrip_preserves_irq14_level() {
    let mut pc = PcPlatform::new_with_config(
        2 * 1024 * 1024,
        PcPlatformConfig {
            enable_ide: true,
            enable_uhci: false,
            enable_ahci: false,
            ..Default::default()
        },
    );

    let bdf = profile::IDE_PIIX3.bdf;
    let bar4 = pci_read_bar(&mut pc, bdf, 4);
    assert_eq!(bar4.kind, BarKind::Io);

    let mut cmd = pci_cfg_read_u16(&mut pc, bdf, 0x04);
    cmd |= 0x0005; // IO + BUSMASTER
    pci_cfg_write_u16(&mut pc, bdf, 0x04, cmd);

    // Attach HDD + ISO backends.
    let mut hdd = RawDisk::create(MemBackend::new(), 8 * aero_storage::SECTOR_SIZE as u64).unwrap();
    let mut sector0 = [0u8; aero_storage::SECTOR_SIZE];
    for (i, b) in sector0.iter_mut().enumerate() {
        let half = (i >= 256) as u8;
        *b = (i as u8).wrapping_add(1 + half * 16);
    }
    hdd.write_at(0, &sector0).unwrap();
    pc.attach_ide_primary_master_disk(Box::new(hdd)).unwrap();

    let mut iso = RawDisk::create(MemBackend::new(), 4 * 2048).unwrap();
    iso.write_at(2048, b"WORLD").unwrap();
    pc.attach_ide_secondary_master_iso(Box::new(iso)).unwrap();

    // Trigger a Bus Master DMA transfer to raise IRQ14, then snapshot.
    let bus_master_base = bar4.base as u16;
    let prd_addr = 0x2000u64;
    let buf0 = 0x0FF00u64;
    let buf1 = 0x10000u64;

    pc.memory.write_u32(prd_addr, buf0 as u32);
    pc.memory.write_u16(prd_addr + 4, 256);
    pc.memory.write_u16(prd_addr + 6, 0x0000);
    pc.memory.write_u32(prd_addr + 8, buf1 as u32);
    pc.memory.write_u16(prd_addr + 12, 256);
    pc.memory.write_u16(prd_addr + 14, 0x8000);

    pc.io.write(bus_master_base + 2, 1, 0x06);
    pc.io.write(bus_master_base + 4, 4, prd_addr as u32);

    let pri_cmd = PRIMARY_PORTS.cmd_base;
    pc.io.write(pri_cmd + 6, 1, 0xE0);
    pc.io.write(pri_cmd + 2, 1, 1);
    pc.io.write(pri_cmd + 3, 1, 0);
    pc.io.write(pri_cmd + 4, 1, 0);
    pc.io.write(pri_cmd + 5, 1, 0);
    pc.io.write(pri_cmd + 7, 1, 0xC8);
    pc.io.write(bus_master_base, 1, 0x09);

    pc.process_ide();

    unmask_pic_irq(&mut pc, 14);
    pc.poll_pci_intx_lines();
    assert_eq!(pic_pending_irq(&pc), Some(14));

    let ide_state = pc
        .ide
        .as_ref()
        .expect("ide enabled")
        .borrow()
        .save_state();

    let mut pc2 = PcPlatform::new_with_config(
        2 * 1024 * 1024,
        PcPlatformConfig {
            enable_ide: true,
            enable_uhci: false,
            enable_ahci: false,
            ..Default::default()
        },
    );
    pc2.ide
        .as_ref()
        .expect("ide enabled")
        .borrow_mut()
        .load_state(&ide_state)
        .unwrap();

    // Re-attach backends post-restore (IDE snapshots drop host disks/ISOs).
    let mut hdd2 =
        RawDisk::create(MemBackend::new(), 8 * aero_storage::SECTOR_SIZE as u64).unwrap();
    hdd2.write_at(0, &sector0).unwrap();
    pc2.attach_ide_primary_master_disk(Box::new(hdd2)).unwrap();

    let mut iso2 = RawDisk::create(MemBackend::new(), 4 * 2048).unwrap();
    iso2.write_at(2048, b"WORLD").unwrap();
    pc2.attach_ide_secondary_master_iso(Box::new(iso2)).unwrap();

    let mut cmd2 = pci_cfg_read_u16(&mut pc2, bdf, 0x04);
    cmd2 |= 0x0005;
    pci_cfg_write_u16(&mut pc2, bdf, 0x04, cmd2);

    unmask_pic_irq(&mut pc2, 14);
    pc2.poll_pci_intx_lines();
    assert_eq!(
        pic_pending_irq(&pc2),
        Some(14),
        "IDE primary IRQ14 should remain asserted after snapshot/restore"
    );
}
