use super::{AtapiCdrom, AtaDevice, IdeController, IsoBackend, PRIMARY_PORTS};
use crate::io::storage::disk::{DiskError, DiskResult, MemDisk};
use memory::MemoryBus;

#[derive(Clone, Debug)]
struct VecMemory {
    data: Vec<u8>,
}

impl VecMemory {
    fn new(size: usize) -> Self {
        Self { data: vec![0; size] }
    }

    fn range(&self, paddr: u64, len: usize) -> core::ops::Range<usize> {
        let start = usize::try_from(paddr).expect("paddr too large");
        let end = start.checked_add(len).expect("address wrap");
        assert!(end <= self.data.len(), "out-of-bounds physical access");
        start..end
    }

    fn slice(&self, addr: u64, len: usize) -> &[u8] {
        let r = self.range(addr, len);
        &self.data[r]
    }
}

impl MemoryBus for VecMemory {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        let r = self.range(paddr, buf.len());
        buf.copy_from_slice(&self.data[r]);
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        let r = self.range(paddr, buf.len());
        self.data[r].copy_from_slice(buf);
    }
}

#[derive(Debug)]
struct MemIso {
    sector_count: u32,
    data: Vec<u8>,
}

impl MemIso {
    fn new(sectors: u32) -> Self {
        Self {
            sector_count: sectors,
            data: vec![0u8; sectors as usize * 2048],
        }
    }
}

impl IsoBackend for MemIso {
    fn sector_count(&self) -> u32 {
        self.sector_count
    }

    fn read_sectors(&mut self, lba: u32, buf: &mut [u8]) -> DiskResult<()> {
        if buf.len() % 2048 != 0 {
            return Err(DiskError::InvalidBufferLength);
        }
        let start = lba as usize * 2048;
        let end = start + buf.len();
        if end > self.data.len() {
            return Err(DiskError::OutOfBounds);
        }
        buf.copy_from_slice(&self.data[start..end]);
        Ok(())
    }
}

#[test]
fn ata_pio_read_multi_sector() {
    let mut disk = MemDisk::new(8);
    let mut expected = vec![0u8; 1024];

    // Fill sectors 1 and 2 with recognizable bytes.
    let data = disk.data_mut();
    for i in 0..512 {
        let a = (i & 0xFF) as u8;
        let b = (255 - (i as u8)) as u8;
        data[1 * 512 + i] = a;
        data[2 * 512 + i] = b;
        expected[i] = a;
        expected[512 + i] = b;
    }

    let mut ide = IdeController::new(0xC000);
    ide.attach_primary_master_ata(AtaDevice::new(Box::new(disk), "Aero HDD"));

    // Select master, LBA mode.
    ide.io_write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    // Sector count = 2.
    ide.io_write(PRIMARY_PORTS.cmd_base + 2, 1, 2);
    // LBA = 1.
    ide.io_write(PRIMARY_PORTS.cmd_base + 3, 1, 1);
    ide.io_write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    ide.io_write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    // Command.
    ide.io_write(PRIMARY_PORTS.cmd_base + 7, 1, 0x20);

    let status = ide.io_read(PRIMARY_PORTS.cmd_base + 7, 1) as u8;
    assert_eq!(status & 0x88, 0x08, "DRQ set, BSY clear");

    let mut data = vec![0u8; 1024];
    for i in 0..(1024 / 2) {
        let w = ide.io_read(PRIMARY_PORTS.cmd_base + 0, 2) as u16;
        data[i * 2..i * 2 + 2].copy_from_slice(&w.to_le_bytes());
    }

    assert_eq!(data, expected);
}

#[test]
fn ata_pio_write_multi_sector() {
    let disk = MemDisk::new(4);
    let mut ide = IdeController::new(0xC000);
    ide.attach_primary_master_ata(AtaDevice::new(Box::new(disk), "Aero HDD"));

    // Select master, LBA mode.
    ide.io_write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    // Sector count = 2.
    ide.io_write(PRIMARY_PORTS.cmd_base + 2, 1, 2);
    // LBA = 0.
    ide.io_write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    ide.io_write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    ide.io_write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    // Command.
    ide.io_write(PRIMARY_PORTS.cmd_base + 7, 1, 0x30);

    let mut payload = vec![0u8; 1024];
    for i in 0..payload.len() {
        payload[i] = (i.wrapping_mul(7) & 0xFF) as u8;
    }
    for i in 0..(payload.len() / 2) {
        let w = u16::from_le_bytes([payload[i * 2], payload[i * 2 + 1]]);
        ide.io_write(PRIMARY_PORTS.cmd_base + 0, 2, w as u32);
    }

    // Re-read via PIO to validate the write stuck.
    ide.io_write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    ide.io_write(PRIMARY_PORTS.cmd_base + 2, 1, 2);
    ide.io_write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    ide.io_write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    ide.io_write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    ide.io_write(PRIMARY_PORTS.cmd_base + 7, 1, 0x20);

    let mut readback = vec![0u8; 1024];
    for i in 0..(readback.len() / 2) {
        let w = ide.io_read(PRIMARY_PORTS.cmd_base + 0, 2) as u16;
        readback[i * 2..i * 2 + 2].copy_from_slice(&w.to_le_bytes());
    }
    assert_eq!(readback, payload);
}

