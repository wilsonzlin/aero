#![cfg(not(target_arch = "wasm32"))]

use aero_devices::pci::{profile, PciInterruptPin, PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT};
use aero_devices_storage::ata::{AtaDrive, ATA_CMD_READ_DMA_EXT, ATA_CMD_WRITE_DMA_EXT};
use aero_devices_storage::atapi::{AtapiCdrom, IsoBackend};
use aero_devices_storage::pci_ide::{PRIMARY_PORTS, SECONDARY_PORTS};
use aero_io_snapshot::io::storage::state::DiskControllersSnapshot;
use aero_machine::{Machine, MachineConfig};
use aero_platform::interrupts::{InterruptController, PlatformInterruptMode, PlatformInterrupts};
use aero_snapshot as snapshot;
use aero_storage::{DiskError, Result as DiskResult, VirtualDisk, SECTOR_SIZE};
use pretty_assertions::assert_eq;
use std::io::{self, Read, Seek, SeekFrom};
use std::sync::{Arc, Mutex};

// AHCI register offsets (HBA + port 0).
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

fn pci_cfg_addr(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    0x8000_0000
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | ((offset as u32) & 0xFC)
}

fn write_cfg_u16(m: &mut Machine, bus: u8, device: u8, function: u8, offset: u8, value: u16) {
    m.io_write(
        PCI_CFG_ADDR_PORT,
        4,
        pci_cfg_addr(bus, device, function, offset),
    );
    m.io_write(PCI_CFG_DATA_PORT, 2, u32::from(value));
}

fn read_cfg_u32(m: &mut Machine, bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    m.io_write(
        PCI_CFG_ADDR_PORT,
        4,
        pci_cfg_addr(bus, device, function, offset),
    );
    m.io_read(PCI_CFG_DATA_PORT, 4)
}

fn program_ioapic_entry(ints: &mut PlatformInterrupts, gsi: u32, low: u32, high: u32) {
    let redtbl_low = 0x10u32 + gsi * 2;
    let redtbl_high = redtbl_low + 1;
    ints.ioapic_mmio_write(0x00, redtbl_low);
    ints.ioapic_mmio_write(0x10, low);
    ints.ioapic_mmio_write(0x00, redtbl_high);
    ints.ioapic_mmio_write(0x10, high);
}

fn write_cmd_header(
    m: &mut Machine,
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
    m.write_physical_u32(addr, flags);
    m.write_physical_u32(addr + 4, 0); // PRDBC
    m.write_physical_u32(addr + 8, ctba as u32);
    m.write_physical_u32(addr + 12, (ctba >> 32) as u32);
}

fn write_prdt(m: &mut Machine, ctba: u64, entry: usize, dba: u64, dbc: u32) {
    let addr = ctba + 0x80 + (entry as u64) * 16;
    m.write_physical_u32(addr, dba as u32);
    m.write_physical_u32(addr + 4, (dba >> 32) as u32);
    m.write_physical_u32(addr + 8, 0);
    // DBC field stores byte_count-1 in bits 0..21.
    m.write_physical_u32(addr + 12, (dbc - 1) & 0x003F_FFFF);
}

fn write_cfis(m: &mut Machine, ctba: u64, command: u8, lba: u64, count: u16) {
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

    m.write_physical(ctba, &cfis);
}

fn send_atapi_packet(
    m: &mut Machine,
    base: u16,
    features: u8,
    pkt: &[u8; 12],
    byte_count: u16,
) {
    m.io_write(base + 1, 1, features as u32);
    m.io_write(base + 4, 1, (byte_count & 0xFF) as u32);
    m.io_write(base + 5, 1, (byte_count >> 8) as u32);
    m.io_write(base + 7, 1, 0xA0); // PACKET
    for i in 0..6 {
        let w = u16::from_le_bytes([pkt[i * 2], pkt[i * 2 + 1]]);
        m.io_write(base, 2, w as u32);
    }
}

#[derive(Clone)]
struct SharedDisk {
    data: Arc<Mutex<Vec<u8>>>,
    capacity: u64,
}

