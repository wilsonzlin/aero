use super::{AtaDevice, AtapiCdrom, IdeController, IsoBackend, PRIMARY_PORTS};
use crate::io::storage::disk::{DiskBackend, DiskError, DiskResult, MemDisk};
use aero_io_snapshot::io::storage::state::MAX_IDE_DATA_BUFFER_BYTES;
use memory::MemoryBus;

#[derive(Clone, Debug)]
struct VecMemory {
    data: Vec<u8>,
}

impl VecMemory {
    fn new(size: usize) -> Self {
        Self {
            data: vec![0; size],
        }
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
        if !buf.len().is_multiple_of(2048) {
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

#[derive(Debug)]
struct RecordingDisk {
    total_sectors: u64,
    last_write_lba: Option<u64>,
    last_write_len: usize,
}

impl RecordingDisk {
    fn new(total_sectors: u64) -> Self {
        Self {
            total_sectors,
            last_write_lba: None,
            last_write_len: 0,
        }
    }
}

#[derive(Clone)]
struct SharedRecordingDisk(std::sync::Arc<std::sync::Mutex<RecordingDisk>>);

impl DiskBackend for SharedRecordingDisk {
    fn sector_size(&self) -> u32 {
        512
    }

    fn total_sectors(&self) -> u64 {
        self.0.lock().unwrap().total_sectors
    }

    fn read_sectors(&mut self, _lba: u64, buf: &mut [u8]) -> Result<(), DiskError> {
        buf.fill(0);
        Ok(())
    }

    fn write_sectors(&mut self, lba: u64, buf: &[u8]) -> Result<(), DiskError> {
        let mut inner = self.0.lock().unwrap();
        inner.last_write_lba = Some(lba);
        inner.last_write_len = buf.len();
        Ok(())
    }

    fn flush(&mut self) -> Result<(), DiskError> {
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
        let b = 255u8 - i as u8;
        data[512 + i] = a;
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
        let w = ide.io_read(PRIMARY_PORTS.cmd_base, 2) as u16;
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
    for (i, b) in payload.iter_mut().enumerate() {
        *b = (i.wrapping_mul(7) & 0xFF) as u8;
    }
    for i in 0..(payload.len() / 2) {
        let w = u16::from_le_bytes([payload[i * 2], payload[i * 2 + 1]]);
        ide.io_write(PRIMARY_PORTS.cmd_base, 2, w as u32);
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
        let w = ide.io_read(PRIMARY_PORTS.cmd_base, 2) as u16;
        readback[i * 2..i * 2 + 2].copy_from_slice(&w.to_le_bytes());
    }
    assert_eq!(readback, payload);
}

#[test]
fn ata_pio_write_sectors_ext_uses_lba48() {
    // Use a "high" LBA that requires the HOB bytes.
    let lba: u64 = 0x01_00_00_00;
    let shared = std::sync::Arc::new(std::sync::Mutex::new(RecordingDisk::new(lba + 16)));
    let disk = SharedRecordingDisk(shared.clone());

    let mut ide = IdeController::new(0xC000);
    ide.attach_primary_master_ata(AtaDevice::new(Box::new(disk), "Aero HDD"));

    // Select master, LBA mode.
    ide.io_write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);

    // Sector count (48-bit): high byte then low byte => 1 sector.
    ide.io_write(PRIMARY_PORTS.cmd_base + 2, 1, 0x00);
    ide.io_write(PRIMARY_PORTS.cmd_base + 2, 1, 0x01);

    // LBA bytes (48-bit): write high bytes first, then low bytes.
    // LBA = 0x01_00_00_00 => HOB LBA0=0x01, others 0.
    ide.io_write(PRIMARY_PORTS.cmd_base + 3, 1, 0x01);
    ide.io_write(PRIMARY_PORTS.cmd_base + 3, 1, 0x00);
    ide.io_write(PRIMARY_PORTS.cmd_base + 4, 1, 0x00);
    ide.io_write(PRIMARY_PORTS.cmd_base + 4, 1, 0x00);
    ide.io_write(PRIMARY_PORTS.cmd_base + 5, 1, 0x00);
    ide.io_write(PRIMARY_PORTS.cmd_base + 5, 1, 0x00);

    // Command: WRITE SECTORS EXT.
    ide.io_write(PRIMARY_PORTS.cmd_base + 7, 1, 0x34);

    // Transfer one sector (PIO OUT).
    for i in 0..256u16 {
        ide.io_write(PRIMARY_PORTS.cmd_base, 2, i as u32);
    }

    let inner = shared.lock().unwrap();
    assert_eq!(inner.last_write_lba, Some(lba));
    assert_eq!(inner.last_write_len, 512);
}

#[test]
fn ata_lba48_oversized_pio_read_is_rejected_without_entering_data_phase() {
    // Construct a transfer size larger than the snapshot/device cap. If the cap ever grows
    // beyond the largest representable LBA48 transfer (65536 sectors), skip the assertion.
    let sectors = (MAX_IDE_DATA_BUFFER_BYTES / 512) as u32 + 1;
    if sectors > 65536 {
        return;
    }

    // Use a lightweight backend so the test doesn't allocate a ~16MiB in-memory disk image.
    let disk = SharedRecordingDisk(std::sync::Arc::new(std::sync::Mutex::new(
        RecordingDisk::new(sectors as u64),
    )));
    let mut ide = IdeController::new(0xC000);
    ide.attach_primary_master_ata(AtaDevice::new(Box::new(disk), "Aero HDD"));

    // Select master, LBA mode.
    ide.io_write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);

    // Sector count (48-bit): high then low.
    ide.io_write(PRIMARY_PORTS.cmd_base + 2, 1, sectors >> 8);
    ide.io_write(PRIMARY_PORTS.cmd_base + 2, 1, sectors & 0xFF);

    // LBA = 0 (48-bit writes, high then low for each byte).
    ide.io_write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    ide.io_write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    ide.io_write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    ide.io_write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    ide.io_write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    ide.io_write(PRIMARY_PORTS.cmd_base + 5, 1, 0);

    // READ SECTORS EXT (PIO).
    ide.io_write(PRIMARY_PORTS.cmd_base + 7, 1, 0x24);

    let status = ide.io_read(PRIMARY_PORTS.cmd_base + 7, 1) as u8;
    assert_eq!(status & 0x80, 0, "BSY should be clear");
    assert_eq!(status & 0x08, 0, "DRQ should be clear (no data phase)");
    assert_ne!(status & 0x01, 0, "ERR should be set");
    assert_eq!(ide.io_read(PRIMARY_PORTS.cmd_base + 1, 1) as u8, 0x04);
}

#[test]
fn pio_out_allocation_failure_aborts_command_instead_of_panicking() {
    let mut chan = super::Channel::new(PRIMARY_PORTS);

    // Seed in-flight state that should get cleared by `abort_command`.
    chan.status = super::IDE_STATUS_BSY | super::IDE_STATUS_DRQ;
    chan.data_mode = super::DataMode::PioIn;
    chan.transfer_kind = Some(super::TransferKind::AtaPioRead);
    chan.data = vec![1, 2, 3];
    chan.data_index = 1;
    chan.irq_pending = false;
    chan.pending_dma = Some(super::busmaster::DmaRequest::atapi_data_in(vec![0xAA]));
    chan.pio_write = Some((0x1234, 1));

    // Use a length that deterministically fails `try_reserve_exact` with a capacity overflow,
    // without actually attempting to allocate an enormous buffer.
    chan.begin_pio_out(super::TransferKind::AtaPioWrite, usize::MAX);

    assert_eq!(chan.data_mode, super::DataMode::None);
    assert_eq!(chan.transfer_kind, None);
    assert!(chan.data.is_empty());
    assert_eq!(chan.data_index, 0);
    assert!(chan.pending_dma.is_none());
    assert!(chan.pio_write.is_none());

    assert_eq!(chan.error, 0x04);
    assert_ne!(chan.status & super::IDE_STATUS_ERR, 0);
    assert_ne!(chan.status & super::IDE_STATUS_DRDY, 0);
    assert_eq!(chan.status & super::IDE_STATUS_BSY, 0);
    assert_eq!(chan.status & super::IDE_STATUS_DRQ, 0);
    assert!(chan.irq_pending);
}

#[test]
fn ata_lba48_oversized_pio_write_is_rejected_without_allocating_buffer() {
    let sectors = (MAX_IDE_DATA_BUFFER_BYTES / 512) as u32 + 1;
    if sectors > 65536 {
        return;
    }

    let disk = MemDisk::new(1);
    let mut ide = IdeController::new(0xC000);
    ide.attach_primary_master_ata(AtaDevice::new(Box::new(disk), "Aero HDD"));

    ide.io_write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    ide.io_write(PRIMARY_PORTS.cmd_base + 2, 1, sectors >> 8);
    ide.io_write(PRIMARY_PORTS.cmd_base + 2, 1, sectors & 0xFF);
    ide.io_write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    ide.io_write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    ide.io_write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    ide.io_write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    ide.io_write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    ide.io_write(PRIMARY_PORTS.cmd_base + 5, 1, 0);

    // WRITE SECTORS EXT (PIO).
    ide.io_write(PRIMARY_PORTS.cmd_base + 7, 1, 0x34);

    let status = ide.io_read(PRIMARY_PORTS.cmd_base + 7, 1) as u8;
    assert_eq!(status & 0x80, 0, "BSY should be clear");
    assert_eq!(status & 0x08, 0, "DRQ should be clear (no data phase)");
    assert_ne!(status & 0x01, 0, "ERR should be set");
    assert_eq!(ide.io_read(PRIMARY_PORTS.cmd_base + 1, 1) as u8, 0x04);
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
    ide.io_write(bm_base, 1, 0x09);
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

    fn send_packet(
        ide: &mut IdeController,
        sec: super::IdePortMap,
        pkt: &[u8; 12],
        byte_count: u16,
    ) {
        // Clear FEATURES (PIO).
        ide.io_write(sec.cmd_base + 1, 1, 0);
        // Byte count registers.
        ide.io_write(sec.cmd_base + 4, 1, (byte_count & 0xFF) as u32);
        ide.io_write(sec.cmd_base + 5, 1, (byte_count >> 8) as u32);
        // PACKET
        ide.io_write(sec.cmd_base + 7, 1, 0xA0);
        for i in 0..6 {
            let w = u16::from_le_bytes([pkt[i * 2], pkt[i * 2 + 1]]);
            ide.io_write(sec.cmd_base, 2, w as u32);
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
        let _ = ide.io_read(sec.cmd_base, 2);
    }

    // Send READ(10) for LBA=1, blocks=1 (should return "WORLD" at start).
    let mut pkt = [0u8; 12];
    pkt[0] = 0x28;
    pkt[2..6].copy_from_slice(&1u32.to_be_bytes());
    pkt[7..9].copy_from_slice(&1u16.to_be_bytes());
    send_packet(&mut ide, sec, &pkt, 2048);

    let mut out = vec![0u8; 2048];
    for i in 0..(2048 / 2) {
        let w = ide.io_read(sec.cmd_base, 2) as u16;
        out[i * 2..i * 2 + 2].copy_from_slice(&w.to_le_bytes());
    }

    assert_eq!(&out[..5], b"WORLD");
}

#[test]
fn atapi_read_12_rejects_oversized_transfer_without_allocating_buffer() {
    #[derive(Debug)]
    struct ZeroIso {
        sector_count: u32,
    }

    impl IsoBackend for ZeroIso {
        fn sector_count(&self) -> u32 {
            self.sector_count
        }

        fn read_sectors(&mut self, _lba: u32, buf: &mut [u8]) -> DiskResult<()> {
            if !buf.len().is_multiple_of(2048) {
                return Err(DiskError::InvalidBufferLength);
            }
            buf.fill(0);
            Ok(())
        }
    }

    let blocks = (MAX_IDE_DATA_BUFFER_BYTES / 2048) as u32 + 1;
    let iso = ZeroIso {
        sector_count: blocks,
    };
    let mut ide = IdeController::new(0xC000);
    ide.attach_secondary_master_atapi(AtapiCdrom::new(Some(Box::new(iso))));
    let sec = super::SECONDARY_PORTS;
    ide.io_write(sec.cmd_base + 6, 1, 0xA0);

    fn send_packet(
        ide: &mut IdeController,
        sec: super::IdePortMap,
        pkt: &[u8; 12],
        byte_count: u16,
    ) {
        ide.io_write(sec.cmd_base + 1, 1, 0);
        ide.io_write(sec.cmd_base + 4, 1, (byte_count & 0xFF) as u32);
        ide.io_write(sec.cmd_base + 5, 1, (byte_count >> 8) as u32);
        ide.io_write(sec.cmd_base + 7, 1, 0xA0);
        for i in 0..6 {
            let w = u16::from_le_bytes([pkt[i * 2], pkt[i * 2 + 1]]);
            ide.io_write(sec.cmd_base, 2, w as u32);
        }
    }

    // Clear initial UNIT ATTENTION.
    let tur = [0u8; 12];
    send_packet(&mut ide, sec, &tur, 0);
    let _ = ide.io_read(sec.cmd_base + 7, 1);
    let mut req_sense = [0u8; 12];
    req_sense[0] = 0x03;
    req_sense[4] = 18;
    send_packet(&mut ide, sec, &req_sense, 18);
    for _ in 0..9 {
        let _ = ide.io_read(sec.cmd_base, 2);
    }

    let mut pkt = [0u8; 12];
    pkt[0] = 0xA8; // READ(12)
    pkt[6..10].copy_from_slice(&blocks.to_be_bytes());
    send_packet(&mut ide, sec, &pkt, 2048);

    assert!(ide.secondary_irq_pending());
    assert_eq!(ide.io_read(sec.cmd_base + 2, 1) as u8, 0x03);

    let status = ide.io_read(sec.cmd_base + 7, 1) as u8;
    assert_eq!(status & 0x80, 0, "BSY should be clear");
    assert_eq!(status & 0x08, 0, "DRQ should be clear (no data phase)");
    assert_ne!(status & 0x01, 0, "ERR should be set");
    assert_eq!(ide.io_read(sec.cmd_base + 1, 1) as u8, 0x04);
}

#[test]
fn atapi_dma_read_10_transfers_via_bus_master() {
    let mut iso = MemIso::new(2);
    iso.data[0..8].copy_from_slice(b"DMATEST!");

    let mut ide = IdeController::new(0xC000);
    ide.attach_secondary_master_atapi(AtapiCdrom::new(Some(Box::new(iso))));

    let sec = super::SECONDARY_PORTS;
    // Select master.
    ide.io_write(sec.cmd_base + 6, 1, 0xA0);

    let mut mem = VecMemory::new(0x10000);
    let prd_addr = 0x1000u64;
    let dma_buf = 0x3000u64;

    // PRD entry: one 2048-byte segment, end-of-table.
    mem.write_u32(prd_addr, dma_buf as u32);
    mem.write_u16(prd_addr + 4, 2048);
    mem.write_u16(prd_addr + 6, 0x8000);

    let bm_base = ide.bus_master_base();
    // Program secondary PRD pointer (base + 8 + 4).
    ide.io_write(bm_base + 8 + 4, 4, prd_addr as u32);

    // Helper: send an ATAPI PACKET command with DMA enabled.
    fn send_packet_dma(
        ide: &mut IdeController,
        sec: super::IdePortMap,
        pkt: &[u8; 12],
        byte_count: u16,
    ) {
        ide.io_write(sec.cmd_base + 1, 1, 0x01); // FEATURES bit0 = DMA
        ide.io_write(sec.cmd_base + 4, 1, (byte_count & 0xFF) as u32);
        ide.io_write(sec.cmd_base + 5, 1, (byte_count >> 8) as u32);
        ide.io_write(sec.cmd_base + 7, 1, 0xA0);
        for i in 0..6 {
            let w = u16::from_le_bytes([pkt[i * 2], pkt[i * 2 + 1]]);
            ide.io_write(sec.cmd_base, 2, w as u32);
        }
    }

    // Clear initial UNIT ATTENTION by issuing TEST UNIT READY and REQUEST SENSE.
    let tur = [0u8; 12];
    send_packet_dma(&mut ide, sec, &tur, 0);
    let _ = ide.io_read(sec.cmd_base + 7, 1);

    let mut req_sense = [0u8; 12];
    req_sense[0] = 0x03;
    req_sense[4] = 18;
    send_packet_dma(&mut ide, sec, &req_sense, 18);
    for _ in 0..9 {
        let _ = ide.io_read(sec.cmd_base, 2);
    }

    // READ(10) for LBA=0, blocks=1.
    let mut pkt = [0u8; 12];
    pkt[0] = 0x28;
    pkt[2..6].copy_from_slice(&0u32.to_be_bytes());
    pkt[7..9].copy_from_slice(&1u16.to_be_bytes());
    send_packet_dma(&mut ide, sec, &pkt, 2048);

    // Start the secondary bus master engine, direction=read (device -> memory).
    ide.io_write(bm_base + 8, 1, 0x09);
    ide.tick(&mut mem);

    // DMA should have populated the guest buffer.
    assert_eq!(&mem.slice(dma_buf, 8), b"DMATEST!");

    // Bus master status should indicate interrupt.
    let st = ide.io_read(bm_base + 8 + 2, 1) as u8;
    assert_ne!(st & 0x04, 0);

    // ATAPI interrupt reason should be in the status phase.
    assert_eq!(ide.io_read(sec.cmd_base + 2, 1) as u8, 0x03);
    assert!(ide.secondary_irq_pending());
}

#[test]
fn atapi_get_event_status_notification_returns_media_event_header() {
    let iso = MemIso::new(1);
    let mut ide = IdeController::new(0xC000);
    ide.attach_secondary_master_atapi(AtapiCdrom::new(Some(Box::new(iso))));
    let sec = super::SECONDARY_PORTS;

    ide.io_write(sec.cmd_base + 6, 1, 0xA0);

    fn send_packet(
        ide: &mut IdeController,
        sec: super::IdePortMap,
        pkt: &[u8; 12],
        byte_count: u16,
    ) {
        ide.io_write(sec.cmd_base + 1, 1, 0);
        ide.io_write(sec.cmd_base + 4, 1, (byte_count & 0xFF) as u32);
        ide.io_write(sec.cmd_base + 5, 1, (byte_count >> 8) as u32);
        ide.io_write(sec.cmd_base + 7, 1, 0xA0);
        for i in 0..6 {
            let w = u16::from_le_bytes([pkt[i * 2], pkt[i * 2 + 1]]);
            ide.io_write(sec.cmd_base, 2, w as u32);
        }
    }

    // Clear initial UNIT ATTENTION.
    let tur = [0u8; 12];
    send_packet(&mut ide, sec, &tur, 0);
    let _ = ide.io_read(sec.cmd_base + 7, 1);
    let mut req_sense = [0u8; 12];
    req_sense[0] = 0x03;
    req_sense[4] = 18;
    send_packet(&mut ide, sec, &req_sense, 18);
    for _ in 0..9 {
        let _ = ide.io_read(sec.cmd_base, 2);
    }

    // GET EVENT STATUS NOTIFICATION, request nonzero so we get an 8-byte response.
    let mut gesn = [0u8; 12];
    gesn[0] = 0x4A;
    gesn[4] = 0x01;
    gesn[7..9].copy_from_slice(&8u16.to_be_bytes());
    send_packet(&mut ide, sec, &gesn, 8);

    let mut out = [0u8; 8];
    for i in 0..4 {
        let w = ide.io_read(sec.cmd_base, 2) as u16;
        out[i * 2..i * 2 + 2].copy_from_slice(&w.to_le_bytes());
    }
    assert_eq!(out[0..2], [0, 6]); // payload length following the first 2 bytes
    assert_eq!(out[2], 0x08); // media event class
    assert_eq!(out[3], 0x08); // supported classes mask
    assert_eq!(out[4], 0); // no change
    assert_eq!(out[5] & 0x01, 0x01); // media present
}

#[test]
fn atapi_read_disc_information_returns_valid_length_field() {
    let iso = MemIso::new(1);
    let mut ide = IdeController::new(0xC000);
    ide.attach_secondary_master_atapi(AtapiCdrom::new(Some(Box::new(iso))));
    let sec = super::SECONDARY_PORTS;
    ide.io_write(sec.cmd_base + 6, 1, 0xA0);

    fn send_packet(
        ide: &mut IdeController,
        sec: super::IdePortMap,
        pkt: &[u8; 12],
        byte_count: u16,
    ) {
        ide.io_write(sec.cmd_base + 1, 1, 0);
        ide.io_write(sec.cmd_base + 4, 1, (byte_count & 0xFF) as u32);
        ide.io_write(sec.cmd_base + 5, 1, (byte_count >> 8) as u32);
        ide.io_write(sec.cmd_base + 7, 1, 0xA0);
        for i in 0..6 {
            let w = u16::from_le_bytes([pkt[i * 2], pkt[i * 2 + 1]]);
            ide.io_write(sec.cmd_base, 2, w as u32);
        }
    }

    // Clear initial UNIT ATTENTION.
    let tur = [0u8; 12];
    send_packet(&mut ide, sec, &tur, 0);
    let _ = ide.io_read(sec.cmd_base + 7, 1);
    let mut req_sense = [0u8; 12];
    req_sense[0] = 0x03;
    req_sense[4] = 18;
    send_packet(&mut ide, sec, &req_sense, 18);
    for _ in 0..9 {
        let _ = ide.io_read(sec.cmd_base, 2);
    }

    let mut disc_info = [0u8; 12];
    disc_info[0] = 0x51;
    disc_info[7..9].copy_from_slice(&34u16.to_be_bytes());
    send_packet(&mut ide, sec, &disc_info, 34);

    let mut out = [0u8; 34];
    for i in 0..(34 / 2) {
        let w = ide.io_read(sec.cmd_base, 2) as u16;
        out[i * 2..i * 2 + 2].copy_from_slice(&w.to_le_bytes());
    }
    assert_eq!(out[0..2], [0, 32]); // out.len() - 2
    assert_eq!(out[2], 0x0E);
}

#[test]
fn pci_bar4_probe_returns_size_mask_and_relocation_updates_io_decode() {
    let mut ide = IdeController::new(0xC000);

    // Initial BAR4 value is base|IO.
    assert_eq!(ide.pci_config_read(0x20, 4), 0xC000 | 0x01);

    // Probe.
    ide.pci_config_write(0x20, 4, 0xffff_ffff);
    assert_eq!(ide.pci_config_read(0x20, 4), 0xffff_fff1);

    // Relocate to 0xD000.
    ide.pci_config_write(0x20, 4, 0xD000);
    assert_eq!(ide.bus_master_base(), 0xD000);
    assert_eq!(ide.pci_config_read(0x20, 4), 0xD000 | 0x01);

    // The bus master decode should now only respond to the relocated base.
    // Read the BM command register (reg 0) for primary channel.
    assert_eq!(ide.io_read(0xC000, 1), 0xffff_ffff);
    assert_eq!(ide.io_read(0xD000, 1), 0);
}

#[test]
fn pci_wrapper_gates_ide_ports_on_pci_command_io_bit() {
    let mut ide = IdeController::new(0xC000);

    // Sanity: with COMMAND.IO set by default, primary status is visible.
    assert_eq!(
        ide.io_read(PRIMARY_PORTS.cmd_base + 7, 1) as u8,
        0x40,
        "DRDY should be set after reset"
    );

    // Disable I/O space decode.
    ide.pci_config_write(0x04, 2, 0);

    // Reads float high and writes are ignored.
    assert_eq!(ide.io_read(PRIMARY_PORTS.cmd_base + 7, 1), 0xff);
    ide.io_write(PRIMARY_PORTS.cmd_base + 6, 1, 0xe0);

    // Re-enable I/O decode and verify that the ignored write didn't latch.
    ide.pci_config_write(0x04, 2, 1);
    assert_eq!(ide.io_read(PRIMARY_PORTS.cmd_base + 6, 1) as u8, 0x00);
}

#[test]
fn size0_io_access_is_noop() {
    let mut ide = IdeController::new(0xC000);
    ide.attach_primary_master_ata(AtaDevice::new(Box::new(MemDisk::new(1)), "Aero HDD"));

    // Disable PCI I/O space decode.
    ide.pci_config_write(0x04, 2, 0);

    // Size-0 reads should be true no-ops regardless of whether the device is decoded.
    assert_eq!(ide.io_read(PRIMARY_PORTS.cmd_base + 7, 0), 0);

    // Re-enable I/O decode for the remainder of the test.
    ide.pci_config_write(0x04, 2, 1);

    // Trigger an IRQ by issuing IDENTIFY DEVICE.
    ide.io_write(PRIMARY_PORTS.cmd_base + 7, 1, 0xec);
    assert!(ide.primary_irq_pending());

    // Reading STATUS with size=0 should not clear the pending IRQ.
    assert_eq!(ide.io_read(PRIMARY_PORTS.cmd_base + 7, 0), 0);
    assert!(ide.primary_irq_pending());

    // Reading STATUS with size=1 should clear it (normal ATA semantics).
    let _ = ide.io_read(PRIMARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.primary_irq_pending());

    // Size-0 writes should not modify any state (e.g. the DEVICE register).
    let device_before = ide.io_read(PRIMARY_PORTS.cmd_base + 6, 1);
    ide.io_write(PRIMARY_PORTS.cmd_base + 6, 0, 0xe0);
    let device_after = ide.io_read(PRIMARY_PORTS.cmd_base + 6, 1);
    assert_eq!(device_after, device_before);
}
