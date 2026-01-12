use std::cell::RefCell;
use std::io;
use std::rc::Rc;

use aero_devices::pci::profile::IDE_PIIX3;
use aero_devices::pci::{bios_post, PciBarDefinition, PciDevice, PciPlatform, PciResourceAllocator, PciResourceAllocatorConfig};
use aero_devices_storage::ata::AtaDrive;
use aero_devices_storage::atapi::IsoBackend;
use aero_devices_storage::pci_ide::{
    register_piix3_ide_ports, Piix3IdePciDevice, PRIMARY_PORTS, SECONDARY_PORTS,
};
use aero_io_snapshot::io::state::IoSnapshot;
use aero_io_snapshot::io::storage::state::MAX_IDE_DATA_BUFFER_BYTES;
use aero_platform::io::IoPortBus;
use aero_storage::{MemBackend, RawDisk, VirtualDisk, SECTOR_SIZE};
use memory::{Bus, MemoryBus};

fn read_u8(dev: &mut Piix3IdePciDevice, offset: u16) -> u8 {
    dev.config_mut().read(offset, 1) as u8
}

fn read_u16(dev: &mut Piix3IdePciDevice, offset: u16) -> u16 {
    dev.config_mut().read(offset, 2) as u16
}

fn read_u32(dev: &mut Piix3IdePciDevice, offset: u16) -> u32 {
    dev.config_mut().read(offset, 4)
}

#[test]
fn pci_bar_probing_and_programming_matches_piix3_profile() {
    let mut dev = Piix3IdePciDevice::new();

    assert_eq!(read_u16(&mut dev, 0x00), IDE_PIIX3.vendor_id);
    assert_eq!(read_u16(&mut dev, 0x02), IDE_PIIX3.device_id);
    assert_eq!(read_u8(&mut dev, 0x08), IDE_PIIX3.revision_id);
    assert_eq!(read_u8(&mut dev, 0x09), IDE_PIIX3.class.prog_if);
    assert_eq!(read_u8(&mut dev, 0x0a), IDE_PIIX3.class.sub_class);
    assert_eq!(read_u8(&mut dev, 0x0b), IDE_PIIX3.class.base_class);

    assert_eq!(dev.config().bar_definition(0), Some(PciBarDefinition::Io { size: 8 }));
    assert_eq!(dev.config().bar_definition(1), Some(PciBarDefinition::Io { size: 4 }));
    assert_eq!(dev.config().bar_definition(2), Some(PciBarDefinition::Io { size: 8 }));
    assert_eq!(dev.config().bar_definition(3), Some(PciBarDefinition::Io { size: 4 }));
    assert_eq!(dev.config().bar_definition(4), Some(PciBarDefinition::Io { size: 16 }));

    // BAR0 (8-byte I/O).
    dev.config_mut().write(0x10, 4, 0xffff_ffff);
    assert_eq!(read_u32(&mut dev, 0x10), 0xffff_fff9);
    dev.config_mut().write(0x10, 4, 0x0000_1f03);
    assert_eq!(read_u32(&mut dev, 0x10), 0x0000_1f01);

    // BAR1 (4-byte I/O).
    dev.config_mut().write(0x14, 4, 0xffff_ffff);
    assert_eq!(read_u32(&mut dev, 0x14), 0xffff_fffd);
    dev.config_mut().write(0x14, 4, 0x0000_3f07);
    assert_eq!(read_u32(&mut dev, 0x14), 0x0000_3f05);

    // BAR2 (8-byte I/O).
    dev.config_mut().write(0x18, 4, 0xffff_ffff);
    assert_eq!(read_u32(&mut dev, 0x18), 0xffff_fff9);
    dev.config_mut().write(0x18, 4, 0x0000_1703);
    assert_eq!(read_u32(&mut dev, 0x18), 0x0000_1701);

    // BAR3 (4-byte I/O).
    dev.config_mut().write(0x1c, 4, 0xffff_ffff);
    assert_eq!(read_u32(&mut dev, 0x1c), 0xffff_fffd);
    dev.config_mut().write(0x1c, 4, 0x0000_3707);
    assert_eq!(read_u32(&mut dev, 0x1c), 0x0000_3705);

    // BAR4 (16-byte I/O).
    dev.config_mut().write(0x20, 4, 0xffff_ffff);
    assert_eq!(read_u32(&mut dev, 0x20), 0xffff_fff1);
    dev.config_mut().write(0x20, 4, 0x0000_c123);
    assert_eq!(read_u32(&mut dev, 0x20), 0x0000_c121);
}