#[test]
fn ata_dma_prd_scatter_gather_crosses_page_boundary() {
    let mut disk = MemDisk::new(4);
    let mut expected = vec![0u8; 512];
    for (i, b) in expected.iter_mut().enumerate() {
        *b = (i as u8).wrapping_add(1);
    }
    disk.data_mut()[..512].copy_from_slice(&expected);

    let mut ide = IdeController::new(0xC000);
    ide.attach_primary_master_ata(AtaDevice::new(Box::new(disk), "Aero HDD"));

    let mut mem = VecMemory::new(0x10000);

    // PRD table at 0x1000. First segment crosses a 0x2000 boundary.
    let prd_addr = 0x1000u64;
    // Entry 0: addr 0x1FF0 len 16.
    mem.write_u32(prd_addr, 0x1FF0);
    mem.write_u16(prd_addr + 4, 16);
    mem.write_u16(prd_addr + 6, 0x0000);
    // Entry 1: addr 0x3000 len 496, end-of-table.
    mem.write_u32(prd_addr + 8, 0x3000);
    mem.write_u16(prd_addr + 12, 496);
    mem.write_u16(prd_addr + 14, 0x8000);

    // Program PRD address.
    let bm_base = ide.bus_master_base();
    ide.io_write(bm_base + 4, 4, prd_addr as u32);

    // Issue READ DMA for LBA 0, 1 sector.
    ide.io_write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    ide.io_write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    ide.io_write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    ide.io_write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    ide.io_write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    ide.io_write(PRIMARY_PORTS.cmd_base + 7, 1, 0xC8);

    // Start bus master (direction=read).
    ide.io_write(bm_base + 0, 1, 0x09);
    ide.tick(&mut mem);

    // Validate the DMA wrote the correct bytes into both segments.
    assert_eq!(mem.slice(0x1FF0, 16), &expected[..16]);
    assert_eq!(mem.slice(0x3000, 496), &expected[16..512]);
}

#[test]
fn atapi_read_10_returns_correct_bytes() {
    let mut iso = MemIso::new(4);
    iso.data[0..5].copy_from_slice(b"HELLO");
    iso.data[2048..2053].copy_from_slice(b"WORLD");

    let mut ide = IdeController::new(0xC000);
    ide.attach_secondary_master_atapi(AtapiCdrom::new(Some(Box::new(iso))));

    let sec = super::SECONDARY_PORTS;

    // Select master.
    ide.io_write(sec.cmd_base + 6, 1, 0xA0);

    fn send_packet(ide: &mut IdeController, sec: super::IdePortMap, pkt: &[u8; 12], byte_count: u16) {
        // Clear FEATURES (PIO).
        ide.io_write(sec.cmd_base + 1, 1, 0);
        // Byte count registers.
        ide.io_write(sec.cmd_base + 4, 1, (byte_count & 0xFF) as u32);
        ide.io_write(sec.cmd_base + 5, 1, (byte_count >> 8) as u32);
        // PACKET
        ide.io_write(sec.cmd_base + 7, 1, 0xA0);
        for i in 0..6 {
            let w = u16::from_le_bytes([pkt[i * 2], pkt[i * 2 + 1]]);
            ide.io_write(sec.cmd_base + 0, 2, w as u32);
        }
    }

    // First access returns UNIT ATTENTION (media changed). Clear it by requesting sense.
    let tur = [0u8; 12];
    send_packet(&mut ide, sec, &tur, 0);
    let _ = ide.io_read(sec.cmd_base + 7, 1);

    let mut req_sense = [0u8; 12];
    req_sense[0] = 0x03;
    req_sense[4] = 18;
    send_packet(&mut ide, sec, &req_sense, 18);
    for _ in 0..9 {
        let _ = ide.io_read(sec.cmd_base + 0, 2);
    }

    // Send READ(10) for LBA=1, blocks=1 (should return "WORLD" at start).
    let mut pkt = [0u8; 12];
    pkt[0] = 0x28;
    pkt[2..6].copy_from_slice(&1u32.to_be_bytes());
    pkt[7..9].copy_from_slice(&1u16.to_be_bytes());
    send_packet(&mut ide, sec, &pkt, 2048);

    let mut out = vec![0u8; 2048];
    for i in 0..(2048 / 2) {
        let w = ide.io_read(sec.cmd_base + 0, 2) as u16;
        out[i * 2..i * 2 + 2].copy_from_slice(&w.to_le_bytes());
    }

    assert_eq!(&out[..5], b"WORLD");
}

