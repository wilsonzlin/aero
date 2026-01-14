use std::cell::RefCell;
use std::io;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use aero_devices::pci::profile::IDE_PIIX3;
use aero_devices::pci::{
    bios_post, PciBarDefinition, PciDevice, PciPlatform, PciResourceAllocator,
    PciResourceAllocatorConfig,
};
use aero_devices_storage::ata::AtaDrive;
use aero_devices_storage::atapi::IsoBackend;
use aero_devices_storage::pci_ide::{
    register_piix3_ide_ports, Piix3IdePciDevice, PRIMARY_PORTS, SECONDARY_PORTS,
};
use aero_io_snapshot::io::state::IoSnapshot;
use aero_io_snapshot::io::storage::state::MAX_IDE_DATA_BUFFER_BYTES;
use aero_platform::io::IoPortBus;
use aero_storage::{DiskError, MemBackend, RawDisk, Result, VirtualDisk, SECTOR_SIZE};
use memory::{Bus, MemoryBus};

#[derive(Debug)]
struct ZeroDisk {
    capacity: u64,
}

impl VirtualDisk for ZeroDisk {
    fn capacity_bytes(&self) -> u64 {
        self.capacity
    }

    fn read_at(&mut self, _offset: u64, buf: &mut [u8]) -> aero_storage::Result<()> {
        buf.fill(0);
        Ok(())
    }

    fn write_at(&mut self, _offset: u64, _buf: &[u8]) -> aero_storage::Result<()> {
        Ok(())
    }

    fn flush(&mut self) -> aero_storage::Result<()> {
        Ok(())
    }
}

struct DropDetectDisk {
    inner: RawDisk<MemBackend>,
    dropped: Arc<AtomicBool>,
}

impl Drop for DropDetectDisk {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}

impl VirtualDisk for DropDetectDisk {
    fn capacity_bytes(&self) -> u64 {
        self.inner.capacity_bytes()
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> aero_storage::Result<()> {
        self.inner.read_at(offset, buf)
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> aero_storage::Result<()> {
        self.inner.write_at(offset, buf)
    }

    fn flush(&mut self) -> aero_storage::Result<()> {
        self.inner.flush()
    }
}

struct DropDetectIso {
    inner: MemIso,
    dropped: Arc<AtomicBool>,
}

impl Drop for DropDetectIso {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}

impl IsoBackend for DropDetectIso {
    fn sector_count(&self) -> u32 {
        self.inner.sector_count()
    }

    fn read_sectors(&mut self, lba: u32, buf: &mut [u8]) -> io::Result<()> {
        self.inner.read_sectors(lba, buf)
    }
}

fn read_u8(dev: &mut Piix3IdePciDevice, offset: u16) -> u8 {
    dev.config_mut().read(offset, 1) as u8
}

fn read_u16(dev: &mut Piix3IdePciDevice, offset: u16) -> u16 {
    dev.config_mut().read(offset, 2) as u16
}

fn read_u32(dev: &mut Piix3IdePciDevice, offset: u16) -> u32 {
    dev.config_mut().read(offset, 4)
}

fn io_probe_mask(size: u32) -> u32 {
    (!(size.saturating_sub(1)) & 0xFFFF_FFFC) | 0x1
}

#[derive(Debug, Default)]
struct RecordingDisk {
    capacity_bytes: u64,
    last_write_lba: Option<u64>,
    last_write_len: usize,
}

impl RecordingDisk {
    fn new(sectors: u64) -> Self {
        Self {
            capacity_bytes: sectors * SECTOR_SIZE as u64,
            ..Default::default()
        }
    }
}

#[derive(Clone, Debug)]
struct SharedRecordingDisk(Arc<Mutex<RecordingDisk>>);

impl VirtualDisk for SharedRecordingDisk {
    fn capacity_bytes(&self) -> u64 {
        self.0.lock().unwrap().capacity_bytes
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<()> {
        let cap = self.capacity_bytes();
        let end = offset
            .checked_add(buf.len() as u64)
            .ok_or(DiskError::OffsetOverflow)?;
        if end > cap {
            return Err(DiskError::OutOfBounds {
                offset,
                len: buf.len(),
                capacity: cap,
            });
        }
        buf.fill(0);
        Ok(())
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> Result<()> {
        let cap = self.capacity_bytes();
        let end = offset
            .checked_add(buf.len() as u64)
            .ok_or(DiskError::OffsetOverflow)?;
        if end > cap {
            return Err(DiskError::OutOfBounds {
                offset,
                len: buf.len(),
                capacity: cap,
            });
        }
        assert_eq!(
            offset % SECTOR_SIZE as u64,
            0,
            "ATA should only issue sector-aligned writes"
        );
        assert!(
            buf.len().is_multiple_of(SECTOR_SIZE),
            "ATA should only issue whole-sector writes"
        );

        let lba = offset / SECTOR_SIZE as u64;
        let mut inner = self.0.lock().unwrap();
        inner.last_write_lba = Some(lba);
        inner.last_write_len = buf.len();
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
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
    assert_eq!(read_u8(&mut dev, 0x0e), IDE_PIIX3.header_type);
    assert_eq!(read_u16(&mut dev, 0x2c), IDE_PIIX3.subsystem_vendor_id);
    assert_eq!(read_u16(&mut dev, 0x2e), IDE_PIIX3.subsystem_id);
    let expected_pin = IDE_PIIX3
        .interrupt_pin
        .map(|p| p.to_config_u8())
        .unwrap_or(0);
    assert_eq!(read_u8(&mut dev, 0x3d), expected_pin);

    assert_eq!(
        dev.config().bar_definition(0),
        Some(PciBarDefinition::Io { size: 8 })
    );
    assert_eq!(
        dev.config().bar_definition(1),
        Some(PciBarDefinition::Io { size: 4 })
    );
    assert_eq!(
        dev.config().bar_definition(2),
        Some(PciBarDefinition::Io { size: 8 })
    );
    assert_eq!(
        dev.config().bar_definition(3),
        Some(PciBarDefinition::Io { size: 4 })
    );
    assert_eq!(
        dev.config().bar_definition(4),
        Some(PciBarDefinition::Io { size: 16 })
    );

    // BAR0 (8-byte I/O).
    dev.config_mut().write(0x10, 4, 0xffff_ffff);
    assert_eq!(read_u32(&mut dev, 0x10), io_probe_mask(8));
    dev.config_mut().write(0x10, 4, 0x0000_1f03);
    assert_eq!(read_u32(&mut dev, 0x10), 0x0000_1f01);

    // BAR1 (4-byte I/O).
    dev.config_mut().write(0x14, 4, 0xffff_ffff);
    assert_eq!(read_u32(&mut dev, 0x14), io_probe_mask(4));
    dev.config_mut().write(0x14, 4, 0x0000_3f07);
    assert_eq!(read_u32(&mut dev, 0x14), 0x0000_3f05);

    // BAR2 (8-byte I/O).
    dev.config_mut().write(0x18, 4, 0xffff_ffff);
    assert_eq!(read_u32(&mut dev, 0x18), io_probe_mask(8));
    dev.config_mut().write(0x18, 4, 0x0000_1703);
    assert_eq!(read_u32(&mut dev, 0x18), 0x0000_1701);

    // BAR3 (4-byte I/O).
    dev.config_mut().write(0x1c, 4, 0xffff_ffff);
    assert_eq!(read_u32(&mut dev, 0x1c), io_probe_mask(4));
    dev.config_mut().write(0x1c, 4, 0x0000_3707);
    assert_eq!(read_u32(&mut dev, 0x1c), 0x0000_3705);

    // BAR4 (16-byte I/O).
    dev.config_mut().write(0x20, 4, 0xffff_ffff);
    assert_eq!(read_u32(&mut dev, 0x20), io_probe_mask(16));
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
fn ata_boot_sector_read_via_legacy_pio_ports_byte_reads() {
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
    for b in &mut buf {
        *b = io.read(PRIMARY_PORTS.cmd_base, 1) as u8;
    }

    assert_eq!(&buf[0..4], b"BOOT");
    assert_eq!(&buf[510..512], &[0x55, 0xAA]);
}

#[test]
fn ata_boot_sector_read_via_legacy_pio_ports_dword_reads() {
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
    for i in 0..(SECTOR_SIZE / 4) {
        let d = io.read(PRIMARY_PORTS.cmd_base, 4);
        buf[i * 4..i * 4 + 4].copy_from_slice(&d.to_le_bytes());
    }

    assert_eq!(&buf[0..4], b"BOOT");
    assert_eq!(&buf[510..512], &[0x55, 0xAA]);
}

#[test]
fn ata_slave_absent_floats_bus_high_and_does_not_raise_irq() {
    let capacity = 4 * SECTOR_SIZE as u64;
    let disk = RawDisk::create(MemBackend::new(), capacity).unwrap();

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut()
        .controller
        .attach_primary_master_ata(AtaDrive::new(Box::new(disk)).unwrap());
    ide.borrow_mut().config_mut().set_command(0x0001); // IO decode

    let mut io = IoPortBus::new();
    register_piix3_ide_ports(&mut io, ide.clone());

    // Select slave on the primary channel (no device attached there).
    io.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xF0);

    // Status/alt-status should float high.
    assert_eq!(io.read(PRIMARY_PORTS.cmd_base + 7, 1) as u8, 0xFF);
    assert_eq!(io.read(PRIMARY_PORTS.ctrl_base, 1) as u8, 0xFF);
    assert_eq!(io.read(PRIMARY_PORTS.ctrl_base, 2) as u16, 0xFFFF);
    assert_eq!(io.read(PRIMARY_PORTS.ctrl_base, 4), 0xFFFF_FFFF);
    let dadr = io.read(PRIMARY_PORTS.ctrl_base + 1, 1) as u8;
    assert_eq!(dadr, 0xFF);

    // Commands to an absent device should be ignored (no IRQ side effects).
    io.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xEC); // IDENTIFY DEVICE
    assert!(!ide.borrow().controller.primary_irq_pending());

    // Switching back to master should still allow normal operations.
    io.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    io.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xEC);
    assert!(ide.borrow().controller.primary_irq_pending());
}

#[test]
fn status_read_while_slave_absent_selected_does_not_ack_master_irq() {
    let capacity = 4 * SECTOR_SIZE as u64;
    let disk = RawDisk::create(MemBackend::new(), capacity).unwrap();

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut()
        .controller
        .attach_primary_master_ata(AtaDrive::new(Box::new(disk)).unwrap());
    ide.borrow_mut().config_mut().set_command(0x0001); // IO decode

    let mut io = IoPortBus::new();
    register_piix3_ide_ports(&mut io, ide.clone());

    // Issue IDENTIFY DEVICE on the master to assert an interrupt and enter a data phase.
    io.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    io.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xEC);
    assert!(ide.borrow().controller.primary_irq_pending());

    // Select absent slave and read STATUS. This should return bus-high but must not acknowledge the
    // master's interrupt.
    io.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xF0);
    assert_eq!(io.read(PRIMARY_PORTS.cmd_base + 7, 1) as u8, 0xFF);
    assert!(
        ide.borrow().controller.primary_irq_pending(),
        "STATUS read for absent slave should not clear the master's IRQ latch"
    );

    // Select master again and acknowledge the IRQ.
    io.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    let _ = io.read(PRIMARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.primary_irq_pending());

    // Drain the IDENTIFY data to complete the command and then acknowledge its completion IRQ.
    for _ in 0..256 {
        let _ = io.read(PRIMARY_PORTS.cmd_base, 2);
    }
    assert!(ide.borrow().controller.primary_irq_pending());
    let _ = io.read(PRIMARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.primary_irq_pending());
}

#[test]
fn ata_taskfile_writes_to_absent_slave_are_ignored() {
    let capacity = 2 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();

    let sector0 = vec![0x11u8; SECTOR_SIZE];
    let sector1 = vec![0x22u8; SECTOR_SIZE];
    disk.write_sectors(0, &sector0).unwrap();
    disk.write_sectors(1, &sector1).unwrap();

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut()
        .controller
        .attach_primary_master_ata(AtaDrive::new(Box::new(disk)).unwrap());
    ide.borrow_mut().config_mut().set_command(0x0001); // IO decode

    let mut io = IoPortBus::new();
    register_piix3_ide_ports(&mut io, ide.clone());

    // Program a READ SECTORS command (LBA 0, 1 sector) while the master is selected, but do not
    // issue the command yet.
    io.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    io.write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    io.write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    io.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    io.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);

    // Select the absent slave and attempt to clobber the taskfile (LBA 1). These writes must be
    // ignored so they cannot perturb the master's pending register image.
    io.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xF0);
    io.write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    io.write(PRIMARY_PORTS.cmd_base + 3, 1, 1);
    io.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    io.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    io.write(PRIMARY_PORTS.cmd_base + 7, 1, 0x20); // READ SECTORS (must be ignored)
    assert!(
        !ide.borrow().controller.primary_irq_pending(),
        "command to absent slave should not raise an IRQ"
    );

    // Switch back to master and issue the command. It should still read LBA 0.
    io.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    io.write(PRIMARY_PORTS.cmd_base + 7, 1, 0x20);

    let mut out = [0u8; SECTOR_SIZE];
    for i in 0..(SECTOR_SIZE / 2) {
        let w = io.read(PRIMARY_PORTS.cmd_base, 2) as u16;
        out[i * 2..i * 2 + 2].copy_from_slice(&w.to_le_bytes());
    }
    assert_eq!(&out[..], &sector0[..]);
    assert_ne!(&out[..], &sector1[..]);

    let _ = io.read(PRIMARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.primary_irq_pending());
}

#[test]
fn drive_address_master_present_is_stable_and_nonzero() {
    let capacity = 4 * SECTOR_SIZE as u64;
    let disk = RawDisk::create(MemBackend::new(), capacity).unwrap();

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut()
        .controller
        .attach_primary_master_ata(AtaDrive::new(Box::new(disk)).unwrap());
    ide.borrow_mut().config_mut().set_command(0x0001); // IO decode

    let mut io = IoPortBus::new();
    register_piix3_ide_ports(&mut io, ide.clone());

    // Select master.
    io.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);

    let v0 = io.read(PRIMARY_PORTS.ctrl_base + 1, 1) as u8;
    let v1 = io.read(PRIMARY_PORTS.ctrl_base + 1, 1) as u8;

    assert_ne!(v0, 0);
    assert_ne!(
        v0, 0xFF,
        "DADR should not float high when a device is present"
    );
    assert_eq!(v0, v1, "DADR reads should be stable");
}

#[test]
fn drive_address_slave_absent_floats_bus_high() {
    let capacity = 4 * SECTOR_SIZE as u64;
    let disk = RawDisk::create(MemBackend::new(), capacity).unwrap();

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut()
        .controller
        .attach_primary_master_ata(AtaDrive::new(Box::new(disk)).unwrap());
    ide.borrow_mut().config_mut().set_command(0x0001); // IO decode

    let mut io = IoPortBus::new();
    register_piix3_ide_ports(&mut io, ide.clone());

    // Select slave (no device attached).
    io.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xF0);
    let v = io.read(PRIMARY_PORTS.ctrl_base + 1, 1) as u8;

    assert_eq!(v, 0xFF);
}

#[test]
fn drive_address_reads_do_not_clear_irq_latch() {
    let capacity = 4 * SECTOR_SIZE as u64;
    let disk = RawDisk::create(MemBackend::new(), capacity).unwrap();

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut()
        .controller
        .attach_primary_master_ata(AtaDrive::new(Box::new(disk)).unwrap());
    ide.borrow_mut().config_mut().set_command(0x0001); // IO decode

    let mut io = IoPortBus::new();
    register_piix3_ide_ports(&mut io, ide.clone());

    // Issue IDENTIFY to assert IRQ.
    io.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    io.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xEC);
    assert!(ide.borrow().controller.primary_irq_pending());

    // Drive Address reads must not clear the IRQ latch.
    let _ = io.read(PRIMARY_PORTS.ctrl_base + 1, 1);
    assert!(
        ide.borrow().controller.primary_irq_pending(),
        "DADR read cleared IRQ latch"
    );

    // STATUS reads still clear it.
    let _ = io.read(PRIMARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.primary_irq_pending());
}

#[test]
fn reset_clears_channel_and_bus_master_state_but_preserves_attached_media() {
    // ATA disk with a recognizable boot sector.
    let dropped_ata = Arc::new(AtomicBool::new(false));
    let capacity = 8 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut sector0 = vec![0u8; SECTOR_SIZE];
    sector0[0..4].copy_from_slice(b"BOOT");
    sector0[510] = 0x55;
    sector0[511] = 0xAA;
    disk.write_sectors(0, &sector0).unwrap();
    let disk = DropDetectDisk {
        inner: disk,
        dropped: dropped_ata.clone(),
    };

    // ATAPI ISO with recognizable data at LBA 1.
    let dropped_iso = Arc::new(AtomicBool::new(false));
    let mut iso = MemIso::new(2);
    iso.data[2048..2053].copy_from_slice(b"WORLD");
    let iso = DropDetectIso {
        inner: iso,
        dropped: dropped_iso.clone(),
    };

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    {
        let mut dev = ide.borrow_mut();
        dev.controller
            .attach_primary_master_ata(AtaDrive::new(Box::new(disk)).unwrap());
        dev.controller
            .attach_secondary_master_atapi(aero_devices_storage::atapi::AtapiCdrom::new(Some(
                Box::new(iso),
            )));
        dev.config_mut().set_command(0x0005); // IO decode + Bus Master
    }

    let mut ioports = IoPortBus::new();
    register_piix3_ide_ports(&mut ioports, ide.clone());

    // Mutate primary channel state by starting a PIO read and consuming only part of the data.
    ioports.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0); // master + LBA
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, 1); // count
    ioports.write(PRIMARY_PORTS.cmd_base + 3, 1, 0); // lba0
    ioports.write(PRIMARY_PORTS.cmd_base + 4, 1, 0); // lba1
    ioports.write(PRIMARY_PORTS.cmd_base + 5, 1, 0); // lba2
    ioports.write(PRIMARY_PORTS.cmd_base + 7, 1, 0x20); // READ SECTORS
    let _ = ioports.read(PRIMARY_PORTS.cmd_base, 2); // consume 1 word only

    // Mutate bus master registers to non-zero values.
    let bm_base = ide.borrow().bus_master_base();
    ioports.write(bm_base + 4, 4, 0x1234_5678);
    ioports.write(bm_base, 1, 0x01); // start
    assert_ne!(ioports.read(bm_base + 4, 4), 0);

    // Reset in-place (should preserve attached devices/backends).
    ide.borrow_mut().reset();
    assert!(
        !dropped_ata.load(Ordering::SeqCst),
        "reset dropped the attached ATA disk backend"
    );
    assert!(
        !dropped_iso.load(Ordering::SeqCst),
        "reset dropped the attached ISO backend"
    );

    // Re-enable I/O decode so we can observe device state post-reset.
    ide.borrow_mut().config_mut().set_command(0x0005); // IO decode + Bus Master

    // Channel state should be idle (DRDY, no BSY/DRQ) and no IRQ latched.
    assert!(!ide.borrow().controller.primary_irq_pending());
    assert!(!ide.borrow().controller.secondary_irq_pending());
    let st = ioports.read(PRIMARY_PORTS.cmd_base + 7, 1) as u8;
    assert_eq!(st & 0x80, 0, "BSY should be clear after reset");
    assert_eq!(st & 0x08, 0, "DRQ should be clear after reset");
    assert_ne!(st & 0x40, 0, "DRDY should be set after reset");

    // Bus Master IDE runtime registers should be reset (active/error/irq cleared, PRD cleared).
    let bm_cmd = ioports.read(bm_base, 1) as u8;
    assert_eq!(
        bm_cmd & 0x09,
        0,
        "bus master start/direction bits should be clear"
    );
    assert_eq!(
        ioports.read(bm_base + 4, 4),
        0,
        "PRD pointer should reset to 0"
    );
    let bm_st = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(bm_st & 0x07, 0, "bus master status bits should reset");

    // ATA device should still be present and readable after reset.
    ioports.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0); // master + LBA
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, 1); // count
    ioports.write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 7, 1, 0x20); // READ SECTORS
    let mut ata_out = [0u8; SECTOR_SIZE];
    for i in 0..(SECTOR_SIZE / 2) {
        let w = ioports.read(PRIMARY_PORTS.cmd_base, 2) as u16;
        ata_out[i * 2..i * 2 + 2].copy_from_slice(&w.to_le_bytes());
    }
    assert_eq!(&ata_out[0..4], b"BOOT");
    assert_eq!(&ata_out[510..512], &[0x55, 0xAA]);

    // ATAPI backend should still be present and readable after reset.
    ioports.write(SECONDARY_PORTS.cmd_base + 6, 1, 0xA0); // select master on secondary

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

    let mut atapi_out = vec![0u8; 2048];
    for i in 0..(2048 / 2) {
        let w = ioports.read(SECONDARY_PORTS.cmd_base, 2) as u16;
        atapi_out[i * 2..i * 2 + 2].copy_from_slice(&w.to_le_bytes());
    }
    assert_eq!(&atapi_out[0..5], b"WORLD");

    // Replacing the devices should drop the previous backends (sanity check).
    let replacement = RawDisk::create(MemBackend::new(), capacity).unwrap();
    ide.borrow_mut()
        .controller
        .attach_primary_master_ata(AtaDrive::new(Box::new(replacement)).unwrap());
    assert!(
        dropped_ata.load(Ordering::SeqCst),
        "replacing the ATA drive should drop the previous disk backend"
    );

    let replacement_iso = MemIso::new(2);
    ide.borrow_mut().controller.attach_secondary_master_atapi(
        aero_devices_storage::atapi::AtapiCdrom::new(Some(Box::new(replacement_iso))),
    );
    assert!(
        dropped_iso.load(Ordering::SeqCst),
        "replacing the ATAPI device should drop the previous ISO backend"
    );
}

#[test]
fn ata_software_reset_clears_pending_dma_request() {
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
    let bm_base = ide.borrow().bus_master_base();

    let prd_addr = 0x1000u64;
    let dma_buf = 0x3000u64;

    // PRD entry: one 512-byte segment, end-of-table.
    mem.write_u32(prd_addr, dma_buf as u32);
    mem.write_u16(prd_addr + 4, SECTOR_SIZE as u16);
    mem.write_u16(prd_addr + 6, 0x8000);
    ioports.write(bm_base + 4, 4, prd_addr as u32);

    // Seed destination buffer.
    mem.write_physical(dma_buf, &vec![0xFFu8; SECTOR_SIZE]);

    // Start BMIDE engine, then queue a DMA request by issuing READ DMA (but do not tick yet).
    ioports.write(bm_base, 1, 0x09);

    ioports.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    ioports.write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xC8); // READ DMA

    assert!(
        !ide.borrow().controller.primary_irq_pending(),
        "DMA command should not raise IRQ until it completes"
    );

    // Assert software reset (SRST) via Device Control; this should clear the pending DMA request.
    ioports.write(PRIMARY_PORTS.ctrl_base, 1, 0x04);

    // Even though the BMIDE engine is started, there should be no DMA request left to service.
    ide.borrow_mut().tick(&mut mem);

    assert!(
        !ide.borrow().controller.primary_irq_pending(),
        "SRST should clear any pending DMA completion IRQ"
    );
    let bm_st = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(
        bm_st & 0x07,
        0,
        "BMIDE status bits should remain clear when SRST cancels the request"
    );

    let mut out = vec![0u8; SECTOR_SIZE];
    mem.read_physical(dma_buf, &mut out);
    assert!(
        out.iter().all(|&b| b == 0xFF),
        "guest memory should not be modified after SRST clears the DMA request"
    );
}

#[test]
fn ata_software_reset_clears_pending_dma_request_on_secondary_channel() {
    let capacity = 4 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut sector0 = vec![0u8; SECTOR_SIZE];
    sector0[..4].copy_from_slice(b"OKAY");
    disk.write_sectors(0, &sector0).unwrap();

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut()
        .controller
        .attach_secondary_master_ata(AtaDrive::new(Box::new(disk)).unwrap());
    ide.borrow_mut().config_mut().set_command(0x0005); // IO decode + Bus Master

    let mut ioports = IoPortBus::new();
    register_piix3_ide_ports(&mut ioports, ide.clone());

    let mut mem = Bus::new(0x20_000);
    let bm_base = ide.borrow().bus_master_base();

    let prd_addr = 0x1000u64;
    let dma_buf = 0x3000u64;

    // PRD entry: one 512-byte segment, end-of-table.
    mem.write_u32(prd_addr, dma_buf as u32);
    mem.write_u16(prd_addr + 4, SECTOR_SIZE as u16);
    mem.write_u16(prd_addr + 6, 0x8000);
    ioports.write(bm_base + 8 + 4, 4, prd_addr as u32);

    // Seed destination buffer.
    mem.write_physical(dma_buf, &vec![0xFFu8; SECTOR_SIZE]);

    // Start BMIDE engine, then queue a DMA request by issuing READ DMA (but do not tick yet).
    ioports.write(bm_base + 8, 1, 0x09);

    ioports.write(SECONDARY_PORTS.cmd_base + 6, 1, 0xE0);
    ioports.write(SECONDARY_PORTS.cmd_base + 2, 1, 1);
    ioports.write(SECONDARY_PORTS.cmd_base + 3, 1, 0);
    ioports.write(SECONDARY_PORTS.cmd_base + 4, 1, 0);
    ioports.write(SECONDARY_PORTS.cmd_base + 5, 1, 0);
    ioports.write(SECONDARY_PORTS.cmd_base + 7, 1, 0xC8); // READ DMA

    assert!(
        !ide.borrow().controller.secondary_irq_pending(),
        "DMA command should not raise IRQ until it completes"
    );

    // Assert software reset (SRST) via Device Control; this should clear the pending DMA request.
    ioports.write(SECONDARY_PORTS.ctrl_base, 1, 0x04);

    // Even though the BMIDE engine is started, there should be no DMA request left to service.
    ide.borrow_mut().tick(&mut mem);

    assert!(
        !ide.borrow().controller.secondary_irq_pending(),
        "SRST should clear any pending DMA completion IRQ"
    );
    assert!(!ide.borrow().controller.primary_irq_pending());

    let bm_st = ioports.read(bm_base + 8 + 2, 1) as u8;
    assert_eq!(
        bm_st & 0x07,
        0,
        "BMIDE status bits should remain clear when SRST cancels the request"
    );

    let mut out = vec![0u8; SECTOR_SIZE];
    mem.read_physical(dma_buf, &mut out);
    assert!(
        out.iter().all(|&b| b == 0xFF),
        "guest memory should not be modified after SRST clears the DMA request"
    );
}

