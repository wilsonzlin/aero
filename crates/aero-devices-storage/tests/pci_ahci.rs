use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use aero_devices::pci::profile;
use aero_devices::pci::PciDevice as _;
use aero_devices_storage::ata::{AtaDrive, ATA_CMD_IDENTIFY, ATA_CMD_READ_DMA_EXT};
use aero_devices_storage::AhciPciDevice;
use aero_storage::{MemBackend, RawDisk, VirtualDisk, SECTOR_SIZE};
use memory::{Bus, MemoryBus};

const HBA_CAP: u64 = 0x00;
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
const PORT_IS_TFES: u32 = 1 << 30;

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
fn pci_config_header_fields_and_bar5_size_probe() {
    let mut dev = AhciPciDevice::new(1);

    let expected = profile::SATA_AHCI_ICH9;
    let id = dev.config().vendor_device_id();
    assert_eq!(id.vendor_id, expected.vendor_id);
    assert_eq!(id.device_id, expected.device_id);

    let class = dev.config().class_code();
    let class_code =
        ((class.class as u32) << 16) | ((class.subclass as u32) << 8) | (class.prog_if as u32);
    assert_eq!(class_code, 0x010601);

    // BAR5 (ABAR) size probing.
    let bar5_off = 0x10u16 + 5 * 4;
    dev.config_mut().write(bar5_off, 4, 0xFFFF_FFFF);
    let got = dev.config_mut().read(bar5_off, 4);
    assert_eq!(got, 0xFFFF_E000);
}

#[test]
fn reset_clears_registers_and_irq_but_preserves_attached_drive() {
    let dropped = Arc::new(AtomicBool::new(false));
    let capacity = 8 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut sector0 = vec![0u8; SECTOR_SIZE];
    sector0[0..4].copy_from_slice(b"BOOT");
    sector0[510] = 0x55;
    sector0[511] = 0xAA;
    disk.write_sectors(0, &sector0).unwrap();

    let disk = DropDetectDisk {
        inner: disk,
        dropped: dropped.clone(),
    };

    let mut dev = AhciPciDevice::new(1);
    dev.attach_drive(0, AtaDrive::new(Box::new(disk)).unwrap());

    // Enable MMIO + bus mastering so DMA and interrupts are permitted.
    dev.config_mut().set_command(0x0006); // MEM + BUSMASTER

    let mut mem = Bus::new(0x20_000);

    // Basic port programming.
    let clb = 0x1000u64;
    let fb = 0x2000u64;
    let ctba = 0x3000u64;
    let read_buf = 0x4000u64;

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

    // READ DMA EXT for LBA 0, 1 sector. This should assert INTx.
    write_cmd_header(&mut mem, clb, 0, ctba, 1, false);
    write_cfis(&mut mem, ctba, ATA_CMD_READ_DMA_EXT, 0, 1);
    write_prdt(&mut mem, ctba, 0, read_buf, SECTOR_SIZE as u32);

    dev.mmio_write(PORT_BASE + PORT_REG_CI, 4, 1);
    dev.process(&mut mem);
    assert!(dev.intx_level());

    // Reset the device model in-place. This should clear IRQ + guest-visible registers but keep
    // the attached drive.
    dev.reset();
    assert!(!dev.intx_level(), "reset should deassert legacy INTx");
    assert!(
        !dropped.load(Ordering::SeqCst),
        "reset dropped the attached disk backend"
    );

    // Re-enable MMIO + DMA after reset so we can observe register state and issue commands again.
    dev.config_mut().set_command(0x0006); // MEM + BUSMASTER

    // HBA/port registers should be reset to baseline.
    let ghc = dev.mmio_read(HBA_GHC, 4) as u32;
    assert_ne!(ghc & GHC_AE, 0, "AHCI should remain enabled after reset");
    assert_eq!(ghc & GHC_IE, 0, "global interrupt enable should be cleared");

    assert_eq!(dev.mmio_read(PORT_BASE + PORT_REG_CLB, 4), 0);
    assert_eq!(dev.mmio_read(PORT_BASE + PORT_REG_FB, 4), 0);
    assert_eq!(dev.mmio_read(PORT_BASE + PORT_REG_IS, 4), 0);

    // Re-program the port and confirm the drive is still usable after reset.
    let read_buf2 = 0x5000u64;
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
    write_cfis(&mut mem, ctba, ATA_CMD_READ_DMA_EXT, 0, 1);
    write_prdt(&mut mem, ctba, 0, read_buf2, SECTOR_SIZE as u32);

    dev.mmio_write(PORT_BASE + PORT_REG_CI, 4, 1);
    dev.process(&mut mem);

    let mut out = [0u8; SECTOR_SIZE];
    mem.read_physical(read_buf2, &mut out);
    assert_eq!(&out[0..4], b"BOOT");
    assert_eq!(&out[510..512], &[0x55, 0xAA]);

    // Detaching the drive should drop the backend (sanity check).
    dev.detach_drive(0);
    assert!(
        dropped.load(Ordering::SeqCst),
        "detaching the drive should drop the disk backend"
    );
}

