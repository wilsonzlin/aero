use aero_io_snapshot::io::state::IoSnapshot;
use emulator::io::storage::disk::{DiskError, DiskResult, MemDisk};
use emulator::io::storage::ide::{AtaDevice, AtapiCdrom, IdeController, IsoBackend, PRIMARY_PORTS};
use memory::MemoryBus;

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

#[derive(Clone, Debug)]
struct VecMemory {
    data: Vec<u8>,
}

impl VecMemory {
    fn new(size: usize) -> Self {
        Self {
            data: vec![0u8; size],
        }
    }

    fn range(&self, paddr: u64, len: usize) -> core::ops::Range<usize> {
        let start = usize::try_from(paddr).expect("paddr too large");
        let end = start.checked_add(len).expect("address wrap");
        assert!(end <= self.data.len(), "out-of-bounds physical access");
        start..end
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

fn send_atapi_packet_pio(ide: &mut IdeController, cmd_base: u16, pkt: &[u8; 12], byte_count: u16) {
    // FEATURES = 0 (PIO).
    ide.io_write(cmd_base + 1, 1, 0);
    // Byte count registers.
    ide.io_write(cmd_base + 4, 1, (byte_count & 0xFF) as u32);
    ide.io_write(cmd_base + 5, 1, (byte_count >> 8) as u32);
    // PACKET.
    ide.io_write(cmd_base + 7, 1, 0xA0);
    for i in 0..6 {
        let w = u16::from_le_bytes([pkt[i * 2], pkt[i * 2 + 1]]);
        ide.io_write(cmd_base, 2, w as u32);
    }
}

fn clear_atapi_unit_attention(ide: &mut IdeController, cmd_base: u16) {
    // TEST UNIT READY.
    let tur = [0u8; 12];
    send_atapi_packet_pio(ide, cmd_base, &tur, 0);
    let _ = ide.io_read(cmd_base + 7, 1);

    // REQUEST SENSE (18 bytes).
    let mut req_sense = [0u8; 12];
    req_sense[0] = 0x03;
    req_sense[4] = 18;
    send_atapi_packet_pio(ide, cmd_base, &req_sense, 18);
    for _ in 0..9 {
        let _ = ide.io_read(cmd_base, 2);
    }
}

#[test]
fn ide_atapi_pio_read10_snapshot_roundtrip_mid_data_phase() {
    let mut iso = MemIso::new(2);
    // Fill LBA 1 with a deterministic pattern.
    for i in 0..2048usize {
        iso.data[2048 + i] = (i.wrapping_mul(7) & 0xff) as u8;
    }
    let expected = iso.data[2048..2048 + 2048].to_vec();

    let mut ide = IdeController::new(0xC000);
    ide.attach_secondary_master_atapi(AtapiCdrom::new(Some(Box::new(iso))));

    let sec = emulator::io::storage::ide::SECONDARY_PORTS;
    // Select master.
    ide.io_write(sec.cmd_base + 6, 1, 0xA0);

    clear_atapi_unit_attention(&mut ide, sec.cmd_base);

    // READ(10) for LBA=1, blocks=1.
    let mut pkt = [0u8; 12];
    pkt[0] = 0x28;
    pkt[2..6].copy_from_slice(&1u32.to_be_bytes());
    pkt[7..9].copy_from_slice(&1u16.to_be_bytes());
    send_atapi_packet_pio(&mut ide, sec.cmd_base, &pkt, 2048);

    // Read some data, then snapshot mid-transfer.
    let prefix_words = 128usize;
    let mut out = vec![0u8; 2048];
    for i in 0..prefix_words {
        let w = ide.io_read(sec.cmd_base, 2) as u16;
        out[i * 2..i * 2 + 2].copy_from_slice(&w.to_le_bytes());
    }

    let snap = ide.save_state();

    let mut iso2 = MemIso::new(2);
    for i in 0..2048usize {
        iso2.data[2048 + i] = (i.wrapping_mul(7) & 0xff) as u8;
    }

    let mut restored = IdeController::new(0xC000);
    restored.attach_secondary_master_atapi(AtapiCdrom::new(Some(Box::new(iso2))));
    restored.load_state(&snap).unwrap();

    // Continue reading after restore.
    for i in prefix_words..(2048 / 2) {
        let w = restored.io_read(sec.cmd_base, 2) as u16;
        out[i * 2..i * 2 + 2].copy_from_slice(&w.to_le_bytes());
    }

    assert_eq!(out, expected);
}

#[test]
fn ide_ata_dma_snapshot_roundtrip_preserves_irq_and_status_bits() {
    let mut disk = MemDisk::new(4);
    let expected: Vec<u8> = (0..512u32).map(|v| (v & 0xff) as u8).collect();
    disk.data_mut()[..512].copy_from_slice(&expected);

    let mut ide = IdeController::new(0xC000);
    ide.attach_primary_master_ata(AtaDevice::new(Box::new(disk), "Aero HDD"));

    let mut mem = VecMemory::new(0x20_000);

    // PRD table at 0x1000, one 512-byte segment to 0x2000.
    let prd_addr = 0x1000u64;
    let dma_buf = 0x2000u64;
    mem.write_u32(prd_addr, dma_buf as u32);
    mem.write_u16(prd_addr + 4, 512);
    mem.write_u16(prd_addr + 6, 0x8000);

    let bm_base = ide.bus_master_base();
    ide.io_write(bm_base + 4, 4, prd_addr as u32);

    // Issue READ DMA for LBA 0, 1 sector.
    ide.io_write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    ide.io_write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    ide.io_write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    ide.io_write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    ide.io_write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    ide.io_write(PRIMARY_PORTS.cmd_base + 7, 1, 0xC8);

    // Start bus master (direction=read) and complete DMA.
    ide.io_write(bm_base, 1, 0x09);
    ide.tick(&mut mem);

    // Snapshot when DMA is idle but the interrupt is still pending.
    assert!(ide.primary_irq_pending());
    let snap = ide.save_state();

    // Restore into a fresh controller with the same disk contents.
    let mut disk2 = MemDisk::new(4);
    disk2.data_mut()[..512].copy_from_slice(&expected);

    let mut restored = IdeController::new(0xC000);
    restored.attach_primary_master_ata(AtaDevice::new(Box::new(disk2), "Aero HDD"));
    restored.load_state(&snap).unwrap();

    // Interrupt line + Bus Master status should still reflect completion.
    assert!(restored.primary_irq_pending());
    let bm_status = restored.io_read(bm_base + 2, 1) as u8;
    assert_ne!(bm_status & 0x04, 0, "BMIDE status IRQ bit should be set");
    assert_eq!(bm_status & 0x01, 0, "BMIDE status active bit should be clear");

    // ATA status should report DRDY and not be busy/DRQ.
    let st = restored.io_read(PRIMARY_PORTS.cmd_base + 7, 1) as u8;
    assert_ne!(st & 0x40, 0, "DRDY should be set after DMA completion");
    assert_eq!(st & 0x88, 0, "BSY and DRQ should be clear after DMA completion");

    // Reading STATUS clears the pending IRQ.
    assert!(!restored.primary_irq_pending());
}

#[test]
fn ide_dma_is_gated_on_pci_bus_master_enable() {
    let mut disk = MemDisk::new(4);
    let expected: Vec<u8> = (0..512u32).map(|v| (v & 0xff) as u8).collect();
    disk.data_mut()[..512].copy_from_slice(&expected);

    let mut ide = IdeController::new(0xC000);
    ide.attach_primary_master_ata(AtaDevice::new(Box::new(disk), "Aero HDD"));

    let mut mem = VecMemory::new(0x20_000);

    // PRD table at 0x1000, one 512-byte segment to 0x2000.
    let prd_addr = 0x1000u64;
    let dma_buf = 0x2000u64;
    mem.write_u32(prd_addr, dma_buf as u32);
    mem.write_u16(prd_addr + 4, 512);
    mem.write_u16(prd_addr + 6, 0x8000);

    let bm_base = ide.bus_master_base();
    ide.io_write(bm_base + 4, 4, prd_addr as u32);

    // Disable PCI bus mastering (COMMAND.BME bit 2) while keeping I/O decode enabled.
    let command = ide.pci_config_read(0x04, 2) as u16;
    ide.pci_config_write(0x04, 2, u32::from(command & !(1 << 2)));

    // Issue READ DMA for LBA 0, 1 sector.
    ide.io_write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    ide.io_write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    ide.io_write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    ide.io_write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    ide.io_write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    ide.io_write(PRIMARY_PORTS.cmd_base + 7, 1, 0xC8);

    // Start bus master (direction=read) but DMA must not run until bus mastering is enabled.
    ide.io_write(bm_base, 1, 0x09);
    ide.tick(&mut mem);

    let mut prefix = [0u8; 4];
    mem.read_physical(dma_buf, &mut prefix);
    assert_eq!(prefix, [0; 4], "DMA buffer should remain untouched");
    assert!(
        !ide.primary_irq_pending(),
        "DMA completion interrupt should not be raised while bus mastering is disabled"
    );

    // Enable bus mastering and retry; the pending DMA transfer should complete.
    ide.pci_config_write(0x04, 2, u32::from(command | (1 << 2)));
    ide.tick(&mut mem);

    let mut out = vec![0u8; 512];
    mem.read_physical(dma_buf, &mut out);
    assert_eq!(out, expected);
    assert!(ide.primary_irq_pending());
}