#[test]
fn ata_srst_on_primary_channel_does_not_clear_secondary_pending_dma_request() {
    let capacity = 4 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut sector0 = vec![0u8; SECTOR_SIZE];
    for (i, b) in sector0.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(3).wrapping_add(0x11);
    }
    disk.write_sectors(0, &sector0).unwrap();

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut()
        .controller
        .attach_secondary_master_ata(AtaDrive::new(Box::new(disk)).unwrap());
    ide.borrow_mut().config_mut().set_command(0x0005); // IO decode + Bus Master

    let mut ioports = IoPortBus::new();
    register_piix3_ide_ports(&mut ioports, ide.clone());

    let mut mem = Bus::new(0x20_000);
    let bm_base = ide.borrow().bus_master_base();

    let prd_addr = 0x1000u64;
    let dma_buf = 0x3000u64;

    // PRD entry: one 512-byte segment, end-of-table.
    mem.write_u32(prd_addr, dma_buf as u32);
    mem.write_u16(prd_addr + 4, SECTOR_SIZE as u16);
    mem.write_u16(prd_addr + 6, 0x8000);
    ioports.write(bm_base + 8 + 4, 4, prd_addr as u32);

    // Seed destination buffer to prove no DMA occurs until we start the secondary BMIDE engine.
    mem.write_physical(dma_buf, &vec![0xFFu8; SECTOR_SIZE]);

    // Queue a secondary DMA request by issuing READ DMA, but do not start BMIDE yet.
    ioports.write(SECONDARY_PORTS.cmd_base + 6, 1, 0xE0);
    ioports.write(SECONDARY_PORTS.cmd_base + 2, 1, 1);
    ioports.write(SECONDARY_PORTS.cmd_base + 3, 1, 0);
    ioports.write(SECONDARY_PORTS.cmd_base + 4, 1, 0);
    ioports.write(SECONDARY_PORTS.cmd_base + 5, 1, 0);
    ioports.write(SECONDARY_PORTS.cmd_base + 7, 1, 0xC8);

    assert!(!ide.borrow().controller.secondary_irq_pending());

    // Assert SRST on the *primary* channel. This must not clear the secondary channel's pending
    // DMA request.
    ioports.write(PRIMARY_PORTS.ctrl_base, 1, 0x04);
    ioports.write(PRIMARY_PORTS.ctrl_base, 1, 0x00);

    // Start secondary BMIDE and tick; the original request should still complete.
    ioports.write(bm_base + 8, 1, 0x09);
    ide.borrow_mut().tick(&mut mem);

    let bm_st_secondary = ioports.read(bm_base + 8 + 2, 1) as u8;
    assert_eq!(bm_st_secondary & 0x07, 0x04);
    assert!(ide.borrow().controller.secondary_irq_pending());
    assert!(!ide.borrow().controller.primary_irq_pending());

    let mut out = vec![0u8; SECTOR_SIZE];
    mem.read_physical(dma_buf, &mut out);
    assert_eq!(out, sector0);

    let _ = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.secondary_irq_pending());
}

#[test]
fn ata_srst_on_secondary_channel_does_not_clear_primary_pending_dma_request() {
    let capacity = 4 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut sector0 = vec![0u8; SECTOR_SIZE];
    for (i, b) in sector0.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(5).wrapping_add(0x27);
    }
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

    // PRD entry: one 512-byte segment, end-of-table.
    mem.write_u32(prd_addr, dma_buf as u32);
    mem.write_u16(prd_addr + 4, SECTOR_SIZE as u16);
    mem.write_u16(prd_addr + 6, 0x8000);
    ioports.write(bm_base + 4, 4, prd_addr as u32);

    mem.write_physical(dma_buf, &vec![0xFFu8; SECTOR_SIZE]);

    // Queue a primary DMA request by issuing READ DMA, but do not start BMIDE yet.
    ioports.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    ioports.write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xC8);

    assert!(!ide.borrow().controller.primary_irq_pending());

    // Assert SRST on the *secondary* channel. This must not clear the primary channel's pending
    // DMA request.
    ioports.write(SECONDARY_PORTS.ctrl_base, 1, 0x04);
    ioports.write(SECONDARY_PORTS.ctrl_base, 1, 0x00);

    // Start primary BMIDE and tick; the original request should still complete.
    ioports.write(bm_base, 1, 0x09);
    ide.borrow_mut().tick(&mut mem);

    let bm_st_primary = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(bm_st_primary & 0x07, 0x04);
    assert!(ide.borrow().controller.primary_irq_pending());
    assert!(!ide.borrow().controller.secondary_irq_pending());

    let mut out = vec![0u8; SECTOR_SIZE];
    mem.read_physical(dma_buf, &mut out);
    assert_eq!(out, sector0);

    let _ = ioports.read(PRIMARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.primary_irq_pending());
}

#[test]
fn ata_pio_write_sector_via_byte_data_port_writes_roundtrip() {
    let capacity = 8 * SECTOR_SIZE as u64;
    let disk = RawDisk::create(MemBackend::new(), capacity).unwrap();

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut()
        .controller
        .attach_primary_master_ata(AtaDrive::new(Box::new(disk)).unwrap());
    ide.borrow_mut().config_mut().set_command(0x0001); // IO decode

    let mut io = IoPortBus::new();
    register_piix3_ide_ports(&mut io, ide.clone());

    let lba = 2u8;

    // WRITE SECTORS for LBA 2, 1 sector.
    io.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0); // master + LBA
    io.write(PRIMARY_PORTS.cmd_base + 2, 1, 1); // count
    io.write(PRIMARY_PORTS.cmd_base + 3, 1, u32::from(lba)); // lba0
    io.write(PRIMARY_PORTS.cmd_base + 4, 1, 0); // lba1
    io.write(PRIMARY_PORTS.cmd_base + 5, 1, 0); // lba2
    io.write(PRIMARY_PORTS.cmd_base + 7, 1, 0x30); // WRITE SECTORS

    let mut pattern = [0u8; SECTOR_SIZE];
    for (i, b) in pattern.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(3).wrapping_add(7);
    }

    // Transfer the sector via 8-bit data port writes.
    for b in pattern {
        io.write(PRIMARY_PORTS.cmd_base, 1, u32::from(b));
    }

    // READ SECTORS for LBA 2, 1 sector.
    io.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    io.write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    io.write(PRIMARY_PORTS.cmd_base + 3, 1, u32::from(lba));
    io.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    io.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    io.write(PRIMARY_PORTS.cmd_base + 7, 1, 0x20); // READ SECTORS

    let mut out = [0u8; SECTOR_SIZE];
    for b in &mut out {
        *b = io.read(PRIMARY_PORTS.cmd_base, 1) as u8;
    }

    let mut expected = [0u8; SECTOR_SIZE];
    for (i, b) in expected.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(3).wrapping_add(7);
    }
    assert_eq!(out, expected);
}

#[test]
fn ata_pio_write_sector_via_dword_data_port_writes_roundtrip() {
    let capacity = 8 * SECTOR_SIZE as u64;
    let disk = RawDisk::create(MemBackend::new(), capacity).unwrap();

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut()
        .controller
        .attach_primary_master_ata(AtaDrive::new(Box::new(disk)).unwrap());
    ide.borrow_mut().config_mut().set_command(0x0001); // IO decode

    let mut io = IoPortBus::new();
    register_piix3_ide_ports(&mut io, ide.clone());

    let lba = 3u8;

    // WRITE SECTORS for LBA 3, 1 sector.
    io.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0); // master + LBA
    io.write(PRIMARY_PORTS.cmd_base + 2, 1, 1); // count
    io.write(PRIMARY_PORTS.cmd_base + 3, 1, u32::from(lba)); // lba0
    io.write(PRIMARY_PORTS.cmd_base + 4, 1, 0); // lba1
    io.write(PRIMARY_PORTS.cmd_base + 5, 1, 0); // lba2
    io.write(PRIMARY_PORTS.cmd_base + 7, 1, 0x30); // WRITE SECTORS

    let mut pattern = [0u8; SECTOR_SIZE];
    for (i, b) in pattern.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(5).wrapping_add(11);
    }

    for i in 0..(SECTOR_SIZE / 4) {
        let d = u32::from_le_bytes([
            pattern[i * 4],
            pattern[i * 4 + 1],
            pattern[i * 4 + 2],
            pattern[i * 4 + 3],
        ]);
        io.write(PRIMARY_PORTS.cmd_base, 4, d);
    }

    // READ SECTORS for LBA 3, 1 sector.
    io.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    io.write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    io.write(PRIMARY_PORTS.cmd_base + 3, 1, u32::from(lba));
    io.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    io.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    io.write(PRIMARY_PORTS.cmd_base + 7, 1, 0x20); // READ SECTORS

    let mut out = [0u8; SECTOR_SIZE];
    for i in 0..(SECTOR_SIZE / 4) {
        let d = io.read(PRIMARY_PORTS.cmd_base, 4);
        out[i * 4..i * 4 + 4].copy_from_slice(&d.to_le_bytes());
    }

    let mut expected = [0u8; SECTOR_SIZE];
    for (i, b) in expected.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(5).wrapping_add(11);
    }
    assert_eq!(out, expected);
}

#[test]
fn ata_pio_read_out_of_bounds_raises_irq_and_sets_err() {
    let capacity = SECTOR_SIZE as u64; // 1 sector
    let disk = RawDisk::create(MemBackend::new(), capacity).unwrap();

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut()
        .controller
        .attach_primary_master_ata(AtaDrive::new(Box::new(disk)).unwrap());
    ide.borrow_mut().config_mut().set_command(0x0001); // IO decode

    let mut io = IoPortBus::new();
    register_piix3_ide_ports(&mut io, ide.clone());

    // Issue READ SECTORS for LBA 10, 1 sector (out of bounds).
    io.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0); // master + LBA
    io.write(PRIMARY_PORTS.cmd_base + 2, 1, 1); // count
    io.write(PRIMARY_PORTS.cmd_base + 3, 1, 10); // lba0
    io.write(PRIMARY_PORTS.cmd_base + 4, 1, 0); // lba1
    io.write(PRIMARY_PORTS.cmd_base + 5, 1, 0); // lba2
    io.write(PRIMARY_PORTS.cmd_base + 7, 1, 0x20); // READ SECTORS

    assert!(ide.borrow().controller.primary_irq_pending());

    // Alt-status should reflect error without clearing the IRQ.
    let st = io.read(PRIMARY_PORTS.ctrl_base, 1) as u8;
    assert_eq!(st & 0x80, 0, "BSY should be clear");
    assert_eq!(st & 0x08, 0, "DRQ should be clear (no data phase)");
    assert_ne!(st & 0x01, 0, "ERR should be set");
    assert_eq!(io.read(PRIMARY_PORTS.cmd_base + 1, 1) as u8, 0x04);

    // Reading STATUS acknowledges and clears the pending IRQ.
    let _ = io.read(PRIMARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.primary_irq_pending());
}

#[test]
fn ata_lba48_oversized_pio_read_is_rejected_without_entering_data_phase() {
    let sectors = (MAX_IDE_DATA_BUFFER_BYTES / SECTOR_SIZE) as u32 + 1;
    // The largest possible LBA48 transfer is 65536 sectors (sector_count=0). If the cap ever grows
    // beyond that, there's nothing for the IDE layer to reject here.
    if sectors > 65536 {
        return;
    }

    let capacity = u64::from(sectors) * SECTOR_SIZE as u64;
    // Use a lightweight backend so the test doesn't allocate a ~16MiB in-memory disk image.
    let disk = ZeroDisk { capacity };

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
    io.write(PRIMARY_PORTS.cmd_base + 2, 1, sectors >> 8);
    io.write(PRIMARY_PORTS.cmd_base + 2, 1, sectors & 0xFF);

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
    io.write(PRIMARY_PORTS.cmd_base + 2, 1, sectors >> 8);
    io.write(PRIMARY_PORTS.cmd_base + 2, 1, sectors & 0xFF);

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
fn ata_lba48_oversized_dma_read_is_rejected_without_starting_dma() {
    let sectors = (MAX_IDE_DATA_BUFFER_BYTES / SECTOR_SIZE) as u32 + 1;
    // The largest possible LBA48 transfer is 65536 sectors (sector_count=0). If the cap ever grows
    // beyond that, there's nothing for the IDE layer to reject here.
    if sectors > 65536 {
        return;
    }

    let capacity = u64::from(sectors) * SECTOR_SIZE as u64;
    let disk = ZeroDisk { capacity };

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

    // PRD entry: one 512-byte segment, end-of-table (we should never reach DMA execution).
    mem.write_u32(prd_addr, dma_buf as u32);
    mem.write_u16(prd_addr + 4, SECTOR_SIZE as u16);
    mem.write_u16(prd_addr + 6, 0x8000);
    ioports.write(bm_base + 4, 4, prd_addr as u32);

    // Seed destination buffer. If DMA incorrectly runs, we'd observe it changing.
    mem.write_physical(dma_buf, &vec![0xFFu8; SECTOR_SIZE]);

    // Start bus master (direction = to memory).
    ioports.write(bm_base, 1, 0x09);

    // Issue READ DMA EXT for an oversized transfer.
    ioports.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, sectors >> 8);
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, sectors & 0xFF);
    for reg in 3..=5 {
        ioports.write(PRIMARY_PORTS.cmd_base + reg, 1, 0);
        ioports.write(PRIMARY_PORTS.cmd_base + reg, 1, 0);
    }
    ioports.write(PRIMARY_PORTS.cmd_base + 7, 1, 0x25); // READ DMA EXT

    assert!(
        ide.borrow().controller.primary_irq_pending(),
        "oversized DMA command should abort and raise an IRQ immediately"
    );

    // DMA engine should not have progressed.
    let bm_st = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(bm_st & 0x07, 0);

    // Even if we tick, there should be no DMA request to service.
    ide.borrow_mut().tick(&mut mem);
    let bm_st = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(bm_st & 0x07, 0);

    let status = ioports.read(PRIMARY_PORTS.cmd_base + 7, 1) as u8;
    assert_eq!(status & 0x80, 0, "BSY should be clear");
    assert_eq!(status & 0x08, 0, "DRQ should be clear (no DMA transfer)");
    assert_ne!(status & 0x01, 0, "ERR should be set");
    assert_eq!(ioports.read(PRIMARY_PORTS.cmd_base + 1, 1) as u8, 0x04);

    let mut out = vec![0u8; SECTOR_SIZE];
    mem.read_physical(dma_buf, &mut out);
    assert!(out.iter().all(|&b| b == 0xFF));
}

#[test]
fn ata_lba48_sector_count_zero_is_rejected_without_starting_dma() {
    // Sector count=0 encodes 65536 sectors for 48-bit transfers. Ensure this does not get treated
    // as a 0-length transfer, and instead trips the oversized-transfer guard when our max buffer is
    // smaller than 32MiB.
    if (MAX_IDE_DATA_BUFFER_BYTES / SECTOR_SIZE) >= 65536 {
        return;
    }

    let capacity = 65536u64 * SECTOR_SIZE as u64;
    let disk = ZeroDisk { capacity };

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

    // PRD entry: one 512-byte segment, end-of-table (we should never reach DMA execution).
    mem.write_u32(prd_addr, dma_buf as u32);
    mem.write_u16(prd_addr + 4, SECTOR_SIZE as u16);
    mem.write_u16(prd_addr + 6, 0x8000);
    ioports.write(bm_base + 4, 4, prd_addr as u32);

    // Seed destination buffer. If DMA incorrectly runs, we'd observe it changing.
    mem.write_physical(dma_buf, &vec![0xFFu8; SECTOR_SIZE]);

    // Start bus master (direction = to memory).
    ioports.write(bm_base, 1, 0x09);

    // Issue READ DMA EXT with sector_count=0 (high byte=0, low byte=0).
    ioports.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, 0);
    for reg in 3..=5 {
        ioports.write(PRIMARY_PORTS.cmd_base + reg, 1, 0);
        ioports.write(PRIMARY_PORTS.cmd_base + reg, 1, 0);
    }
    ioports.write(PRIMARY_PORTS.cmd_base + 7, 1, 0x25); // READ DMA EXT

    assert!(
        ide.borrow().controller.primary_irq_pending(),
        "sector_count=0 DMA EXT command should abort and raise an IRQ immediately"
    );

    // DMA engine should not have progressed.
    let bm_st = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(bm_st & 0x07, 0);

    // Even if we tick, there should be no DMA request to service.
    ide.borrow_mut().tick(&mut mem);
    let bm_st = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(bm_st & 0x07, 0);

    let status = ioports.read(PRIMARY_PORTS.cmd_base + 7, 1) as u8;
    assert_eq!(status & 0x80, 0, "BSY should be clear");
    assert_eq!(status & 0x08, 0, "DRQ should be clear (no DMA transfer)");
    assert_ne!(status & 0x01, 0, "ERR should be set");
    assert_eq!(ioports.read(PRIMARY_PORTS.cmd_base + 1, 1) as u8, 0x04);

    let mut out = vec![0u8; SECTOR_SIZE];
    mem.read_physical(dma_buf, &mut out);
    assert!(out.iter().all(|&b| b == 0xFF));
}

#[test]
fn ata_lba48_oversized_dma_write_is_rejected_without_allocating_buffer() {
    let sectors = (MAX_IDE_DATA_BUFFER_BYTES / SECTOR_SIZE) as u32 + 1;
    if sectors > 65536 {
        return;
    }

    let shared = Arc::new(Mutex::new(RecordingDisk::new(1)));
    let disk = SharedRecordingDisk(shared.clone());

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

    // PRD entry: one 512-byte segment, end-of-table (we should never reach DMA execution).
    mem.write_u32(prd_addr, dma_buf as u32);
    mem.write_u16(prd_addr + 4, SECTOR_SIZE as u16);
    mem.write_u16(prd_addr + 6, 0x8000);
    ioports.write(bm_base + 4, 4, prd_addr as u32);

    // Fill the DMA source buffer.
    mem.write_physical(dma_buf, &vec![0xA5u8; SECTOR_SIZE]);

    // Start bus master (direction = from memory).
    ioports.write(bm_base, 1, 0x01);

    // Issue WRITE DMA EXT for an oversized transfer.
    ioports.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, sectors >> 8);
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, sectors & 0xFF);
    for reg in 3..=5 {
        ioports.write(PRIMARY_PORTS.cmd_base + reg, 1, 0);
        ioports.write(PRIMARY_PORTS.cmd_base + reg, 1, 0);
    }
    ioports.write(PRIMARY_PORTS.cmd_base + 7, 1, 0x35); // WRITE DMA EXT

    assert!(
        ide.borrow().controller.primary_irq_pending(),
        "oversized DMA command should abort and raise an IRQ immediately"
    );

    let bm_st = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(bm_st & 0x07, 0);

    ide.borrow_mut().tick(&mut mem);
    let bm_st = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(bm_st & 0x07, 0);

    let status = ioports.read(PRIMARY_PORTS.cmd_base + 7, 1) as u8;
    assert_eq!(status & 0x80, 0, "BSY should be clear");
    assert_eq!(status & 0x08, 0, "DRQ should be clear (no DMA transfer)");
    assert_ne!(status & 0x01, 0, "ERR should be set");
    assert_eq!(ioports.read(PRIMARY_PORTS.cmd_base + 1, 1) as u8, 0x04);

    let inner = shared.lock().unwrap();
    assert_eq!(inner.last_write_lba, None);
    assert_eq!(inner.last_write_len, 0);
}

#[test]
fn ata_pio_write_sectors_ext_uses_lba48_hob_bytes() {
    // Use a "high" LBA value that requires HOB bytes (LBA3..LBA5) to be carried through for
    // LBA48 commands. This ensures we don't accidentally truncate to 28-bit addressing.
    let lba: u64 = 0x01_00_00_00;
    let shared = Arc::new(Mutex::new(RecordingDisk::new(lba + 16)));
    let disk = SharedRecordingDisk(shared.clone());

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut()
        .controller
        .attach_primary_master_ata(AtaDrive::new(Box::new(disk)).unwrap());
    ide.borrow_mut().config_mut().set_command(0x0001); // IO decode

    let mut ioports = IoPortBus::new();
    register_piix3_ide_ports(&mut ioports, ide.clone());

    // Select master, LBA mode.
    ioports.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);

    // Sector count (48-bit): high byte then low byte => 1 sector.
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, 0x00);
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, 0x01);

    // LBA bytes (48-bit): write high bytes first, then low bytes.
    // LBA = 0x01_00_00_00 => HOB LBA0=0x01, others 0.
    ioports.write(PRIMARY_PORTS.cmd_base + 3, 1, 0x01);
    ioports.write(PRIMARY_PORTS.cmd_base + 3, 1, 0x00);
    ioports.write(PRIMARY_PORTS.cmd_base + 4, 1, 0x00);
    ioports.write(PRIMARY_PORTS.cmd_base + 4, 1, 0x00);
    ioports.write(PRIMARY_PORTS.cmd_base + 5, 1, 0x00);
    ioports.write(PRIMARY_PORTS.cmd_base + 5, 1, 0x00);

    // Command: WRITE SECTORS EXT.
    ioports.write(PRIMARY_PORTS.cmd_base + 7, 1, 0x34);

    // Transfer one sector (PIO OUT).
    for i in 0..256u16 {
        ioports.write(PRIMARY_PORTS.cmd_base, 2, i as u32);
    }

    let inner = shared.lock().unwrap();
    assert_eq!(inner.last_write_lba, Some(lba));
    assert_eq!(inner.last_write_len, SECTOR_SIZE);
}

#[test]
fn ata_taskfile_hob_reads_expose_high_bytes_for_lba48_registers() {
    let capacity = 8 * SECTOR_SIZE as u64;
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

    // Program 48-bit task file values by writing the high byte first, then the low byte.
    // Sector Count: high=0x12, low=0x34.
    io.write(PRIMARY_PORTS.cmd_base + 2, 1, 0x12);
    io.write(PRIMARY_PORTS.cmd_base + 2, 1, 0x34);
    // LBA0..2: high bytes 0x56/0x9A/0xDE, low bytes 0x78/0xBC/0xF0.
    io.write(PRIMARY_PORTS.cmd_base + 3, 1, 0x56);
    io.write(PRIMARY_PORTS.cmd_base + 3, 1, 0x78);
    io.write(PRIMARY_PORTS.cmd_base + 4, 1, 0x9A);
    io.write(PRIMARY_PORTS.cmd_base + 4, 1, 0xBC);
    io.write(PRIMARY_PORTS.cmd_base + 5, 1, 0xDE);
    io.write(PRIMARY_PORTS.cmd_base + 5, 1, 0xF0);

    // With HOB clear, reads should return the low bytes.
    assert_eq!(io.read(PRIMARY_PORTS.cmd_base + 2, 1) as u8, 0x34);
    assert_eq!(io.read(PRIMARY_PORTS.cmd_base + 3, 1) as u8, 0x78);
    assert_eq!(io.read(PRIMARY_PORTS.cmd_base + 4, 1) as u8, 0xBC);
    assert_eq!(io.read(PRIMARY_PORTS.cmd_base + 5, 1) as u8, 0xF0);

    // Set HOB and verify reads now return the high bytes.
    io.write(PRIMARY_PORTS.ctrl_base, 1, 0x80);
    assert_eq!(io.read(PRIMARY_PORTS.cmd_base + 2, 1) as u8, 0x12);
    assert_eq!(io.read(PRIMARY_PORTS.cmd_base + 3, 1) as u8, 0x56);
    assert_eq!(io.read(PRIMARY_PORTS.cmd_base + 4, 1) as u8, 0x9A);
    assert_eq!(io.read(PRIMARY_PORTS.cmd_base + 5, 1) as u8, 0xDE);

    // Clearing HOB should restore low-byte reads.
    io.write(PRIMARY_PORTS.ctrl_base, 1, 0x00);
    assert_eq!(io.read(PRIMARY_PORTS.cmd_base + 2, 1) as u8, 0x34);
    assert_eq!(io.read(PRIMARY_PORTS.cmd_base + 4, 1) as u8, 0xBC);
}

