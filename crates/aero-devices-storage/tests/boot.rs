use aero_devices_storage::ahci::AhciController;
use aero_devices_storage::ide::{IdeChannelId, IdeController};
use aero_devices_storage::bus::{TestIrqLine, TestMemory};
use aero_devices_storage::{GuestMemory, GuestMemoryExt};
use aero_devices_storage::ata::{AtaDrive, ATA_CMD_READ_SECTORS};
use aero_storage::{MemBackend, RawDisk, VirtualDisk, SECTOR_SIZE};

#[test]
fn boot_sector_read_via_ide_pio() {
    let capacity = 8 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut sector0 = vec![0u8; SECTOR_SIZE];
    sector0[0..4].copy_from_slice(b"BOOT");
    sector0[510] = 0x55;
    sector0[511] = 0xAA;
    disk.write_sectors(0, &sector0).unwrap();

    let mut ide = IdeController::new();
    ide.attach_drive(IdeChannelId::Primary, 0, AtaDrive::new(Box::new(disk)).unwrap());

    let irq14 = TestIrqLine::default();
    let irq15 = TestIrqLine::default();

    // Issue READ SECTORS for LBA 0, 1 sector.
    ide.write_u8(0x1F6, 0xE0, &irq14, &irq15);
    ide.write_u8(0x1F2, 1, &irq14, &irq15);
    ide.write_u8(0x1F3, 0, &irq14, &irq15);
    ide.write_u8(0x1F4, 0, &irq14, &irq15);
    ide.write_u8(0x1F5, 0, &irq14, &irq15);
    ide.write_u8(0x1F7, ATA_CMD_READ_SECTORS, &irq14, &irq15);

    let mut buf = [0u8; SECTOR_SIZE];
    for i in 0..SECTOR_SIZE {
        buf[i] = ide.read_u8(0x1F0, &irq14, &irq15);
    }

    assert_eq!(&buf[0..4], b"BOOT");
    assert_eq!(&buf[510..512], &[0x55, 0xAA]);
}

#[test]
fn boot_sector_read_via_ahci_dma() {
    let capacity = 8 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut sector0 = vec![0u8; SECTOR_SIZE];
    sector0[0..4].copy_from_slice(b"BOOT");
    sector0[510] = 0x55;
    sector0[511] = 0xAA;
    disk.write_sectors(0, &sector0).unwrap();

    let irq = TestIrqLine::default();
    let mut ahci = AhciController::new(Box::new(irq.clone()), 1);
    ahci.attach_drive(0, AtaDrive::new(Box::new(disk)).unwrap());

    let mut mem = TestMemory::new(0x20_000);

    // Basic port programming and command setup.
    let clb = 0x1000;
    let fb = 0x2000;
    let ctba = 0x3000;
    let data_buf = 0x4000;

    ahci.write_u32(0x100 + 0x00, clb as u32);
    ahci.write_u32(0x100 + 0x08, fb as u32);
    ahci.write_u32(0x04, (1 << 1) | (1 << 31)); // GHC.IE | GHC.AE
    ahci.write_u32(0x100 + 0x14, 1); // PxIE.DHRE
    ahci.write_u32(0x100 + 0x18, (1 << 0) | (1 << 4)); // PxCMD.ST | PxCMD.FRE

    // Command header (slot 0).
    let cfl = 5u32;
    let prdtl = 1u32 << 20;
    mem.write_u32(clb, cfl | prdtl);
    mem.write_u32(clb + 4, 0);
    mem.write_u32(clb + 8, ctba as u32);
    mem.write_u32(clb + 12, 0);

    // CFIS: READ DMA EXT, LBA 0, 1 sector.
    let mut cfis = [0u8; 64];
    cfis[0] = 0x27;
    cfis[1] = 0x80;
    cfis[2] = 0x25; // READ DMA EXT
    cfis[7] = 0x40;
    cfis[12] = 1;
    mem.write(ctba, &cfis);

    // PRDT entry.
    let prd = ctba + 0x80;
    mem.write_u32(prd, data_buf as u32);
    mem.write_u32(prd + 4, 0);
    mem.write_u32(prd + 8, 0);
    mem.write_u32(prd + 12, (SECTOR_SIZE as u32 - 1) & 0x003F_FFFF);

    ahci.write_u32(0x100 + 0x38, 1);
    ahci.process(&mut mem);

    assert_eq!(irq.level(), true);

    let mut out = [0u8; SECTOR_SIZE];
    mem.read(data_buf, &mut out);
    assert_eq!(&out[0..4], b"BOOT");
    assert_eq!(&out[510..512], &[0x55, 0xAA]);
}