#[test]
fn mmio_requires_pci_memory_space_enable() {
    let mut dev = AhciPciDevice::new(1);

    // Memory Space Enable (command bit 1) gates MMIO decoding: reads float high and writes are
    // ignored.
    assert_eq!(dev.mmio_read(HBA_CAP, 4), 0xFFFF_FFFF);

    // Try to enable global interrupts while MMIO decoding is disabled; this write should not take
    // effect.
    dev.mmio_write(HBA_GHC, 4, u64::from(GHC_AE | GHC_IE));

    // Enable MMIO decoding and observe real register values again.
    dev.config_mut().set_command(0x0002); // MEM
    assert_ne!(dev.mmio_read(HBA_CAP, 4), 0xFFFF_FFFF);

    // GHC.IE should still be clear because the earlier write was ignored.
    let ghc = dev.mmio_read(HBA_GHC, 4) as u32;
    assert_eq!(ghc & GHC_IE, 0);
}

#[test]
fn mmio_size0_is_noop() {
    let mut dev = AhciPciDevice::new(1);

    // Size-0 reads should be treated as a no-op and return 0 (even when MMIO decoding is disabled).
    assert_eq!(dev.mmio_read(HBA_CAP, 0), 0);

    // Enable MMIO decoding and ensure size-0 remains a no-op.
    dev.config_mut().set_command(0x0002); // MEM
    assert_eq!(dev.mmio_read(HBA_CAP, 0), 0);

    // Size-0 writes should have no effect.
    let before = dev.mmio_read(HBA_GHC, 4);
    dev.mmio_write(HBA_GHC, 0, u64::from(GHC_AE | GHC_IE));
    let after = dev.mmio_read(HBA_GHC, 4);
    assert_eq!(after, before);
}

#[test]
fn mmio_identify_and_read_dma_ext_via_pci_wrapper() {
    let capacity = 8 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut sector0 = vec![0u8; SECTOR_SIZE];
    sector0[0..4].copy_from_slice(b"BOOT");
    sector0[510] = 0x55;
    sector0[511] = 0xAA;
    disk.write_sectors(0, &sector0).unwrap();

    let mut dev = AhciPciDevice::new(1);
    dev.attach_drive(0, AtaDrive::new(Box::new(disk)).unwrap());

    // Enable bus mastering so DMA is permitted.
    dev.config_mut().set_command(0x0006); // MEM + BUSMASTER

    let mut mem = Bus::new(0x20_000);

    // Basic port programming.
    let clb = 0x1000u64;
    let fb = 0x2000u64;
    let ctba = 0x3000u64;
    let identify_buf = 0x4000u64;

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

    // IDENTIFY DMA.
    write_cmd_header(&mut mem, clb, 0, ctba, 1, false);
    write_cfis(&mut mem, ctba, ATA_CMD_IDENTIFY, 0, 0);
    write_prdt(&mut mem, ctba, 0, identify_buf, SECTOR_SIZE as u32);

    dev.mmio_write(PORT_BASE + PORT_REG_CI, 4, 1);
    dev.process(&mut mem);

    assert!(dev.intx_level());
    assert_eq!(dev.mmio_read(PORT_BASE + PORT_REG_CI, 4) as u32, 0);
    assert_ne!(
        dev.mmio_read(PORT_BASE + PORT_REG_IS, 4) as u32 & PORT_IS_DHRS,
        0
    );

    let mut identify = [0u8; SECTOR_SIZE];
    mem.read_physical(identify_buf, &mut identify);
    assert_eq!(identify[0], 0x40);

    // Clear interrupt and ensure INTx deasserts.
    dev.mmio_write(PORT_BASE + PORT_REG_IS, 4, u64::from(PORT_IS_DHRS));
    assert!(!dev.intx_level());

    // READ DMA EXT for LBA 0, 1 sector.
    let read_buf = 0x5000u64;
    write_cmd_header(&mut mem, clb, 0, ctba, 1, false);
    write_cfis(&mut mem, ctba, ATA_CMD_READ_DMA_EXT, 0, 1);
    write_prdt(&mut mem, ctba, 0, read_buf, SECTOR_SIZE as u32);

    dev.mmio_write(PORT_BASE + PORT_REG_CI, 4, 1);
    dev.process(&mut mem);

    let mut out = [0u8; SECTOR_SIZE];
    mem.read_physical(read_buf, &mut out);
    assert_eq!(&out[0..4], b"BOOT");
    assert_eq!(&out[510..512], &[0x55, 0xAA]);
}