#[test]
fn ata_dma_write_out_of_bounds_sets_bus_master_and_ata_error() {
    let capacity = SECTOR_SIZE as u64; // 1 sector
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
    let dma_buf = 0x3000u64;
    let bm_base = ide.borrow().bus_master_base();

    // Fill one sector worth of DMA source data.
    let pattern: Vec<u8> = (0..SECTOR_SIZE as u32).map(|v| (v & 0xff) as u8).collect();
    mem.write_physical(dma_buf, &pattern);

    // PRD entry: one 512-byte segment, EOT.
    mem.write_u32(prd_addr, dma_buf as u32);
    mem.write_u16(prd_addr + 4, SECTOR_SIZE as u16);
    mem.write_u16(prd_addr + 6, 0x8000);
    ioports.write(bm_base + 4, 4, prd_addr as u32);

    // WRITE DMA for out-of-bounds LBA 10.
    ioports.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    ioports.write(PRIMARY_PORTS.cmd_base + 3, 1, 10);
    ioports.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xCA); // WRITE DMA

    // Start bus master (direction = from memory).
    ioports.write(bm_base, 1, 0x01);
    ide.borrow_mut().tick(&mut mem);

    let bm_status = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(
        bm_status & 0x06,
        0x06,
        "BMIDE status should have IRQ+ERR set"
    );

    // ATA status should reflect an error completion.
    let st = ioports.read(PRIMARY_PORTS.ctrl_base, 1) as u8;
    assert_eq!(st & 0x80, 0, "BSY should be clear");
    assert_eq!(st & 0x08, 0, "DRQ should be clear");
    assert_ne!(st & 0x01, 0, "ERR should be set");
    assert_eq!(ioports.read(PRIMARY_PORTS.cmd_base + 1, 1) as u8, 0x04);

    assert!(ide.borrow().controller.primary_irq_pending());
}

#[test]
fn ata_dma_read_out_of_bounds_aborts_without_setting_bus_master_error() {
    let capacity = SECTOR_SIZE as u64; // 1 sector
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
    let dma_buf = 0x3000u64;
    let bm_base = ide.borrow().bus_master_base();

    // Seed destination buffer so we can detect unintended DMA.
    mem.write_physical(dma_buf, &vec![0xFFu8; SECTOR_SIZE]);

    // PRD entry: one 512-byte segment, EOT.
    mem.write_u32(prd_addr, dma_buf as u32);
    mem.write_u16(prd_addr + 4, SECTOR_SIZE as u16);
    mem.write_u16(prd_addr + 6, 0x8000);
    ioports.write(bm_base + 4, 4, prd_addr as u32);

    // Start bus master (direction = to memory). Even though BMIDE is started, the command below
    // will fail before a DMA request is queued.
    ioports.write(bm_base, 1, 0x09);

    // READ DMA for out-of-bounds LBA 10.
    ioports.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    ioports.write(PRIMARY_PORTS.cmd_base + 3, 1, 10);
    ioports.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xC8); // READ DMA

    assert!(
        ide.borrow().controller.primary_irq_pending(),
        "out-of-bounds DMA read should abort and raise an IRQ immediately"
    );

    // Because the read failed before a DMA request could be queued, BMIDE should not report an
    // error/interrupt.
    let bm_status = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(bm_status & 0x07, 0);

    // Even if we tick, there is no pending DMA request to service.
    ide.borrow_mut().tick(&mut mem);
    let bm_status = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(bm_status & 0x07, 0);

    // ATA status should reflect an error completion.
    let st = ioports.read(PRIMARY_PORTS.ctrl_base, 1) as u8;
    assert_eq!(st & 0x80, 0, "BSY should be clear");
    assert_eq!(st & 0x08, 0, "DRQ should be clear");
    assert_ne!(st & 0x01, 0, "ERR should be set");
    assert_eq!(ioports.read(PRIMARY_PORTS.cmd_base + 1, 1) as u8, 0x04);

    let mut out = vec![0u8; SECTOR_SIZE];
    mem.read_physical(dma_buf, &mut out);
    assert!(out.iter().all(|&b| b == 0xFF));

    // STATUS acknowledges and clears the IRQ.
    let _ = ioports.read(PRIMARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.primary_irq_pending());
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
fn ata_dma_succeeds_when_bus_master_is_started_before_command_is_issued() {
    // Some guests may start the BMIDE engine before issuing the ATA command. The controller should
    // still perform DMA once the command queues a request.
    let capacity = 4 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();

    let mut sector0 = vec![0u8; SECTOR_SIZE];
    for (i, b) in sector0.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(3).wrapping_add(0x11);
    }
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
    let dma_buf = 0x3000u64;
    let bm_base = ide.borrow().bus_master_base();

    // PRD table: one entry, end-of-table, 512 bytes.
    mem.write_u32(prd_addr, dma_buf as u32);
    mem.write_u16(prd_addr + 4, SECTOR_SIZE as u16);
    mem.write_u16(prd_addr + 6, 0x8000);
    ioports.write(bm_base + 4, 4, prd_addr as u32);

    // Start bus master engine *before* issuing the command (direction = to memory).
    ioports.write(bm_base, 1, 0x09);

    // Issue READ DMA (LBA 0, 1 sector).
    ioports.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    ioports.write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xC8); // READ DMA

    ide.borrow_mut().tick(&mut mem);

    let bm_status = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(bm_status & 0x07, 0x04);
    assert!(ide.borrow().controller.primary_irq_pending());

    let mut out = vec![0u8; SECTOR_SIZE];
    mem.read_physical(dma_buf, &mut out);
    assert_eq!(out, sector0);

    let _ = ioports.read(PRIMARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.primary_irq_pending());
}

#[test]
fn ata_dma_can_run_back_to_back_without_restarting_bus_master() {
    // Some guests keep the BMIDE start bit set and issue multiple DMA commands back-to-back. Ensure
    // the controller can complete successive requests without requiring the guest to re-write the
    // BMIDE command register.
    let capacity = 2 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();

    let mut sector0 = vec![0u8; SECTOR_SIZE];
    let mut sector1 = vec![0u8; SECTOR_SIZE];
    for (i, b) in sector0.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(3).wrapping_add(0x11);
    }
    for (i, b) in sector1.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(5).wrapping_add(0x22);
    }
    disk.write_sectors(0, &sector0).unwrap();
    disk.write_sectors(1, &sector1).unwrap();

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

    // PRD entry: one 512-byte segment, end-of-table.
    mem.write_u32(prd_addr, dma_buf as u32);
    mem.write_u16(prd_addr + 4, SECTOR_SIZE as u16);
    mem.write_u16(prd_addr + 6, 0x8000);
    ioports.write(bm_base + 4, 4, prd_addr as u32);

    // Start BMIDE engine once (direction = to memory).
    ioports.write(bm_base, 1, 0x09);

    // READ DMA for LBA 0, 1 sector.
    mem.write_physical(dma_buf, &vec![0xFFu8; SECTOR_SIZE]);
    ioports.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    ioports.write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xC8);

    ide.borrow_mut().tick(&mut mem);
    assert!(ide.borrow().controller.primary_irq_pending());

    let mut out = vec![0u8; SECTOR_SIZE];
    mem.read_physical(dma_buf, &mut out);
    assert_eq!(out, sector0);

    // ACK IDE IRQ latch and clear BMIDE IRQ status.
    let _ = ioports.read(PRIMARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.primary_irq_pending());
    ioports.write(bm_base + 2, 1, 0x04);
    let bm_st = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(bm_st & 0x07, 0);

    // BMIDE start bit should still be set.
    let bm_cmd = ioports.read(bm_base, 1) as u8;
    assert_eq!(bm_cmd & 0x09, 0x09);

    // READ DMA for LBA 1, 1 sector without re-writing BMIDE command.
    mem.write_physical(dma_buf, &vec![0xFFu8; SECTOR_SIZE]);
    ioports.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    ioports.write(PRIMARY_PORTS.cmd_base + 3, 1, 1);
    ioports.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xC8);

    ide.borrow_mut().tick(&mut mem);
    assert!(ide.borrow().controller.primary_irq_pending());

    mem.read_physical(dma_buf, &mut out);
    assert_eq!(out, sector1);

    let _ = ioports.read(PRIMARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.primary_irq_pending());
}

#[test]
fn ata_dma_does_not_run_until_bus_master_is_started() {
    // Guests may issue an ATA DMA command before starting the BMIDE engine. The controller should
    // not move any data or raise an interrupt until the BMIDE start bit is set.
    let capacity = 4 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut sector0 = vec![0u8; SECTOR_SIZE];
    for (i, b) in sector0.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(7).wrapping_add(0x21);
    }
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

    // PRD entry: one 512-byte segment, end-of-table.
    mem.write_u32(prd_addr, dma_buf as u32);
    mem.write_u16(prd_addr + 4, SECTOR_SIZE as u16);
    mem.write_u16(prd_addr + 6, 0x8000);
    ioports.write(bm_base + 4, 4, prd_addr as u32);

    // Seed destination buffer to prove that no DMA happens before the BMIDE start bit.
    mem.write_physical(dma_buf, &vec![0xFFu8; SECTOR_SIZE]);

    // READ DMA for LBA 0, 1 sector. Do NOT start BMIDE yet.
    ioports.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    ioports.write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xC8);

    ide.borrow_mut().tick(&mut mem);
    assert!(!ide.borrow().controller.primary_irq_pending());
    let bm_st = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(bm_st & 0x07, 0x00);

    let mut out = vec![0u8; SECTOR_SIZE];
    mem.read_physical(dma_buf, &mut out);
    assert!(out.iter().all(|&b| b == 0xFF));

    // Start the BMIDE engine and tick again; DMA should now complete.
    ioports.write(bm_base, 1, 0x09);
    ide.borrow_mut().tick(&mut mem);

    let bm_st = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(bm_st & 0x07, 0x04);
    assert!(ide.borrow().controller.primary_irq_pending());

    mem.read_physical(dma_buf, &mut out);
    assert_eq!(out, sector0);

    let _ = ioports.read(PRIMARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.primary_irq_pending());
}

#[test]
fn ata_bus_master_dma_scatter_gather_with_odd_prd_lengths() {
    // Exercise a PRD scatter/gather list where the first segment length is odd. Real guests should
    // normally use word-aligned lengths, but the controller should still behave sensibly for
    // unusual PRDs.
    let capacity = 4 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();

    let mut sector0 = vec![0u8; SECTOR_SIZE];
    for (i, b) in sector0.iter_mut().enumerate() {
        *b = (i as u8).wrapping_add(1);
    }
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
    let buf0 = 0x2000u64;
    let buf1 = 0x3000u64;

    let len0: u16 = 17;
    let len1: u16 = SECTOR_SIZE as u16 - len0;

    // Two-entry PRD table that splits one sector across an odd-sized prefix and the remainder.
    mem.write_u32(prd_addr, buf0 as u32);
    mem.write_u16(prd_addr + 4, len0);
    mem.write_u16(prd_addr + 6, 0x0000);
    mem.write_u32(prd_addr + 8, buf1 as u32);
    mem.write_u16(prd_addr + 12, len1);
    mem.write_u16(prd_addr + 14, 0x8000); // EOT

    let bm_base = ide.borrow().bus_master_base();
    ioports.write(bm_base + 4, 4, prd_addr as u32);

    // Issue READ DMA (LBA 0, 1 sector).
    ioports.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    ioports.write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xC8); // READ DMA

    ioports.write(bm_base, 1, 0x09); // start + read (device -> memory)
    ide.borrow_mut().tick(&mut mem);

    let mut first = vec![0u8; len0 as usize];
    mem.read_physical(buf0, &mut first);
    assert_eq!(first, sector0[..len0 as usize].to_vec());

    let mut rest = vec![0u8; len1 as usize];
    mem.read_physical(buf1, &mut rest);
    assert_eq!(rest, sector0[len0 as usize..].to_vec());
}

#[test]
fn ata_irq_is_latched_while_nien_is_set_and_surfaces_after_reenable() {
    // Guests often mask IDE interrupts (Device Control nIEN=1) while polling for completion. The
    // interrupt condition should be latched so re-enabling interrupts can still surface it if the
    // guest never acknowledged the completion by reading the Status register.
    let capacity = 4 * SECTOR_SIZE as u64;
    let disk = RawDisk::create(MemBackend::new(), capacity).unwrap();

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut()
        .controller
        .attach_primary_master_ata(AtaDrive::new(Box::new(disk)).unwrap());
    ide.borrow_mut().config_mut().set_command(0x0001); // IO decode

    let mut ioports = IoPortBus::new();
    register_piix3_ide_ports(&mut ioports, ide.clone());

    // Mask interrupts.
    ioports.write(PRIMARY_PORTS.ctrl_base, 1, 0x02);

    // Issue a non-data command that completes immediately.
    ioports.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    ioports.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xE7); // FLUSH CACHE

    // Interrupt should be pending internally but masked by nIEN.
    assert!(!ide.borrow().controller.primary_irq_pending());

    // Re-enable interrupts; the pending completion should now surface.
    ioports.write(PRIMARY_PORTS.ctrl_base, 1, 0x00);
    assert!(ide.borrow().controller.primary_irq_pending());

    // Reading Status acknowledges and clears the pending interrupt.
    let _ = ioports.read(PRIMARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.primary_irq_pending());
}

#[test]
fn ata_bus_master_dma_ext_read_write_256_sectors_roundtrip() {
    // Exercise 48-bit sector count handling for DMA EXT commands by transferring 256 sectors
    // (requires writing a non-zero high byte to the sector count register).
    let sectors: u16 = 256;
    let byte_len = sectors as usize * SECTOR_SIZE;
    let capacity = byte_len as u64;
    let disk = RawDisk::create(MemBackend::new(), capacity).unwrap();

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut()
        .controller
        .attach_primary_master_ata(AtaDrive::new(Box::new(disk)).unwrap());
    ide.borrow_mut().config_mut().set_command(0x0005); // IO decode + Bus Master

    let mut ioports = IoPortBus::new();
    register_piix3_ide_ports(&mut ioports, ide.clone());

    // Use a larger guest memory region so we can keep separate write/read buffers.
    let mut mem = Bus::new(0x50_000);

    let prd_addr = 0x1000u64;
    let write_buf = 0x2000u64;
    let read_buf = write_buf + byte_len as u64;

    // Fill the write buffer with a deterministic pattern.
    let mut pattern = vec![0u8; byte_len];
    for (i, b) in pattern.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(5).wrapping_add(7);
    }
    mem.write_physical(write_buf, &pattern);

    let bm_base = ide.borrow().bus_master_base();

    // PRD table: 2 entries, 64KiB each (byte_count=0 encodes 64KiB).
    assert_eq!(byte_len, 2 * 65536);
    mem.write_u32(prd_addr, write_buf as u32);
    mem.write_u16(prd_addr + 4, 0);
    mem.write_u16(prd_addr + 6, 0x0000);
    mem.write_u32(prd_addr + 8, (write_buf + 0x10000) as u32);
    mem.write_u16(prd_addr + 12, 0);
    mem.write_u16(prd_addr + 14, 0x8000);
    ioports.write(bm_base + 4, 4, prd_addr as u32);

    // WRITE DMA EXT (LBA 0, 256 sectors).
    ioports.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    // sector count (48-bit): high then low.
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, (sectors >> 8) as u32);
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, (sectors & 0xFF) as u32);
    // LBA = 0 (48-bit): high then low for each byte register.
    for reg in 3..=5 {
        ioports.write(PRIMARY_PORTS.cmd_base + reg, 1, 0);
        ioports.write(PRIMARY_PORTS.cmd_base + reg, 1, 0);
    }
    ioports.write(PRIMARY_PORTS.cmd_base + 7, 1, 0x35); // WRITE DMA EXT

    // Start bus master (direction = from memory).
    ioports.write(bm_base, 1, 0x01);
    ide.borrow_mut().tick(&mut mem);

    // PRD table for the read-back buffer.
    mem.write_u32(prd_addr, read_buf as u32);
    mem.write_u16(prd_addr + 4, 0);
    mem.write_u16(prd_addr + 6, 0x0000);
    mem.write_u32(prd_addr + 8, (read_buf + 0x10000) as u32);
    mem.write_u16(prd_addr + 12, 0);
    mem.write_u16(prd_addr + 14, 0x8000);
    ioports.write(bm_base + 4, 4, prd_addr as u32);

    // READ DMA EXT (LBA 0, 256 sectors).
    ioports.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, (sectors >> 8) as u32);
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, (sectors & 0xFF) as u32);
    for reg in 3..=5 {
        ioports.write(PRIMARY_PORTS.cmd_base + reg, 1, 0);
        ioports.write(PRIMARY_PORTS.cmd_base + reg, 1, 0);
    }
    ioports.write(PRIMARY_PORTS.cmd_base + 7, 1, 0x25); // READ DMA EXT

    // Start bus master (direction = to memory).
    ioports.write(bm_base, 1, 0x09);
    ide.borrow_mut().tick(&mut mem);

    let mut out = vec![0u8; byte_len];
    mem.read_physical(read_buf, &mut out);
    assert_eq!(out, pattern);
}

#[test]
fn ata_dma_ext_read_uses_lba48_hob_bytes() {
    // Pick an LBA that requires the HOB LBA1 byte (bits 32..39) so we catch truncation bugs.
    let lba: u64 = 0x01_00_00_00_00;

    let expected: Vec<u8> = (0..SECTOR_SIZE as u32)
        .map(|i| (i as u8).wrapping_mul(3).wrapping_add(0x11))
        .collect();

    let read_called = Arc::new(AtomicBool::new(false));

    #[derive(Debug)]
    struct AssertingReadDisk {
        capacity_bytes: u64,
        expected_offset: u64,
        expected: Vec<u8>,
        called: Arc<AtomicBool>,
    }

    impl VirtualDisk for AssertingReadDisk {
        fn capacity_bytes(&self) -> u64 {
            self.capacity_bytes
        }

        fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<()> {
            assert_eq!(offset, self.expected_offset);
            assert_eq!(buf.len(), self.expected.len());
            buf.copy_from_slice(&self.expected);
            self.called.store(true, Ordering::SeqCst);
            Ok(())
        }

        fn write_at(&mut self, _offset: u64, _buf: &[u8]) -> Result<()> {
            Ok(())
        }

        fn flush(&mut self) -> Result<()> {
            Ok(())
        }
    }

    let disk = AssertingReadDisk {
        capacity_bytes: (lba + 16) * SECTOR_SIZE as u64,
        expected_offset: lba * SECTOR_SIZE as u64,
        expected: expected.clone(),
        called: read_called.clone(),
    };

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

    // PRD entry: one 512-byte segment, end-of-table.
    mem.write_u32(prd_addr, dma_buf as u32);
    mem.write_u16(prd_addr + 4, SECTOR_SIZE as u16);
    mem.write_u16(prd_addr + 6, 0x8000);
    ioports.write(bm_base + 4, 4, prd_addr as u32);

    // Seed destination buffer.
    mem.write_physical(dma_buf, &vec![0xFFu8; SECTOR_SIZE]);

    // READ DMA EXT (LBA48) for 1 sector.
    ioports.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, 0x00); // sector count high
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, 0x01); // sector count low

    let b0 = (lba & 0xFF) as u8;
    let b1 = ((lba >> 8) & 0xFF) as u8;
    let b2 = ((lba >> 16) & 0xFF) as u8;
    let b3 = ((lba >> 24) & 0xFF) as u8;
    let b4 = ((lba >> 32) & 0xFF) as u8;
    let b5 = ((lba >> 40) & 0xFF) as u8;

    ioports.write(PRIMARY_PORTS.cmd_base + 3, 1, b3 as u32);
    ioports.write(PRIMARY_PORTS.cmd_base + 3, 1, b0 as u32);
    ioports.write(PRIMARY_PORTS.cmd_base + 4, 1, b4 as u32);
    ioports.write(PRIMARY_PORTS.cmd_base + 4, 1, b1 as u32);
    ioports.write(PRIMARY_PORTS.cmd_base + 5, 1, b5 as u32);
    ioports.write(PRIMARY_PORTS.cmd_base + 5, 1, b2 as u32);

    ioports.write(PRIMARY_PORTS.cmd_base + 7, 1, 0x25); // READ DMA EXT

    // Start bus master (direction = to memory).
    ioports.write(bm_base, 1, 0x09);
    ide.borrow_mut().tick(&mut mem);

    assert!(read_called.load(Ordering::SeqCst));

    let bm_st = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(bm_st & 0x07, 0x04);
    assert!(ide.borrow().controller.primary_irq_pending());

    let mut out = vec![0u8; SECTOR_SIZE];
    mem.read_physical(dma_buf, &mut out);
    assert_eq!(out, expected);

    let _ = ioports.read(PRIMARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.primary_irq_pending());
}

#[test]
fn ata_dma_ext_write_uses_lba48_hob_bytes() {
    // Use an LBA that requires HOB LBA1 so we catch truncation to 28-bit parameters.
    let lba: u64 = 0x01_00_00_00_00;
    let shared = Arc::new(Mutex::new(RecordingDisk::new(lba + 16)));
    let disk = SharedRecordingDisk(shared.clone());

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

    // PRD entry: one 512-byte segment, end-of-table.
    mem.write_u32(prd_addr, dma_buf as u32);
    mem.write_u16(prd_addr + 4, SECTOR_SIZE as u16);
    mem.write_u16(prd_addr + 6, 0x8000);
    ioports.write(bm_base + 4, 4, prd_addr as u32);

    // Fill one sector of source data.
    let payload: Vec<u8> = (0..SECTOR_SIZE as u32)
        .map(|i| (i as u8).wrapping_mul(7).wrapping_add(3))
        .collect();
    mem.write_physical(dma_buf, &payload);

    // WRITE DMA EXT (LBA48) for 1 sector.
    ioports.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, 0x00); // sector count high
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, 0x01); // sector count low

    let b0 = (lba & 0xFF) as u8;
    let b1 = ((lba >> 8) & 0xFF) as u8;
    let b2 = ((lba >> 16) & 0xFF) as u8;
    let b3 = ((lba >> 24) & 0xFF) as u8;
    let b4 = ((lba >> 32) & 0xFF) as u8;
    let b5 = ((lba >> 40) & 0xFF) as u8;

    ioports.write(PRIMARY_PORTS.cmd_base + 3, 1, b3 as u32);
    ioports.write(PRIMARY_PORTS.cmd_base + 3, 1, b0 as u32);
    ioports.write(PRIMARY_PORTS.cmd_base + 4, 1, b4 as u32);
    ioports.write(PRIMARY_PORTS.cmd_base + 4, 1, b1 as u32);
    ioports.write(PRIMARY_PORTS.cmd_base + 5, 1, b5 as u32);
    ioports.write(PRIMARY_PORTS.cmd_base + 5, 1, b2 as u32);

    ioports.write(PRIMARY_PORTS.cmd_base + 7, 1, 0x35); // WRITE DMA EXT

    // Start bus master (direction = from memory).
    ioports.write(bm_base, 1, 0x01);
    ide.borrow_mut().tick(&mut mem);

    let bm_st = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(bm_st & 0x07, 0x04);
    assert!(ide.borrow().controller.primary_irq_pending());

    let inner = shared.lock().unwrap();
    assert_eq!(inner.last_write_lba, Some(lba));
    assert_eq!(inner.last_write_len, SECTOR_SIZE);

    let _ = ioports.read(PRIMARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.primary_irq_pending());
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
fn pci_io_decode_disabled_floats_ports_and_ignores_writes() {
    let capacity = 4 * SECTOR_SIZE as u64;
    let disk = RawDisk::create(MemBackend::new(), capacity).unwrap();

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut()
        .controller
        .attach_primary_master_ata(AtaDrive::new(Box::new(disk)).unwrap());

    // Keep PCI command bits cleared: IO decode disabled.
    ide.borrow_mut().config_mut().set_command(0x0000);

    let mut ioports = IoPortBus::new();
    register_piix3_ide_ports(&mut ioports, ide.clone());

    let bm_base = ide.borrow().bus_master_base();

    // Reads should float high when IO decode is disabled.
    assert_eq!(ioports.read(PRIMARY_PORTS.cmd_base + 7, 1) as u8, 0xFF);
    assert_eq!(ioports.read(bm_base + 4, 4), 0xFFFF_FFFF);

    // Writes must be ignored.
    ioports.write(bm_base + 4, 4, 0x1234_5678);
    ioports.write(bm_base, 1, 0x09);
    ioports.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xC8);

    // Re-enable IO decode so we can observe state.
    ide.borrow_mut().config_mut().set_command(0x0001);

    // Bus master registers should remain at reset defaults.
    assert_eq!(ioports.read(bm_base + 4, 4), 0);
    assert_eq!(ioports.read(bm_base, 1) as u8 & 0x09, 0);
    let bm_st = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(bm_st & 0x07, 0);

    // Channel should remain idle (DRDY, no BSY/DRQ).
    let st = ioports.read(PRIMARY_PORTS.cmd_base + 7, 1) as u8;
    assert_eq!(st & 0x80, 0);
    assert_eq!(st & 0x08, 0);
    assert_ne!(st & 0x40, 0);
}