#[test]
fn ata_boot_sector_read_via_legacy_pio_ports() {
    let capacity = 8 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut sector0 = vec![0u8; SECTOR_SIZE];
    sector0[0..4].copy_from_slice(b"BOOT");
    sector0[510] = 0x55;
    sector0[511] = 0xAA;
    disk.write_sectors(0, &sector0).unwrap();

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut()
        .controller
        .attach_primary_master_ata(AtaDrive::new(Box::new(disk)).unwrap());
    ide.borrow_mut().config_mut().set_command(0x0001); // IO decode

    let mut io = IoPortBus::new();
    register_piix3_ide_ports(&mut io, ide.clone());

    // Issue READ SECTORS for LBA 0, 1 sector.
    io.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0); // master + LBA
    io.write(PRIMARY_PORTS.cmd_base + 2, 1, 1); // count
    io.write(PRIMARY_PORTS.cmd_base + 3, 1, 0); // lba0
    io.write(PRIMARY_PORTS.cmd_base + 4, 1, 0); // lba1
    io.write(PRIMARY_PORTS.cmd_base + 5, 1, 0); // lba2
    io.write(PRIMARY_PORTS.cmd_base + 7, 1, 0x20); // READ SECTORS

    let mut buf = [0u8; SECTOR_SIZE];
    for i in 0..(SECTOR_SIZE / 2) {
        let w = io.read(PRIMARY_PORTS.cmd_base, 2) as u16;
        buf[i * 2..i * 2 + 2].copy_from_slice(&w.to_le_bytes());
    }

    assert_eq!(&buf[0..4], b"BOOT");
    assert_eq!(&buf[510..512], &[0x55, 0xAA]);
}

#[test]
fn ata_lba48_oversized_pio_read_is_rejected_without_entering_data_phase() {
    let sectors = (MAX_IDE_DATA_BUFFER_BYTES / SECTOR_SIZE) as u32 + 1;
    // The largest possible LBA48 transfer is 65536 sectors (sector_count=0). If the cap ever grows
    // beyond that, there's nothing for the IDE layer to reject here.
    if sectors > 65536 {
        return;
    }

    // Allocate a disk large enough that the transfer would otherwise succeed (to ensure we're
    // testing the MAX_IDE_DATA_BUFFER_BYTES cap rather than an out-of-bounds read).
    let capacity = u64::from(sectors) * SECTOR_SIZE as u64;
    let disk = RawDisk::create(MemBackend::new(), capacity).unwrap();

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut()
        .controller
        .attach_primary_master_ata(AtaDrive::new(Box::new(disk)).unwrap());
    ide.borrow_mut().config_mut().set_command(0x0001); // IO decode

    let mut io = IoPortBus::new();
    register_piix3_ide_ports(&mut io, ide.clone());

    // Select master, LBA mode.
    io.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);

    // Sector count (48-bit): high byte then low byte.
    io.write(PRIMARY_PORTS.cmd_base + 2, 1, (sectors >> 8) as u32);
    io.write(PRIMARY_PORTS.cmd_base + 2, 1, (sectors & 0xFF) as u32);

    // LBA = 0 (48-bit writes: high then low per register).
    for reg in 3..=5 {
        io.write(PRIMARY_PORTS.cmd_base + reg, 1, 0);
        io.write(PRIMARY_PORTS.cmd_base + reg, 1, 0);
    }

    // READ SECTORS EXT.
    io.write(PRIMARY_PORTS.cmd_base + 7, 1, 0x24);

    let status = io.read(PRIMARY_PORTS.cmd_base + 7, 1) as u8;
    assert_eq!(status & 0x80, 0, "BSY should be clear");
    assert_eq!(status & 0x08, 0, "DRQ should be clear (no data phase)");
    assert_ne!(status & 0x01, 0, "ERR should be set");
    assert_eq!(io.read(PRIMARY_PORTS.cmd_base + 1, 1) as u8, 0x04);
}

#[test]
fn ata_lba48_oversized_pio_write_is_rejected_without_allocating_buffer() {
    let sectors = (MAX_IDE_DATA_BUFFER_BYTES / SECTOR_SIZE) as u32 + 1;
    if sectors > 65536 {
        return;
    }

    let capacity = SECTOR_SIZE as u64;
    let disk = RawDisk::create(MemBackend::new(), capacity).unwrap();

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut()
        .controller
        .attach_primary_master_ata(AtaDrive::new(Box::new(disk)).unwrap());
    ide.borrow_mut().config_mut().set_command(0x0001); // IO decode

    let mut io = IoPortBus::new();
    register_piix3_ide_ports(&mut io, ide.clone());

    io.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    io.write(PRIMARY_PORTS.cmd_base + 2, 1, (sectors >> 8) as u32);
    io.write(PRIMARY_PORTS.cmd_base + 2, 1, (sectors & 0xFF) as u32);

    for reg in 3..=5 {
        io.write(PRIMARY_PORTS.cmd_base + reg, 1, 0);
        io.write(PRIMARY_PORTS.cmd_base + reg, 1, 0);
    }

    // WRITE SECTORS EXT.
    io.write(PRIMARY_PORTS.cmd_base + 7, 1, 0x34);

    let status = io.read(PRIMARY_PORTS.cmd_base + 7, 1) as u8;
    assert_eq!(status & 0x80, 0, "BSY should be clear");
    assert_eq!(status & 0x08, 0, "DRQ should be clear (no data phase)");
    assert_ne!(status & 0x01, 0, "ERR should be set");
    assert_eq!(io.read(PRIMARY_PORTS.cmd_base + 1, 1) as u8, 0x04);
}