impl SharedDisk {
    fn new(sectors: usize) -> Self {
        let capacity = sectors
            .checked_mul(SECTOR_SIZE)
            .expect("disk capacity overflow") as u64;
        Self {
            data: Arc::new(Mutex::new(vec![0u8; capacity as usize])),
            capacity,
        }
    }
}

impl VirtualDisk for SharedDisk {
    fn capacity_bytes(&self) -> u64 {
        self.capacity
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> DiskResult<()> {
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

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> DiskResult<()> {
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

    fn flush(&mut self) -> DiskResult<()> {
        Ok(())
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
fn machine_storage_snapshot_roundtrip_preserves_controllers_and_allows_backend_reattach() {
    const RAM_SIZE: u64 = 2 * 1024 * 1024;
    const AHCI_VECTOR: u8 = 0x70;

    let mut cfg = MachineConfig::win7_storage(RAM_SIZE);
    // Keep the machine minimal and deterministic for snapshot tests.
    cfg.enable_vga = false;
    cfg.enable_serial = false;
    cfg.enable_i8042 = false;
    cfg.enable_a20_gate = false;
    cfg.enable_reset_ctrl = false;

    let ahci_disk = SharedDisk::new(64);
    let ide_disk = SharedDisk::new(16);

    // Seed AHCI disk with a known pattern.
    let mut ahci_seed = vec![0u8; SECTOR_SIZE];
    ahci_seed[0..4].copy_from_slice(&[9, 8, 7, 6]);
    ahci_disk.clone().write_sectors(4, &ahci_seed).unwrap();

    // Seed IDE disk sector 0 so a PIO read has a visible prefix.
    let mut ide_seed = vec![0u8; SECTOR_SIZE];
    ide_seed[0..4].copy_from_slice(b"BOOT");
    ide_disk.clone().write_sectors(0, &ide_seed).unwrap();

    let mut iso = MemIso::new(2);
    iso.data[2048..2053].copy_from_slice(b"WORLD");

    // --- Setup: build a machine with AHCI + IDE, attach disks, and enable decoding/DMA via PCI
    // command registers (through the standard 0xCF8/0xCFC config ports).
    let mut src = Machine::new(cfg.clone()).unwrap();

    src.attach_ahci_drive_port0(AtaDrive::new(Box::new(ahci_disk.clone())).unwrap());
    src.attach_ide_primary_master_drive(AtaDrive::new(Box::new(ide_disk.clone())).unwrap());
    src.attach_ide_secondary_master_atapi(AtapiCdrom::new(Some(Box::new(iso))));

    // Enable memory decoding + bus mastering for AHCI (required for MMIO + DMA).
    {
        let bdf = profile::SATA_AHCI_ICH9.bdf;
        write_cfg_u16(&mut src, bdf.bus, bdf.device, bdf.function, 0x04, 0x0006);
    }
    // Enable I/O decoding + bus mastering for IDE.
    {
        let bdf = profile::IDE_PIIX3.bdf;
        write_cfg_u16(&mut src, bdf.bus, bdf.device, bdf.function, 0x04, 0x0005);
    }

    let ahci_gsi = {
        let pci_intx = src.pci_intx_router().expect("pc platform enabled");
        let gsi = pci_intx
            .borrow()
            .gsi_for_intx(profile::SATA_AHCI_ICH9.bdf, PciInterruptPin::IntA);
        gsi
    };

    // Route AHCI INTx through the IOAPIC to a known vector (active-low + level-triggered).
    {
        let interrupts = src.platform_interrupts().expect("pc platform enabled");
        let mut ints = interrupts.borrow_mut();
        ints.set_mode(PlatformInterruptMode::Apic);
        let low = u32::from(AHCI_VECTOR) | (1 << 13) | (1 << 15);
        program_ioapic_entry(&mut ints, ahci_gsi, low, 0);
    }

    let ahci_abar = {
        // BAR5 at offset 0x24. Mask off the low flag bits (MMIO BAR).
        let bdf = profile::SATA_AHCI_ICH9.bdf;
        u64::from(read_cfg_u32(
            &mut src,
            bdf.bus,
            bdf.device,
            bdf.function,
            0x24,
        ) & 0xFFFF_FFF0)
    };
    assert!(ahci_abar != 0, "AHCI BAR5 must be programmed");

    // --- AHCI: issue a READ DMA EXT to leave the controller in a non-default state (pending INTx).
    let clb = 0x1000u64;
    let fb = 0x2000u64;
    let ctba = 0x3000u64;
    let data_buf = 0x4000u64;

    src.write_physical_u32(ahci_abar + PORT_BASE + PORT_REG_CLB, clb as u32);
    src.write_physical_u32(ahci_abar + PORT_BASE + PORT_REG_CLBU, (clb >> 32) as u32);
    src.write_physical_u32(ahci_abar + PORT_BASE + PORT_REG_FB, fb as u32);
    src.write_physical_u32(ahci_abar + PORT_BASE + PORT_REG_FBU, (fb >> 32) as u32);
    src.write_physical_u32(ahci_abar + HBA_GHC, GHC_IE | GHC_AE);
    src.write_physical_u32(ahci_abar + PORT_BASE + PORT_REG_IE, PORT_IS_DHRS);
    src.write_physical_u32(
        ahci_abar + PORT_BASE + PORT_REG_CMD,
        PORT_CMD_ST | PORT_CMD_FRE,
    );

    write_cmd_header(&mut src, clb, 0, ctba, 1, false);
    write_cfis(&mut src, ctba, ATA_CMD_READ_DMA_EXT, 4, 1);
    write_prdt(&mut src, ctba, 0, data_buf, SECTOR_SIZE as u32);
    src.write_physical_u32(ahci_abar + PORT_BASE + PORT_REG_CI, 1);

    src.process_ahci();
    src.poll_pci_intx_lines();

    assert!(src.ahci().unwrap().borrow().intx_level());
    {
        let interrupts = src.platform_interrupts().expect("pc platform enabled");
        assert_eq!(interrupts.borrow().get_pending(), Some(AHCI_VECTOR));
    }

    let mut out = [0u8; 4];
    out.copy_from_slice(&src.read_physical_bytes(data_buf, 4));
    assert_eq!(out, [9, 8, 7, 6]);

    // --- IDE: issue a PIO READ and leave the transfer mid-sector.
    src.io_write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    src.io_write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    src.io_write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    src.io_write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    src.io_write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    src.io_write(PRIMARY_PORTS.cmd_base + 7, 1, 0x20);

    // Consume the first 4 bytes ("BOOT") but leave the transfer in progress.
    let w0 = src.io_read(PRIMARY_PORTS.cmd_base, 2) as u16;
    let w1 = src.io_read(PRIMARY_PORTS.cmd_base, 2) as u16;
    let mut first4 = [0u8; 4];
    first4[0..2].copy_from_slice(&w0.to_le_bytes());
    first4[2..4].copy_from_slice(&w1.to_le_bytes());
    assert_eq!(&first4, b"BOOT");

    // Trigger ATAPI UNIT ATTENTION and leave sense state pending.
    src.io_write(SECONDARY_PORTS.cmd_base + 6, 1, 0xA0);
    let tur = [0u8; 12];
    send_atapi_packet(&mut src, SECONDARY_PORTS.cmd_base, 0, &tur, 0);
    let _ = src.io_read(SECONDARY_PORTS.cmd_base + 7, 1);

    // --- Snapshot.
    let snapshot = src.take_snapshot_full().unwrap();

    // Canonical snapshot encoding: store storage controller(s) under a single DISK_CONTROLLER entry
    // using the DSKC wrapper (per docs/16-snapshots.md). This avoids `(id, version, flags)`
    // collisions when multiple controllers share the same io-snapshot version.
    {
        let mut r = io::Cursor::new(snapshot.as_slice());
        let index = snapshot::inspect_snapshot(&mut r).expect("snapshot should be inspectable");
        let devices = index
            .sections
            .iter()
            .find(|s| s.id == snapshot::SectionId::DEVICES)
            .expect("snapshot should contain a DEVICES section");
        r.seek(SeekFrom::Start(devices.offset))
            .expect("seek to DEVICES payload");
        let mut limited = r.take(devices.len);
        let mut count_buf = [0u8; 4];
        limited.read_exact(&mut count_buf).expect("read device count");
        let count = u32::from_le_bytes(count_buf) as usize;
        let mut disk_controller_entries = Vec::new();
        for _ in 0..count {
            let state = snapshot::DeviceState::decode(&mut limited, devices.len)
                .expect("decode device entry");
            if state.id == snapshot::DeviceId::DISK_CONTROLLER {
                disk_controller_entries.push(state);
            }
        }
        assert_eq!(
            disk_controller_entries.len(),
            1,
            "snapshot should contain exactly one DISK_CONTROLLER entry (DSKC wrapper)"
        );
        assert_eq!(
            disk_controller_entries[0]
                .data
                .get(8..12)
                .expect("io-snapshot header must contain a device id"),
            b"DSKC",
            "DISK_CONTROLLER entry should be encoded as a DSKC wrapper"
        );

        let mut wrapper = DiskControllersSnapshot::default();
        snapshot::apply_io_snapshot_to_device(&disk_controller_entries[0], &mut wrapper)
            .expect("decode DSKC wrapper");
        assert!(
            wrapper
                .controllers()
                .contains_key(&profile::SATA_AHCI_ICH9.bdf.pack_u16()),
            "DSKC wrapper should contain the ICH9 AHCI controller entry"
        );
        assert!(
            wrapper
                .controllers()
                .contains_key(&profile::IDE_PIIX3.bdf.pack_u16()),
            "DSKC wrapper should contain the PIIX3 IDE controller entry"
        );
    }

    // --- Restore into a fresh machine instance (backends must be reattached explicitly).
    let mut restored = Machine::new(cfg).unwrap();
    restored.restore_snapshot_bytes(&snapshot).unwrap();

    let ahci_abar2 = {
        let bdf = profile::SATA_AHCI_ICH9.bdf;
        u64::from(read_cfg_u32(
            &mut restored,
            bdf.bus,
            bdf.device,
            bdf.function,
            0x24,
        ) & 0xFFFF_FFF0)
    };
    assert_eq!(
        ahci_abar2, ahci_abar,
        "AHCI BAR5 should survive snapshot/restore"
    );

    // Verify key AHCI register state and that the pending INTx survived restore.
    assert_eq!(
        restored.read_physical_u32(ahci_abar2 + HBA_GHC),
        GHC_IE | GHC_AE,
        "AHCI GHC should survive snapshot/restore"
    );
    assert_eq!(
        restored.read_physical_u32(ahci_abar2 + PORT_BASE + PORT_REG_CLB),
        clb as u32,
        "AHCI PxCLB should survive snapshot/restore"
    );
    assert!(restored.ahci().unwrap().borrow().intx_level());
    {
        let interrupts = restored.platform_interrupts().expect("pc platform enabled");
        assert_eq!(interrupts.borrow().get_pending(), Some(AHCI_VECTOR));
    }

    // Reattach storage backends (controller load_state intentionally drops them).
    restored.attach_ahci_drive_port0(AtaDrive::new(Box::new(ahci_disk.clone())).unwrap());
    restored.attach_ide_primary_master_drive(AtaDrive::new(Box::new(ide_disk.clone())).unwrap());
    restored.attach_ide_secondary_master_atapi_backend_for_restore(Box::new(MemIso::new(2)));

    // Acknowledge the restored AHCI interrupt, clear it in the device, and ensure it does not get
    // re-delivered once deasserted.
    {
        let interrupts = restored.platform_interrupts().expect("pc platform enabled");
        interrupts.borrow_mut().acknowledge(AHCI_VECTOR);
        assert_eq!(interrupts.borrow().get_pending(), None);
    }
    restored.write_physical_u32(ahci_abar2 + PORT_BASE + PORT_REG_IS, PORT_IS_DHRS);
    restored.poll_pci_intx_lines();
    {
        let interrupts = restored.platform_interrupts().expect("pc platform enabled");
        interrupts.borrow_mut().eoi(AHCI_VECTOR);
        assert_eq!(interrupts.borrow().get_pending(), None);
    }

    // --- Continue with AHCI: perform a WRITE DMA EXT after restore.
    let write_buf = 0x5000u64;
    let mut sector = vec![0u8; SECTOR_SIZE];
    sector[0..4].copy_from_slice(&[1, 2, 3, 4]);
    restored.write_physical(write_buf, &sector);

    write_cmd_header(&mut restored, clb, 0, ctba, 1, true);
    write_cfis(&mut restored, ctba, ATA_CMD_WRITE_DMA_EXT, 5, 1);
    write_prdt(&mut restored, ctba, 0, write_buf, SECTOR_SIZE as u32);
    restored.write_physical_u32(ahci_abar2 + PORT_BASE + PORT_REG_CI, 1);

    restored.process_ahci();
    restored.poll_pci_intx_lines();
    assert!(restored.ahci().unwrap().borrow().intx_level());

    let mut verify = vec![0u8; SECTOR_SIZE];
    ahci_disk.clone().read_sectors(5, &mut verify).unwrap();
    assert_eq!(&verify[..4], &[1, 2, 3, 4]);

    // --- Continue the restored IDE PIO read: read the rest of the sector.
    let mut buf = vec![0u8; SECTOR_SIZE];
    buf[0..4].copy_from_slice(b"BOOT");
    for i in 2..(SECTOR_SIZE / 2) {
        let w = restored.io_read(PRIMARY_PORTS.cmd_base, 2) as u16;
        buf[i * 2..i * 2 + 2].copy_from_slice(&w.to_le_bytes());
    }
    assert_eq!(&buf[0..4], b"BOOT");

    // Reading status clears the pending IRQ.
    let _ = restored.io_read(PRIMARY_PORTS.cmd_base + 7, 1);
    assert!(!restored
        .ide()
        .unwrap()
        .borrow()
        .controller
        .primary_irq_pending());

    // --- Verify ATAPI sense state still reports UNIT ATTENTION / medium changed.
    restored.io_write(SECONDARY_PORTS.cmd_base + 6, 1, 0xA0);
    let mut req_sense = [0u8; 12];
    req_sense[0] = 0x03;
    req_sense[4] = 18;
    send_atapi_packet(&mut restored, SECONDARY_PORTS.cmd_base, 0, &req_sense, 18);

    let mut sense = [0u8; 18];
    for i in 0..(18 / 2) {
        let w = restored.io_read(SECONDARY_PORTS.cmd_base, 2) as u16;
        sense[i * 2..i * 2 + 2].copy_from_slice(&w.to_le_bytes());
    }
    assert_eq!(sense[2] & 0x0F, 0x06); // UNIT ATTENTION
    assert_eq!(sense[12], 0x28); // MEDIUM CHANGED

    // --- Perform an IDE PIO write after restore to ensure the reattached disk backend is used.
    restored.io_write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    restored.io_write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    restored.io_write(PRIMARY_PORTS.cmd_base + 3, 1, 1);
    restored.io_write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    restored.io_write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    restored.io_write(PRIMARY_PORTS.cmd_base + 7, 1, 0x30); // WRITE SECTORS

    restored.io_write(
        PRIMARY_PORTS.cmd_base,
        2,
        u32::from(u16::from_le_bytes([5, 6])),
    );
    restored.io_write(
        PRIMARY_PORTS.cmd_base,
        2,
        u32::from(u16::from_le_bytes([7, 8])),
    );
    for _ in 0..((SECTOR_SIZE / 2) - 2) {
        restored.io_write(PRIMARY_PORTS.cmd_base, 2, 0);
    }

    let mut verify = vec![0u8; SECTOR_SIZE];
    ide_disk.clone().read_sectors(1, &mut verify).unwrap();
    assert_eq!(&verify[..4], &[5, 6, 7, 8]);
}
