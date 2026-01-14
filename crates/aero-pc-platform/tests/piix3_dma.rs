//! Platform-level integration tests for the PIIX3-compatible PCI IDE controller's
//! Bus Master IDE (BMIDE) DMA engine.
//!
//! These tests exercise the full end-to-end plumbing:
//! - PCI enumeration (BAR4 + INTx routing metadata)
//! - guest-programmed PRD tables and DMA transfers into guest RAM
//! - legacy IDE IRQ delivery (IRQ14/IRQ15) after DMA completion

mod helpers;

use aero_devices::pci::profile;
use aero_devices::pci::PciInterruptPin;
use aero_devices_storage::pci_ide::{PRIMARY_PORTS, SECONDARY_PORTS};
use aero_pc_platform::{PcPlatform, PcPlatformConfig};
use aero_storage::{MemBackend, RawDisk, VirtualDisk as _, SECTOR_SIZE};
use memory::MemoryBus as _;

use helpers::*;

fn assert_piix3_ide_pci_intx_is_configured(pc: &mut PcPlatform) {
    let bdf = profile::IDE_PIIX3.bdf;
    let pin = profile::IDE_PIIX3
        .interrupt_pin
        .expect("IDE profile should expose an INTx pin");
    assert_eq!(pin, PciInterruptPin::IntA);

    let expected_irq = u8::try_from(pc.pci_intx.gsi_for_intx(bdf, pin))
        .expect("router-selected GSI should fit in u8");
    assert_eq!(
        expected_irq, 11,
        "topology contract expects IDE INTA# -> GSI11"
    );

    // PCI config-space Interrupt Line/Pin should match the router-selected GSI.
    assert_eq!(pci_cfg_read_u8(pc, bdf, 0x3C), expected_irq);
    assert_eq!(pci_cfg_read_u8(pc, bdf, 0x3D), pin.to_config_u8());
}

fn wait_drq(pc: &mut PcPlatform, cmd_base: u16) {
    for _ in 0..1000 {
        let st = pc.io.read(cmd_base + 7, 1) as u8;
        if (st & 0x80) == 0 && (st & 0x08) != 0 {
            return;
        }
    }
    panic!("timeout waiting for DRQ on IDE port {cmd_base:#x}");
}

fn atapi_send_packet(
    pc: &mut PcPlatform,
    cmd_base: u16,
    features: u8,
    pkt: &[u8; 12],
    byte_count: u16,
) {
    pc.io.write(cmd_base + 1, 1, u32::from(features));
    pc.io.write(cmd_base + 4, 1, u32::from(byte_count & 0xFF));
    pc.io.write(cmd_base + 5, 1, u32::from(byte_count >> 8));
    pc.io.write(cmd_base + 7, 1, 0xA0); // PACKET

    // Wait for the device to request the 12-byte packet.
    wait_drq(pc, cmd_base);

    for i in 0..6 {
        let w = u16::from_le_bytes([pkt[i * 2], pkt[i * 2 + 1]]);
        pc.io.write(cmd_base, 2, u32::from(w));
    }
}

#[test]
fn piix3_ide_ata_read_dma_moves_data_into_guest_memory_and_raises_irq14() {
    let mut pc = PcPlatform::new_with_config(
        2 * 1024 * 1024,
        PcPlatformConfig {
            enable_ide: true,
            enable_ahci: false,
            enable_uhci: false,
            ..Default::default()
        },
    );

    assert_piix3_ide_pci_intx_is_configured(&mut pc);

    // Attach an ATA HDD with a known marker at LBA 0.
    let mut disk = RawDisk::create(MemBackend::new(), 8 * SECTOR_SIZE as u64).unwrap();
    let mut sector0 = vec![0u8; SECTOR_SIZE];
    sector0[0..4].copy_from_slice(b"DMA!");
    disk.write_sectors(0, &sector0).unwrap();
    pc.attach_ide_primary_master_disk(Box::new(disk)).unwrap();

    let bdf = profile::IDE_PIIX3.bdf;
    let bar4 = pci_read_bar(&mut pc, bdf, 4);
    assert_eq!(bar4.kind, BarKind::Io);
    assert_ne!(bar4.base, 0, "BAR4 should be programmed by BIOS POST");
    let bm_base = bar4.base as u16;

    // Ensure BAR4 decodes by reading the BMIDE command register (should default to 0).
    assert_eq!(pc.io.read(bm_base, 1), 0);

    // Enable I/O decode + bus mastering.
    let mut cmd = pci_cfg_read_u16(&mut pc, bdf, 0x04);
    cmd |= 0x0005; // IO + BUSMASTER
    pci_cfg_write_u16(&mut pc, bdf, 0x04, cmd);

    // Observe primary IDE legacy IRQ (IRQ14) via the PIC.
    unmask_pic_irq(&mut pc, 14);

    // Guest memory layout.
    let mut alloc = GuestAllocator::new(2 * 1024 * 1024, 0x1000);
    let prd_addr = alloc.alloc_bytes(8, 4);
    let dma_buf = alloc.alloc_bytes(SECTOR_SIZE, 512);

    // PRD table: one entry, EOT, 512 bytes.
    pc.memory.write_u32(prd_addr, dma_buf as u32);
    pc.memory.write_u16(prd_addr + 4, SECTOR_SIZE as u16);
    pc.memory.write_u16(prd_addr + 6, 0x8000);

    // Program primary PRD pointer and clear BMIDE status.
    pc.io.write(bm_base + 2, 1, 0x06);
    pc.io.write(bm_base + 4, 4, prd_addr as u32);

    // READ DMA (LBA 0, 1 sector).
    pc.io.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0); // master + LBA
    pc.io.write(PRIMARY_PORTS.cmd_base + 2, 1, 1); // count
    pc.io.write(PRIMARY_PORTS.cmd_base + 3, 1, 0); // lba0
    pc.io.write(PRIMARY_PORTS.cmd_base + 4, 1, 0); // lba1
    pc.io.write(PRIMARY_PORTS.cmd_base + 5, 1, 0); // lba2
    pc.io.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xC8); // READ DMA

    // Start bus master (direction = device -> memory).
    pc.io.write(bm_base, 1, 0x09);
    pc.process_ide();
    pc.poll_pci_intx_lines();

    // DMA writes should have landed in guest memory.
    let mut out = [0u8; 4];
    mem_read(&mut pc, dma_buf, &mut out);
    assert_eq!(&out, b"DMA!");

    // Bus master status should reflect successful completion + IRQ.
    let bm_status = pc.io.read(bm_base + 2, 1) as u8;
    assert_ne!(
        bm_status & 0x04,
        0,
        "BMIDE should set IRQ bit on completion"
    );

    assert_eq!(pic_pending_irq(&pc), Some(14));
}