#[test]
fn ata_bus_master_dma_read_write_roundtrip() {
    let capacity = 16 * SECTOR_SIZE as u64;
    let disk = RawDisk::create(MemBackend::new(), capacity).unwrap();

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut()
        .controller
        .attach_primary_master_ata(AtaDrive::new(Box::new(disk)).unwrap());
    ide.borrow_mut().config_mut().set_command(0x0005); // IO decode + Bus Master

    let mut ioports = IoPortBus::new();
    register_piix3_ide_ports(&mut ioports, ide.clone());

    let mut mem = Bus::new(0x20_000);

    let prd_addr = 0x1000u64;
    let write_buf = 0x3000u64;
    let read_buf = 0x4000u64;

    // Fill a sector worth of data in guest memory.
    let mut pattern = vec![0u8; SECTOR_SIZE];
    for (i, b) in pattern.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(7).wrapping_add(3);
    }
    mem.write_physical(write_buf, &pattern);

    // PRD table: one entry, end-of-table, 512 bytes.
    mem.write_u32(prd_addr, write_buf as u32);
    mem.write_u16(prd_addr + 4, SECTOR_SIZE as u16);
    mem.write_u16(prd_addr + 6, 0x8000);

    // Program PRD address for primary channel.
    let bm_base = ide.borrow().bus_master_base();
    ioports.write(bm_base + 4, 4, prd_addr as u32);

    // Issue WRITE DMA (LBA 2, 1 sector).
    ioports.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    ioports.write(PRIMARY_PORTS.cmd_base + 3, 1, 2);
    ioports.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xCA); // WRITE DMA

    // Start bus master (direction = from memory).
    ioports.write(bm_base, 1, 0x01);
    ide.borrow_mut().tick(&mut mem);

    // Prepare PRD for the read-back buffer.
    mem.write_u32(prd_addr, read_buf as u32);
    mem.write_u16(prd_addr + 4, SECTOR_SIZE as u16);
    mem.write_u16(prd_addr + 6, 0x8000);
    ioports.write(bm_base + 4, 4, prd_addr as u32);

    // Issue READ DMA (LBA 2, 1 sector).
    ioports.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    ioports.write(PRIMARY_PORTS.cmd_base + 3, 1, 2);
    ioports.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xC8); // READ DMA

    // Start bus master (direction = to memory).
    ioports.write(bm_base, 1, 0x09);
    ide.borrow_mut().tick(&mut mem);

    let mut out = vec![0u8; SECTOR_SIZE];
    mem.read_physical(read_buf, &mut out);
    assert_eq!(out, pattern);
}

#[test]
fn bus_master_registers_mask_command_bits_and_require_dword_prd_writes() {
    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut().config_mut().set_command(0x0001); // IO decode

    let mut ioports = IoPortBus::new();
    register_piix3_ide_ports(&mut ioports, ide.clone());

    let bm_base = ide.borrow().bus_master_base();

    // Command register only exposes bits 0 (start) and 3 (direction).
    ioports.write(bm_base, 1, 0xFF);
    assert_eq!(ioports.read(bm_base, 1) as u8, 0x09);

    // Clear start while keeping direction.
    ioports.write(bm_base, 1, 0x08);
    assert_eq!(ioports.read(bm_base, 1) as u8, 0x08);

    // PRD address register only updates on 32-bit writes and is 4-byte aligned.
    ioports.write(bm_base + 4, 4, 0x1234_5679);
    assert_eq!(ioports.read(bm_base + 4, 4), 0x1234_5678);

    // Partial write must be ignored.
    ioports.write(bm_base + 4, 2, 0xABCD);
    assert_eq!(ioports.read(bm_base + 4, 4), 0x1234_5678);
}

