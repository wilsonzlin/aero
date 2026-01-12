use aero_devices::pci::PciDevice as _;
use aero_devices_storage::AhciPciDevice;
use aero_devices_storage::ata::AtaDrive;
use aero_io_snapshot::io::state::codec::Encoder;
use aero_io_snapshot::io::state::{IoSnapshot, SnapshotError, SnapshotVersion, SnapshotWriter};
use aero_io_snapshot::io::storage::state::AhciControllerState;
use aero_storage::{MemBackend, RawDisk, VirtualDisk, SECTOR_SIZE};
use memory::{Bus, MemoryBus};

fn make_disk_with_boot_sector() -> impl VirtualDisk {
    let capacity = 16 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut sector0 = vec![0u8; SECTOR_SIZE];
    sector0[0..4].copy_from_slice(b"BOOT");
    disk.write_sectors(0, &sector0).unwrap();
    disk
}

fn program_read_dma_ext_slot0(
    mem: &mut dyn MemoryBus,
    clb: u64,
    ctba: u64,
    data_buf: u64,
    lba: u64,
    sectors: u16,
) {
    // Command header (slot 0).
    let cfl = 5u32;
    let prdtl = 1u32 << 16;
    mem.write_u32(clb, cfl | prdtl);
    mem.write_u32(clb + 4, 0); // PRDBC
    mem.write_u32(clb + 8, ctba as u32);
    mem.write_u32(clb + 12, (ctba >> 32) as u32);

    // CFIS: READ DMA EXT.
    let mut cfis = [0u8; 64];
    cfis[0] = 0x27;
    cfis[1] = 0x80;
    cfis[2] = 0x25; // READ DMA EXT
    cfis[7] = 0x40; // LBA mode

    cfis[4] = (lba & 0xff) as u8;
    cfis[5] = ((lba >> 8) & 0xff) as u8;
    cfis[6] = ((lba >> 16) & 0xff) as u8;
    cfis[8] = ((lba >> 24) & 0xff) as u8;
    cfis[9] = ((lba >> 32) & 0xff) as u8;
    cfis[10] = ((lba >> 40) & 0xff) as u8;

    cfis[12] = (sectors & 0xff) as u8;
    cfis[13] = (sectors >> 8) as u8;

    mem.write_physical(ctba, &cfis);

    // PRDT entry 0.
    let prd = ctba + 0x80;
    mem.write_u32(prd, data_buf as u32);
    mem.write_u32(prd + 4, (data_buf >> 32) as u32);
    mem.write_u32(prd + 8, 0);
    let bytes = u32::from(sectors) * SECTOR_SIZE as u32;
    mem.write_u32(prd + 12, (bytes - 1) & 0x003F_FFFF);
}