#[test]
fn piix3_ide_atapi_read10_dma_moves_data_into_guest_memory_and_raises_irq15() {
    let mut pc = PcPlatform::new_with_config(
        2 * 1024 * 1024,
        PcPlatformConfig {
            enable_ide: true,
            enable_ahci: false,
            enable_uhci: false,
            ..Default::default()
        },
    );

    assert_piix3_ide_pci_intx_is_configured(&mut pc);

    // Attach an ATAPI ISO backend with a known marker in the first 2048-byte sector.
    let mut iso = RawDisk::create(MemBackend::new(), 4 * 2048).unwrap();
    iso.write_at(0, b"DMATEST!").unwrap();
    pc.attach_ide_secondary_master_iso(Box::new(iso)).unwrap();

    let bdf = profile::IDE_PIIX3.bdf;
    let bar4 = pci_read_bar(&mut pc, bdf, 4);
    assert_eq!(bar4.kind, BarKind::Io);
    assert_ne!(bar4.base, 0, "BAR4 should be programmed by BIOS POST");
    let bm_base = bar4.base as u16;

    // Enable I/O decode + bus mastering.
    let mut cmd = pci_cfg_read_u16(&mut pc, bdf, 0x04);
    cmd |= 0x0005; // IO + BUSMASTER
    pci_cfg_write_u16(&mut pc, bdf, 0x04, cmd);

    // Select secondary master.
    let sec_cmd = SECONDARY_PORTS.cmd_base;
    pc.io.write(sec_cmd + 6, 1, 0xA0);

    // Clear initial UNIT ATTENTION (media change): TEST UNIT READY then REQUEST SENSE.
    let tur = [0u8; 12];
    atapi_send_packet(&mut pc, sec_cmd, 0, &tur, 0);
    let _ = pc.io.read(sec_cmd + 7, 1);

    let mut req_sense = [0u8; 12];
    req_sense[0] = 0x03;
    req_sense[4] = 18;
    atapi_send_packet(&mut pc, sec_cmd, 0, &req_sense, 18);
    wait_drq(&mut pc, sec_cmd);
    for _ in 0..(18 / 2) {
        let _ = pc.io.read(sec_cmd, 2);
    }
    let _ = pc.io.read(sec_cmd + 7, 1);

    // Guest memory layout.
    let mut alloc = GuestAllocator::new(2 * 1024 * 1024, 0x1000);
    let prd_addr = alloc.alloc_bytes(8, 4);
    let dma_buf = alloc.alloc_bytes(2048, 2048);

    // PRD table: one entry, EOT, 2048 bytes.
    pc.memory.write_u32(prd_addr, dma_buf as u32);
    pc.memory.write_u16(prd_addr + 4, 2048);
    pc.memory.write_u16(prd_addr + 6, 0x8000);

    // Program secondary PRD pointer and clear BMIDE status.
    pc.io.write(bm_base + 8 + 2, 1, 0x06);
    pc.io.write(bm_base + 8 + 4, 4, prd_addr as u32);

    // Unmask secondary IDE legacy IRQ (IRQ15) via the PIC.
    unmask_pic_irq(&mut pc, 15);

    // READ(10) for LBA=0, blocks=1 with DMA enabled (FEATURES bit0).
    let mut read10 = [0u8; 12];
    read10[0] = 0x28;
    read10[2..6].copy_from_slice(&0u32.to_be_bytes());
    read10[7..9].copy_from_slice(&1u16.to_be_bytes());
    atapi_send_packet(&mut pc, sec_cmd, 0x01, &read10, 2048);

    // The PACKET command latches an interrupt to request the command packet. Clear it so the IRQ
    // we observe after processing corresponds to DMA completion.
    let _ = pc.io.read(sec_cmd + 7, 1);
    pc.poll_pci_intx_lines();
    assert_eq!(pic_pending_irq(&pc), None);

    // Start bus master (secondary channel, device -> memory).
    pc.io.write(bm_base + 8, 1, 0x09);
    pc.process_ide();
    pc.poll_pci_intx_lines();

    // DMA writes should have landed in guest memory.
    let mut out = [0u8; 8];
    mem_read(&mut pc, dma_buf, &mut out);
    assert_eq!(&out, b"DMATEST!");

    // Bus master status should reflect successful completion + IRQ.
    let bm_status = pc.io.read(bm_base + 8 + 2, 1) as u8;
    assert_ne!(
        bm_status & 0x04,
        0,
        "BMIDE should set IRQ bit on completion"
    );

    assert_eq!(pic_pending_irq(&pc), Some(15));
}