#[test]
fn interrupt_enable_bits_and_pci_interrupt_disable_gate_intx() {
    let capacity = 8 * SECTOR_SIZE as u64;
    let disk = RawDisk::create(MemBackend::new(), capacity).unwrap();

    let mut dev = AhciPciDevice::new(1);
    dev.attach_drive(0, AtaDrive::new(Box::new(disk)).unwrap());
    dev.config_mut().set_command(0x0006); // MEM + BUSMASTER

    let mut mem = Bus::new(0x20_000);

    let clb = 0x1000u64;
    let fb = 0x2000u64;
    let ctba = 0x3000u64;
    let buf = 0x4000u64;

    dev.mmio_write(PORT_BASE + PORT_REG_CLB, 4, clb);
    dev.mmio_write(PORT_BASE + PORT_REG_FB, 4, fb);
    dev.mmio_write(
        PORT_BASE + PORT_REG_CMD,
        4,
        u64::from(PORT_CMD_ST | PORT_CMD_FRE),
    );

    // Issue an IDENTIFY command with interrupts initially disabled.
    write_cmd_header(&mut mem, clb, 0, ctba, 1, false);
    write_cfis(&mut mem, ctba, ATA_CMD_IDENTIFY, 0, 0);
    write_prdt(&mut mem, ctba, 0, buf, SECTOR_SIZE as u32);

    dev.mmio_write(PORT_BASE + PORT_REG_CI, 4, 1);
    dev.process(&mut mem);

    // PxIS is set, but both PxIE and GHC.IE are clear, so no INTx.
    assert_ne!(
        dev.mmio_read(PORT_BASE + PORT_REG_IS, 4) as u32 & PORT_IS_DHRS,
        0
    );
    assert!(!dev.intx_level());

    // Enable global interrupts only (still no INTx because PxIE=0).
    dev.mmio_write(HBA_GHC, 4, u64::from(GHC_AE | GHC_IE));
    assert!(!dev.intx_level());

    // Enable per-port interrupts: INTx should assert because PxIS already has DHRS pending.
    dev.mmio_write(PORT_BASE + PORT_REG_IE, 4, u64::from(PORT_IS_DHRS));
    assert!(dev.intx_level());

    // PCI command bit 10 should mask legacy INTx.
    let cmd = dev.config().command();
    dev.config_mut().set_command(cmd | (1 << 10));
    assert!(!dev.intx_level());
    dev.config_mut().set_command(cmd & !(1 << 10));
    assert!(dev.intx_level());

    // Clearing PxIS should deassert the interrupt.
    dev.mmio_write(PORT_BASE + PORT_REG_IS, 4, u64::from(PORT_IS_DHRS));
    assert!(!dev.intx_level());
}

