//! Windows 7 canonical storage topology: IDE Bus Master DMA integration tests.
//!
//! The Win7 topology always instantiates the PIIX3 IDE controller (`00:01.1`) for ATAPI install
//! media, but DMA is exercised by real guests (Windows `pciide.sys` + `atapi.sys`) for both ATA and
//! ATAPI transfers. This test verifies that the full `PcPlatform` wiring (PCI config, BAR4 routing,
//! DMA into guest RAM) cooperates correctly for those paths when bootstrapped via
//! `PcPlatform::new_with_windows7_storage_topology(...)`.

mod helpers;

use std::io;

use aero_devices::pci::profile;
use aero_devices_storage::ata::AtaDrive;
use aero_devices_storage::atapi::{AtapiCdrom, IsoBackend};
use aero_devices_storage::pci_ide::{PRIMARY_PORTS, SECONDARY_PORTS};
use aero_pc_platform::{PcPlatform, Windows7StorageTopologyConfig};
use aero_storage::{MemBackend, RawDisk, VirtualDisk as _, SECTOR_SIZE};
use memory::MemoryBus as _;

use helpers::*;

struct MemIso {
    bytes: Vec<u8>,
}

impl IsoBackend for MemIso {
    fn sector_count(&self) -> u32 {
        (self.bytes.len() / AtapiCdrom::SECTOR_SIZE) as u32
    }

    fn read_sectors(&mut self, lba: u32, buf: &mut [u8]) -> io::Result<()> {
        if !buf.len().is_multiple_of(AtapiCdrom::SECTOR_SIZE) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "buf length not multiple of 2048",
            ));
        }
        let byte_off = (lba as usize)
            .checked_mul(AtapiCdrom::SECTOR_SIZE)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "offset overflow"))?;
        let byte_end = byte_off
            .checked_add(buf.len())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "offset overflow"))?;
        if byte_end > self.bytes.len() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "read past EOF",
            ));
        }
        buf.copy_from_slice(&self.bytes[byte_off..byte_end]);
        Ok(())
    }
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
    pc.io
        .write(cmd_base + 4, 1, u32::from((byte_count & 0xFF) as u8));
    pc.io
        .write(cmd_base + 5, 1, u32::from((byte_count >> 8) as u8));
    pc.io.write(cmd_base + 7, 1, 0xA0); // PACKET

    // Wait for the device to request the 12-byte packet.
    wait_drq(pc, cmd_base);
    for i in 0..6 {
        let w = u16::from_le_bytes([pkt[i * 2], pkt[i * 2 + 1]]);
        pc.io.write(cmd_base, 2, u32::from(w));
    }
}

fn atapi_clear_unit_attention(pc: &mut PcPlatform, cmd_base: u16) {
    // The ATAPI model reports UNIT ATTENTION once after media insertion.
    // Clear it via TEST UNIT READY followed by REQUEST SENSE.
    let tur = [0u8; 12];
    atapi_send_packet(pc, cmd_base, 0, &tur, 0);
    // Drain/clear any interrupt state.
    let _ = pc.io.read(cmd_base + 7, 1);

    let mut req_sense = [0u8; 12];
    req_sense[0] = 0x03;
    req_sense[4] = 18;
    atapi_send_packet(pc, cmd_base, 0, &req_sense, 18);

    // Wait for the data-in phase, then drain 18 bytes.
    wait_drq(pc, cmd_base);
    for _ in 0..(18 / 2) {
        let _ = pc.io.read(cmd_base, 2);
    }
    let _ = pc.io.read(cmd_base + 7, 1);
}

fn pump_ide_until_bm_irq(pc: &mut PcPlatform, bm_status_port: u16) {
    for _ in 0..16 {
        pc.process_ide();
        let st = pc.io.read(bm_status_port, 1) as u8;
        if (st & 0x04) != 0 {
            return;
        }
    }
    panic!("timeout waiting for Bus Master IDE completion (status port {bm_status_port:#x})");
}