#[test]
fn bus_master_status_advertises_dma_capability_for_attached_ata_drive() {
    let capacity = 4 * SECTOR_SIZE as u64;
    let disk = RawDisk::create(MemBackend::new(), capacity).unwrap();

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut()
        .controller
        .attach_primary_master_ata(AtaDrive::new(Box::new(disk)).unwrap());

    // Bus Master IDE registers decode when PCI I/O space is enabled; bus mastering is not required
    // to observe the DMA capability bits.
    ide.borrow_mut().config_mut().set_command(0x0001);

    let mut ioports = IoPortBus::new();
    register_piix3_ide_ports(&mut ioports, ide.clone());

    let bm_base = ide.borrow().bus_master_base();
    let st = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(st & 0x07, 0, "BMIDE status bits should be clear at reset");
    assert_ne!(
        st & 0x20,
        0,
        "BMIDE DMA capability bit for master should be set"
    );
    assert_eq!(
        st & 0x40,
        0,
        "BMIDE DMA capability bit for slave should be clear"
    );

    // Controller reset should clear runtime status bits but preserve capability bits derived from
    // attached devices.
    ide.borrow_mut().controller.reset();
    let st = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(st & 0x07, 0);
    assert_ne!(st & 0x20, 0);
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
    assert_eq!(
        st & 0x07,
        0x04,
        "interrupt should be set, active/error clear"
    );

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
fn bmide_stopping_engine_does_not_clear_irq_status_bit() {
    let capacity = 4 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut sector0 = vec![0u8; SECTOR_SIZE];
    sector0[..4].copy_from_slice(b"PING");
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
    let dma_buf = 0x2000u64;

    // PRD table: one entry, end-of-table, 512 bytes.
    mem.write_u32(prd_addr, dma_buf as u32);
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
    assert_eq!(st & 0x07, 0x04);

    // Stopping the engine should clear only ACTIVE, not the IRQ status bit.
    ioports.write(bm_base, 1, 0x00);
    let cmd = ioports.read(bm_base, 1) as u8;
    assert_eq!(cmd & 0x09, 0);

    let st = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(
        st & 0x07,
        0x04,
        "BMIDE IRQ bit should persist until cleared via RW1C"
    );

    // Clear interrupt (RW1C).
    ioports.write(bm_base + 2, 1, 0x04);
    let st = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(st & 0x07, 0);

    // Acknowledge IDE IRQ latch.
    let _ = ioports.read(PRIMARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.primary_irq_pending());
}

#[test]
fn bmide_rw1c_clear_is_channel_specific_and_does_not_acknowledge_ide_irqs() {
    let capacity = 4 * SECTOR_SIZE as u64;

    let mut disk0 = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut sector0 = vec![0u8; SECTOR_SIZE];
    for (i, b) in sector0.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(3).wrapping_add(1);
    }
    disk0.write_sectors(0, &sector0).unwrap();

    let mut disk1 = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut sector1 = vec![0u8; SECTOR_SIZE];
    for (i, b) in sector1.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(5).wrapping_add(7);
    }
    disk1.write_sectors(0, &sector1).unwrap();

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    {
        let mut dev = ide.borrow_mut();
        dev.controller
            .attach_primary_master_ata(AtaDrive::new(Box::new(disk0)).unwrap());
        dev.controller
            .attach_secondary_master_ata(AtaDrive::new(Box::new(disk1)).unwrap());
        dev.config_mut().set_command(0x0005); // IO decode + Bus Master
    }

    let mut ioports = IoPortBus::new();
    register_piix3_ide_ports(&mut ioports, ide.clone());

    let mut mem = Bus::new(0x40_000);
    let bm_base = ide.borrow().bus_master_base();

    // Program PRDs for each channel.
    let prd_primary = 0x1000u64;
    let buf_primary = 0x3000u64;
    mem.write_u32(prd_primary, buf_primary as u32);
    mem.write_u16(prd_primary + 4, SECTOR_SIZE as u16);
    mem.write_u16(prd_primary + 6, 0x8000);
    ioports.write(bm_base + 4, 4, prd_primary as u32);

    let prd_secondary = 0x1010u64;
    let buf_secondary = 0x4000u64;
    mem.write_u32(prd_secondary, buf_secondary as u32);
    mem.write_u16(prd_secondary + 4, SECTOR_SIZE as u16);
    mem.write_u16(prd_secondary + 6, 0x8000);
    ioports.write(bm_base + 8 + 4, 4, prd_secondary as u32);

    // Issue READ DMA on both channels.
    ioports.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    ioports.write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xC8);

    ioports.write(SECONDARY_PORTS.cmd_base + 6, 1, 0xE0);
    ioports.write(SECONDARY_PORTS.cmd_base + 2, 1, 1);
    ioports.write(SECONDARY_PORTS.cmd_base + 3, 1, 0);
    ioports.write(SECONDARY_PORTS.cmd_base + 4, 1, 0);
    ioports.write(SECONDARY_PORTS.cmd_base + 5, 1, 0);
    ioports.write(SECONDARY_PORTS.cmd_base + 7, 1, 0xC8);

    // Start both BMIDE engines and tick once to complete both transfers.
    ioports.write(bm_base, 1, 0x09);
    ioports.write(bm_base + 8, 1, 0x09);
    ide.borrow_mut().tick(&mut mem);

    assert!(ide.borrow().controller.primary_irq_pending());
    assert!(ide.borrow().controller.secondary_irq_pending());

    let bm_st_primary = ioports.read(bm_base + 2, 1) as u8;
    let bm_st_secondary = ioports.read(bm_base + 8 + 2, 1) as u8;
    assert_eq!(bm_st_primary & 0x07, 0x04);
    assert_eq!(bm_st_secondary & 0x07, 0x04);

    // Clear primary BMIDE IRQ bit via RW1C; secondary should remain unchanged.
    ioports.write(bm_base + 2, 1, 0x04);
    let bm_st_primary = ioports.read(bm_base + 2, 1) as u8;
    let bm_st_secondary = ioports.read(bm_base + 8 + 2, 1) as u8;
    assert_eq!(bm_st_primary & 0x07, 0);
    assert_eq!(
        bm_st_secondary & 0x07,
        0x04,
        "clearing primary BMIDE status must not clear secondary BMIDE status"
    );

    // RW1C must not acknowledge the IDE IRQ latches.
    assert!(ide.borrow().controller.primary_irq_pending());
    assert!(ide.borrow().controller.secondary_irq_pending());

    // Clear secondary BMIDE IRQ bit as well.
    ioports.write(bm_base + 8 + 2, 1, 0x04);
    let bm_st_secondary = ioports.read(bm_base + 8 + 2, 1) as u8;
    assert_eq!(bm_st_secondary & 0x07, 0);

    // IDE IRQ latches remain pending until STATUS is read per channel.
    let _ = ioports.read(PRIMARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.primary_irq_pending());
    assert!(ide.borrow().controller.secondary_irq_pending());

    let _ = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.secondary_irq_pending());
}

#[test]
fn bmide_rw1c_does_not_acknowledge_ide_irq_latch() {
    let capacity = 4 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut sector0 = vec![0u8; SECTOR_SIZE];
    sector0[..4].copy_from_slice(b"PING");
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
    let dma_buf = 0x2000u64;

    // PRD table: one entry, end-of-table, 512 bytes.
    mem.write_u32(prd_addr, dma_buf as u32);
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

    assert!(
        ide.borrow().controller.primary_irq_pending(),
        "DMA completion should raise an IDE IRQ"
    );
    let bm_st = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(bm_st & 0x07, 0x04);

    // Clear BMIDE IRQ bit (RW1C); this must not acknowledge the IDE IRQ latch.
    ioports.write(bm_base + 2, 1, 0x04);
    let bm_st = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(bm_st & 0x07, 0x00);
    assert!(
        ide.borrow().controller.primary_irq_pending(),
        "clearing BMIDE status must not clear the IDE IRQ latch"
    );

    // Reading STATUS acknowledges and clears the IDE IRQ latch.
    let _ = ioports.read(PRIMARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.primary_irq_pending());
}

#[test]
fn ata_alt_status_does_not_clear_irq_latch_on_dma_completion() {
    let capacity = 4 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut sector0 = vec![0u8; SECTOR_SIZE];
    for (i, b) in sector0.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(3).wrapping_add(1);
    }
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

    // PRD entry: one 512-byte segment, end-of-table.
    mem.write_u32(prd_addr, dma_buf as u32);
    mem.write_u16(prd_addr + 4, SECTOR_SIZE as u16);
    mem.write_u16(prd_addr + 6, 0x8000);
    ioports.write(bm_base + 4, 4, prd_addr as u32);

    // READ DMA for LBA 0, 1 sector.
    ioports.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    ioports.write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xC8);

    ioports.write(bm_base, 1, 0x09);
    ide.borrow_mut().tick(&mut mem);

    assert!(ide.borrow().controller.primary_irq_pending());

    // ALT_STATUS reads must not acknowledge/clear the IDE IRQ latch.
    let st = ioports.read(PRIMARY_PORTS.ctrl_base, 1) as u8;
    assert_eq!(st & 0x80, 0, "BSY should be clear");
    assert_eq!(st & 0x08, 0, "DRQ should be clear");
    assert_ne!(st & 0x40, 0, "DRDY should be set");
    assert_eq!(st & 0x01, 0, "ERR should be clear");
    assert!(
        ide.borrow().controller.primary_irq_pending(),
        "ALT_STATUS read cleared IRQ latch"
    );

    // STATUS reads still acknowledge/clear the latch.
    let _ = ioports.read(PRIMARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.primary_irq_pending());
}

#[test]
fn status_read_acknowledges_irq_only_on_the_target_channel() {
    // Ensure STATUS reads clear only the IRQ latch for that channel, not globally.
    let capacity = 4 * SECTOR_SIZE as u64;

    let mut disk0 = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut sector0 = vec![0u8; SECTOR_SIZE];
    for (i, b) in sector0.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(3).wrapping_add(1);
    }
    disk0.write_sectors(0, &sector0).unwrap();

    let mut disk1 = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut sector1 = vec![0u8; SECTOR_SIZE];
    for (i, b) in sector1.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(5).wrapping_add(7);
    }
    disk1.write_sectors(0, &sector1).unwrap();

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    {
        let mut dev = ide.borrow_mut();
        dev.controller
            .attach_primary_master_ata(AtaDrive::new(Box::new(disk0)).unwrap());
        dev.controller
            .attach_secondary_master_ata(AtaDrive::new(Box::new(disk1)).unwrap());
        dev.config_mut().set_command(0x0005); // IO decode + Bus Master
    }

    let mut ioports = IoPortBus::new();
    register_piix3_ide_ports(&mut ioports, ide.clone());

    let mut mem = Bus::new(0x40_000);
    let bm_base = ide.borrow().bus_master_base();

    // Program PRDs for each channel.
    let prd_primary = 0x1000u64;
    let buf_primary = 0x3000u64;
    mem.write_u32(prd_primary, buf_primary as u32);
    mem.write_u16(prd_primary + 4, SECTOR_SIZE as u16);
    mem.write_u16(prd_primary + 6, 0x8000);
    ioports.write(bm_base + 4, 4, prd_primary as u32);

    let prd_secondary = 0x1010u64;
    let buf_secondary = 0x4000u64;
    mem.write_u32(prd_secondary, buf_secondary as u32);
    mem.write_u16(prd_secondary + 4, SECTOR_SIZE as u16);
    mem.write_u16(prd_secondary + 6, 0x8000);
    ioports.write(bm_base + 8 + 4, 4, prd_secondary as u32);

    // Issue READ DMA on both channels.
    ioports.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    ioports.write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xC8);

    ioports.write(SECONDARY_PORTS.cmd_base + 6, 1, 0xE0);
    ioports.write(SECONDARY_PORTS.cmd_base + 2, 1, 1);
    ioports.write(SECONDARY_PORTS.cmd_base + 3, 1, 0);
    ioports.write(SECONDARY_PORTS.cmd_base + 4, 1, 0);
    ioports.write(SECONDARY_PORTS.cmd_base + 5, 1, 0);
    ioports.write(SECONDARY_PORTS.cmd_base + 7, 1, 0xC8);

    // Start both BMIDE engines and tick once to complete both transfers.
    ioports.write(bm_base, 1, 0x09);
    ioports.write(bm_base + 8, 1, 0x09);
    ide.borrow_mut().tick(&mut mem);

    assert!(ide.borrow().controller.primary_irq_pending());
    assert!(ide.borrow().controller.secondary_irq_pending());

    // Acknowledge only the primary channel IRQ.
    let _ = ioports.read(PRIMARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.primary_irq_pending());
    assert!(
        ide.borrow().controller.secondary_irq_pending(),
        "primary STATUS read must not clear secondary IRQ latch"
    );

    // Now acknowledge the secondary IRQ.
    let _ = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.secondary_irq_pending());

    // Data should have landed in both DMA buffers.
    let mut out0 = vec![0u8; SECTOR_SIZE];
    let mut out1 = vec![0u8; SECTOR_SIZE];
    mem.read_physical(buf_primary, &mut out0);
    mem.read_physical(buf_secondary, &mut out1);
    assert_eq!(out0, sector0);
    assert_eq!(out1, sector1);
}

#[test]
fn ata_dma_is_gated_by_pci_bus_master_enable() {
    // Disk with recognizable first sector.
    let capacity = 4 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut sector0 = vec![0u8; SECTOR_SIZE];
    for (i, b) in sector0.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(7).wrapping_add(3);
    }
    disk.write_sectors(0, &sector0).unwrap();

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut()
        .controller
        .attach_primary_master_ata(AtaDrive::new(Box::new(disk)).unwrap());

    // Enable IO decode so we can program the bus master and issue commands, but keep PCI Bus Master
    // Enable cleared so `Piix3IdePciDevice::tick()` should not perform DMA.
    ide.borrow_mut().config_mut().set_command(0x0001);

    let mut ioports = IoPortBus::new();
    register_piix3_ide_ports(&mut ioports, ide.clone());

    let mut mem = Bus::new(0x20_000);
    let bm_base = ide.borrow().bus_master_base();

    let prd_addr = 0x1000u64;
    let dma_buf = 0x3000u64;

    // PRD entry: one 512-byte segment, end-of-table.
    mem.write_u32(prd_addr, dma_buf as u32);
    mem.write_u16(prd_addr + 4, SECTOR_SIZE as u16);
    mem.write_u16(prd_addr + 6, 0x8000);
    ioports.write(bm_base + 4, 4, prd_addr as u32);

    // Seed destination buffer; without bus master enable we should see no writes.
    mem.write_physical(dma_buf, &vec![0xFFu8; SECTOR_SIZE]);

    // Issue READ DMA for LBA 0, 1 sector.
    ioports.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    ioports.write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xC8);

    // Start bus master (direction = to memory).
    ioports.write(bm_base, 1, 0x09);

    // DMA must not run until PCI Bus Master Enable is set.
    ide.borrow_mut().tick(&mut mem);
    assert!(!ide.borrow().controller.primary_irq_pending());
    let bm_st = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(bm_st & 0x07, 0x00);

    let mut out = vec![0u8; SECTOR_SIZE];
    mem.read_physical(dma_buf, &mut out);
    assert!(out.iter().all(|&b| b == 0xFF));

    // Enable bus mastering and tick again; DMA should now complete.
    ide.borrow_mut().config_mut().set_command(0x0005);
    ide.borrow_mut().tick(&mut mem);

    let bm_st = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(bm_st & 0x07, 0x04);
    assert!(ide.borrow().controller.primary_irq_pending());

    mem.read_physical(dma_buf, &mut out);
    assert_eq!(out, sector0);

    let _ = ioports.read(PRIMARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.primary_irq_pending());
}

#[test]
fn ata_dma_can_complete_with_pci_io_decode_disabled() {
    // PCI I/O space decode (command bit 0) gates guest access to IDE/BMIDE registers, but bus
    // mastering (command bit 2) should still allow DMA to run once the device is programmed.
    let capacity = 4 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut sector0 = vec![0u8; SECTOR_SIZE];
    for (i, b) in sector0.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(5).wrapping_add(0x2D);
    }
    disk.write_sectors(0, &sector0).unwrap();

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut()
        .controller
        .attach_primary_master_ata(AtaDrive::new(Box::new(disk)).unwrap());

    // Enable IO decode so we can program the bus master and issue commands.
    ide.borrow_mut().config_mut().set_command(0x0005);

    let mut ioports = IoPortBus::new();
    register_piix3_ide_ports(&mut ioports, ide.clone());

    let mut mem = Bus::new(0x20_000);
    let bm_base = ide.borrow().bus_master_base();

    let prd_addr = 0x1000u64;
    let dma_buf = 0x3000u64;

    // PRD entry: one 512-byte segment, end-of-table.
    mem.write_u32(prd_addr, dma_buf as u32);
    mem.write_u16(prd_addr + 4, SECTOR_SIZE as u16);
    mem.write_u16(prd_addr + 6, 0x8000);
    ioports.write(bm_base + 4, 4, prd_addr as u32);

    mem.write_physical(dma_buf, &vec![0xFFu8; SECTOR_SIZE]);

    // Issue READ DMA for LBA 0, 1 sector.
    ioports.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    ioports.write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xC8);
    ioports.write(bm_base, 1, 0x09);

    // Disable IO decode but keep bus mastering enabled.
    ide.borrow_mut().config_mut().set_command(0x0004);

    // DMA should still complete because bus mastering remains enabled.
    ide.borrow_mut().tick(&mut mem);

    assert!(ide.borrow().controller.primary_irq_pending());

    let mut out = vec![0u8; SECTOR_SIZE];
    mem.read_physical(dma_buf, &mut out);
    assert_eq!(out, sector0);

    // Re-enable IO decode so we can observe BMIDE status and acknowledge the IRQ.
    ide.borrow_mut().config_mut().set_command(0x0001);

    let bm_st = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(bm_st & 0x07, 0x04);

    let _ = ioports.read(PRIMARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.primary_irq_pending());
}

#[test]
fn ata_dma_success_irq_is_latched_while_nien_is_set_and_surfaces_after_reenable() {
    let capacity = 4 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut sector0 = vec![0u8; SECTOR_SIZE];
    for (i, b) in sector0.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(3).wrapping_add(1);
    }
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

    mem.write_u32(prd_addr, dma_buf as u32);
    mem.write_u16(prd_addr + 4, SECTOR_SIZE as u16);
    mem.write_u16(prd_addr + 6, 0x8000);
    ioports.write(bm_base + 4, 4, prd_addr as u32);

    // Mask interrupts (nIEN=1).
    ioports.write(PRIMARY_PORTS.ctrl_base, 1, 0x02);

    // READ DMA for LBA 0, 1 sector.
    ioports.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    ioports.write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xC8);

    ioports.write(bm_base, 1, 0x09);
    ide.borrow_mut().tick(&mut mem);

    let st = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(st & 0x07, 0x04);
    assert!(!ide.borrow().controller.primary_irq_pending());

    let mut out = vec![0u8; SECTOR_SIZE];
    mem.read_physical(dma_buf, &mut out);
    assert_eq!(out, sector0);

    // Re-enable interrupts; the latched completion should now surface.
    ioports.write(PRIMARY_PORTS.ctrl_base, 1, 0x00);
    assert!(ide.borrow().controller.primary_irq_pending());

    let _ = ioports.read(PRIMARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.primary_irq_pending());
}

#[test]
fn ata_dma_success_irq_can_be_acknowledged_while_nien_is_set() {
    // If the guest services the interrupt status while nIEN=1, it should not see a spurious IRQ
    // once interrupts are re-enabled.
    let capacity = 4 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut sector0 = vec![0u8; SECTOR_SIZE];
    for (i, b) in sector0.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(5).wrapping_add(0x33);
    }
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

    mem.write_u32(prd_addr, dma_buf as u32);
    mem.write_u16(prd_addr + 4, SECTOR_SIZE as u16);
    mem.write_u16(prd_addr + 6, 0x8000);
    ioports.write(bm_base + 4, 4, prd_addr as u32);

    // Mask interrupts (nIEN=1).
    ioports.write(PRIMARY_PORTS.ctrl_base, 1, 0x02);

    // READ DMA for LBA 0, 1 sector.
    ioports.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    ioports.write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xC8);

    ioports.write(bm_base, 1, 0x09);
    ide.borrow_mut().tick(&mut mem);

    let bm_st = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(bm_st & 0x07, 0x04);
    assert!(!ide.borrow().controller.primary_irq_pending());

    let mut out = vec![0u8; SECTOR_SIZE];
    mem.read_physical(dma_buf, &mut out);
    assert_eq!(out, sector0);

    // Acknowledge the completion while interrupts are still masked.
    let _ = ioports.read(PRIMARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.primary_irq_pending());

    // Re-enable interrupts; because the completion was already acknowledged, it should not
    // surface.
    ioports.write(PRIMARY_PORTS.ctrl_base, 1, 0x00);
    assert!(!ide.borrow().controller.primary_irq_pending());

    // BMIDE status remains set until explicitly cleared.
    let bm_st = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(bm_st & 0x07, 0x04);
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

#[test]
fn ata_sector_count_zero_encodes_256_sectors_for_dma_read() {
    // ATA 28-bit sector_count=0 encodes 256 sectors.
    let sectors: u64 = 256;
    let capacity = sectors * SECTOR_SIZE as u64;

    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut pattern = vec![0u8; capacity as usize];
    for (i, b) in pattern.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(3).wrapping_add(0x17);
    }
    disk.write_sectors(0, &pattern).unwrap();

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut()
        .controller
        .attach_primary_master_ata(AtaDrive::new(Box::new(disk)).unwrap());
    ide.borrow_mut().config_mut().set_command(0x0005); // IO decode + Bus Master

    let mut ioports = IoPortBus::new();
    register_piix3_ide_ports(&mut ioports, ide.clone());

    // Need enough space for the PRD table plus a 128KiB destination buffer.
    let mut mem = Bus::new(0x40_000);
    let bm_base = ide.borrow().bus_master_base();

    let prd_addr = 0x1000u64;
    let dma_buf = 0x4000u64;

    // PRD table: four 32KiB segments, end-of-table on the last entry.
    for i in 0..4u64 {
        let entry_addr = prd_addr + i * 8;
        mem.write_u32(entry_addr, (dma_buf + i * 0x8000) as u32);
        mem.write_u16(entry_addr + 4, 0x8000);
        mem.write_u16(entry_addr + 6, if i == 3 { 0x8000 } else { 0 });
    }
    ioports.write(bm_base + 4, 4, prd_addr as u32);

    // Seed destination buffer; without DMA we'd see all 0xFF.
    mem.write_physical(dma_buf, &vec![0xFFu8; capacity as usize]);

    // READ DMA (LBA 0, sector_count=0 => 256 sectors).
    ioports.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xC8);

    ioports.write(bm_base, 1, 0x09);
    ide.borrow_mut().tick(&mut mem);

    let bm_st = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(bm_st & 0x07, 0x04);
    assert!(ide.borrow().controller.primary_irq_pending());

    let mut out = vec![0u8; capacity as usize];
    mem.read_physical(dma_buf, &mut out);
    assert_eq!(out, pattern);

    let _ = ioports.read(PRIMARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.primary_irq_pending());
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
fn atapi_identify_device_aborts_with_signature() {
    // Many OSes probe for ATAPI by issuing ATA IDENTIFY DEVICE (0xEC) and then checking
    // LBA Mid/High for the ATAPI signature (0x14/0xEB) after ABRT.
    let iso = MemIso::new(1);

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut().controller.attach_secondary_master_atapi(
        aero_devices_storage::atapi::AtapiCdrom::new(Some(Box::new(iso))),
    );
    ide.borrow_mut().config_mut().set_command(0x0001); // IO decode

    let mut ioports = IoPortBus::new();
    register_piix3_ide_ports(&mut ioports, ide.clone());

    // Select master on secondary channel.
    ioports.write(SECONDARY_PORTS.cmd_base + 6, 1, 0xA0);

    // IDENTIFY DEVICE (expected to abort on ATAPI).
    ioports.write(SECONDARY_PORTS.cmd_base + 7, 1, 0xEC);

    // Completion should raise an interrupt.
    assert!(ide.borrow().controller.secondary_irq_pending());

    let status = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1) as u8;
    assert_eq!(status & 0x80, 0, "BSY should be clear");
    assert_eq!(status & 0x08, 0, "DRQ should be clear");
    assert_ne!(status & 0x01, 0, "ERR should be set");
    assert_eq!(ioports.read(SECONDARY_PORTS.cmd_base + 1, 1) as u8, 0x04);

    let lba1 = ioports.read(SECONDARY_PORTS.cmd_base + 4, 1) as u8;
    let lba2 = ioports.read(SECONDARY_PORTS.cmd_base + 5, 1) as u8;
    assert_eq!((lba1, lba2), (0x14, 0xEB));
}

#[test]
fn atapi_identify_packet_device_returns_identify_data() {
    // Many OSes also issue IDENTIFY PACKET DEVICE (0xA1) to ATAPI drives to fetch identify words.
    // This should succeed even without media present.
    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut()
        .controller
        .attach_secondary_master_atapi(aero_devices_storage::atapi::AtapiCdrom::new(None));
    ide.borrow_mut().config_mut().set_command(0x0001); // IO decode

    let mut io = IoPortBus::new();
    register_piix3_ide_ports(&mut io, ide.clone());

    // Select master on secondary channel.
    io.write(SECONDARY_PORTS.cmd_base + 6, 1, 0xA0);

    // IDENTIFY PACKET DEVICE.
    io.write(SECONDARY_PORTS.cmd_base + 7, 1, 0xA1);
    assert!(ide.borrow().controller.secondary_irq_pending());

    // Read 256 words.
    let mut buf = vec![0u8; SECTOR_SIZE];
    for i in 0..256 {
        let w = io.read(SECONDARY_PORTS.cmd_base, 2) as u16;
        buf[i * 2..i * 2 + 2].copy_from_slice(&w.to_le_bytes());
    }

    // Word 0: ATAPI device (0x8580) + packet size indicator (bit0=1).
    let word0 = u16::from_le_bytes([buf[0], buf[1]]);
    assert_eq!(word0, 0x8581);

    // Word 49: DMA capability bit should be set.
    let word49 = u16::from_le_bytes([buf[49 * 2], buf[49 * 2 + 1]]);
    assert_ne!(word49 & (1 << 8), 0);

    // Model string words 27..46 (40 bytes), ATA-encoded (bytes swapped within each word).
    let mut model_bytes = Vec::new();
    for chunk in buf[27 * 2..47 * 2].chunks_exact(2) {
        model_bytes.push(chunk[1]);
        model_bytes.push(chunk[0]);
    }
    let model = String::from_utf8_lossy(&model_bytes);
    assert!(
        model.contains("Aero ATAPI CD-ROM"),
        "unexpected model string: {model:?}"
    );

    // Reading STATUS acknowledges and clears the pending IRQ.
    let _ = io.read(SECONDARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.secondary_irq_pending());
}