#[test]
fn ahci_pci_snapshot_roundtrip_preserves_pci_config_mmio_regs_and_irq_level() {
    let disk = make_disk_with_boot_sector();
    let drive = AtaDrive::new(Box::new(disk)).unwrap();

    let mut dev = AhciPciDevice::new(1);
    dev.attach_drive(0, drive);

    // Program PCI config state (BAR5 + command).
    dev.config_mut().set_bar_base(5, 0x1000_0000);
    dev.config_mut().set_command(0x0006); // memory decode + bus master
    let pci_before = dev.config().snapshot_state();

    let mut mem = Bus::new(0x20_000);

    // Basic port programming and command setup.
    let clb = 0x1000u64;
    let fb = 0x2000u64;
    let ctba = 0x3000u64;
    let data_buf = 0x4000u64;

    const HBA_IS: u64 = 0x08;
    const HBA_PI: u64 = 0x0C;
    const PORT_BASE: u64 = 0x100;
    const PORT_REG_IS: u64 = 0x10;
    const PORT_REG_IE: u64 = 0x14;
    const PORT_REG_CMD: u64 = 0x18;
    const PORT_REG_CI: u64 = 0x38;

    const GHC_IE: u32 = 1 << 1;
    const GHC_AE: u32 = 1 << 31;
    const PORT_CMD_ST: u32 = 1 << 0;
    const PORT_CMD_FRE: u32 = 1 << 4;
    const PORT_IS_DHRS: u32 = 1 << 0;

    dev.mmio_write(PORT_BASE, 4, clb); // PxCLB
    dev.mmio_write(PORT_BASE + 0x08, 4, fb); // PxFB
    dev.mmio_write(0x04, 4, u64::from(GHC_IE | GHC_AE)); // GHC.IE | GHC.AE
    dev.mmio_write(PORT_BASE + PORT_REG_IE, 4, u64::from(PORT_IS_DHRS));
    dev.mmio_write(
        PORT_BASE + PORT_REG_CMD,
        4,
        u64::from(PORT_CMD_ST | PORT_CMD_FRE),
    );

    program_read_dma_ext_slot0(&mut mem, clb, ctba, data_buf, 0, 1);

    dev.mmio_write(PORT_BASE + PORT_REG_CI, 4, 1);
    dev.process(&mut mem);

    assert!(dev.intx_level(), "IRQ should be asserted after command completion");

    // Snapshot guest-visible register values at snapshot time.
    let regs = [
        0x00u64, // CAP
        0x04u64, // GHC
        HBA_IS,  // IS (derived from PxIS)
        HBA_PI,  // PI
        0x10u64, // VS
        0x24u64, // CAP2
        0x28u64, // BOHC
        // Port 0
        PORT_BASE, // CLB
        0x100 + 0x04, // CLBU
        0x100 + 0x08, // FB
        0x100 + 0x0c, // FBU
        0x100 + 0x10, // IS
        0x100 + 0x14, // IE
        0x100 + 0x18, // CMD
        0x100 + 0x20, // TFD
        0x100 + 0x24, // SIG
        0x100 + 0x28, // SSTS
        0x100 + 0x2c, // SCTL
        0x100 + 0x30, // SERR
        0x100 + 0x34, // SACT
        0x100 + 0x38, // CI
    ];
    let mut reg_vals = Vec::with_capacity(regs.len());
    for &off in &regs {
        reg_vals.push((off, dev.mmio_read(off, 4) as u32));
    }

    // Confirm the DMA completed (sanity).
    let mut out = [0u8; SECTOR_SIZE];
    mem.read_physical(data_buf, &mut out);
    assert_eq!(&out[0..4], b"BOOT");

    // Snapshot the device (PCI config + AHCI controller state).
    let snap = dev.save_state();

    // Restore into a fresh device with an identical disk.
    let disk = make_disk_with_boot_sector();
    let drive = AtaDrive::new(Box::new(disk)).unwrap();

    let mut restored = AhciPciDevice::new(1);
    restored.load_state(&snap).unwrap();
    restored.attach_drive(0, drive);

    assert_eq!(restored.config().snapshot_state(), pci_before);

    for (off, val) in reg_vals {
        assert_eq!(
            restored.mmio_read(off, 4) as u32,
            val,
            "MMIO reg {off:#x} mismatch"
        );
    }

    // The IRQ line level should remain asserted because PxIS is still pending and enabled.
    assert!(restored.intx_level());

    // Clear the interrupt: PxIS is RW1C.
    restored.mmio_write(PORT_BASE + PORT_REG_IS, 4, u64::from(PORT_IS_DHRS));
    assert!(!restored.intx_level());

    // Issue a second command after restore to ensure the controller continues to make progress.
    let data_buf2 = 0x5000u64;
    program_read_dma_ext_slot0(&mut mem, clb, ctba, data_buf2, 0, 1);
    restored.mmio_write(PORT_BASE + PORT_REG_CI, 4, 1);
    restored.process(&mut mem);

    assert!(restored.intx_level());

    let mut out2 = [0u8; SECTOR_SIZE];
    mem.read_physical(data_buf2, &mut out2);
    assert_eq!(&out2[0..4], b"BOOT");
}

#[test]
fn ahci_snapshot_rejects_snapshots_claiming_more_than_32_ports() {
    // Construct an intentionally-corrupt AHCI controller snapshot that declares 33 ports.
    // Decoding must not panic or attempt unbounded allocations.
    const TAG_PORTS: u16 = 2;

    let mut w = SnapshotWriter::new(
        <AhciControllerState as IoSnapshot>::DEVICE_ID,
        <AhciControllerState as IoSnapshot>::DEVICE_VERSION,
    );
    let ports = Encoder::new().u32(33).finish();
    w.field_bytes(TAG_PORTS, ports);
    let bytes = w.finish();

    let mut state = AhciControllerState::default();
    let err = state.load_state(&bytes).unwrap_err();
    assert!(matches!(err, SnapshotError::InvalidFieldEncoding(_)));

    // Also verify the header is still parsed correctly (sanity).
    let header = aero_io_snapshot::io::state::SnapshotReader::parse(
        &bytes,
        <AhciControllerState as IoSnapshot>::DEVICE_ID,
    )
    .unwrap()
    .header();
    assert_eq!(header.device_version, SnapshotVersion::new(1, 0));
}