#[test]
fn win7_storage_topology_piix3_ide_busmaster_dma_ata_and_atapi() {
    let ram_size = 2 * 1024 * 1024;

    // --- Canonical Win7 devices ---
    // AHCI HDD on port 0 (not used by this test directly, but required by the Win7 constructor).
    let ahci_capacity = 8 * SECTOR_SIZE as u64;
    let ahci_disk = RawDisk::create(MemBackend::new(), ahci_capacity).unwrap();
    let hdd = AtaDrive::new(Box::new(ahci_disk)).unwrap();

    // ATAPI install media on PIIX3 IDE secondary master.
    let mut iso_bytes = vec![0u8; AtapiCdrom::SECTOR_SIZE];
    for (i, b) in iso_bytes.iter_mut().enumerate() {
        *b = (i as u8).wrapping_add(0x40);
    }
    let expected_iso = iso_bytes.clone();
    let cdrom = AtapiCdrom::new(Some(Box::new(MemIso { bytes: iso_bytes })));

    let mut pc = PcPlatform::new_with_windows7_storage_topology(
        ram_size,
        Windows7StorageTopologyConfig { hdd, cdrom },
    );

    // Observe IDE legacy IRQs via the PIC.
    unmask_pic_irq(&mut pc, 14);
    unmask_pic_irq(&mut pc, 15);

    // Optional Win7 topology slot: IDE primary master ATA disk (disk_id=2).
    let ide_capacity = 8 * SECTOR_SIZE as u64;
    let mut ide_disk = RawDisk::create(MemBackend::new(), ide_capacity).unwrap();
    let mut expected_ata = [0u8; SECTOR_SIZE];
    for (i, b) in expected_ata.iter_mut().enumerate() {
        *b = (i as u8).wrapping_add(0x10);
    }
    ide_disk.write_at(0, &expected_ata).unwrap();
    pc.attach_ide_primary_master_disk(Box::new(ide_disk))
        .unwrap();

    // Locate the BMIDE BAR4 base and enable bus mastering.
    let bdf = profile::IDE_PIIX3.bdf;
    let bar4 = pci_read_bar(&mut pc, bdf, 4);
    assert_eq!(bar4.kind, BarKind::Io);
    assert_ne!(bar4.base, 0);
    let bm_base = bar4.base as u16;

    // Enable I/O decoding and bus mastering (DMA).
    let mut cmd = pci_cfg_read_u16(&mut pc, bdf, 0x04);
    cmd |= 0x0005; // IO + BUSMASTER
    pci_cfg_write_u16(&mut pc, bdf, 0x04, cmd);

    // Guest memory scratch: keep allocations small.
    let mut alloc = GuestAllocator::new(ram_size as u64, 0x1000);
    let ata_prd = alloc.alloc_bytes(8, 4);
    let ata_buf = alloc.alloc_bytes(SECTOR_SIZE, 4);
    let atapi_prd = alloc.alloc_bytes(8, 4);
    let atapi_buf = alloc.alloc_bytes(AtapiCdrom::SECTOR_SIZE, 4);

    // -------------------------------------------------------------------------
    // 1) ATA DMA: READ DMA (0xC8) from IDE primary master to guest memory.
    // -------------------------------------------------------------------------
    // One PRD entry: 512 bytes, EOT.
    pc.memory.write_u32(ata_prd, ata_buf as u32);
    pc.memory.write_u16(ata_prd + 4, SECTOR_SIZE as u16);
    pc.memory.write_u16(ata_prd + 6, 0x8000);

    // Clear BMIDE status and program PRD pointer (primary channel).
    pc.io.write(bm_base + 2, 1, 0x06);
    pc.io.write(bm_base + 4, 4, ata_prd as u32);

    // READ DMA (LBA 0, 1 sector).
    let pri_cmd = PRIMARY_PORTS.cmd_base;
    pc.io.write(pri_cmd + 6, 1, 0xE0); // master + LBA
    pc.io.write(pri_cmd + 2, 1, 1); // sector count
    pc.io.write(pri_cmd + 3, 1, 0); // LBA0
    pc.io.write(pri_cmd + 4, 1, 0);
    pc.io.write(pri_cmd + 5, 1, 0);
    pc.io.write(pri_cmd + 7, 1, 0xC8); // READ DMA

    pc.io.write(bm_base, 1, 0x09); // start + direction=read (device -> memory)
    pump_ide_until_bm_irq(&mut pc, bm_base + 2);
    pc.poll_pci_intx_lines();
    assert_eq!(pic_pending_irq(&pc), Some(14));

    let bm_st = pc.io.read(bm_base + 2, 1) as u8;
    assert_eq!(
        bm_st & 0x07,
        0x04,
        "expected BMIDE primary status IRQ=1, ACTIVE=0, ERR=0"
    );

    let mut got = [0u8; SECTOR_SIZE];
    mem_read(&mut pc, ata_buf, &mut got);
    assert_eq!(got, expected_ata);

    // ACK+EOI the interrupt so the PIC doesn't retain stale state.
    if let Some(vector) = pic_pending_vector(&pc) {
        pic_acknowledge_and_eoi(&mut pc, vector);
    }
    // Clear the device interrupt by reading Status, then propagate deassertion.
    let _ = pc.io.read(pri_cmd + 7, 1);
    pc.poll_pci_intx_lines();
    assert_eq!(pic_pending_vector(&pc), None);

    // Stop + clear status bits to mimic typical driver behavior.
    pc.io.write(bm_base, 1, 0);
    pc.io.write(bm_base + 2, 1, 0x06);

    // -------------------------------------------------------------------------
    // 2) ATAPI DMA: PACKET READ(10) with DMA requested, secondary master.
    // -------------------------------------------------------------------------
    // One PRD entry: 2048 bytes, EOT.
    pc.memory.write_u32(atapi_prd, atapi_buf as u32);
    pc.memory
        .write_u16(atapi_prd + 4, AtapiCdrom::SECTOR_SIZE as u16);
    pc.memory.write_u16(atapi_prd + 6, 0x8000);

    // Program secondary PRD pointer (BMIDE base + 8 + 4) and clear status.
    pc.io.write(bm_base + 8 + 2, 1, 0x06);
    pc.io.write(bm_base + 8 + 4, 4, atapi_prd as u32);

    // Select secondary master.
    let sec_cmd = SECONDARY_PORTS.cmd_base;
    pc.io.write(sec_cmd + 6, 1, 0xA0);
    atapi_clear_unit_attention(&mut pc, sec_cmd);
    // Clear any pending IRQ from TEST UNIT READY / REQUEST SENSE before starting the DMA read.
    pc.poll_pci_intx_lines();
    if let Some(vector) = pic_pending_vector(&pc) {
        pic_acknowledge_and_eoi(&mut pc, vector);
    }
    assert_eq!(pic_pending_vector(&pc), None);

    // READ(10) LBA=0 blocks=1 with DMA enabled (FEATURES bit0).
    let mut read10 = [0u8; 12];
    read10[0] = 0x28;
    read10[7..9].copy_from_slice(&1u16.to_be_bytes());
    atapi_send_packet(
        &mut pc,
        sec_cmd,
        0x01,
        &read10,
        AtapiCdrom::SECTOR_SIZE as u16,
    );
    // The PACKET command phase can raise an interrupt requesting the 12-byte packet; our helper
    // supplies it synchronously, so clear that interrupt before checking DMA completion.
    let _ = pc.io.read(sec_cmd + 7, 1);
    pc.poll_pci_intx_lines();
    if let Some(vector) = pic_pending_vector(&pc) {
        pic_acknowledge_and_eoi(&mut pc, vector);
    }
    assert_eq!(pic_pending_vector(&pc), None);

    pc.io.write(bm_base + 8, 1, 0x09); // start + direction=read
    pump_ide_until_bm_irq(&mut pc, bm_base + 8 + 2);
    pc.poll_pci_intx_lines();
    assert_eq!(pic_pending_irq(&pc), Some(15));

    let bm_st = pc.io.read(bm_base + 8 + 2, 1) as u8;
    assert_eq!(
        bm_st & 0x07,
        0x04,
        "expected BMIDE secondary status IRQ=1, ACTIVE=0, ERR=0"
    );

    let mut got = [0u8; AtapiCdrom::SECTOR_SIZE];
    mem_read(&mut pc, atapi_buf, &mut got);
    assert_eq!(&got[..], expected_iso.as_slice());

    if let Some(vector) = pic_pending_vector(&pc) {
        pic_acknowledge_and_eoi(&mut pc, vector);
    }
    let _ = pc.io.read(sec_cmd + 7, 1);
    pc.poll_pci_intx_lines();
    assert_eq!(pic_pending_vector(&pc), None);
}