#[test]
fn bus_master_status_register_is_rw1c_for_irq_and_error_bits() {
    let capacity = 4 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut sector0 = vec![0u8; SECTOR_SIZE];
    sector0[..4].copy_from_slice(b"OKAY");
    disk.write_sectors(0, &sector0).unwrap();

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut()
        .controller
        .attach_primary_master_ata(AtaDrive::new(Box::new(disk)).unwrap());
    ide.borrow_mut().config_mut().set_command(0x0005); // IO decode + Bus Master

    let mut ioports = IoPortBus::new();
    register_piix3_ide_ports(&mut ioports, ide.clone());

    let mut mem = Bus::new(0x20_000);
    let prd_addr = 0x1000u64;
    let read_buf = 0x2000u64;

    // PRD table: one entry, end-of-table, 512 bytes.
    mem.write_u32(prd_addr, read_buf as u32);
    mem.write_u16(prd_addr + 4, SECTOR_SIZE as u16);
    mem.write_u16(prd_addr + 6, 0x8000);

    let bm_base = ide.borrow().bus_master_base();
    ioports.write(bm_base + 4, 4, prd_addr as u32);

    // Successful READ DMA (LBA 0, 1 sector).
    ioports.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    ioports.write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xC8); // READ DMA
    ioports.write(bm_base, 1, 0x09); // start + read (device -> memory)
    ide.borrow_mut().tick(&mut mem);

    let st = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(st & 0x07, 0x04, "interrupt should be set, active/error clear");

    // Clear interrupt (RW1C).
    ioports.write(bm_base + 2, 1, 0x04);
    let st = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(st & 0x07, 0x00, "interrupt bit should clear via RW1C");

    // Now trigger an error via missing EOT PRD: 512 bytes but no end-of-table bit.
    mem.write_u32(prd_addr, read_buf as u32);
    mem.write_u16(prd_addr + 4, SECTOR_SIZE as u16);
    mem.write_u16(prd_addr + 6, 0x0000); // no EOT
    ioports.write(bm_base + 4, 4, prd_addr as u32);

    ioports.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    ioports.write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xC8);
    ioports.write(bm_base, 1, 0x09);
    ide.borrow_mut().tick(&mut mem);

    let st = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(
        st & 0x07,
        0x06,
        "error + interrupt should be set on DMA failure"
    );

    // Clear error (RW1C) should not clear interrupt.
    ioports.write(bm_base + 2, 1, 0x02);
    let st = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(st & 0x07, 0x04, "clearing error should preserve interrupt");

    // Clear interrupt as well.
    ioports.write(bm_base + 2, 1, 0x04);
    let st = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(st & 0x07, 0x00);
}

#[test]
fn prd_byte_count_zero_encodes_64kib_transfer() {
    // 128 sectors * 512 bytes = 65536 bytes.
    let sectors: u64 = 128;
    let capacity = sectors * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();

    let mut pattern = vec![0u8; capacity as usize];
    for (i, b) in pattern.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(3).wrapping_add(1);
    }
    disk.write_sectors(0, &pattern).unwrap();

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut()
        .controller
        .attach_primary_master_ata(AtaDrive::new(Box::new(disk)).unwrap());
    ide.borrow_mut().config_mut().set_command(0x0005); // IO decode + Bus Master

    let mut ioports = IoPortBus::new();
    register_piix3_ide_ports(&mut ioports, ide.clone());

    let mut mem = Bus::new(0x20_000);
    let prd_addr = 0x1000u64;
    let read_buf = 0x2000u64;

    // One PRD entry: byte_count = 0 => 64KiB, end-of-table.
    mem.write_u32(prd_addr, read_buf as u32);
    mem.write_u16(prd_addr + 4, 0);
    mem.write_u16(prd_addr + 6, 0x8000);

    let bm_base = ide.borrow().bus_master_base();
    ioports.write(bm_base + 4, 4, prd_addr as u32);

    // READ DMA for LBA 0, 128 sectors.
    ioports.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, sectors as u32);
    ioports.write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xC8);

    ioports.write(bm_base, 1, 0x09);
    ide.borrow_mut().tick(&mut mem);

    let mut out = vec![0u8; capacity as usize];
    mem.read_physical(read_buf, &mut out);
    assert_eq!(out, pattern);
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

    fn read_sectors(&mut self, lba: u32, buf: &mut [u8]) -> io::Result<()> {
        if !buf.len().is_multiple_of(2048) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unaligned buffer length",
            ));
        }
        let start = lba as usize * 2048;
        let end = start
            .checked_add(buf.len())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "overflow"))?;
        if end > self.data.len() {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "OOB"));
        }
        buf.copy_from_slice(&self.data[start..end]);
        Ok(())
    }
}

fn send_atapi_packet(io: &mut IoPortBus, base: u16, features: u8, pkt: &[u8; 12], byte_count: u16) {
    io.write(base + 1, 1, features as u32);
    io.write(base + 4, 1, (byte_count & 0xFF) as u32);
    io.write(base + 5, 1, (byte_count >> 8) as u32);
    io.write(base + 7, 1, 0xA0); // PACKET
    for i in 0..6 {
        let w = u16::from_le_bytes([pkt[i * 2], pkt[i * 2 + 1]]);
        io.write(base, 2, w as u32);
    }
}