#[test]
fn w1c_hba_is_byte_write_does_not_clear_other_port_summary_bits() {
    // Regression test: sub-dword writes to RW1C registers must not inadvertently clear bits in
    // unwritten bytes (e.g. by read-modify-write merging).
    const HBA_IS: u64 = 0x08;

    let capacity = 8 * SECTOR_SIZE as u64;
    let disk0 = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let disk8 = RawDisk::create(MemBackend::new(), capacity).unwrap();

    let mut dev = AhciPciDevice::new(9);
    dev.attach_drive(0, AtaDrive::new(Box::new(disk0)).unwrap());
    dev.attach_drive(8, AtaDrive::new(Box::new(disk8)).unwrap());
    dev.config_mut().set_command(0x0006); // MEM + BUSMASTER

    let mut mem = Bus::new(0x20_000);

    // Port 0 command buffers.
    let clb0 = 0x1000u64;
    let fb0 = 0x2000u64;
    let ctba0 = 0x3000u64;
    let buf0 = 0x4000u64;

    // Port 8 command buffers.
    let clb8 = 0x5000u64;
    let fb8 = 0x6000u64;
    let ctba8 = 0x7000u64;
    let buf8 = 0x8000u64;

    let port0_base = PORT_BASE;
    let port8_base = PORT_BASE + 8 * 0x80;

    // Enable global interrupts and AHCI mode.
    dev.mmio_write(HBA_GHC, 4, u64::from(GHC_IE | GHC_AE));

    // Program both ports.
    for (port_base, clb, fb) in [(port0_base, clb0, fb0), (port8_base, clb8, fb8)] {
        dev.mmio_write(port_base + PORT_REG_CLB, 4, clb);
        dev.mmio_write(port_base + PORT_REG_FB, 4, fb);
        dev.mmio_write(port_base + PORT_REG_IE, 4, u64::from(PORT_IS_DHRS));
        dev.mmio_write(
            port_base + PORT_REG_CMD,
            4,
            u64::from(PORT_CMD_ST | PORT_CMD_FRE),
        );
    }

    // IDENTIFY DMA on both ports.
    write_cmd_header(&mut mem, clb0, 0, ctba0, 1, false);
    write_cfis(&mut mem, ctba0, ATA_CMD_IDENTIFY, 0, 0);
    write_prdt(&mut mem, ctba0, 0, buf0, SECTOR_SIZE as u32);

    write_cmd_header(&mut mem, clb8, 0, ctba8, 1, false);
    write_cfis(&mut mem, ctba8, ATA_CMD_IDENTIFY, 0, 0);
    write_prdt(&mut mem, ctba8, 0, buf8, SECTOR_SIZE as u32);

    dev.mmio_write(port0_base + PORT_REG_CI, 4, 1);
    dev.mmio_write(port8_base + PORT_REG_CI, 4, 1);
    dev.process(&mut mem);

    assert!(dev.intx_level());

    let is = dev.mmio_read(HBA_IS, 4) as u32;
    assert_eq!(is & ((1 << 0) | (1 << 8)), (1 << 0) | (1 << 8));

    // Clear bit 0 via a *byte* write to HBA.IS.
    dev.mmio_write(HBA_IS, 1, 1);

    let is2 = dev.mmio_read(HBA_IS, 4) as u32;
    assert_eq!(is2 & (1 << 0), 0);
    assert_eq!(is2 & (1 << 8), 1 << 8);

    // Port 8 interrupt is still pending, so INTx should remain asserted.
    assert!(dev.intx_level());
}

#[test]
fn w1c_px_is_byte_write_does_not_clear_other_status_bits() {
    // Trigger both DHRS (bit 0) and TFES (bit 30) then clear DHRS via a byte write.
    // The TFES bit lives in an upper byte of the same dword, so a buggy RMW merge would clear it.
    let capacity = 8 * SECTOR_SIZE as u64;
    let disk = RawDisk::create(MemBackend::new(), capacity).unwrap();

    let mut dev = AhciPciDevice::new(1);
    dev.attach_drive(0, AtaDrive::new(Box::new(disk)).unwrap());
    dev.config_mut().set_command(0x0006); // MEM + BUSMASTER

    let mut mem = Bus::new(0x20_000);

    let clb = 0x1000u64;
    let fb = 0x2000u64;
    let ctba = 0x3000u64;

    dev.mmio_write(PORT_BASE + PORT_REG_CLB, 4, clb);
    dev.mmio_write(PORT_BASE + PORT_REG_FB, 4, fb);
    dev.mmio_write(HBA_GHC, 4, u64::from(GHC_IE | GHC_AE));
    dev.mmio_write(
        PORT_BASE + PORT_REG_IE,
        4,
        u64::from(PORT_IS_DHRS | PORT_IS_TFES),
    );
    dev.mmio_write(
        PORT_BASE + PORT_REG_CMD,
        4,
        u64::from(PORT_CMD_ST | PORT_CMD_FRE),
    );

    // Program an IDENTIFY command with PRDTL=0 so DMA fails and the port raises TFES.
    write_cmd_header(&mut mem, clb, 0, ctba, 0, false);
    write_cfis(&mut mem, ctba, ATA_CMD_IDENTIFY, 0, 0);

    dev.mmio_write(PORT_BASE + PORT_REG_CI, 4, 1);
    dev.process(&mut mem);

    let is = dev.mmio_read(PORT_BASE + PORT_REG_IS, 4) as u32;
    assert_ne!(is & PORT_IS_DHRS, 0);
    assert_ne!(is & PORT_IS_TFES, 0);
    assert!(dev.intx_level());

    // Clear DHRS via a byte write.
    dev.mmio_write(PORT_BASE + PORT_REG_IS, 1, 1);

    let is2 = dev.mmio_read(PORT_BASE + PORT_REG_IS, 4) as u32;
    assert_eq!(is2 & PORT_IS_DHRS, 0);
    assert_ne!(is2 & PORT_IS_TFES, 0);

    // TFES is still enabled and pending, so INTx should remain asserted.
    assert!(dev.intx_level());
}