#[test]
fn atapi_inquiry_and_read_10_pio() {
    let mut iso = MemIso::new(2);
    iso.data[2048..2053].copy_from_slice(b"WORLD");

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut().controller.attach_secondary_master_atapi(
        aero_devices_storage::atapi::AtapiCdrom::new(Some(Box::new(iso))),
    );
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
    #[derive(Debug)]
    struct ZeroIso {
        sector_count: u32,
    }

    impl IsoBackend for ZeroIso {
        fn sector_count(&self) -> u32 {
            self.sector_count
        }

        fn read_sectors(&mut self, _lba: u32, buf: &mut [u8]) -> io::Result<()> {
            if !buf.len().is_multiple_of(2048) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "unaligned buffer length",
                ));
            }
            buf.fill(0);
            Ok(())
        }
    }

    let blocks = (MAX_IDE_DATA_BUFFER_BYTES / 2048) as u32 + 1;
    let iso = ZeroIso {
        sector_count: blocks,
    };

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut().controller.attach_secondary_master_atapi(
        aero_devices_storage::atapi::AtapiCdrom::new(Some(Box::new(iso))),
    );
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
    send_atapi_packet(&mut ioports, SECONDARY_PORTS.cmd_base, 0, &read12, 2048);

    // Error completions should still raise an interrupt.
    assert!(ide.borrow().controller.secondary_irq_pending());
    // Interrupt reason: status phase.
    assert_eq!(ioports.read(SECONDARY_PORTS.cmd_base + 2, 1) as u8, 0x03);

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
fn bus_master_bar4_near_u16_max_is_aligned_and_mapped() {
    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));

    // Place BAR4 close to the end of the 16-bit I/O port space so the 16-byte window would extend
    // past 0xFFFF if we used wrapping arithmetic.
    ide.borrow_mut().config_mut().write(0x20, 4, 0x0000_fff8);
    ide.borrow_mut().config_mut().set_command(0x0001); // IO decode

    let mut ioports = IoPortBus::new();
    register_piix3_ide_ports(&mut ioports, ide.clone());

    // The BAR should be visible via the helper as well.
    assert_eq!(
        ide.borrow().bus_master_base(),
        0xFFF0,
        "BAR4 should be aligned to its 16-byte size"
    );

    // Ports at the end of the space should decode normally.
    assert_eq!(ioports.read(0xFFF0, 1) as u8, 0, "BMIDE command reg");
    assert_eq!(ioports.read(0xFFFF, 1) as u8, 0, "BMIDE PRD addr high byte");

    // But the mapping must not wrap around and claim ports at the start of the space.
    assert_eq!(ioports.read(0x0000, 1), 0xFF);
    assert_eq!(ioports.read(0x0007, 1), 0xFF);
}

#[test]
fn atapi_read_10_dma_via_bus_master() {
    let mut iso = MemIso::new(1);
    // Fill the first (and only) 2048-byte sector with a deterministic pattern so we can validate
    // the full DMA payload, not just a prefix.
    let expected: Vec<u8> = (0..2048u32)
        .map(|i| (i as u8).wrapping_mul(7).wrapping_add(3))
        .collect();
    iso.data[..2048].copy_from_slice(&expected);

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut().controller.attach_secondary_master_atapi(
        aero_devices_storage::atapi::AtapiCdrom::new(Some(Box::new(iso))),
    );
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

    // ACK the packet-phase interrupt (DRQ for PACKET data) so we can observe that the DMA
    // completion itself raises a new interrupt.
    let _ = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1);
    assert!(
        !ide.borrow().controller.secondary_irq_pending(),
        "IRQ should be clear before starting the DMA engine"
    );

    // Start the secondary bus master engine, direction=read (device -> memory).
    ioports.write(bm_base + 8, 1, 0x09);
    ide.borrow_mut().tick(&mut mem);

    let mut out = vec![0u8; 2048];
    mem.read_physical(dma_buf, &mut out);
    assert_eq!(out, expected);

    // Bus master status should indicate interrupt and no error.
    let st = ioports.read(bm_base + 8 + 2, 1) as u8;
    assert_eq!(
        st & 0x01,
        0,
        "BMIDE active bit should be clear after completion"
    );
    assert_ne!(st & 0x04, 0, "BMIDE IRQ bit should be set on completion");
    assert_eq!(st & 0x02, 0, "BMIDE ERR bit should be clear");

    // ATAPI interrupt reason: status phase.
    assert_eq!(ioports.read(SECONDARY_PORTS.cmd_base + 2, 1) as u8, 0x03);

    assert!(
        ide.borrow().controller.secondary_irq_pending(),
        "IDE IRQ line should be asserted on DMA completion"
    );

    // Acknowledge the IDE interrupt by reading STATUS (standard IDE behavior).
    let _ = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1);
    assert!(
        !ide.borrow().controller.secondary_irq_pending(),
        "reading STATUS should clear the IDE IRQ latch"
    );

    // BMIDE status IRQ bit is separate from the IDE IRQ line latch and must be cleared via RW1C.
    assert_ne!(
        ioports.read(bm_base + 8 + 2, 1) as u8 & 0x04,
        0,
        "BMIDE IRQ bit should remain set until explicitly cleared"
    );
    ioports.write(bm_base + 8 + 2, 1, 0x04);
    assert_eq!(
        ioports.read(bm_base + 8 + 2, 1) as u8 & 0x04,
        0,
        "BMIDE IRQ bit should clear via RW1C"
    );
}

#[test]
fn atapi_bus_master_dma_scatter_gather_with_odd_prd_lengths() {
    // Exercise a PRD scatter/gather list where the first segment length is odd.
    let mut iso = MemIso::new(1);
    let expected: Vec<u8> = (0..2048u32)
        .map(|i| (i as u8).wrapping_mul(7).wrapping_add(0x11))
        .collect();
    iso.data[..2048].copy_from_slice(&expected);

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut().controller.attach_secondary_master_atapi(
        aero_devices_storage::atapi::AtapiCdrom::new(Some(Box::new(iso))),
    );
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
    let _ = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1);

    let mut mem = Bus::new(0x20_000);
    let bm_base = ide.borrow().bus_master_base();

    let prd_addr = 0x1000u64;
    let buf0 = 0x4000u64;
    let buf1 = 0x5000u64;

    let len0: u16 = 17;
    let len1: u16 = 2048 - len0;

    // Two-entry PRD table that splits the 2048-byte transfer across an odd-sized prefix and the
    // remainder.
    mem.write_u32(prd_addr, buf0 as u32);
    mem.write_u16(prd_addr + 4, len0);
    mem.write_u16(prd_addr + 6, 0x0000);
    mem.write_u32(prd_addr + 8, buf1 as u32);
    mem.write_u16(prd_addr + 12, len1);
    mem.write_u16(prd_addr + 14, 0x8000); // EOT

    ioports.write(bm_base + 8 + 4, 4, prd_addr as u32);

    // Seed destination buffers.
    mem.write_physical(buf0, &vec![0xFFu8; len0 as usize]);
    mem.write_physical(buf1, &vec![0xFFu8; len1 as usize]);

    // READ(10) for LBA=0, blocks=1 with DMA enabled (FEATURES bit0).
    let mut read10 = [0u8; 12];
    read10[0] = 0x28;
    read10[2..6].copy_from_slice(&0u32.to_be_bytes());
    read10[7..9].copy_from_slice(&1u16.to_be_bytes());
    send_atapi_packet(&mut ioports, SECONDARY_PORTS.cmd_base, 0x01, &read10, 2048);

    // ACK packet-phase IRQ.
    let _ = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.secondary_irq_pending());

    // Start DMA.
    ioports.write(bm_base + 8, 1, 0x09);
    ide.borrow_mut().tick(&mut mem);

    let bm_st = ioports.read(bm_base + 8 + 2, 1) as u8;
    assert_ne!(bm_st & 0x04, 0);
    assert_eq!(bm_st & 0x02, 0, "BMIDE ERR bit should be clear");
    assert!(ide.borrow().controller.secondary_irq_pending());

    let mut out0 = vec![0u8; len0 as usize];
    let mut out1 = vec![0u8; len1 as usize];
    mem.read_physical(buf0, &mut out0);
    mem.read_physical(buf1, &mut out1);
    assert_eq!(out0, expected[..len0 as usize].to_vec());
    assert_eq!(out1, expected[len0 as usize..].to_vec());

    let _ = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.secondary_irq_pending());
}

#[test]
fn atapi_alt_status_does_not_clear_irq_latch_on_dma_completion() {
    let mut iso = MemIso::new(1);
    let expected: Vec<u8> = (0..2048u32)
        .map(|i| (i as u8).wrapping_mul(5).wrapping_add(1))
        .collect();
    iso.data[..2048].copy_from_slice(&expected);

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut().controller.attach_secondary_master_atapi(
        aero_devices_storage::atapi::AtapiCdrom::new(Some(Box::new(iso))),
    );
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
    ioports.write(bm_base + 8 + 4, 4, prd_addr as u32);

    // READ(10) for LBA=0, blocks=1 with DMA enabled (FEATURES bit0).
    let mut read10 = [0u8; 12];
    read10[0] = 0x28;
    read10[2..6].copy_from_slice(&0u32.to_be_bytes());
    read10[7..9].copy_from_slice(&1u16.to_be_bytes());
    send_atapi_packet(&mut ioports, SECONDARY_PORTS.cmd_base, 0x01, &read10, 2048);

    // ACK packet-phase interrupt.
    let _ = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.secondary_irq_pending());

    ioports.write(bm_base + 8, 1, 0x09);
    ide.borrow_mut().tick(&mut mem);

    assert!(ide.borrow().controller.secondary_irq_pending());

    // ALT_STATUS must not clear the IRQ latch.
    let st = ioports.read(SECONDARY_PORTS.ctrl_base, 1) as u8;
    assert_eq!(st & 0x80, 0, "BSY should be clear");
    assert_eq!(st & 0x08, 0, "DRQ should be clear");
    assert_ne!(st & 0x40, 0, "DRDY should be set");
    assert_eq!(st & 0x01, 0, "ERR should be clear");
    assert!(
        ide.borrow().controller.secondary_irq_pending(),
        "ALT_STATUS read cleared IRQ latch"
    );

    // STATUS acknowledges and clears the IRQ.
    let _ = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.secondary_irq_pending());
}

#[test]
fn atapi_dma_is_gated_by_pci_bus_master_enable() {
    let mut iso = MemIso::new(1);
    let expected: Vec<u8> = (0..2048u32)
        .map(|i| (i as u8).wrapping_mul(9).wrapping_add(3))
        .collect();
    iso.data[..2048].copy_from_slice(&expected);

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut().controller.attach_secondary_master_atapi(
        aero_devices_storage::atapi::AtapiCdrom::new(Some(Box::new(iso))),
    );

    // Enable IO decode so we can issue commands and program BMIDE, but keep PCI Bus Master Enable
    // cleared so `Piix3IdePciDevice::tick()` should not execute DMA.
    ide.borrow_mut().config_mut().set_command(0x0001);

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
    ioports.write(bm_base + 8 + 4, 4, prd_addr as u32);

    // Seed destination buffer to prove DMA doesn't run before bus mastering is enabled.
    mem.write_physical(dma_buf, &vec![0xFFu8; 2048]);

    // READ(10) for LBA=0, blocks=1 with DMA enabled (FEATURES bit0).
    let mut read10 = [0u8; 12];
    read10[0] = 0x28;
    read10[2..6].copy_from_slice(&0u32.to_be_bytes());
    read10[7..9].copy_from_slice(&1u16.to_be_bytes());
    send_atapi_packet(&mut ioports, SECONDARY_PORTS.cmd_base, 0x01, &read10, 2048);

    // ACK packet-phase IRQ so any later IRQ must be the DMA completion.
    let _ = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.secondary_irq_pending());

    // Start the secondary bus master engine, direction=read (device -> memory).
    ioports.write(bm_base + 8, 1, 0x09);

    // DMA must not run until PCI Bus Master Enable is set.
    ide.borrow_mut().tick(&mut mem);
    assert!(!ide.borrow().controller.secondary_irq_pending());
    let bm_st = ioports.read(bm_base + 8 + 2, 1) as u8;
    assert_eq!(bm_st & 0x07, 0x00);

    let mut out = vec![0u8; 2048];
    mem.read_physical(dma_buf, &mut out);
    assert!(out.iter().all(|&b| b == 0xFF));

    // Enable bus mastering and tick again; DMA should now complete.
    ide.borrow_mut().config_mut().set_command(0x0005);
    ide.borrow_mut().tick(&mut mem);

    let bm_st = ioports.read(bm_base + 8 + 2, 1) as u8;
    assert_eq!(bm_st & 0x07, 0x04);
    assert!(ide.borrow().controller.secondary_irq_pending());

    mem.read_physical(dma_buf, &mut out);
    assert_eq!(out, expected);

    let _ = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.secondary_irq_pending());
}

#[test]
fn atapi_dma_succeeds_when_bus_master_is_started_before_command_is_issued() {
    let mut iso = MemIso::new(1);
    let expected: Vec<u8> = (0..2048u32)
        .map(|i| (i as u8).wrapping_mul(5).wrapping_add(7))
        .collect();
    iso.data[..2048].copy_from_slice(&expected);

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut().controller.attach_secondary_master_atapi(
        aero_devices_storage::atapi::AtapiCdrom::new(Some(Box::new(iso))),
    );
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
    ioports.write(bm_base + 8 + 4, 4, prd_addr as u32);

    // Start the bus master engine *before* issuing the packet.
    ioports.write(bm_base + 8, 1, 0x09);

    // READ(10) for LBA=0, blocks=1 with DMA enabled (FEATURES bit0).
    let mut read10 = [0u8; 12];
    read10[0] = 0x28;
    read10[2..6].copy_from_slice(&0u32.to_be_bytes());
    read10[7..9].copy_from_slice(&1u16.to_be_bytes());
    send_atapi_packet(&mut ioports, SECONDARY_PORTS.cmd_base, 0x01, &read10, 2048);

    // ACK packet-phase IRQ.
    assert!(ide.borrow().controller.secondary_irq_pending());
    let _ = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.secondary_irq_pending());

    ide.borrow_mut().tick(&mut mem);

    let bm_st = ioports.read(bm_base + 8 + 2, 1) as u8;
    assert_eq!(bm_st & 0x07, 0x04);
    assert!(ide.borrow().controller.secondary_irq_pending());

    let mut out = vec![0u8; 2048];
    mem.read_physical(dma_buf, &mut out);
    assert_eq!(out, expected);

    let _ = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.secondary_irq_pending());
}

#[test]
fn atapi_dma_can_run_back_to_back_without_restarting_bus_master() {
    // Guests may leave the BMIDE start bit set and issue multiple ATAPI DMA commands back-to-back.
    // Ensure we can service successive requests without requiring the guest to re-write the BMIDE
    // command register.
    let mut iso = MemIso::new(2);
    let expected0: Vec<u8> = (0..2048u32)
        .map(|i| (i as u8).wrapping_mul(3).wrapping_add(0x11))
        .collect();
    let expected1: Vec<u8> = (0..2048u32)
        .map(|i| (i as u8).wrapping_mul(5).wrapping_add(0x22))
        .collect();
    iso.data[..2048].copy_from_slice(&expected0);
    iso.data[2048..4096].copy_from_slice(&expected1);

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut().controller.attach_secondary_master_atapi(
        aero_devices_storage::atapi::AtapiCdrom::new(Some(Box::new(iso))),
    );
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
    let _ = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1);

    let mut mem = Bus::new(0x20_000);
    let bm_base = ide.borrow().bus_master_base();

    let prd_addr = 0x1000u64;
    let dma_buf = 0x3000u64;

    // PRD entry: one 2048-byte segment, end-of-table.
    mem.write_u32(prd_addr, dma_buf as u32);
    mem.write_u16(prd_addr + 4, 2048);
    mem.write_u16(prd_addr + 6, 0x8000);
    ioports.write(bm_base + 8 + 4, 4, prd_addr as u32);

    // Start BMIDE engine once (direction = to memory).
    ioports.write(bm_base + 8, 1, 0x09);

    // Helper to issue READ(10) for one block.
    let mut do_read10 = |lba: u32, expected: &Vec<u8>| {
        // Seed destination buffer.
        mem.write_physical(dma_buf, &vec![0xFFu8; 2048]);

        let mut read10 = [0u8; 12];
        read10[0] = 0x28;
        read10[2..6].copy_from_slice(&lba.to_be_bytes());
        read10[7..9].copy_from_slice(&1u16.to_be_bytes());
        send_atapi_packet(&mut ioports, SECONDARY_PORTS.cmd_base, 0x01, &read10, 2048);

        // ACK packet-phase IRQ.
        assert!(ide.borrow().controller.secondary_irq_pending());
        let _ = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1);
        assert!(!ide.borrow().controller.secondary_irq_pending());

        // Complete DMA.
        ide.borrow_mut().tick(&mut mem);
        assert!(ide.borrow().controller.secondary_irq_pending());

        let mut out = vec![0u8; 2048];
        mem.read_physical(dma_buf, &mut out);
        assert_eq!(&out, expected);

        let _ = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1);
        assert!(!ide.borrow().controller.secondary_irq_pending());

        // Clear BMIDE status IRQ bit so we can observe it being set again on the next command.
        ioports.write(bm_base + 8 + 2, 1, 0x04);
        let bm_st = ioports.read(bm_base + 8 + 2, 1) as u8;
        assert_eq!(bm_st & 0x07, 0);

        // BMIDE start bit should still be set.
        assert_eq!(ioports.read(bm_base + 8, 1) as u8 & 0x09, 0x09);
    };

    do_read10(0, &expected0);
    do_read10(1, &expected1);
}

#[test]
fn atapi_dma_does_not_run_until_bus_master_is_started() {
    // The ATAPI DMA request should remain pending until the BMIDE start bit is set.
    let mut iso = MemIso::new(1);
    let expected: Vec<u8> = (0..2048u32)
        .map(|i| (i as u8).wrapping_mul(7).wrapping_add(9))
        .collect();
    iso.data[..2048].copy_from_slice(&expected);

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut().controller.attach_secondary_master_atapi(
        aero_devices_storage::atapi::AtapiCdrom::new(Some(Box::new(iso))),
    );
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
    ioports.write(bm_base + 8 + 4, 4, prd_addr as u32);

    // Seed destination buffer.
    mem.write_physical(dma_buf, &vec![0xFFu8; 2048]);

    // READ(10) for LBA=0, blocks=1 with DMA enabled (FEATURES bit0).
    let mut read10 = [0u8; 12];
    read10[0] = 0x28;
    read10[2..6].copy_from_slice(&0u32.to_be_bytes());
    read10[7..9].copy_from_slice(&1u16.to_be_bytes());
    send_atapi_packet(&mut ioports, SECONDARY_PORTS.cmd_base, 0x01, &read10, 2048);

    // ACK packet-phase IRQ.
    assert!(ide.borrow().controller.secondary_irq_pending());
    let _ = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.secondary_irq_pending());

    // Without BMIDE start, tick should not perform any DMA.
    ide.borrow_mut().tick(&mut mem);
    assert!(!ide.borrow().controller.secondary_irq_pending());
    let bm_st = ioports.read(bm_base + 8 + 2, 1) as u8;
    assert_eq!(bm_st & 0x07, 0x00);

    let mut out = vec![0u8; 2048];
    mem.read_physical(dma_buf, &mut out);
    assert!(out.iter().all(|&b| b == 0xFF));

    // Start BMIDE and tick; DMA should now complete.
    ioports.write(bm_base + 8, 1, 0x09);
    ide.borrow_mut().tick(&mut mem);

    let bm_st = ioports.read(bm_base + 8 + 2, 1) as u8;
    assert_eq!(bm_st & 0x07, 0x04);
    assert!(ide.borrow().controller.secondary_irq_pending());

    mem.read_physical(dma_buf, &mut out);
    assert_eq!(out, expected);

    let _ = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.secondary_irq_pending());
}

#[test]
fn atapi_dma_success_on_secondary_channel_requires_secondary_bus_master_start() {
    let mut iso = MemIso::new(1);
    let expected: Vec<u8> = (0..2048u32)
        .map(|i| (i as u8).wrapping_mul(9).wrapping_add(0x33))
        .collect();
    iso.data[..2048].copy_from_slice(&expected);

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut().controller.attach_secondary_master_atapi(
        aero_devices_storage::atapi::AtapiCdrom::new(Some(Box::new(iso))),
    );
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

    // Program both PRD pointers so a buggy implementation that uses the wrong channel's PRD
    // pointer will still touch the known DMA buffer.
    ioports.write(bm_base + 4, 4, prd_addr as u32);
    ioports.write(bm_base + 8 + 4, 4, prd_addr as u32);

    // Seed destination buffer so we can prove no DMA happens before the correct bus master engine
    // is started.
    mem.write_physical(dma_buf, &vec![0xFFu8; 2048]);

    // READ(10) for LBA=0, blocks=1 with DMA enabled (FEATURES bit0).
    let mut read10 = [0u8; 12];
    read10[0] = 0x28;
    read10[2..6].copy_from_slice(&0u32.to_be_bytes());
    read10[7..9].copy_from_slice(&1u16.to_be_bytes());
    send_atapi_packet(&mut ioports, SECONDARY_PORTS.cmd_base, 0x01, &read10, 2048);

    // ACK packet-phase IRQ.
    assert!(ide.borrow().controller.secondary_irq_pending());
    let _ = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.secondary_irq_pending());

    // Start the *primary* bus master engine (wrong channel) and tick. The secondary DMA request
    // must remain pending, with no memory writes and no interrupt.
    ioports.write(bm_base, 1, 0x09);
    ide.borrow_mut().tick(&mut mem);

    assert!(!ide.borrow().controller.secondary_irq_pending());
    assert!(!ide.borrow().controller.primary_irq_pending());

    let bm_st_secondary = ioports.read(bm_base + 8 + 2, 1) as u8;
    assert_eq!(bm_st_secondary & 0x07, 0);

    let mut out = vec![0u8; 2048];
    mem.read_physical(dma_buf, &mut out);
    assert!(out.iter().all(|&b| b == 0xFF));

    // Now start the correct bus master engine and tick; the transfer should complete.
    ioports.write(bm_base + 8, 1, 0x09);
    ide.borrow_mut().tick(&mut mem);

    let bm_st_secondary = ioports.read(bm_base + 8 + 2, 1) as u8;
    assert_eq!(bm_st_secondary & 0x07, 0x04);
    assert!(ide.borrow().controller.secondary_irq_pending());
    assert!(!ide.borrow().controller.primary_irq_pending());

    mem.read_physical(dma_buf, &mut out);
    assert_eq!(out, expected);

    // Acknowledge the interrupt.
    let _ = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.secondary_irq_pending());
}