#[test]
fn atapi_inquiry_and_read_10_pio() {
    let mut iso = MemIso::new(2);
    iso.data[2048..2053].copy_from_slice(b"WORLD");

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut()
        .controller
        .attach_secondary_master_atapi(aero_devices_storage::atapi::AtapiCdrom::new(Some(
            Box::new(iso),
        )));
    ide.borrow_mut().config_mut().set_command(0x0001); // IO decode

    let mut ioports = IoPortBus::new();
    register_piix3_ide_ports(&mut ioports, ide.clone());

    // Select master on secondary channel.
    ioports.write(SECONDARY_PORTS.cmd_base + 6, 1, 0xA0);

    // INQUIRY (alloc 36).
    let mut inquiry = [0u8; 12];
    inquiry[0] = 0x12;
    inquiry[4] = 36;
    send_atapi_packet(&mut ioports, SECONDARY_PORTS.cmd_base, 0, &inquiry, 36);

    let mut inq_buf = [0u8; 36];
    for i in 0..(36 / 2) {
        let w = ioports.read(SECONDARY_PORTS.cmd_base, 2) as u16;
        inq_buf[i * 2..i * 2 + 2].copy_from_slice(&w.to_le_bytes());
    }
    assert_eq!(&inq_buf[8..12], b"AERO");

    // Clear initial UNIT ATTENTION: TEST UNIT READY then REQUEST SENSE.
    let tur = [0u8; 12];
    send_atapi_packet(&mut ioports, SECONDARY_PORTS.cmd_base, 0, &tur, 0);
    let _ = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1);

    let mut req_sense = [0u8; 12];
    req_sense[0] = 0x03;
    req_sense[4] = 18;
    send_atapi_packet(&mut ioports, SECONDARY_PORTS.cmd_base, 0, &req_sense, 18);
    for _ in 0..(18 / 2) {
        let _ = ioports.read(SECONDARY_PORTS.cmd_base, 2);
    }

    // READ(10) for LBA=1, blocks=1 (should start with "WORLD").
    let mut read10 = [0u8; 12];
    read10[0] = 0x28;
    read10[2..6].copy_from_slice(&1u32.to_be_bytes());
    read10[7..9].copy_from_slice(&1u16.to_be_bytes());
    send_atapi_packet(&mut ioports, SECONDARY_PORTS.cmd_base, 0, &read10, 2048);

    let mut out = vec![0u8; 2048];
    for i in 0..(2048 / 2) {
        let w = ioports.read(SECONDARY_PORTS.cmd_base, 2) as u16;
        out[i * 2..i * 2 + 2].copy_from_slice(&w.to_le_bytes());
    }
    assert_eq!(&out[..5], b"WORLD");
}

#[test]
fn atapi_read_12_rejects_oversized_transfer_without_allocating_buffer() {
    let blocks = (MAX_IDE_DATA_BUFFER_BYTES / 2048) as u32 + 1;
    // Allocate an ISO large enough that the transfer would otherwise succeed, so we specifically
    // exercise the MAX_IDE_DATA_BUFFER_BYTES cap.
    let iso = MemIso::new(blocks);

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut()
        .controller
        .attach_secondary_master_atapi(aero_devices_storage::atapi::AtapiCdrom::new(Some(
            Box::new(iso),
        )));
    ide.borrow_mut().config_mut().set_command(0x0001); // IO decode

    let mut ioports = IoPortBus::new();
    register_piix3_ide_ports(&mut ioports, ide.clone());

    // Select master on secondary channel.
    ioports.write(SECONDARY_PORTS.cmd_base + 6, 1, 0xA0);

    // Clear initial UNIT ATTENTION: TEST UNIT READY then REQUEST SENSE.
    let tur = [0u8; 12];
    send_atapi_packet(&mut ioports, SECONDARY_PORTS.cmd_base, 0, &tur, 0);
    let _ = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1);

    let mut req_sense = [0u8; 12];
    req_sense[0] = 0x03;
    req_sense[4] = 18;
    send_atapi_packet(&mut ioports, SECONDARY_PORTS.cmd_base, 0, &req_sense, 18);
    for _ in 0..(18 / 2) {
        let _ = ioports.read(SECONDARY_PORTS.cmd_base, 2);
    }

    let mut read12 = [0u8; 12];
    read12[0] = 0xA8; // READ(12)
    read12[6..10].copy_from_slice(&blocks.to_be_bytes());
    send_atapi_packet(
        &mut ioports,
        SECONDARY_PORTS.cmd_base,
        0,
        &read12,
        2048,
    );

    // Error completions should still raise an interrupt.
    assert!(ide.borrow().controller.secondary_irq_pending());
    // Interrupt reason: status phase.
    assert_eq!(
        ioports.read(SECONDARY_PORTS.cmd_base + 2, 1) as u8,
        0x03
    );

    let status = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1) as u8;
    assert_eq!(status & 0x80, 0, "BSY should be clear");
    assert_eq!(status & 0x08, 0, "DRQ should be clear (no data phase)");
    assert_ne!(status & 0x01, 0, "ERR should be set");
    assert_eq!(ioports.read(SECONDARY_PORTS.cmd_base + 1, 1) as u8, 0x04);
}

#[test]
fn bus_master_bar4_relocation_affects_registered_ports() {
    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));

    // Reprogram BAR4 before wiring the device onto the IO bus.
    ide.borrow_mut().config_mut().write(0x20, 4, 0x0000_d000);
    ide.borrow_mut().config_mut().set_command(0x0001); // IO decode

    let mut ioports = IoPortBus::new();
    register_piix3_ide_ports(&mut ioports, ide.clone());

    // Old base should be unmapped.
    assert_eq!(
        ioports.read(Piix3IdePciDevice::DEFAULT_BUS_MASTER_BASE, 1),
        0xFF
    );

    // New base should decode bus master command register.
    assert_eq!(ioports.read(0xD000, 1) as u8, 0);
}

