use std::cell::RefCell;
use std::io;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use aero_devices::pci::PciDevice as _;
use aero_devices_storage::ata::{AtaDrive, ATA_CMD_READ_DMA_EXT, ATA_CMD_WRITE_DMA_EXT};
use aero_devices_storage::atapi::{AtapiCdrom, IsoBackend};
use aero_devices_storage::pci_ide::{
    register_piix3_ide_ports, Piix3IdePciDevice, PRIMARY_PORTS, SECONDARY_PORTS,
};
use aero_devices_storage::AhciPciDevice;
use aero_io_snapshot::io::state::IoSnapshot;
use aero_platform::io::IoPortBus;
use aero_storage::{DiskError, Result, VirtualDisk, SECTOR_SIZE};
use memory::{Bus, MemoryBus};

// AHCI register offsets.
const HBA_GHC: u64 = 0x04;
const PORT_BASE: u64 = 0x100;
const PORT_REG_CLB: u64 = 0x00;
const PORT_REG_CLBU: u64 = 0x04;
const PORT_REG_FB: u64 = 0x08;
const PORT_REG_FBU: u64 = 0x0C;
const PORT_REG_IS: u64 = 0x10;
const PORT_REG_IE: u64 = 0x14;
const PORT_REG_CMD: u64 = 0x18;
const PORT_REG_CI: u64 = 0x38;

const GHC_IE: u32 = 1 << 1;
const GHC_AE: u32 = 1 << 31;

const PORT_CMD_ST: u32 = 1 << 0;
const PORT_CMD_FRE: u32 = 1 << 4;

const PORT_IS_DHRS: u32 = 1 << 0;

#[derive(Clone)]
struct SharedDisk {
    data: Arc<Mutex<Vec<u8>>>,
    capacity: u64,
}

impl SharedDisk {
    fn new(sectors: usize) -> Self {
        let cap = sectors * SECTOR_SIZE;
        Self {
            data: Arc::new(Mutex::new(vec![0u8; cap])),
            capacity: cap as u64,
        }
    }
}