#[test]
fn atapi_dma_success_on_primary_channel_requires_primary_bus_master_start() {
    let mut iso = MemIso::new(1);
    let expected: Vec<u8> = (0..2048u32)
        .map(|i| (i as u8).wrapping_mul(13).wrapping_add(0x19))
        .collect();
    iso.data[..2048].copy_from_slice(&expected);

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut().controller.attach_primary_master_atapi(
        aero_devices_storage::atapi::AtapiCdrom::new(Some(Box::new(iso))),
    );
    ide.borrow_mut().config_mut().set_command(0x0005); // IO decode + Bus Master

    let mut ioports = IoPortBus::new();
    register_piix3_ide_ports(&mut ioports, ide.clone());

    // Select master on primary channel.
    ioports.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xA0);

    // Clear initial UNIT ATTENTION: TEST UNIT READY then REQUEST SENSE.
    let tur = [0u8; 12];
    send_atapi_packet(&mut ioports, PRIMARY_PORTS.cmd_base, 0, &tur, 0);
    let _ = ioports.read(PRIMARY_PORTS.cmd_base + 7, 1);

    let mut req_sense = [0u8; 12];
    req_sense[0] = 0x03;
    req_sense[4] = 18;
    send_atapi_packet(&mut ioports, PRIMARY_PORTS.cmd_base, 0, &req_sense, 18);
    for _ in 0..(18 / 2) {
        let _ = ioports.read(PRIMARY_PORTS.cmd_base, 2);
    }

    let mut mem = Bus::new(0x20_000);
    let bm_base = ide.borrow().bus_master_base();

    let prd_addr = 0x1000u64;
    let dma_buf = 0x3000u64;

    // PRD entry: one 2048-byte segment, end-of-table.
    mem.write_u32(prd_addr, dma_buf as u32);
    mem.write_u16(prd_addr + 4, 2048);
    mem.write_u16(prd_addr + 6, 0x8000);

    // Program both PRD pointers so a buggy implementation that uses the wrong channel's PRD
    // pointer will still touch the known DMA buffer.
    ioports.write(bm_base + 4, 4, prd_addr as u32);
    ioports.write(bm_base + 8 + 4, 4, prd_addr as u32);

    // Seed destination buffer so we can prove no DMA happens before the correct bus master engine
    // is started.
    mem.write_physical(dma_buf, &vec![0xFFu8; 2048]);

    // READ(10) for LBA=0, blocks=1 with DMA enabled (FEATURES bit0).
    let mut read10 = [0u8; 12];
    read10[0] = 0x28;
    read10[2..6].copy_from_slice(&0u32.to_be_bytes());
    read10[7..9].copy_from_slice(&1u16.to_be_bytes());
    send_atapi_packet(&mut ioports, PRIMARY_PORTS.cmd_base, 0x01, &read10, 2048);

    // ACK packet-phase IRQ.
    assert!(ide.borrow().controller.primary_irq_pending());
    let _ = ioports.read(PRIMARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.primary_irq_pending());

    // Start the *secondary* bus master engine (wrong channel) and tick. The primary DMA request
    // must remain pending, with no memory writes and no interrupt.
    ioports.write(bm_base + 8, 1, 0x09);
    ide.borrow_mut().tick(&mut mem);

    assert!(!ide.borrow().controller.primary_irq_pending());
    assert!(!ide.borrow().controller.secondary_irq_pending());

    let bm_st_primary = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(bm_st_primary & 0x07, 0);
    let bm_st_secondary = ioports.read(bm_base + 8 + 2, 1) as u8;
    assert_eq!(bm_st_secondary & 0x07, 0);

    let mut out = vec![0u8; 2048];
    mem.read_physical(dma_buf, &mut out);
    assert!(out.iter().all(|&b| b == 0xFF));

    // Now start the correct bus master engine and tick; the transfer should complete.
    ioports.write(bm_base, 1, 0x09);
    ide.borrow_mut().tick(&mut mem);

    let bm_st_primary = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(bm_st_primary & 0x07, 0x04);
    assert!(ide.borrow().controller.primary_irq_pending());
    assert!(!ide.borrow().controller.secondary_irq_pending());

    mem.read_physical(dma_buf, &mut out);
    assert_eq!(out, expected);

    // Acknowledge the interrupt.
    let _ = ioports.read(PRIMARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.primary_irq_pending());
}

#[test]
fn atapi_dma_read_out_of_bounds_aborts_without_setting_bus_master_error() {
    // Attempt to read beyond the end of the ISO image with DMA enabled. This should fail during
    // packet processing (before a DMA request is queued), so BMIDE status should remain clear and
    // guest memory must not be modified.
    let iso = MemIso::new(1);

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut().controller.attach_secondary_master_atapi(
        aero_devices_storage::atapi::AtapiCdrom::new(Some(Box::new(iso))),
    );
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
    ioports.write(bm_base + 8 + 4, 4, prd_addr as u32);

    // Seed destination buffer.
    mem.write_physical(dma_buf, &vec![0xFFu8; 2048]);

    // Start the secondary bus master engine, direction=read (device -> memory).
    ioports.write(bm_base + 8, 1, 0x09);

    // READ(10) for out-of-bounds LBA=10, blocks=1 with DMA enabled (FEATURES bit0).
    let mut read10 = [0u8; 12];
    read10[0] = 0x28;
    read10[2..6].copy_from_slice(&10u32.to_be_bytes());
    read10[7..9].copy_from_slice(&1u16.to_be_bytes());
    send_atapi_packet(&mut ioports, SECONDARY_PORTS.cmd_base, 0x01, &read10, 2048);

    assert!(
        ide.borrow().controller.secondary_irq_pending(),
        "out-of-bounds ATAPI DMA read should abort and raise an IRQ immediately"
    );

    let bm_status = ioports.read(bm_base + 8 + 2, 1) as u8;
    assert_eq!(
        bm_status & 0x07,
        0,
        "BMIDE status bits should remain clear when no DMA request is queued"
    );

    // Even if we tick, there is no pending DMA request to service.
    ide.borrow_mut().tick(&mut mem);
    let bm_status = ioports.read(bm_base + 8 + 2, 1) as u8;
    assert_eq!(bm_status & 0x07, 0);

    // ATAPI interrupt reason: status phase.
    assert_eq!(ioports.read(SECONDARY_PORTS.cmd_base + 2, 1) as u8, 0x03);
    assert_eq!(ioports.read(SECONDARY_PORTS.cmd_base + 1, 1) as u8, 0x04);

    let mut out = vec![0u8; 2048];
    mem.read_physical(dma_buf, &mut out);
    assert!(out.iter().all(|&b| b == 0xFF));

    // STATUS acknowledges and clears the IRQ.
    let _ = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.secondary_irq_pending());
}

#[test]
fn atapi_packet_phase_irq_is_latched_while_nien_is_set_and_surfaces_after_reenable() {
    let iso = MemIso::new(1);

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut().controller.attach_primary_master_atapi(
        aero_devices_storage::atapi::AtapiCdrom::new(Some(Box::new(iso))),
    );
    ide.borrow_mut().config_mut().set_command(0x0005); // IO decode + Bus Master

    let mut ioports = IoPortBus::new();
    register_piix3_ide_ports(&mut ioports, ide.clone());

    // Select master on primary channel.
    ioports.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xA0);

    // Clear initial UNIT ATTENTION: TEST UNIT READY then REQUEST SENSE.
    let tur = [0u8; 12];
    send_atapi_packet(&mut ioports, PRIMARY_PORTS.cmd_base, 0, &tur, 0);
    let _ = ioports.read(PRIMARY_PORTS.cmd_base + 7, 1);

    let mut req_sense = [0u8; 12];
    req_sense[0] = 0x03;
    req_sense[4] = 18;
    send_atapi_packet(&mut ioports, PRIMARY_PORTS.cmd_base, 0, &req_sense, 18);
    for _ in 0..(18 / 2) {
        let _ = ioports.read(PRIMARY_PORTS.cmd_base, 2);
    }
    let _ = ioports.read(PRIMARY_PORTS.cmd_base + 7, 1);

    // Mask interrupts before issuing the PACKET command.
    ioports.write(PRIMARY_PORTS.ctrl_base, 1, 0x02);

    // Issue a DMA-capable READ(10) packet; the initial packet-phase IRQ should latch internally
    // but be masked from the output.
    let mut read10 = [0u8; 12];
    read10[0] = 0x28;
    read10[2..6].copy_from_slice(&0u32.to_be_bytes());
    read10[7..9].copy_from_slice(&1u16.to_be_bytes());
    send_atapi_packet(&mut ioports, PRIMARY_PORTS.cmd_base, 0x01, &read10, 2048);

    assert!(!ide.borrow().controller.primary_irq_pending());

    // Re-enable interrupts; the latched packet-phase IRQ should now surface.
    ioports.write(PRIMARY_PORTS.ctrl_base, 1, 0x00);
    assert!(ide.borrow().controller.primary_irq_pending());

    let _ = ioports.read(PRIMARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.primary_irq_pending());
}

#[test]
fn atapi_packet_phase_irq_can_be_acknowledged_while_nien_is_set() {
    let iso = MemIso::new(1);

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut().controller.attach_primary_master_atapi(
        aero_devices_storage::atapi::AtapiCdrom::new(Some(Box::new(iso))),
    );
    ide.borrow_mut().config_mut().set_command(0x0005); // IO decode + Bus Master

    let mut ioports = IoPortBus::new();
    register_piix3_ide_ports(&mut ioports, ide.clone());

    // Select master on primary channel.
    ioports.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xA0);

    // Clear initial UNIT ATTENTION: TEST UNIT READY then REQUEST SENSE.
    let tur = [0u8; 12];
    send_atapi_packet(&mut ioports, PRIMARY_PORTS.cmd_base, 0, &tur, 0);
    let _ = ioports.read(PRIMARY_PORTS.cmd_base + 7, 1);

    let mut req_sense = [0u8; 12];
    req_sense[0] = 0x03;
    req_sense[4] = 18;
    send_atapi_packet(&mut ioports, PRIMARY_PORTS.cmd_base, 0, &req_sense, 18);
    for _ in 0..(18 / 2) {
        let _ = ioports.read(PRIMARY_PORTS.cmd_base, 2);
    }
    let _ = ioports.read(PRIMARY_PORTS.cmd_base + 7, 1);

    // Mask interrupts before issuing the PACKET command.
    ioports.write(PRIMARY_PORTS.ctrl_base, 1, 0x02);

    let mut read10 = [0u8; 12];
    read10[0] = 0x28;
    read10[2..6].copy_from_slice(&0u32.to_be_bytes());
    read10[7..9].copy_from_slice(&1u16.to_be_bytes());
    send_atapi_packet(&mut ioports, PRIMARY_PORTS.cmd_base, 0x01, &read10, 2048);

    // Output should remain masked by nIEN.
    assert!(!ide.borrow().controller.primary_irq_pending());

    // Acknowledge the latched interrupt while still masked.
    let _ = ioports.read(PRIMARY_PORTS.cmd_base + 7, 1);

    // Re-enable interrupts; because it was already acknowledged, it should not surface.
    ioports.write(PRIMARY_PORTS.ctrl_base, 1, 0x00);
    assert!(!ide.borrow().controller.primary_irq_pending());
}

#[test]
fn atapi_alt_status_does_not_clear_irq_latch_on_packet_phase() {
    let iso = MemIso::new(1);

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut().controller.attach_primary_master_atapi(
        aero_devices_storage::atapi::AtapiCdrom::new(Some(Box::new(iso))),
    );
    ide.borrow_mut().config_mut().set_command(0x0001); // IO decode

    let mut ioports = IoPortBus::new();
    register_piix3_ide_ports(&mut ioports, ide.clone());

    // Select master on primary channel.
    ioports.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xA0);

    // Clear initial UNIT ATTENTION: TEST UNIT READY then REQUEST SENSE.
    let tur = [0u8; 12];
    send_atapi_packet(&mut ioports, PRIMARY_PORTS.cmd_base, 0, &tur, 0);
    let _ = ioports.read(PRIMARY_PORTS.cmd_base + 7, 1);

    let mut req_sense = [0u8; 12];
    req_sense[0] = 0x03;
    req_sense[4] = 18;
    send_atapi_packet(&mut ioports, PRIMARY_PORTS.cmd_base, 0, &req_sense, 18);
    for _ in 0..(18 / 2) {
        let _ = ioports.read(PRIMARY_PORTS.cmd_base, 2);
    }
    let _ = ioports.read(PRIMARY_PORTS.cmd_base + 7, 1);

    // Issue a DMA-capable READ(10) packet. This should raise the packet-phase IRQ requesting the
    // 12-byte packet; the DMA completion IRQ won't occur until the BMIDE engine is started and
    // `tick()` runs.
    let mut read10 = [0u8; 12];
    read10[0] = 0x28;
    read10[2..6].copy_from_slice(&0u32.to_be_bytes());
    read10[7..9].copy_from_slice(&1u16.to_be_bytes());
    send_atapi_packet(&mut ioports, PRIMARY_PORTS.cmd_base, 0x01, &read10, 2048);

    assert!(ide.borrow().controller.primary_irq_pending());

    // ALT_STATUS reads must not acknowledge/clear the IDE IRQ latch.
    let _ = ioports.read(PRIMARY_PORTS.ctrl_base, 1);
    assert!(
        ide.borrow().controller.primary_irq_pending(),
        "ALT_STATUS read cleared IRQ latch"
    );

    // STATUS reads still acknowledge/clear the latch.
    let _ = ioports.read(PRIMARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.primary_irq_pending());
}

#[test]
fn atapi_software_reset_clears_pending_dma_request() {
    let mut iso = MemIso::new(1);
    let expected: Vec<u8> = (0..2048u32)
        .map(|i| (i as u8).wrapping_mul(7).wrapping_add(0x55))
        .collect();
    iso.data[..2048].copy_from_slice(&expected);

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut().controller.attach_secondary_master_atapi(
        aero_devices_storage::atapi::AtapiCdrom::new(Some(Box::new(iso))),
    );
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
    let _ = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1);

    let mut mem = Bus::new(0x20_000);
    let bm_base = ide.borrow().bus_master_base();

    let prd_addr = 0x1000u64;
    let dma_buf = 0x3000u64;

    // PRD entry: one 2048-byte segment, end-of-table.
    mem.write_u32(prd_addr, dma_buf as u32);
    mem.write_u16(prd_addr + 4, 2048);
    mem.write_u16(prd_addr + 6, 0x8000);
    ioports.write(bm_base + 8 + 4, 4, prd_addr as u32);

    // Seed destination buffer.
    mem.write_physical(dma_buf, &vec![0xFFu8; 2048]);

    // Start BMIDE engine and queue a DMA request by issuing READ(10) with DMA enabled (but do not
    // tick yet).
    ioports.write(bm_base + 8, 1, 0x09);

    let mut read10 = [0u8; 12];
    read10[0] = 0x28;
    read10[2..6].copy_from_slice(&0u32.to_be_bytes());
    read10[7..9].copy_from_slice(&1u16.to_be_bytes());
    send_atapi_packet(&mut ioports, SECONDARY_PORTS.cmd_base, 0x01, &read10, 2048);

    // ACK packet-phase IRQ.
    assert!(ide.borrow().controller.secondary_irq_pending());
    let _ = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.secondary_irq_pending());

    // Assert software reset (SRST) via Device Control; this should clear the pending DMA request.
    ioports.write(SECONDARY_PORTS.ctrl_base, 1, 0x04);

    // Even though the BMIDE engine is started, there should be no DMA request left to service.
    ide.borrow_mut().tick(&mut mem);

    assert!(
        !ide.borrow().controller.secondary_irq_pending(),
        "SRST should clear any pending DMA completion IRQ"
    );
    assert!(!ide.borrow().controller.primary_irq_pending());

    let bm_st = ioports.read(bm_base + 8 + 2, 1) as u8;
    assert_eq!(
        bm_st & 0x07,
        0,
        "BMIDE status bits should remain clear when SRST cancels the request"
    );

    let mut out = vec![0u8; 2048];
    mem.read_physical(dma_buf, &mut out);
    assert!(
        out.iter().all(|&b| b == 0xFF),
        "guest memory should not be modified after SRST clears the DMA request"
    );
}

#[test]
fn atapi_dma_success_irq_is_latched_while_nien_is_set_and_surfaces_after_reenable() {
    let mut iso = MemIso::new(1);
    let expected: Vec<u8> = (0..2048u32)
        .map(|i| (i as u8).wrapping_mul(9).wrapping_add(1))
        .collect();
    iso.data[..2048].copy_from_slice(&expected);

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut().controller.attach_secondary_master_atapi(
        aero_devices_storage::atapi::AtapiCdrom::new(Some(Box::new(iso))),
    );
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

    ioports.write(bm_base + 8 + 4, 4, prd_addr as u32);

    // READ(10) for LBA=0, blocks=1 with DMA enabled (FEATURES bit0).
    let mut read10 = [0u8; 12];
    read10[0] = 0x28;
    read10[2..6].copy_from_slice(&0u32.to_be_bytes());
    read10[7..9].copy_from_slice(&1u16.to_be_bytes());
    send_atapi_packet(&mut ioports, SECONDARY_PORTS.cmd_base, 0x01, &read10, 2048);

    // ACK the packet-phase interrupt so the only remaining IRQ is the DMA completion.
    assert!(ide.borrow().controller.secondary_irq_pending());
    let _ = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.secondary_irq_pending());

    // Mask interrupts before running DMA; completion should latch internally.
    ioports.write(SECONDARY_PORTS.ctrl_base, 1, 0x02);

    ioports.write(bm_base + 8, 1, 0x09);
    ide.borrow_mut().tick(&mut mem);

    let bm_st = ioports.read(bm_base + 8 + 2, 1) as u8;
    assert_eq!(bm_st & 0x07, 0x04);
    assert!(!ide.borrow().controller.secondary_irq_pending());

    // Re-enable interrupts; the pending completion should now surface.
    ioports.write(SECONDARY_PORTS.ctrl_base, 1, 0x00);
    assert!(ide.borrow().controller.secondary_irq_pending());

    let mut out = vec![0u8; 2048];
    mem.read_physical(dma_buf, &mut out);
    assert_eq!(out, expected);

    let _ = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.secondary_irq_pending());
}

#[test]
fn atapi_dma_success_irq_can_be_acknowledged_while_nien_is_set() {
    let mut iso = MemIso::new(1);
    let expected: Vec<u8> = (0..2048u32)
        .map(|i| (i as u8).wrapping_mul(11).wrapping_add(3))
        .collect();
    iso.data[..2048].copy_from_slice(&expected);

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut().controller.attach_secondary_master_atapi(
        aero_devices_storage::atapi::AtapiCdrom::new(Some(Box::new(iso))),
    );
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
    ioports.write(bm_base + 8 + 4, 4, prd_addr as u32);

    // READ(10) for LBA=0, blocks=1 with DMA enabled (FEATURES bit0).
    let mut read10 = [0u8; 12];
    read10[0] = 0x28;
    read10[2..6].copy_from_slice(&0u32.to_be_bytes());
    read10[7..9].copy_from_slice(&1u16.to_be_bytes());
    send_atapi_packet(&mut ioports, SECONDARY_PORTS.cmd_base, 0x01, &read10, 2048);

    // ACK the packet-phase interrupt so the only remaining IRQ is the DMA completion.
    assert!(ide.borrow().controller.secondary_irq_pending());
    let _ = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.secondary_irq_pending());

    // Mask interrupts before running DMA.
    ioports.write(SECONDARY_PORTS.ctrl_base, 1, 0x02);

    ioports.write(bm_base + 8, 1, 0x09);
    ide.borrow_mut().tick(&mut mem);

    let bm_st = ioports.read(bm_base + 8 + 2, 1) as u8;
    assert_eq!(bm_st & 0x07, 0x04);
    assert!(!ide.borrow().controller.secondary_irq_pending());

    let mut out = vec![0u8; 2048];
    mem.read_physical(dma_buf, &mut out);
    assert_eq!(out, expected);

    // Acknowledge completion while interrupts are masked.
    let _ = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.secondary_irq_pending());

    // Re-enable interrupts; completion should not surface now that it was acked.
    ioports.write(SECONDARY_PORTS.ctrl_base, 1, 0x00);
    assert!(!ide.borrow().controller.secondary_irq_pending());

    // BMIDE status remains set until explicitly cleared.
    let bm_st = ioports.read(bm_base + 8 + 2, 1) as u8;
    assert_eq!(bm_st & 0x07, 0x04);
}

#[test]
fn atapi_nien_mask_on_primary_channel_does_not_mask_secondary_dma_irq() {
    let mut iso = MemIso::new(1);
    let expected: Vec<u8> = (0..2048u32)
        .map(|i| (i as u8).wrapping_mul(7).wrapping_add(0x21))
        .collect();
    iso.data[..2048].copy_from_slice(&expected);

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut().controller.attach_secondary_master_atapi(
        aero_devices_storage::atapi::AtapiCdrom::new(Some(Box::new(iso))),
    );
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
    let _ = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1);

    // Mask interrupts only on the primary channel (nIEN=1).
    ioports.write(PRIMARY_PORTS.ctrl_base, 1, 0x02);

    let mut mem = Bus::new(0x20_000);
    let bm_base = ide.borrow().bus_master_base();

    let prd_addr = 0x1000u64;
    let dma_buf = 0x3000u64;

    // PRD entry: one 2048-byte segment, end-of-table.
    mem.write_u32(prd_addr, dma_buf as u32);
    mem.write_u16(prd_addr + 4, 2048);
    mem.write_u16(prd_addr + 6, 0x8000);
    ioports.write(bm_base + 8 + 4, 4, prd_addr as u32);

    mem.write_physical(dma_buf, &vec![0xFFu8; 2048]);

    // READ(10) for LBA=0, blocks=1 with DMA enabled.
    let mut read10 = [0u8; 12];
    read10[0] = 0x28;
    read10[2..6].copy_from_slice(&0u32.to_be_bytes());
    read10[7..9].copy_from_slice(&1u16.to_be_bytes());
    send_atapi_packet(&mut ioports, SECONDARY_PORTS.cmd_base, 0x01, &read10, 2048);

    // ACK packet-phase IRQ.
    assert!(ide.borrow().controller.secondary_irq_pending());
    let _ = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.secondary_irq_pending());

    // Complete DMA.
    ioports.write(bm_base + 8, 1, 0x09);
    ide.borrow_mut().tick(&mut mem);

    let bm_st = ioports.read(bm_base + 8 + 2, 1) as u8;
    assert_eq!(bm_st & 0x07, 0x04);
    assert!(
        ide.borrow().controller.secondary_irq_pending(),
        "secondary ATAPI DMA IRQ should not be masked by primary nIEN"
    );
    assert!(!ide.borrow().controller.primary_irq_pending());

    let mut out = vec![0u8; 2048];
    mem.read_physical(dma_buf, &mut out);
    assert_eq!(out, expected);

    let _ = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.secondary_irq_pending());
}

#[test]
fn atapi_nien_mask_on_secondary_channel_does_not_mask_primary_dma_irq() {
    let mut iso = MemIso::new(1);
    let expected: Vec<u8> = (0..2048u32)
        .map(|i| (i as u8).wrapping_mul(9).wrapping_add(0x17))
        .collect();
    iso.data[..2048].copy_from_slice(&expected);

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut().controller.attach_primary_master_atapi(
        aero_devices_storage::atapi::AtapiCdrom::new(Some(Box::new(iso))),
    );
    ide.borrow_mut().config_mut().set_command(0x0005); // IO decode + Bus Master

    let mut ioports = IoPortBus::new();
    register_piix3_ide_ports(&mut ioports, ide.clone());

    // Select master on primary channel.
    ioports.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xA0);

    // Clear initial UNIT ATTENTION: TEST UNIT READY then REQUEST SENSE.
    let tur = [0u8; 12];
    send_atapi_packet(&mut ioports, PRIMARY_PORTS.cmd_base, 0, &tur, 0);
    let _ = ioports.read(PRIMARY_PORTS.cmd_base + 7, 1);

    let mut req_sense = [0u8; 12];
    req_sense[0] = 0x03;
    req_sense[4] = 18;
    send_atapi_packet(&mut ioports, PRIMARY_PORTS.cmd_base, 0, &req_sense, 18);
    for _ in 0..(18 / 2) {
        let _ = ioports.read(PRIMARY_PORTS.cmd_base, 2);
    }
    let _ = ioports.read(PRIMARY_PORTS.cmd_base + 7, 1);

    // Mask interrupts only on the secondary channel (nIEN=1).
    ioports.write(SECONDARY_PORTS.ctrl_base, 1, 0x02);

    let mut mem = Bus::new(0x20_000);
    let bm_base = ide.borrow().bus_master_base();

    let prd_addr = 0x1000u64;
    let dma_buf = 0x3000u64;

    // PRD entry: one 2048-byte segment, end-of-table.
    mem.write_u32(prd_addr, dma_buf as u32);
    mem.write_u16(prd_addr + 4, 2048);
    mem.write_u16(prd_addr + 6, 0x8000);
    ioports.write(bm_base + 4, 4, prd_addr as u32);

    mem.write_physical(dma_buf, &vec![0xFFu8; 2048]);

    // READ(10) for LBA=0, blocks=1 with DMA enabled.
    let mut read10 = [0u8; 12];
    read10[0] = 0x28;
    read10[2..6].copy_from_slice(&0u32.to_be_bytes());
    read10[7..9].copy_from_slice(&1u16.to_be_bytes());
    send_atapi_packet(&mut ioports, PRIMARY_PORTS.cmd_base, 0x01, &read10, 2048);

    // ACK packet-phase IRQ.
    assert!(ide.borrow().controller.primary_irq_pending());
    let _ = ioports.read(PRIMARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.primary_irq_pending());

    // Complete DMA.
    ioports.write(bm_base, 1, 0x09);
    ide.borrow_mut().tick(&mut mem);

    let bm_st = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(bm_st & 0x07, 0x04);
    assert!(
        ide.borrow().controller.primary_irq_pending(),
        "primary ATAPI DMA IRQ should not be masked by secondary nIEN"
    );
    assert!(!ide.borrow().controller.secondary_irq_pending());

    let mut out = vec![0u8; 2048];
    mem.read_physical(dma_buf, &mut out);
    assert_eq!(out, expected);

    let _ = ioports.read(PRIMARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.primary_irq_pending());
}