#[test]
fn atapi_read_10_dma_via_bus_master() {
    let mut iso = MemIso::new(1);
    iso.data[0..8].copy_from_slice(b"DMATEST!");

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut()
        .controller
        .attach_secondary_master_atapi(aero_devices_storage::atapi::AtapiCdrom::new(Some(
            Box::new(iso),
        )));
    ide.borrow_mut().config_mut().set_command(0x0005); // IO decode + Bus Master

    let mut ioports = IoPortBus::new();
    register_piix3_ide_ports(&mut ioports, ide.clone());

    // Select master on secondary channel.
    ioports.write(SECONDARY_PORTS.cmd_base + 6, 1, 0xA0);

    // Clear initial UNIT ATTENTION: TEST UNIT READY then REQUEST SENSE.
    let tur = [0u8; 12];
    send_atapi_packet(&mut ioports, SECONDARY_PORTS.cmd_base, 0, &tur, 0);
    let _ = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1);

    let mut req_sense = [0u8; 12];
    req_sense[0] = 0x03;
    req_sense[4] = 18;
    send_atapi_packet(&mut ioports, SECONDARY_PORTS.cmd_base, 0, &req_sense, 18);
    for _ in 0..(18 / 2) {
        let _ = ioports.read(SECONDARY_PORTS.cmd_base, 2);
    }

    let mut mem = Bus::new(0x20_000);
    let bm_base = ide.borrow().bus_master_base();

    let prd_addr = 0x1000u64;
    let dma_buf = 0x3000u64;

    // PRD entry: one 2048-byte segment, end-of-table.
    mem.write_u32(prd_addr, dma_buf as u32);
    mem.write_u16(prd_addr + 4, 2048);
    mem.write_u16(prd_addr + 6, 0x8000);

    // Program secondary PRD pointer.
    ioports.write(bm_base + 8 + 4, 4, prd_addr as u32);

    // READ(10) for LBA=0, blocks=1 with DMA enabled (FEATURES bit0).
    let mut read10 = [0u8; 12];
    read10[0] = 0x28;
    read10[2..6].copy_from_slice(&0u32.to_be_bytes());
    read10[7..9].copy_from_slice(&1u16.to_be_bytes());
    send_atapi_packet(&mut ioports, SECONDARY_PORTS.cmd_base, 0x01, &read10, 2048);

    // Start the secondary bus master engine, direction=read (device -> memory).
    ioports.write(bm_base + 8, 1, 0x09);
    ide.borrow_mut().tick(&mut mem);

    let mut out = [0u8; 8];
    mem.read_physical(dma_buf, &mut out);
    assert_eq!(&out, b"DMATEST!");

    // Bus master status should indicate interrupt and no error.
    let st = ioports.read(bm_base + 8 + 2, 1) as u8;
    assert_ne!(st & 0x04, 0);
    assert_eq!(st & 0x02, 0);

    assert!(ide.borrow().controller.secondary_irq_pending());
}

#[test]
fn ata_dma_missing_prd_eot_sets_error_status() {
    // Disk with recognizable first sector.
    let capacity = 4 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut sector0 = vec![0u8; SECTOR_SIZE];
    sector0[0..4].copy_from_slice(b"TEST");
    disk.write_sectors(0, &sector0).unwrap();

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut()
        .controller
        .attach_primary_master_ata(AtaDrive::new(Box::new(disk)).unwrap());
    ide.borrow_mut().config_mut().set_command(0x0005); // IO decode + Bus Master

    let mut ioports = IoPortBus::new();
    register_piix3_ide_ports(&mut ioports, ide.clone());

    let mut mem = Bus::new(0x20_000);
    let bm_base = ide.borrow().bus_master_base();

    let prd_addr = 0x1000u64;
    let dma_buf = 0x3000u64;

    // PRD entry without EOT flag (malformed): 512 bytes.
    mem.write_u32(prd_addr, dma_buf as u32);
    mem.write_u16(prd_addr + 4, SECTOR_SIZE as u16);
    mem.write_u16(prd_addr + 6, 0x0000);
    ioports.write(bm_base + 4, 4, prd_addr as u32);

    // READ DMA for LBA 0, 1 sector.
    ioports.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    ioports.write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xC8);

    // Start bus master (direction = to memory).
    ioports.write(bm_base, 1, 0x09);
    ide.borrow_mut().tick(&mut mem);

    let st = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(st & 0x06, 0x06, "BMIDE status should have IRQ+ERR set");
}