impl VirtualDisk for SharedDisk {
    fn capacity_bytes(&self) -> u64 {
        self.capacity
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<()> {
        let end = offset
            .checked_add(buf.len() as u64)
            .ok_or(DiskError::OffsetOverflow)?;
        if end > self.capacity {
            return Err(DiskError::OutOfBounds {
                offset,
                len: buf.len(),
                capacity: self.capacity,
            });
        }
        let guard = self.data.lock().unwrap();
        buf.copy_from_slice(&guard[offset as usize..end as usize]);
        Ok(())
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> Result<()> {
        let end = offset
            .checked_add(buf.len() as u64)
            .ok_or(DiskError::OffsetOverflow)?;
        if end > self.capacity {
            return Err(DiskError::OutOfBounds {
                offset,
                len: buf.len(),
                capacity: self.capacity,
            });
        }
        let mut guard = self.data.lock().unwrap();
        guard[offset as usize..end as usize].copy_from_slice(buf);
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}

fn write_cmd_header(
    mem: &mut dyn MemoryBus,
    clb: u64,
    slot: usize,
    ctba: u64,
    prdtl: u16,
    write: bool,
) {
    let cfl = 5u32;
    let w = if write { 1u32 << 6 } else { 0 };
    let flags = cfl | w | ((prdtl as u32) << 16);
    let addr = clb + (slot as u64) * 32;
    mem.write_u32(addr, flags);
    mem.write_u32(addr + 4, 0); // PRDBC
    mem.write_u32(addr + 8, ctba as u32);
    mem.write_u32(addr + 12, (ctba >> 32) as u32);
}

fn write_prdt(mem: &mut dyn MemoryBus, ctba: u64, entry: usize, dba: u64, dbc: u32) {
    let addr = ctba + 0x80 + (entry as u64) * 16;
    mem.write_u32(addr, dba as u32);
    mem.write_u32(addr + 4, (dba >> 32) as u32);
    mem.write_u32(addr + 8, 0);
    // DBC field stores byte_count-1 in bits 0..21.
    mem.write_u32(addr + 12, (dbc - 1) & 0x003F_FFFF);
}

fn write_cfis(mem: &mut dyn MemoryBus, ctba: u64, command: u8, lba: u64, count: u16) {
    let mut cfis = [0u8; 64];
    cfis[0] = 0x27;
    cfis[1] = 0x80;
    cfis[2] = command;
    cfis[7] = 0x40; // LBA mode

    cfis[4] = (lba & 0xFF) as u8;
    cfis[5] = ((lba >> 8) & 0xFF) as u8;
    cfis[6] = ((lba >> 16) & 0xFF) as u8;
    cfis[8] = ((lba >> 24) & 0xFF) as u8;
    cfis[9] = ((lba >> 32) & 0xFF) as u8;
    cfis[10] = ((lba >> 40) & 0xFF) as u8;

    cfis[12] = (count & 0xFF) as u8;
    cfis[13] = (count >> 8) as u8;

    mem.write_physical(ctba, &cfis);
}

#[test]
fn ahci_pci_snapshot_roundtrip_preserves_mmio_regs_intx_and_io() {
    let disk = SharedDisk::new(64);
    let mut seed = vec![0u8; SECTOR_SIZE];
    seed[0..4].copy_from_slice(&[9, 8, 7, 6]);
    disk.clone().write_sectors(4, &seed).unwrap();

    let mut dev = AhciPciDevice::new(1);
    dev.attach_drive(0, AtaDrive::new(Box::new(disk.clone())).unwrap());
    dev.config_mut().set_command(0x0006); // MEM + BUSMASTER

    let mut mem = Bus::new(0x20_000);

    // Program port and issue a READ DMA EXT to leave an interrupt pending.
    let clb = 0x1000u64;
    let fb = 0x2000u64;
    let ctba = 0x3000u64;
    let data_buf = 0x4000u64;

    dev.mmio_write(PORT_BASE + PORT_REG_CLB, 4, clb);
    dev.mmio_write(PORT_BASE + PORT_REG_CLBU, 4, clb >> 32);
    dev.mmio_write(PORT_BASE + PORT_REG_FB, 4, fb);
    dev.mmio_write(PORT_BASE + PORT_REG_FBU, 4, fb >> 32);
    dev.mmio_write(HBA_GHC, 4, u64::from(GHC_IE | GHC_AE));
    dev.mmio_write(PORT_BASE + PORT_REG_IE, 4, u64::from(PORT_IS_DHRS));
    dev.mmio_write(
        PORT_BASE + PORT_REG_CMD,
        4,
        u64::from(PORT_CMD_ST | PORT_CMD_FRE),
    );

    write_cmd_header(&mut mem, clb, 0, ctba, 1, false);
    write_cfis(&mut mem, ctba, ATA_CMD_READ_DMA_EXT, 4, 1);
    write_prdt(&mut mem, ctba, 0, data_buf, SECTOR_SIZE as u32);
    dev.mmio_write(PORT_BASE + PORT_REG_CI, 4, 1);
    dev.process(&mut mem);

    assert!(dev.intx_level());
    let mut out = [0u8; 4];
    mem.read_physical(data_buf, &mut out);
    assert_eq!(out, [9, 8, 7, 6]);

    let snap = dev.save_state();

    // Restore into a fresh device instance.
    let mut restored = AhciPciDevice::new(1);
    restored.load_state(&snap).unwrap();

    // Re-establish PCI config state (handled by PCI core snapshot in a full machine).
    restored.config_mut().set_command(0x0006); // MEM + BUSMASTER

    assert_eq!(restored.mmio_read(HBA_GHC, 4) as u32, GHC_IE | GHC_AE);
    assert_eq!(restored.mmio_read(PORT_BASE + PORT_REG_CLB, 4), clb);
    assert_eq!(restored.mmio_read(PORT_BASE + PORT_REG_FB, 4), fb);
    assert!(restored.intx_level());

    // Re-attach the disk backend and continue with a WRITE DMA EXT.
    restored.attach_drive(0, AtaDrive::new(Box::new(disk.clone())).unwrap());

    // Clear interrupt after restore.
    restored.mmio_write(PORT_BASE + PORT_REG_IS, 4, u64::from(PORT_IS_DHRS));
    assert!(!restored.intx_level());

    let write_buf = 0x5000u64;
    mem.write_physical(write_buf, &[1, 2, 3, 4]);
    mem.write_physical(write_buf + 4, &vec![0u8; SECTOR_SIZE - 4]);

    write_cmd_header(&mut mem, clb, 0, ctba, 1, true);
    write_cfis(&mut mem, ctba, ATA_CMD_WRITE_DMA_EXT, 5, 1);
    write_prdt(&mut mem, ctba, 0, write_buf, SECTOR_SIZE as u32);
    restored.mmio_write(PORT_BASE + PORT_REG_CI, 4, 1);
    restored.process(&mut mem);
    assert!(restored.intx_level());

    let mut verify = vec![0u8; SECTOR_SIZE];
    disk.clone().read_sectors(5, &mut verify).unwrap();
    assert_eq!(&verify[..4], &[1, 2, 3, 4]);
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
fn piix3_ide_snapshot_roundtrip_preserves_pio_progress_and_continues_io() {
    let disk = SharedDisk::new(16);
    let mut sector0 = vec![0u8; SECTOR_SIZE];
    sector0[0..4].copy_from_slice(b"BOOT");
    disk.clone().write_sectors(0, &sector0).unwrap();

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut()
        .controller
        .attach_primary_master_ata(AtaDrive::new(Box::new(disk.clone())).unwrap());
    // Enable PCI I/O space decoding so legacy ports respond.
    ide.borrow_mut().config_mut().set_command(0x0001);

    let mut ioports = IoPortBus::new();
    register_piix3_ide_ports(&mut ioports, ide.clone());

    // Issue READ SECTORS for LBA 0, 1 sector.
    ioports.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    ioports.write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 7, 1, 0x20);

    // Consume the first 4 bytes ("BOOT") but leave the transfer in progress.
    let w0 = ioports.read(PRIMARY_PORTS.cmd_base, 2) as u16;
    let w1 = ioports.read(PRIMARY_PORTS.cmd_base, 2) as u16;
    let mut first4 = [0u8; 4];
    first4[0..2].copy_from_slice(&w0.to_le_bytes());
    first4[2..4].copy_from_slice(&w1.to_le_bytes());
    assert_eq!(&first4, b"BOOT");

    let snap = ide.borrow().save_state();

    let restored_dev = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    restored_dev.borrow_mut().load_state(&snap).unwrap();
    restored_dev.borrow_mut().config_mut().set_command(0x0001);
    assert!(restored_dev.borrow().controller.primary_irq_pending());

    let mut io2 = IoPortBus::new();
    register_piix3_ide_ports(&mut io2, restored_dev.clone());

    // Read the rest of the sector and ensure it's still correct.
    let mut buf = vec![0u8; SECTOR_SIZE];
    buf[0..4].copy_from_slice(b"BOOT");
    for i in 2..(SECTOR_SIZE / 2) {
        let w = io2.read(PRIMARY_PORTS.cmd_base, 2) as u16;
        buf[i * 2..i * 2 + 2].copy_from_slice(&w.to_le_bytes());
    }
    assert_eq!(&buf[0..4], b"BOOT");

    // Reading status clears the pending IRQ.
    let _ = io2.read(PRIMARY_PORTS.cmd_base + 7, 1);
    assert!(!restored_dev.borrow().controller.primary_irq_pending());

    // Re-attach the backend and perform a WRITE SECTORS PIO to LBA 1.
    restored_dev
        .borrow_mut()
        .controller
        .attach_primary_master_ata(AtaDrive::new(Box::new(disk.clone())).unwrap());

    io2.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    io2.write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    io2.write(PRIMARY_PORTS.cmd_base + 3, 1, 1);
    io2.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    io2.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    io2.write(PRIMARY_PORTS.cmd_base + 7, 1, 0x30); // WRITE SECTORS

    // Write [5,6,7,8] then zero-fill.
    io2.write(PRIMARY_PORTS.cmd_base, 2, u16::from_le_bytes([5, 6]) as u32);
    io2.write(PRIMARY_PORTS.cmd_base, 2, u16::from_le_bytes([7, 8]) as u32);
    for _ in 0..((SECTOR_SIZE / 2) - 2) {
        io2.write(PRIMARY_PORTS.cmd_base, 2, 0);
    }

    let mut verify = vec![0u8; SECTOR_SIZE];
    disk.clone().read_sectors(1, &mut verify).unwrap();
    assert_eq!(&verify[..4], &[5, 6, 7, 8]);
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

#[test]
fn piix3_ide_snapshot_roundtrip_preserves_dma_inflight_and_atapi_sense() {
    let disk = SharedDisk::new(32);

    // Pattern to DMA into the disk.
    let mut pattern = vec![0u8; SECTOR_SIZE];
    for (i, b) in pattern.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(7).wrapping_add(3);
    }

    let mut iso = MemIso::new(2);
    iso.data[2048..2053].copy_from_slice(b"WORLD");

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut()
        .controller
        .attach_primary_master_ata(AtaDrive::new(Box::new(disk.clone())).unwrap());
    ide.borrow_mut()
        .controller
        .attach_secondary_master_atapi(AtapiCdrom::new(Some(Box::new(iso))));
    // Enable I/O decode + bus mastering (required for Bus Master IDE DMA).
    ide.borrow_mut().config_mut().set_command(0x0005);

    let mut ioports = IoPortBus::new();
    register_piix3_ide_ports(&mut ioports, ide.clone());

    let mut mem = Bus::new(0x20_000);
    let prd_addr = 0x1000u64;
    let write_buf = 0x3000u64;

    mem.write_physical(write_buf, &pattern);
    mem.write_u32(prd_addr, write_buf as u32);
    mem.write_u16(prd_addr + 4, SECTOR_SIZE as u16);
    mem.write_u16(prd_addr + 6, 0x8000);

    let bm_base = ide.borrow().bus_master_base();
    ioports.write(bm_base + 4, 4, prd_addr as u32);

    // Issue WRITE DMA (LBA 2, 1 sector), but don't tick yet.
    ioports.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    ioports.write(PRIMARY_PORTS.cmd_base + 3, 1, 2);
    ioports.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xCA);
    ioports.write(bm_base, 1, 0x01); // start (from memory)

    // Trigger ATAPI UNIT ATTENTION and leave sense state pending.
    ioports.write(SECONDARY_PORTS.cmd_base + 6, 1, 0xA0);
    let tur = [0u8; 12];
    send_atapi_packet(&mut ioports, SECONDARY_PORTS.cmd_base, 0, &tur, 0);
    let _ = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1);

    let snap = ide.borrow().save_state();

    // Restore and re-attach backends.
    let mut restored = Piix3IdePciDevice::new();
    restored.load_state(&snap).unwrap();
    restored.config_mut().set_command(0x0005);
    restored
        .controller
        .attach_primary_master_ata(AtaDrive::new(Box::new(disk.clone())).unwrap());
    restored
        .controller
        .attach_secondary_master_atapi_backend_for_restore(Box::new(MemIso::new(2)));

    // Complete the in-flight DMA write.
    restored.tick(&mut mem);

    let mut out = vec![0u8; SECTOR_SIZE];
    disk.clone().read_sectors(2, &mut out).unwrap();
    assert_eq!(out, pattern);

    // Verify ATAPI REQUEST SENSE still reports UNIT ATTENTION / medium changed.
    let restored = Rc::new(RefCell::new(restored));
    let mut io2 = IoPortBus::new();
    register_piix3_ide_ports(&mut io2, restored.clone());

    io2.write(SECONDARY_PORTS.cmd_base + 6, 1, 0xA0);
    let mut req_sense = [0u8; 12];
    req_sense[0] = 0x03;
    req_sense[4] = 18;
    send_atapi_packet(&mut io2, SECONDARY_PORTS.cmd_base, 0, &req_sense, 18);

    let mut sense = [0u8; 18];
    for i in 0..(18 / 2) {
        let w = io2.read(SECONDARY_PORTS.cmd_base, 2) as u16;
        sense[i * 2..i * 2 + 2].copy_from_slice(&w.to_le_bytes());
    }
    assert_eq!(sense[2] & 0x0F, 0x06); // UNIT ATTENTION
    assert_eq!(sense[12], 0x28); // MEDIUM CHANGED
}