#[test]
fn atapi_dma_success_sets_interrupt_reason_and_byte_count_registers() {
    let mut iso = MemIso::new(1);
    let expected: Vec<u8> = (0..2048u32)
        .map(|i| (i as u8).wrapping_mul(3).wrapping_add(0x5B))
        .collect();
    iso.data[..2048].copy_from_slice(&expected);

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut().controller.attach_secondary_master_atapi(
        aero_devices_storage::atapi::AtapiCdrom::new(Some(Box::new(iso))),
    );
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
    let _ = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1);

    let mut mem = Bus::new(0x20_000);
    let bm_base = ide.borrow().bus_master_base();

    let prd_addr = 0x1000u64;
    let dma_buf = 0x3000u64;

    // PRD entry: one 2048-byte segment, end-of-table.
    mem.write_u32(prd_addr, dma_buf as u32);
    mem.write_u16(prd_addr + 4, 2048);
    mem.write_u16(prd_addr + 6, 0x8000);
    ioports.write(bm_base + 8 + 4, 4, prd_addr as u32);

    // Seed destination buffer.
    mem.write_physical(dma_buf, &vec![0xFFu8; 2048]);

    // READ(10) for LBA=0, blocks=1 with DMA enabled (FEATURES bit0).
    let mut read10 = [0u8; 12];
    read10[0] = 0x28;
    read10[2..6].copy_from_slice(&0u32.to_be_bytes());
    read10[7..9].copy_from_slice(&1u16.to_be_bytes());
    send_atapi_packet(&mut ioports, SECONDARY_PORTS.cmd_base, 0x01, &read10, 2048);

    // ACK packet-phase IRQ.
    assert!(ide.borrow().controller.secondary_irq_pending());
    let _ = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.secondary_irq_pending());

    // While the DMA request is pending, interrupt reason should indicate Data-In (IO=1, CoD=0),
    // and the byte count registers should reflect the transfer length (0x0800).
    assert_eq!(ioports.read(SECONDARY_PORTS.cmd_base + 2, 1) as u8, 0x02);
    assert_eq!(ioports.read(SECONDARY_PORTS.cmd_base + 4, 1) as u8, 0x00);
    assert_eq!(ioports.read(SECONDARY_PORTS.cmd_base + 5, 1) as u8, 0x08);

    // Complete DMA.
    ioports.write(bm_base + 8, 1, 0x09);
    ide.borrow_mut().tick(&mut mem);

    // ATAPI should transition to status phase (IO=1, CoD=1) on completion.
    assert_eq!(ioports.read(SECONDARY_PORTS.cmd_base + 2, 1) as u8, 0x03);
    assert!(ide.borrow().controller.secondary_irq_pending());

    let mut out = vec![0u8; 2048];
    mem.read_physical(dma_buf, &mut out);
    assert_eq!(out, expected);

    let _ = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.secondary_irq_pending());
}

#[test]
fn atapi_dma_missing_prd_eot_sets_error_status_on_secondary_channel() {
    let mut iso = MemIso::new(1);
    let expected: Vec<u8> = (0..2048u32)
        .map(|i| (i as u8).wrapping_mul(13).wrapping_add(5))
        .collect();
    iso.data[..2048].copy_from_slice(&expected);

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut().controller.attach_secondary_master_atapi(
        aero_devices_storage::atapi::AtapiCdrom::new(Some(Box::new(iso))),
    );
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

    // Malformed PRD: one segment large enough to cover the entire transfer but missing EOT.
    mem.write_u32(prd_addr, dma_buf as u32);
    mem.write_u16(prd_addr + 4, 2048);
    mem.write_u16(prd_addr + 6, 0x0000);

    ioports.write(bm_base + 8 + 4, 4, prd_addr as u32);

    // READ(10) for LBA=0, blocks=1 with DMA enabled (FEATURES bit0).
    let mut read10 = [0u8; 12];
    read10[0] = 0x28;
    read10[2..6].copy_from_slice(&0u32.to_be_bytes());
    read10[7..9].copy_from_slice(&1u16.to_be_bytes());
    send_atapi_packet(&mut ioports, SECONDARY_PORTS.cmd_base, 0x01, &read10, 2048);

    // ACK the packet-phase interrupt so we can observe the DMA completion interrupt.
    assert!(ide.borrow().controller.secondary_irq_pending());
    let _ = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.secondary_irq_pending());

    // Start the secondary bus master engine, direction=read (device -> memory).
    ioports.write(bm_base + 8, 1, 0x09);
    ide.borrow_mut().tick(&mut mem);

    assert!(
        ide.borrow().controller.secondary_irq_pending(),
        "DMA error should raise a secondary IRQ"
    );
    assert!(
        !ide.borrow().controller.primary_irq_pending(),
        "DMA error on secondary should not raise a primary IRQ"
    );

    let bm_st_primary = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(
        bm_st_primary & 0x07,
        0,
        "primary BMIDE status bits should be unaffected"
    );

    let bm_st_secondary = ioports.read(bm_base + 8 + 2, 1) as u8;
    assert_eq!(
        bm_st_secondary & 0x07,
        0x06,
        "secondary BMIDE status should have IRQ+ERR set and ACTIVE clear"
    );
    assert_ne!(
        bm_st_secondary & 0x20,
        0,
        "secondary BMIDE DMA capability bit for master should be set"
    );

    // ATAPI uses Sector Count as interrupt reason; errors should transition to status phase.
    assert_eq!(ioports.read(SECONDARY_PORTS.cmd_base + 2, 1) as u8, 0x03);

    // Use ALT_STATUS so we don't accidentally clear the interrupt.
    let st = ioports.read(SECONDARY_PORTS.ctrl_base, 1) as u8;
    assert_eq!(st & 0x80, 0, "BSY should be clear after DMA failure");
    assert_eq!(st & 0x08, 0, "DRQ should be clear after DMA failure");
    assert_ne!(st & 0x40, 0, "DRDY should be set after DMA failure");
    assert_ne!(st & 0x01, 0, "ERR should be set after DMA failure");
    assert!(
        ide.borrow().controller.secondary_irq_pending(),
        "reading ALT_STATUS must not clear the IDE IRQ latch"
    );
    assert_eq!(ioports.read(SECONDARY_PORTS.cmd_base + 1, 1) as u8, 0x04);

    // Even though the PRD table is malformed, the DMA engine should still have written the data
    // before detecting the missing EOT bit.
    let mut out = vec![0u8; 2048];
    mem.read_physical(dma_buf, &mut out);
    assert_eq!(out, expected);

    // Reading STATUS acknowledges and clears the IDE IRQ latch, but does not clear BMIDE status
    // bits.
    let _ = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1);
    assert!(
        !ide.borrow().controller.secondary_irq_pending(),
        "reading STATUS should clear the IDE IRQ latch"
    );
    let bm_st_after_ack = ioports.read(bm_base + 8 + 2, 1) as u8;
    assert_eq!(
        bm_st_after_ack & 0x07,
        0x06,
        "BMIDE status bits should remain set after STATUS acknowledges the IDE IRQ"
    );

    // Secondary BMIDE status should be RW1C for IRQ/ERR bits.
    ioports.write(bm_base + 8 + 2, 1, 0x02);
    let bm_st = ioports.read(bm_base + 8 + 2, 1) as u8;
    assert_eq!(bm_st & 0x07, 0x04, "clearing ERR should preserve IRQ");
    ioports.write(bm_base + 8 + 2, 1, 0x04);
    let bm_st = ioports.read(bm_base + 8 + 2, 1) as u8;
    assert_eq!(bm_st & 0x07, 0x00);
}

#[test]
fn atapi_dma_prd_too_short_sets_error_status_and_partially_transfers_data() {
    const PRD_LEN: u16 = 512;

    let mut iso = MemIso::new(1);
    let expected: Vec<u8> = (0..2048u32)
        .map(|i| (i as u8).wrapping_mul(11).wrapping_add(9))
        .collect();
    iso.data[..2048].copy_from_slice(&expected);

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut().controller.attach_secondary_master_atapi(
        aero_devices_storage::atapi::AtapiCdrom::new(Some(Box::new(iso))),
    );
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

    // PRD entry that is too short for a 2048-byte request (512 bytes, EOT).
    mem.write_u32(prd_addr, dma_buf as u32);
    mem.write_u16(prd_addr + 4, PRD_LEN);
    mem.write_u16(prd_addr + 6, 0x8000);
    ioports.write(bm_base + 8 + 4, 4, prd_addr as u32);

    // Seed destination buffer to validate partial-transfer semantics.
    mem.write_physical(dma_buf, &vec![0xFFu8; 2048]);

    // READ(10) for LBA=0, blocks=1 with DMA enabled (FEATURES bit0).
    let mut read10 = [0u8; 12];
    read10[0] = 0x28;
    read10[2..6].copy_from_slice(&0u32.to_be_bytes());
    read10[7..9].copy_from_slice(&1u16.to_be_bytes());
    send_atapi_packet(&mut ioports, SECONDARY_PORTS.cmd_base, 0x01, &read10, 2048);

    // ACK the packet-phase interrupt so we can observe the DMA completion interrupt.
    assert!(ide.borrow().controller.secondary_irq_pending());
    let _ = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.secondary_irq_pending());

    // Start the secondary bus master engine, direction=read (device -> memory).
    ioports.write(bm_base + 8, 1, 0x09);
    ide.borrow_mut().tick(&mut mem);

    assert!(
        ide.borrow().controller.secondary_irq_pending(),
        "DMA error should raise a secondary IRQ"
    );
    assert!(!ide.borrow().controller.primary_irq_pending());

    let bm_st_secondary = ioports.read(bm_base + 8 + 2, 1) as u8;
    assert_eq!(bm_st_secondary & 0x07, 0x06);
    assert_ne!(bm_st_secondary & 0x20, 0);

    // ATAPI interrupt reason: status phase.
    assert_eq!(ioports.read(SECONDARY_PORTS.cmd_base + 2, 1) as u8, 0x03);

    // IDE status/error after abort.
    let st = ioports.read(SECONDARY_PORTS.ctrl_base, 1) as u8;
    assert_eq!(st & 0x80, 0, "BSY should be clear after DMA failure");
    assert_eq!(st & 0x08, 0, "DRQ should be clear after DMA failure");
    assert_ne!(st & 0x40, 0, "DRDY should be set after DMA failure");
    assert_ne!(st & 0x01, 0, "ERR should be set after DMA failure");
    assert!(
        ide.borrow().controller.secondary_irq_pending(),
        "reading ALT_STATUS must not clear the IDE IRQ latch"
    );
    assert_eq!(ioports.read(SECONDARY_PORTS.cmd_base + 1, 1) as u8, 0x04);

    let mut out = vec![0u8; 2048];
    mem.read_physical(dma_buf, &mut out);
    assert_eq!(&out[..PRD_LEN as usize], &expected[..PRD_LEN as usize]);
    assert!(out[PRD_LEN as usize..].iter().all(|&b| b == 0xFF));

    let _ = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.secondary_irq_pending());
    let bm_st = ioports.read(bm_base + 8 + 2, 1) as u8;
    assert_eq!(bm_st & 0x07, 0x06);
}

#[test]
fn atapi_dma_direction_mismatch_sets_error_status_and_does_not_transfer_data() {
    let mut iso = MemIso::new(1);
    let expected: Vec<u8> = (0..2048u32)
        .map(|i| (i as u8).wrapping_mul(11).wrapping_add(9))
        .collect();
    iso.data[..2048].copy_from_slice(&expected);

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut().controller.attach_secondary_master_atapi(
        aero_devices_storage::atapi::AtapiCdrom::new(Some(Box::new(iso))),
    );
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

    // Valid PRD entry (2048 bytes, EOT).
    mem.write_u32(prd_addr, dma_buf as u32);
    mem.write_u16(prd_addr + 4, 2048);
    mem.write_u16(prd_addr + 6, 0x8000);
    ioports.write(bm_base + 8 + 4, 4, prd_addr as u32);

    // Seed destination buffer; direction mismatch should prevent DMA writes.
    mem.write_physical(dma_buf, &vec![0xFFu8; 2048]);

    // Program bus master with *wrong* direction: FromMemory (bit3 clear) + start.
    ioports.write(bm_base + 8, 1, 0x01);

    // READ(10) for LBA=0, blocks=1 with DMA enabled (FEATURES bit0).
    let mut read10 = [0u8; 12];
    read10[0] = 0x28;
    read10[2..6].copy_from_slice(&0u32.to_be_bytes());
    read10[7..9].copy_from_slice(&1u16.to_be_bytes());
    send_atapi_packet(&mut ioports, SECONDARY_PORTS.cmd_base, 0x01, &read10, 2048);

    // ACK the packet-phase interrupt so we can observe the DMA completion interrupt.
    assert!(ide.borrow().controller.secondary_irq_pending());
    let _ = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.secondary_irq_pending());

    ide.borrow_mut().tick(&mut mem);

    assert!(ide.borrow().controller.secondary_irq_pending());
    assert!(!ide.borrow().controller.primary_irq_pending());

    let bm_st_secondary = ioports.read(bm_base + 8 + 2, 1) as u8;
    assert_eq!(bm_st_secondary & 0x07, 0x06);
    assert_ne!(bm_st_secondary & 0x20, 0);

    assert_eq!(ioports.read(SECONDARY_PORTS.cmd_base + 2, 1) as u8, 0x03);

    let st = ioports.read(SECONDARY_PORTS.ctrl_base, 1) as u8;
    assert_eq!(st & 0x80, 0);
    assert_eq!(st & 0x08, 0);
    assert_ne!(st & 0x40, 0);
    assert_ne!(st & 0x01, 0);
    assert!(
        ide.borrow().controller.secondary_irq_pending(),
        "reading ALT_STATUS must not clear the IDE IRQ latch"
    );
    assert_eq!(ioports.read(SECONDARY_PORTS.cmd_base + 1, 1) as u8, 0x04);

    let mut out = vec![0u8; 2048];
    mem.read_physical(dma_buf, &mut out);
    assert!(out.iter().all(|&b| b == 0xFF));

    let _ = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.secondary_irq_pending());
    let bm_st = ioports.read(bm_base + 8 + 2, 1) as u8;
    assert_eq!(bm_st & 0x07, 0x06);
}

#[test]
fn atapi_dma_error_irq_is_latched_while_nien_is_set_and_surfaces_after_reenable() {
    let mut iso = MemIso::new(1);
    let expected: Vec<u8> = (0..2048u32)
        .map(|i| (i as u8).wrapping_mul(13).wrapping_add(5))
        .collect();
    iso.data[..2048].copy_from_slice(&expected);

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut().controller.attach_secondary_master_atapi(
        aero_devices_storage::atapi::AtapiCdrom::new(Some(Box::new(iso))),
    );
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

    // Malformed PRD: one segment long enough to cover the entire transfer but missing EOT.
    mem.write_u32(prd_addr, dma_buf as u32);
    mem.write_u16(prd_addr + 4, 2048);
    mem.write_u16(prd_addr + 6, 0x0000);
    ioports.write(bm_base + 8 + 4, 4, prd_addr as u32);

    // READ(10) for LBA=0, blocks=1 with DMA enabled (FEATURES bit0).
    let mut read10 = [0u8; 12];
    read10[0] = 0x28;
    read10[2..6].copy_from_slice(&0u32.to_be_bytes());
    read10[7..9].copy_from_slice(&1u16.to_be_bytes());
    send_atapi_packet(&mut ioports, SECONDARY_PORTS.cmd_base, 0x01, &read10, 2048);

    // ACK the packet-phase interrupt so we can observe the DMA completion interrupt.
    assert!(ide.borrow().controller.secondary_irq_pending());
    let _ = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.secondary_irq_pending());

    // Mask interrupts before running DMA; completion should latch irq_pending.
    ioports.write(SECONDARY_PORTS.ctrl_base, 1, 0x02);

    // Start the secondary bus master engine, direction=read (device -> memory).
    ioports.write(bm_base + 8, 1, 0x09);
    ide.borrow_mut().tick(&mut mem);

    let bm_st = ioports.read(bm_base + 8 + 2, 1) as u8;
    assert_eq!(bm_st & 0x07, 0x06);

    // Output should be masked by nIEN.
    assert!(!ide.borrow().controller.secondary_irq_pending());

    // Re-enable interrupts; the pending IRQ should now surface.
    ioports.write(SECONDARY_PORTS.ctrl_base, 1, 0x00);
    assert!(ide.borrow().controller.secondary_irq_pending());

    // Use ALT_STATUS so we don't clear the IRQ.
    let st = ioports.read(SECONDARY_PORTS.ctrl_base, 1) as u8;
    assert_ne!(st & 0x01, 0);
    assert_eq!(
        ioports.read(SECONDARY_PORTS.cmd_base + 2, 1) as u8,
        0x03,
        "expected ATAPI status phase after DMA failure"
    );
    assert_eq!(ioports.read(SECONDARY_PORTS.cmd_base + 1, 1) as u8, 0x04);

    // Reading STATUS acknowledges and clears the latch.
    let _ = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.secondary_irq_pending());

    // BMIDE status bits remain set until guest clears them explicitly.
    let bm_st_after = ioports.read(bm_base + 8 + 2, 1) as u8;
    assert_eq!(bm_st_after & 0x07, 0x06);

    // Data should still have been written to guest memory before detecting missing EOT.
    let mut out = vec![0u8; 2048];
    mem.read_physical(dma_buf, &mut out);
    assert_eq!(out, expected);
}

#[test]
fn atapi_dma_error_irq_can_be_acknowledged_while_nien_is_set() {
    let mut iso = MemIso::new(1);
    let expected: Vec<u8> = (0..2048u32)
        .map(|i| (i as u8).wrapping_mul(13).wrapping_add(5))
        .collect();
    iso.data[..2048].copy_from_slice(&expected);

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut().controller.attach_secondary_master_atapi(
        aero_devices_storage::atapi::AtapiCdrom::new(Some(Box::new(iso))),
    );
    ide.borrow_mut().config_mut().set_command(0x0005); // IO decode + Bus Master

    let mut ioports = IoPortBus::new();
    register_piix3_ide_ports(&mut ioports, ide.clone());

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

    // Malformed PRD: one segment long enough to cover the entire transfer but missing EOT.
    mem.write_u32(prd_addr, dma_buf as u32);
    mem.write_u16(prd_addr + 4, 2048);
    mem.write_u16(prd_addr + 6, 0x0000);
    ioports.write(bm_base + 8 + 4, 4, prd_addr as u32);

    let mut read10 = [0u8; 12];
    read10[0] = 0x28;
    read10[2..6].copy_from_slice(&0u32.to_be_bytes());
    read10[7..9].copy_from_slice(&1u16.to_be_bytes());
    send_atapi_packet(&mut ioports, SECONDARY_PORTS.cmd_base, 0x01, &read10, 2048);

    // ACK packet-phase IRQ.
    let _ = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1);

    // Mask interrupts before running DMA.
    ioports.write(SECONDARY_PORTS.ctrl_base, 1, 0x02);

    ioports.write(bm_base + 8, 1, 0x09);
    ide.borrow_mut().tick(&mut mem);

    let bm_st = ioports.read(bm_base + 8 + 2, 1) as u8;
    assert_eq!(bm_st & 0x07, 0x06);

    // IRQ output masked.
    assert!(!ide.borrow().controller.secondary_irq_pending());

    // Confirm it would surface if unmasked (irq_pending is latched).
    ioports.write(SECONDARY_PORTS.ctrl_base, 1, 0x00);
    assert!(ide.borrow().controller.secondary_irq_pending());

    // Mask again and acknowledge while interrupts are disabled.
    ioports.write(SECONDARY_PORTS.ctrl_base, 1, 0x02);
    assert!(!ide.borrow().controller.secondary_irq_pending());
    let _ = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1);

    // Re-enable interrupts; the IRQ should not surface now that it was acknowledged.
    ioports.write(SECONDARY_PORTS.ctrl_base, 1, 0x00);
    assert!(!ide.borrow().controller.secondary_irq_pending());

    // BMIDE status remains set until guest clears it explicitly.
    let bm_st_after = ioports.read(bm_base + 8 + 2, 1) as u8;
    assert_eq!(bm_st_after & 0x07, 0x06);
}

#[test]
fn pci_io_decode_gates_legacy_and_bus_master_ports() {
    let capacity = 4 * SECTOR_SIZE as u64;
    let disk = RawDisk::create(MemBackend::new(), capacity).unwrap();

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut()
        .controller
        .attach_primary_master_ata(AtaDrive::new(Box::new(disk)).unwrap());

    // Leave PCI command at the power-on default (IO decode disabled).
    assert_eq!(ide.borrow().config().command(), 0);

    let mut ioports = IoPortBus::new();
    register_piix3_ide_ports(&mut ioports, ide.clone());

    let bm_base = ide.borrow().bus_master_base();

    // Without PCI IO decode, all reads should float high regardless of which port is accessed.
    assert_eq!(ioports.read(PRIMARY_PORTS.cmd_base + 7, 1), 0xFF);
    assert_eq!(ioports.read(PRIMARY_PORTS.ctrl_base, 1), 0xFF);
    assert_eq!(ioports.read(bm_base, 1), 0xFF);
    assert_eq!(ioports.read(bm_base + 4, 4), 0xFFFF_FFFF);

    // Writes should be ignored while IO decode is disabled.
    ioports.write(bm_base, 1, 0x09);
    ioports.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xE7); // FLUSH CACHE
    assert!(!ide.borrow().controller.primary_irq_pending());

    // Enable PCI IO decode; reads should now be dispatched to the controller.
    ide.borrow_mut().config_mut().set_command(0x0001);

    // Bus master command register should still be at its reset value since the write while IO
    // decode was disabled must have been ignored.
    assert_eq!(ioports.read(bm_base, 1) as u8, 0);

    // Status register should report DRDY with no busy/data/error bits set.
    let st = ioports.read(PRIMARY_PORTS.cmd_base + 7, 1) as u8;
    assert_eq!(st & 0x80, 0, "BSY should be clear");
    assert_eq!(st & 0x08, 0, "DRQ should be clear");
    assert_eq!(st & 0x01, 0, "ERR should be clear");
    assert_ne!(st & 0x40, 0, "DRDY should be set");
}

#[test]
fn bus_master_reset_clears_command_status_and_prd_pointer() {
    let mut iso = MemIso::new(1);
    iso.data[0..8].copy_from_slice(b"DMATEST!");

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut().controller.attach_secondary_master_atapi(
        aero_devices_storage::atapi::AtapiCdrom::new(Some(Box::new(iso))),
    );
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

    // Ensure we actually latched non-zero DMA engine state.
    let cmd_before = ioports.read(bm_base + 8, 1) as u8;
    let st_before = ioports.read(bm_base + 8 + 2, 1) as u8;
    let prd_before = ioports.read(bm_base + 8 + 4, 4);
    assert_ne!(cmd_before & 0x01, 0);
    assert_ne!(st_before & 0x04, 0);
    assert_ne!(prd_before, 0);

    ide.borrow_mut().controller.reset();

    // Bus Master registers should be back at their power-on baseline (but capability bits should
    // remain, since they reflect attached devices).
    assert_eq!(ioports.read(bm_base + 8, 1) as u8, 0);
    assert_eq!(ioports.read(bm_base + 8 + 4, 4), 0);
    let st_after = ioports.read(bm_base + 8 + 2, 1) as u8;
    assert_eq!(st_after & 0x07, 0);
    assert_ne!(st_after & 0x20, 0);

    // Controller reset should also clear any latched IRQs.
    assert!(!ide.borrow().controller.secondary_irq_pending());
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
    assert_eq!(
        st & 0x07,
        0x06,
        "BMIDE status should have IRQ+ERR set and ACTIVE clear"
    );
    assert!(ide.borrow().controller.primary_irq_pending());

    // Even though the PRD table is malformed, the DMA engine should still have written the full
    // sector before detecting the missing EOT bit.
    let mut out = vec![0u8; SECTOR_SIZE];
    mem.read_physical(dma_buf, &mut out);
    assert_eq!(out, sector0);

    // ATA status/error should reflect an aborted command.
    let st = ioports.read(PRIMARY_PORTS.ctrl_base, 1) as u8;
    assert_eq!(st & 0x80, 0, "BSY should be clear");
    assert_eq!(st & 0x08, 0, "DRQ should be clear");
    assert_ne!(st & 0x40, 0, "DRDY should be set");
    assert_ne!(st & 0x01, 0, "ERR should be set");
    assert!(
        ide.borrow().controller.primary_irq_pending(),
        "reading ALT_STATUS must not clear the IDE IRQ latch"
    );
    assert_eq!(ioports.read(PRIMARY_PORTS.cmd_base + 1, 1) as u8, 0x04);

    // Reading Status acknowledges and clears the pending interrupt.
    let _ = ioports.read(PRIMARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.primary_irq_pending());

    // BMIDE status bits persist until RW1C cleared by the guest.
    let bm_st_after = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(bm_st_after & 0x07, 0x06);
}

#[test]
fn ata_dma_error_irq_is_latched_while_nien_is_set_and_surfaces_after_reenable() {
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

    // Malformed PRD entry without EOT flag (but long enough for the transfer).
    mem.write_u32(prd_addr, dma_buf as u32);
    mem.write_u16(prd_addr + 4, SECTOR_SIZE as u16);
    mem.write_u16(prd_addr + 6, 0x0000);
    ioports.write(bm_base + 4, 4, prd_addr as u32);

    // Mask IDE interrupts (nIEN=1).
    ioports.write(PRIMARY_PORTS.ctrl_base, 1, 0x02);

    // READ DMA for LBA 0, 1 sector.
    ioports.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    ioports.write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xC8);

    ioports.write(bm_base, 1, 0x09);
    ide.borrow_mut().tick(&mut mem);

    // DMA error occurred.
    let bm_st = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(bm_st & 0x07, 0x06);
    assert_eq!(ioports.read(PRIMARY_PORTS.cmd_base + 1, 1) as u8, 0x04);

    // Interrupt should be latched internally but masked from the output.
    assert!(!ide.borrow().controller.primary_irq_pending());

    // Re-enable interrupts; the pending IRQ should now surface.
    ioports.write(PRIMARY_PORTS.ctrl_base, 1, 0x00);
    assert!(ide.borrow().controller.primary_irq_pending());

    // Reading Status acknowledges and clears the pending interrupt.
    let _ = ioports.read(PRIMARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.primary_irq_pending());
}