#[test]
fn ata_dma_prd_too_short_sets_error_status() {
    let capacity = 4 * SECTOR_SIZE as u64;
    let disk = RawDisk::create(MemBackend::new(), capacity).unwrap();

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut()
        .controller
        .attach_primary_master_ata(AtaDrive::new(Box::new(disk)).unwrap());
    ide.borrow_mut().config_mut().set_command(0x0005); // IO decode + Bus Master

    let mut ioports = IoPortBus::new();
    register_piix3_ide_ports(&mut ioports, ide.clone());

    let mut mem = Bus::new(0x20_000);
    let bm_base = ide.borrow().bus_master_base();

    let prd_addr = 0x1000u64;
    let dma_buf = 0x3000u64;

    // PRD entry that is too short for a 512-byte request (256 bytes, EOT).
    mem.write_u32(prd_addr, dma_buf as u32);
    mem.write_u16(prd_addr + 4, 256);
    mem.write_u16(prd_addr + 6, 0x8000);
    ioports.write(bm_base + 4, 4, prd_addr as u32);

    // READ DMA for LBA 0, 1 sector.
    ioports.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    ioports.write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xC8);

    // Start bus master (direction = to memory).
    ioports.write(bm_base, 1, 0x09);
    ide.borrow_mut().tick(&mut mem);

    let st = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(st & 0x06, 0x06, "BMIDE status should have IRQ+ERR set");
    assert!(ide.borrow().controller.primary_irq_pending());
}

#[test]
fn ata_dma_direction_mismatch_sets_error_status() {
    // Disk with recognizable first sector.
    let capacity = 4 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut sector0 = vec![0u8; SECTOR_SIZE];
    sector0[..4].copy_from_slice(b"TEST");
    disk.write_sectors(0, &sector0).unwrap();

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut()
        .controller
        .attach_primary_master_ata(AtaDrive::new(Box::new(disk)).unwrap());
    ide.borrow_mut().config_mut().set_command(0x0005); // IO decode + Bus Master

    let mut ioports = IoPortBus::new();
    register_piix3_ide_ports(&mut ioports, ide.clone());

    let mut mem = Bus::new(0x20_000);
    let bm_base = ide.borrow().bus_master_base();

    let prd_addr = 0x1000u64;
    let dma_buf = 0x3000u64;

    // Valid PRD entry (512 bytes, EOT).
    mem.write_u32(prd_addr, dma_buf as u32);
    mem.write_u16(prd_addr + 4, SECTOR_SIZE as u16);
    mem.write_u16(prd_addr + 6, 0x8000);
    ioports.write(bm_base + 4, 4, prd_addr as u32);

    // READ DMA for LBA 0, 1 sector (device -> memory request).
    ioports.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    ioports.write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xC8);

    // Start bus master with direction bit cleared (from memory), which mismatches the request.
    ioports.write(bm_base, 1, 0x01);
    ide.borrow_mut().tick(&mut mem);

    let st = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(st & 0x06, 0x06, "BMIDE status should have IRQ+ERR set");
    assert!(ide.borrow().controller.primary_irq_pending());
}

#[test]
fn bios_post_preserves_piix3_legacy_bar_bases() {
    let mut bus = PciPlatform::build_bus();
    let bdf = IDE_PIIX3.bdf;

    // The device initializes its BARs to legacy port addresses; BIOS POST should preserve those
    // fixed assignments rather than allocating new ones.
    bus.add_device(bdf, Box::new(Piix3IdePciDevice::new()));

    let mut alloc = PciResourceAllocator::new(PciResourceAllocatorConfig::default());
    bios_post(&mut bus, &mut alloc).unwrap();

    let cfg = bus.device_config(bdf).unwrap();

    assert_eq!(cfg.bar_range(0).unwrap().base, 0x1F0);
    assert_eq!(cfg.bar_range(1).unwrap().base, 0x3F4);
    assert_eq!(cfg.bar_range(2).unwrap().base, 0x170);
    assert_eq!(cfg.bar_range(3).unwrap().base, 0x374);
    assert_eq!(
        cfg.bar_range(4).unwrap().base,
        u64::from(Piix3IdePciDevice::DEFAULT_BUS_MASTER_BASE)
    );

    assert_eq!(cfg.command() & 0x1, 0x1, "bios_post should enable IO decoding");
}

#[test]
fn piix3_ide_atapi_pio_read10_snapshot_roundtrip_mid_data_phase() {
    let mut iso = MemIso::new(2);
    // Fill LBA 1 with a deterministic pattern.
    for i in 0..2048usize {
        iso.data[2048 + i] = (i.wrapping_mul(7) & 0xff) as u8;
    }
    let expected = iso.data[2048..2048 + 2048].to_vec();

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut()
        .controller
        .attach_secondary_master_atapi(aero_devices_storage::atapi::AtapiCdrom::new(Some(
            Box::new(iso),
        )));
    ide.borrow_mut().config_mut().set_command(0x0001); // IO decode

    let mut ioports = IoPortBus::new();
    register_piix3_ide_ports(&mut ioports, ide.clone());

    // Select master on secondary channel.
    ioports.write(SECONDARY_PORTS.cmd_base + 6, 1, 0xA0);

    // Clear initial UNIT ATTENTION: TEST UNIT READY then REQUEST SENSE.
    let tur = [0u8; 12];
    send_atapi_packet(&mut ioports, SECONDARY_PORTS.cmd_base, 0, &tur, 0);
    let _ = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1);

    let mut req_sense = [0u8; 12];
    req_sense[0] = 0x03;
    req_sense[4] = 18;
    send_atapi_packet(&mut ioports, SECONDARY_PORTS.cmd_base, 0, &req_sense, 18);
    for _ in 0..(18 / 2) {
        let _ = ioports.read(SECONDARY_PORTS.cmd_base, 2);
    }

    // READ(10) for LBA=1, blocks=1.
    let mut read10 = [0u8; 12];
    read10[0] = 0x28;
    read10[2..6].copy_from_slice(&1u32.to_be_bytes());
    read10[7..9].copy_from_slice(&1u16.to_be_bytes());
    send_atapi_packet(&mut ioports, SECONDARY_PORTS.cmd_base, 0, &read10, 2048);

    // Read some data, then snapshot mid-transfer.
    let prefix_words = 128usize;
    let mut out = vec![0u8; 2048];
    for i in 0..prefix_words {
        let w = ioports.read(SECONDARY_PORTS.cmd_base, 2) as u16;
        out[i * 2..i * 2 + 2].copy_from_slice(&w.to_le_bytes());
    }

    let snap = ide.borrow().save_state();

    // Restore into a fresh controller with identical media.
    let mut iso2 = MemIso::new(2);
    for i in 0..2048usize {
        iso2.data[2048 + i] = (i.wrapping_mul(7) & 0xff) as u8;
    }

    let restored = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    restored
        .borrow_mut()
        .controller
        .attach_secondary_master_atapi(aero_devices_storage::atapi::AtapiCdrom::new(Some(
            Box::new(iso2),
        )));
    restored.borrow_mut().load_state(&snap).unwrap();

    let mut ioports2 = IoPortBus::new();
    register_piix3_ide_ports(&mut ioports2, restored.clone());

    // Continue reading after restore.
    for i in prefix_words..(2048 / 2) {
        let w = ioports2.read(SECONDARY_PORTS.cmd_base, 2) as u16;
        out[i * 2..i * 2 + 2].copy_from_slice(&w.to_le_bytes());
    }

    assert_eq!(out, expected);
}

#[test]
fn piix3_ide_ata_dma_snapshot_roundtrip_preserves_irq_and_status_bits() {
    let capacity = 4 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let expected: Vec<u8> = (0..SECTOR_SIZE as u32).map(|v| (v & 0xff) as u8).collect();
    disk.write_sectors(0, &expected).unwrap();

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut()
        .controller
        .attach_primary_master_ata(AtaDrive::new(Box::new(disk)).unwrap());
    ide.borrow_mut().config_mut().set_command(0x0005); // IO decode + Bus Master

    let mut ioports = IoPortBus::new();
    register_piix3_ide_ports(&mut ioports, ide.clone());

    let mut mem = Bus::new(0x20_000);

    // PRD table at 0x1000, one 512-byte segment to 0x2000.
    let prd_addr = 0x1000u64;
    let dma_buf = 0x2000u64;
    mem.write_u32(prd_addr, dma_buf as u32);
    mem.write_u16(prd_addr + 4, SECTOR_SIZE as u16);
    mem.write_u16(prd_addr + 6, 0x8000);

    let bm_base = ide.borrow().bus_master_base();
    ioports.write(bm_base + 4, 4, prd_addr as u32);

    // Issue READ DMA for LBA 0, 1 sector.
    ioports.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    ioports.write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xC8);

    // Start bus master (direction = to memory).
    ioports.write(bm_base, 1, 0x09);
    ide.borrow_mut().tick(&mut mem);

    // Snapshot when DMA is idle but the interrupt is still pending.
    assert!(ide.borrow().controller.primary_irq_pending());
    let snap = ide.borrow().save_state();

    // Restore into a fresh controller with the same disk contents.
    let mut disk2 = RawDisk::create(MemBackend::new(), capacity).unwrap();
    disk2.write_sectors(0, &expected).unwrap();

    let restored = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    restored
        .borrow_mut()
        .controller
        .attach_primary_master_ata(AtaDrive::new(Box::new(disk2)).unwrap());
    restored.borrow_mut().load_state(&snap).unwrap();

    let mut ioports2 = IoPortBus::new();
    register_piix3_ide_ports(&mut ioports2, restored.clone());
    let bm_base2 = restored.borrow().bus_master_base();

    // Interrupt line + Bus Master status should still reflect completion.
    assert!(restored.borrow().controller.primary_irq_pending());
    let bm_status = ioports2.read(bm_base2 + 2, 1) as u8;
    assert_ne!(bm_status & 0x04, 0, "BMIDE status IRQ bit should be set");
    assert_eq!(bm_status & 0x01, 0, "BMIDE status active bit should be clear");

    // ATA status should report DRDY and not be busy/DRQ.
    let st = ioports2.read(PRIMARY_PORTS.cmd_base + 7, 1) as u8;
    assert_ne!(st & 0x40, 0, "DRDY should be set after DMA completion");
    assert_eq!(st & 0x88, 0, "BSY and DRQ should be clear after DMA completion");

    // Reading STATUS clears the pending IRQ.
    assert!(!restored.borrow().controller.primary_irq_pending());
}