#[test]
fn ata_dma_error_irq_can_be_acknowledged_while_nien_is_set() {
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

    mem.write_u32(prd_addr, dma_buf as u32);
    mem.write_u16(prd_addr + 4, SECTOR_SIZE as u16);
    mem.write_u16(prd_addr + 6, 0x0000); // missing EOT
    ioports.write(bm_base + 4, 4, prd_addr as u32);

    // Mask IDE interrupts (nIEN=1).
    ioports.write(PRIMARY_PORTS.ctrl_base, 1, 0x02);

    // READ DMA for LBA 0, 1 sector.
    ioports.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    ioports.write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xC8);

    ioports.write(bm_base, 1, 0x09);
    ide.borrow_mut().tick(&mut mem);

    // Confirm DMA error occurred.
    let bm_st = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(bm_st & 0x07, 0x06);

    // Output should be masked by nIEN.
    assert!(!ide.borrow().controller.primary_irq_pending());

    // Acknowledge the interrupt while still masked.
    let _ = ioports.read(PRIMARY_PORTS.cmd_base + 7, 1);

    // Re-enable interrupts; the IRQ should not surface.
    ioports.write(PRIMARY_PORTS.ctrl_base, 1, 0x00);
    assert!(!ide.borrow().controller.primary_irq_pending());

    // BMIDE status bits are independent and must be cleared explicitly.
    let bm_st_after = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(bm_st_after & 0x07, 0x06);
}

#[test]
fn ata_dma_success_on_primary_channel_requires_primary_bus_master_start() {
    let capacity = 4 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut sector0 = vec![0u8; SECTOR_SIZE];
    for (i, b) in sector0.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(11).wrapping_add(5);
    }
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

    // PRD entry: one 512-byte segment, end-of-table.
    mem.write_u32(prd_addr, dma_buf as u32);
    mem.write_u16(prd_addr + 4, SECTOR_SIZE as u16);
    mem.write_u16(prd_addr + 6, 0x8000);
    // Program both PRD pointers so a buggy implementation that consults the wrong channel's PRD
    // pointer will still touch the known DMA buffer.
    ioports.write(bm_base + 4, 4, prd_addr as u32);
    ioports.write(bm_base + 8 + 4, 4, prd_addr as u32);

    // Seed destination buffer so we can prove no DMA happens before the correct bus master engine
    // is started.
    mem.write_physical(dma_buf, &vec![0xFFu8; SECTOR_SIZE]);

    // READ DMA for LBA 0, 1 sector on the primary channel.
    ioports.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    ioports.write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xC8);

    // Start the *secondary* bus master engine (wrong channel) and tick. The primary DMA request
    // must remain pending, with no memory writes and no interrupt.
    ioports.write(bm_base + 8, 1, 0x09);
    ide.borrow_mut().tick(&mut mem);

    assert!(
        !ide.borrow().controller.primary_irq_pending(),
        "primary DMA must not run while only the secondary BMIDE engine is started"
    );
    assert!(!ide.borrow().controller.secondary_irq_pending());

    let bm_st_primary = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(bm_st_primary & 0x07, 0);
    let bm_st_secondary = ioports.read(bm_base + 8 + 2, 1) as u8;
    assert_eq!(bm_st_secondary & 0x07, 0);

    let mut out = vec![0u8; SECTOR_SIZE];
    mem.read_physical(dma_buf, &mut out);
    assert!(out.iter().all(|&b| b == 0xFF));

    // Now start the correct bus master engine and tick; the transfer should complete.
    ioports.write(bm_base, 1, 0x09);
    ide.borrow_mut().tick(&mut mem);

    let bm_st_primary = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(bm_st_primary & 0x07, 0x04);
    assert!(ide.borrow().controller.primary_irq_pending());
    assert!(!ide.borrow().controller.secondary_irq_pending());

    mem.read_physical(dma_buf, &mut out);
    assert_eq!(out, sector0);

    // Acknowledge the interrupt.
    let _ = ioports.read(PRIMARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.primary_irq_pending());
}

#[test]
fn ata_dma_success_on_secondary_channel_requires_secondary_bus_master_start() {
    let capacity = 4 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut sector0 = vec![0u8; SECTOR_SIZE];
    for (i, b) in sector0.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(5).wrapping_add(7);
    }
    disk.write_sectors(0, &sector0).unwrap();

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut()
        .controller
        .attach_secondary_master_ata(AtaDrive::new(Box::new(disk)).unwrap());
    ide.borrow_mut().config_mut().set_command(0x0005); // IO decode + Bus Master

    let mut ioports = IoPortBus::new();
    register_piix3_ide_ports(&mut ioports, ide.clone());

    let mut mem = Bus::new(0x20_000);
    let bm_base = ide.borrow().bus_master_base();

    let prd_addr = 0x1000u64;
    let dma_buf = 0x3000u64;

    // PRD entry: one 512-byte segment, end-of-table.
    mem.write_u32(prd_addr, dma_buf as u32);
    mem.write_u16(prd_addr + 4, SECTOR_SIZE as u16);
    mem.write_u16(prd_addr + 6, 0x8000);
    // Program both PRD pointers so a buggy implementation that consults the wrong channel's PRD
    // pointer will still touch the known DMA buffer.
    ioports.write(bm_base + 4, 4, prd_addr as u32);
    ioports.write(bm_base + 8 + 4, 4, prd_addr as u32);

    // Seed destination buffer so we can prove no DMA happens before the correct bus master engine
    // is started.
    mem.write_physical(dma_buf, &vec![0xFFu8; SECTOR_SIZE]);

    // READ DMA for LBA 0, 1 sector on the secondary channel.
    ioports.write(SECONDARY_PORTS.cmd_base + 6, 1, 0xE0);
    ioports.write(SECONDARY_PORTS.cmd_base + 2, 1, 1);
    ioports.write(SECONDARY_PORTS.cmd_base + 3, 1, 0);
    ioports.write(SECONDARY_PORTS.cmd_base + 4, 1, 0);
    ioports.write(SECONDARY_PORTS.cmd_base + 5, 1, 0);
    ioports.write(SECONDARY_PORTS.cmd_base + 7, 1, 0xC8);

    // Start the *primary* bus master engine (wrong channel) and tick. The secondary DMA request
    // must remain pending, with no memory writes and no interrupt.
    ioports.write(bm_base, 1, 0x09);
    ide.borrow_mut().tick(&mut mem);

    assert!(
        !ide.borrow().controller.secondary_irq_pending(),
        "secondary DMA must not run while only the primary BMIDE engine is started"
    );
    assert!(!ide.borrow().controller.primary_irq_pending());

    let bm_st_primary = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(bm_st_primary & 0x07, 0);

    let bm_st_secondary = ioports.read(bm_base + 8 + 2, 1) as u8;
    assert_eq!(bm_st_secondary & 0x07, 0);

    let mut out = vec![0u8; SECTOR_SIZE];
    mem.read_physical(dma_buf, &mut out);
    assert!(out.iter().all(|&b| b == 0xFF));

    // Now start the correct bus master engine and tick; the transfer should complete.
    ioports.write(bm_base + 8, 1, 0x09);
    ide.borrow_mut().tick(&mut mem);

    let bm_st_secondary = ioports.read(bm_base + 8 + 2, 1) as u8;
    assert_eq!(bm_st_secondary & 0x07, 0x04);
    assert!(ide.borrow().controller.secondary_irq_pending());
    assert!(!ide.borrow().controller.primary_irq_pending());

    mem.read_physical(dma_buf, &mut out);
    assert_eq!(out, sector0);

    // Acknowledge the interrupt.
    let _ = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.secondary_irq_pending());
}

#[test]
fn ata_nien_mask_on_primary_channel_does_not_mask_secondary_dma_irq() {
    let capacity = 4 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut sector0 = vec![0u8; SECTOR_SIZE];
    for (i, b) in sector0.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(17).wrapping_add(3);
    }
    disk.write_sectors(0, &sector0).unwrap();

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut()
        .controller
        .attach_secondary_master_ata(AtaDrive::new(Box::new(disk)).unwrap());
    ide.borrow_mut().config_mut().set_command(0x0005); // IO decode + Bus Master

    let mut ioports = IoPortBus::new();
    register_piix3_ide_ports(&mut ioports, ide.clone());

    // Mask IDE interrupts only on the primary channel.
    ioports.write(PRIMARY_PORTS.ctrl_base, 1, 0x02);

    let mut mem = Bus::new(0x20_000);
    let bm_base = ide.borrow().bus_master_base();

    let prd_addr = 0x1000u64;
    let dma_buf = 0x3000u64;

    // PRD entry: one 512-byte segment, end-of-table.
    mem.write_u32(prd_addr, dma_buf as u32);
    mem.write_u16(prd_addr + 4, SECTOR_SIZE as u16);
    mem.write_u16(prd_addr + 6, 0x8000);
    ioports.write(bm_base + 8 + 4, 4, prd_addr as u32);

    // READ DMA for LBA 0, 1 sector on the secondary channel.
    ioports.write(SECONDARY_PORTS.cmd_base + 6, 1, 0xE0);
    ioports.write(SECONDARY_PORTS.cmd_base + 2, 1, 1);
    ioports.write(SECONDARY_PORTS.cmd_base + 3, 1, 0);
    ioports.write(SECONDARY_PORTS.cmd_base + 4, 1, 0);
    ioports.write(SECONDARY_PORTS.cmd_base + 5, 1, 0);
    ioports.write(SECONDARY_PORTS.cmd_base + 7, 1, 0xC8);

    ioports.write(bm_base + 8, 1, 0x09);
    ide.borrow_mut().tick(&mut mem);

    let bm_st_secondary = ioports.read(bm_base + 8 + 2, 1) as u8;
    assert_eq!(bm_st_secondary & 0x07, 0x04);
    assert!(
        ide.borrow().controller.secondary_irq_pending(),
        "secondary DMA completion IRQ should not be masked by primary nIEN"
    );
    assert!(!ide.borrow().controller.primary_irq_pending());

    let mut out = vec![0u8; SECTOR_SIZE];
    mem.read_physical(dma_buf, &mut out);
    assert_eq!(out, sector0);

    let _ = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.secondary_irq_pending());
}

#[test]
fn ata_nien_mask_on_secondary_channel_does_not_mask_primary_dma_irq() {
    let capacity = 4 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut sector0 = vec![0u8; SECTOR_SIZE];
    for (i, b) in sector0.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(19).wrapping_add(7);
    }
    disk.write_sectors(0, &sector0).unwrap();

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut()
        .controller
        .attach_primary_master_ata(AtaDrive::new(Box::new(disk)).unwrap());
    ide.borrow_mut().config_mut().set_command(0x0005); // IO decode + Bus Master

    let mut ioports = IoPortBus::new();
    register_piix3_ide_ports(&mut ioports, ide.clone());

    // Mask IDE interrupts only on the secondary channel.
    ioports.write(SECONDARY_PORTS.ctrl_base, 1, 0x02);

    let mut mem = Bus::new(0x20_000);
    let bm_base = ide.borrow().bus_master_base();

    let prd_addr = 0x1000u64;
    let dma_buf = 0x3000u64;

    // PRD entry: one 512-byte segment, end-of-table.
    mem.write_u32(prd_addr, dma_buf as u32);
    mem.write_u16(prd_addr + 4, SECTOR_SIZE as u16);
    mem.write_u16(prd_addr + 6, 0x8000);
    ioports.write(bm_base + 4, 4, prd_addr as u32);

    // READ DMA for LBA 0, 1 sector on the primary channel.
    ioports.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    ioports.write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xC8);

    ioports.write(bm_base, 1, 0x09);
    ide.borrow_mut().tick(&mut mem);

    let bm_st_primary = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(bm_st_primary & 0x07, 0x04);
    assert!(
        ide.borrow().controller.primary_irq_pending(),
        "primary DMA completion IRQ should not be masked by secondary nIEN"
    );
    assert!(!ide.borrow().controller.secondary_irq_pending());

    let mut out = vec![0u8; SECTOR_SIZE];
    mem.read_physical(dma_buf, &mut out);
    assert_eq!(out, sector0);

    let _ = ioports.read(PRIMARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.primary_irq_pending());
}

#[test]
fn ata_dma_missing_prd_eot_sets_error_status_on_secondary_channel() {
    // Disk with recognizable first sector.
    let capacity = 4 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut sector0 = vec![0u8; SECTOR_SIZE];
    sector0[0..4].copy_from_slice(b"TEST");
    disk.write_sectors(0, &sector0).unwrap();

    let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
    ide.borrow_mut()
        .controller
        .attach_secondary_master_ata(AtaDrive::new(Box::new(disk)).unwrap());
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
    ioports.write(bm_base + 8 + 4, 4, prd_addr as u32);

    // READ DMA for LBA 0, 1 sector on the secondary channel.
    ioports.write(SECONDARY_PORTS.cmd_base + 6, 1, 0xE0);
    ioports.write(SECONDARY_PORTS.cmd_base + 2, 1, 1);
    ioports.write(SECONDARY_PORTS.cmd_base + 3, 1, 0);
    ioports.write(SECONDARY_PORTS.cmd_base + 4, 1, 0);
    ioports.write(SECONDARY_PORTS.cmd_base + 5, 1, 0);
    ioports.write(SECONDARY_PORTS.cmd_base + 7, 1, 0xC8);

    // Start bus master (direction = to memory) for the secondary channel.
    ioports.write(bm_base + 8, 1, 0x09);
    ide.borrow_mut().tick(&mut mem);

    assert!(ide.borrow().controller.secondary_irq_pending());
    assert!(!ide.borrow().controller.primary_irq_pending());

    let bm_st_primary = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(
        bm_st_primary & 0x07,
        0,
        "primary BMIDE status bits should be unaffected"
    );

    let bm_st_secondary = ioports.read(bm_base + 8 + 2, 1) as u8;
    assert_eq!(
        bm_st_secondary & 0x07,
        0x06,
        "secondary BMIDE status should have IRQ+ERR set and ACTIVE clear"
    );
    assert_ne!(
        bm_st_secondary & 0x20,
        0,
        "secondary BMIDE DMA capability bit for master should be set"
    );

    // Even though the PRD table is malformed, the DMA engine should still have written the full
    // sector before detecting the missing EOT bit.
    let mut out = vec![0u8; SECTOR_SIZE];
    mem.read_physical(dma_buf, &mut out);
    assert_eq!(out, sector0);

    // ATA status/error should reflect an aborted command.
    let st = ioports.read(SECONDARY_PORTS.ctrl_base, 1) as u8;
    assert_eq!(st & 0x80, 0, "BSY should be clear");
    assert_eq!(st & 0x08, 0, "DRQ should be clear");
    assert_ne!(st & 0x40, 0, "DRDY should be set");
    assert_ne!(st & 0x01, 0, "ERR should be set");
    assert!(
        ide.borrow().controller.secondary_irq_pending(),
        "reading ALT_STATUS must not clear the IDE IRQ latch"
    );
    assert_eq!(ioports.read(SECONDARY_PORTS.cmd_base + 1, 1) as u8, 0x04);

    // Reading STATUS acknowledges and clears the pending interrupt.
    let _ = ioports.read(SECONDARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.secondary_irq_pending());

    // BMIDE status bits persist until RW1C cleared by the guest.
    let bm_st_after = ioports.read(bm_base + 8 + 2, 1) as u8;
    assert_eq!(bm_st_after & 0x07, 0x06);
}

#[test]
fn ata_dma_prd_too_short_sets_error_status() {
    let capacity = 4 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let sector0: Vec<u8> = (0..SECTOR_SIZE as u32)
        .map(|i| (i as u8).wrapping_mul(7).wrapping_add(3))
        .collect();
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

    // PRD entry that is too short for a 512-byte request (256 bytes, EOT).
    mem.write_u32(prd_addr, dma_buf as u32);
    mem.write_u16(prd_addr + 4, 256);
    mem.write_u16(prd_addr + 6, 0x8000);
    ioports.write(bm_base + 4, 4, prd_addr as u32);

    // Seed the destination buffer so we can verify partial-transfer semantics.
    mem.write_physical(dma_buf, &vec![0xFFu8; SECTOR_SIZE]);

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
    assert_eq!(
        st & 0x07,
        0x06,
        "BMIDE status should have IRQ+ERR set and ACTIVE clear"
    );
    assert!(ide.borrow().controller.primary_irq_pending());

    // ATA status/error should reflect an aborted command.
    let st = ioports.read(PRIMARY_PORTS.ctrl_base, 1) as u8;
    assert_eq!(st & 0x80, 0, "BSY should be clear");
    assert_eq!(st & 0x08, 0, "DRQ should be clear");
    assert_ne!(st & 0x40, 0, "DRDY should be set");
    assert_ne!(st & 0x01, 0, "ERR should be set");
    assert!(
        ide.borrow().controller.primary_irq_pending(),
        "reading ALT_STATUS must not clear the IDE IRQ latch"
    );
    assert_eq!(ioports.read(PRIMARY_PORTS.cmd_base + 1, 1) as u8, 0x04);

    // Partial transfer: first PRD segment written, remainder untouched.
    let mut out = vec![0u8; SECTOR_SIZE];
    mem.read_physical(dma_buf, &mut out);
    assert_eq!(&out[..256], &sector0[..256]);
    assert!(out[256..].iter().all(|&b| b == 0xFF));

    // Reading Status acknowledges and clears the pending interrupt.
    let _ = ioports.read(PRIMARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.primary_irq_pending());
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

    // Seed the destination buffer; a direction mismatch should prevent any DMA writes.
    mem.write_physical(dma_buf, &vec![0xFFu8; SECTOR_SIZE]);

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
    assert_eq!(
        st & 0x07,
        0x06,
        "BMIDE status should have IRQ+ERR set and ACTIVE clear"
    );
    assert!(ide.borrow().controller.primary_irq_pending());

    // ATA status/error should reflect an aborted command.
    let st = ioports.read(PRIMARY_PORTS.ctrl_base, 1) as u8;
    assert_eq!(st & 0x80, 0, "BSY should be clear");
    assert_eq!(st & 0x08, 0, "DRQ should be clear");
    assert_ne!(st & 0x40, 0, "DRDY should be set");
    assert_ne!(st & 0x01, 0, "ERR should be set");
    assert!(
        ide.borrow().controller.primary_irq_pending(),
        "reading ALT_STATUS must not clear the IDE IRQ latch"
    );
    assert_eq!(ioports.read(PRIMARY_PORTS.cmd_base + 1, 1) as u8, 0x04);

    // Direction mismatch should prevent any DMA writes.
    let mut out = vec![0u8; SECTOR_SIZE];
    mem.read_physical(dma_buf, &mut out);
    assert!(out.iter().all(|&b| b == 0xFF));

    // Reading Status acknowledges and clears the pending interrupt.
    let _ = ioports.read(PRIMARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.primary_irq_pending());
}

#[test]
fn ata_write_dma_missing_prd_eot_sets_error_status_and_does_not_write_disk() {
    let shared = Arc::new(Mutex::new(RecordingDisk::new(1)));
    let disk = SharedRecordingDisk(shared.clone());

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

    // Fill one sector worth of DMA source data.
    mem.write_physical(dma_buf, &vec![0xA5u8; SECTOR_SIZE]);

    // Malformed PRD entry without EOT flag, but long enough for the whole transfer.
    mem.write_u32(prd_addr, dma_buf as u32);
    mem.write_u16(prd_addr + 4, SECTOR_SIZE as u16);
    mem.write_u16(prd_addr + 6, 0x0000);
    ioports.write(bm_base + 4, 4, prd_addr as u32);

    // WRITE DMA for LBA 0, 1 sector.
    ioports.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    ioports.write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xCA); // WRITE DMA

    // Start bus master (direction = from memory).
    ioports.write(bm_base, 1, 0x01);
    ide.borrow_mut().tick(&mut mem);

    let st = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(
        st & 0x07,
        0x06,
        "BMIDE status should have IRQ+ERR set and ACTIVE clear"
    );

    let st = ioports.read(PRIMARY_PORTS.ctrl_base, 1) as u8;
    assert_eq!(st & 0x80, 0, "BSY should be clear");
    assert_eq!(st & 0x08, 0, "DRQ should be clear");
    assert_ne!(st & 0x40, 0, "DRDY should be set");
    assert_ne!(st & 0x01, 0, "ERR should be set");
    assert_eq!(ioports.read(PRIMARY_PORTS.cmd_base + 1, 1) as u8, 0x04);

    assert!(ide.borrow().controller.primary_irq_pending());
    let _ = ioports.read(PRIMARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.primary_irq_pending());

    let inner = shared.lock().unwrap();
    assert_eq!(
        inner.last_write_lba, None,
        "DMA PRD error should prevent committing the write to disk"
    );
    assert_eq!(inner.last_write_len, 0);
}

#[test]
fn ata_write_dma_prd_too_short_sets_error_status_and_does_not_write_disk() {
    let shared = Arc::new(Mutex::new(RecordingDisk::new(1)));
    let disk = SharedRecordingDisk(shared.clone());

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

    // Fill one sector worth of DMA source data.
    mem.write_physical(dma_buf, &vec![0xA5u8; SECTOR_SIZE]);

    // PRD entry that is too short for a 512-byte request (256 bytes, EOT).
    mem.write_u32(prd_addr, dma_buf as u32);
    mem.write_u16(prd_addr + 4, 256);
    mem.write_u16(prd_addr + 6, 0x8000);
    ioports.write(bm_base + 4, 4, prd_addr as u32);

    // WRITE DMA for LBA 0, 1 sector.
    ioports.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    ioports.write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xCA);

    ioports.write(bm_base, 1, 0x01);
    ide.borrow_mut().tick(&mut mem);

    let st = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(st & 0x07, 0x06);

    let st = ioports.read(PRIMARY_PORTS.ctrl_base, 1) as u8;
    assert_eq!(st & 0x80, 0);
    assert_eq!(st & 0x08, 0);
    assert_ne!(st & 0x40, 0);
    assert_ne!(st & 0x01, 0);
    assert_eq!(ioports.read(PRIMARY_PORTS.cmd_base + 1, 1) as u8, 0x04);

    assert!(ide.borrow().controller.primary_irq_pending());
    let _ = ioports.read(PRIMARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.primary_irq_pending());

    let inner = shared.lock().unwrap();
    assert_eq!(inner.last_write_lba, None);
}

#[test]
fn ata_write_dma_direction_mismatch_sets_error_status_and_does_not_write_disk() {
    let shared = Arc::new(Mutex::new(RecordingDisk::new(1)));
    let disk = SharedRecordingDisk(shared.clone());

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

    // Fill one sector worth of DMA source data.
    mem.write_physical(dma_buf, &vec![0xA5u8; SECTOR_SIZE]);

    // Valid PRD entry (512 bytes, EOT).
    mem.write_u32(prd_addr, dma_buf as u32);
    mem.write_u16(prd_addr + 4, SECTOR_SIZE as u16);
    mem.write_u16(prd_addr + 6, 0x8000);
    ioports.write(bm_base + 4, 4, prd_addr as u32);

    // WRITE DMA for LBA 0, 1 sector.
    ioports.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    ioports.write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    ioports.write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    ioports.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xCA);

    // Start bus master with direction=read (device -> memory), mismatching the write request.
    ioports.write(bm_base, 1, 0x09);
    ide.borrow_mut().tick(&mut mem);

    let st = ioports.read(bm_base + 2, 1) as u8;
    assert_eq!(st & 0x07, 0x06);

    let st = ioports.read(PRIMARY_PORTS.ctrl_base, 1) as u8;
    assert_eq!(st & 0x80, 0);
    assert_eq!(st & 0x08, 0);
    assert_ne!(st & 0x40, 0);
    assert_ne!(st & 0x01, 0);
    assert_eq!(ioports.read(PRIMARY_PORTS.cmd_base + 1, 1) as u8, 0x04);

    assert!(ide.borrow().controller.primary_irq_pending());
    let _ = ioports.read(PRIMARY_PORTS.cmd_base + 7, 1);
    assert!(!ide.borrow().controller.primary_irq_pending());

    let inner = shared.lock().unwrap();
    assert_eq!(inner.last_write_lba, None);
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

    assert_eq!(
        cfg.bar_range(0).unwrap().base,
        u64::from(PRIMARY_PORTS.cmd_base)
    );
    assert_eq!(
        cfg.bar_range(1).unwrap().base,
        u64::from(PRIMARY_PORTS.ctrl_base - 2)
    );
    assert_eq!(
        cfg.bar_range(2).unwrap().base,
        u64::from(SECONDARY_PORTS.cmd_base)
    );
    assert_eq!(
        cfg.bar_range(3).unwrap().base,
        u64::from(SECONDARY_PORTS.ctrl_base - 2)
    );
    assert_eq!(
        cfg.bar_range(4).unwrap().base,
        u64::from(Piix3IdePciDevice::DEFAULT_BUS_MASTER_BASE)
    );

    assert_eq!(
        cfg.command() & 0x1,
        0x1,
        "bios_post should enable IO decoding"
    );
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
    ide.borrow_mut().controller.attach_secondary_master_atapi(
        aero_devices_storage::atapi::AtapiCdrom::new(Some(Box::new(iso))),
    );
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
    assert_eq!(
        bm_status & 0x01,
        0,
        "BMIDE status active bit should be clear"
    );

    // ATA status should report DRDY and not be busy/DRQ.
    let st = ioports2.read(PRIMARY_PORTS.cmd_base + 7, 1) as u8;
    assert_ne!(st & 0x40, 0, "DRDY should be set after DMA completion");
    assert_eq!(
        st & 0x88,
        0,
        "BSY and DRQ should be clear after DMA completion"
    );

    // Reading STATUS clears the pending IRQ.
    assert!(!restored.borrow().controller.primary_irq_pending());
}
