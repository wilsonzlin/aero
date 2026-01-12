use aero_devices::pci::profile::IDE_PIIX3;
use aero_devices_storage::ata::AtaDrive;
use aero_devices_storage::pci_ide::PRIMARY_PORTS;
use aero_pc_platform::PcPlatform;
use aero_storage::{MemBackend, RawDisk, VirtualDisk, SECTOR_SIZE};
use memory::MemoryBus as _;

fn cfg_addr(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    0x8000_0000
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | (offset as u32 & 0xFC)
}

fn read_cfg_u32(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    pc.io
        .write(0xCF8, 4, cfg_addr(bus, device, function, offset));
    pc.io.read(0xCFC, 4)
}

fn write_cfg_u16(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, offset: u8, value: u16) {
    pc.io
        .write(0xCF8, 4, cfg_addr(bus, device, function, offset));
    pc.io.write(0xCFC, 2, u32::from(value));
}

fn read_io_bar_base(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, bar: u8) -> u16 {
    let off = 0x10 + bar * 4;
    let val = read_cfg_u32(pc, bus, device, function, off);
    u16::try_from(val & 0xFFFF_FFFC).unwrap()
}

#[test]
fn pc_platform_enumerates_ide_and_preserves_legacy_bar_bases() {
    let mut pc = PcPlatform::new_with_ide(2 * 1024 * 1024);
    let bdf = IDE_PIIX3.bdf;

    let id = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x00);
    assert_eq!(id & 0xffff, u32::from(IDE_PIIX3.vendor_id));
    assert_eq!((id >> 16) & 0xffff, u32::from(IDE_PIIX3.device_id));

    let class = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x08);
    assert_eq!((class >> 8) & 0x00ff_ffff, 0x01018A);

    let command = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04) & 0xffff;
    assert_ne!(command & 0x1, 0, "BIOS POST should enable I/O decoding");

    // IDE PCI config space should expose legacy compatible BAR assignments.
    assert_eq!(
        read_io_bar_base(&mut pc, bdf.bus, bdf.device, bdf.function, 0),
        0x1F0
    );
    assert_eq!(
        read_io_bar_base(&mut pc, bdf.bus, bdf.device, bdf.function, 1),
        0x3F4
    );
    assert_eq!(
        read_io_bar_base(&mut pc, bdf.bus, bdf.device, bdf.function, 2),
        0x170
    );
    assert_eq!(
        read_io_bar_base(&mut pc, bdf.bus, bdf.device, bdf.function, 3),
        0x374
    );
    assert_eq!(
        read_io_bar_base(&mut pc, bdf.bus, bdf.device, bdf.function, 4),
        0xC000
    );
}

#[test]
fn pc_platform_ide_pio_reads_boot_sector() {
    let capacity = 8 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut sector0 = vec![0u8; SECTOR_SIZE];
    sector0[0..4].copy_from_slice(b"BOOT");
    sector0[510] = 0x55;
    sector0[511] = 0xAA;
    disk.write_sectors(0, &sector0).unwrap();

    let mut pc = PcPlatform::new_with_ide(2 * 1024 * 1024);
    pc.ide
        .as_ref()
        .unwrap()
        .borrow_mut()
        .controller
        .attach_primary_master_ata(AtaDrive::new(Box::new(disk)).unwrap());

    // Issue READ SECTORS for LBA 0, 1 sector.
    pc.io.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0); // master + LBA
    pc.io.write(PRIMARY_PORTS.cmd_base + 2, 1, 1); // count
    pc.io.write(PRIMARY_PORTS.cmd_base + 3, 1, 0); // lba0
    pc.io.write(PRIMARY_PORTS.cmd_base + 4, 1, 0); // lba1
    pc.io.write(PRIMARY_PORTS.cmd_base + 5, 1, 0); // lba2
    pc.io.write(PRIMARY_PORTS.cmd_base + 7, 1, 0x20); // READ SECTORS

    let mut buf = [0u8; SECTOR_SIZE];
    for i in 0..(SECTOR_SIZE / 2) {
        let w = pc.io.read(PRIMARY_PORTS.cmd_base, 2) as u16;
        buf[i * 2..i * 2 + 2].copy_from_slice(&w.to_le_bytes());
    }

    assert_eq!(&buf[0..4], b"BOOT");
    assert_eq!(&buf[510..512], &[0x55, 0xAA]);
}

#[test]
fn pc_platform_ide_dma_and_irq14_routing_work() {
    let capacity = 8 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut sector0 = vec![0u8; SECTOR_SIZE];
    sector0[0..4].copy_from_slice(b"BOOT");
    disk.write_sectors(0, &sector0).unwrap();

    let mut pc = PcPlatform::new_with_ide(2 * 1024 * 1024);
    pc.ide
        .as_ref()
        .unwrap()
        .borrow_mut()
        .controller
        .attach_primary_master_ata(AtaDrive::new(Box::new(disk)).unwrap());

    // Unmask IRQ2 (cascade) and IRQ14 so we can observe primary IDE IRQ delivery via the PIC.
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().set_offsets(0x20, 0x28);
        interrupts.pic_mut().set_masked(2, false);
        interrupts.pic_mut().set_masked(14, false);
    }

    let bdf = IDE_PIIX3.bdf;
    let bm_base = read_io_bar_base(&mut pc, bdf.bus, bdf.device, bdf.function, 4);
    assert_ne!(bm_base, 0);

    // Enable bus mastering for DMA (keep I/O decoding enabled).
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0005);

    // PRD table at 0x1000: one entry, end-of-table, 512 bytes.
    let prd_addr = 0x1000u64;
    let read_buf = 0x2000u64;
    pc.memory.write_u32(prd_addr, read_buf as u32);
    pc.memory.write_u16(prd_addr + 4, SECTOR_SIZE as u16);
    pc.memory.write_u16(prd_addr + 6, 0x8000);

    pc.io.write(bm_base + 4, 4, prd_addr as u32);

    // Issue READ DMA (LBA 0, 1 sector).
    pc.io.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    pc.io.write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    pc.io.write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    pc.io.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    pc.io.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    pc.io.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xC8); // READ DMA

    // Start bus master (direction = to memory).
    pc.io.write(bm_base, 1, 0x09);
    pc.process_ide();
    pc.poll_pci_intx_lines();

    let pending = pc
        .interrupts
        .borrow()
        .pic()
        .get_pending_vector()
        .expect("IRQ14 should be pending after IDE DMA completion");
    let irq = pc
        .interrupts
        .borrow()
        .pic()
        .vector_to_irq(pending)
        .expect("pending vector should decode to an IRQ number");
    assert_eq!(irq, 14);

    // Consume and EOI the interrupt so subsequent assertions are not affected by PIC latching.
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().acknowledge(pending);
        interrupts.pic_mut().eoi(pending);
    }

    let mut out = [0u8; SECTOR_SIZE];
    pc.memory.read_physical(read_buf, &mut out);
    assert_eq!(&out[0..4], b"BOOT");

    // Clear the IDE device interrupt by reading the status register.
    let _ = pc.io.read(PRIMARY_PORTS.cmd_base + 7, 1);
    pc.poll_pci_intx_lines();
    assert_eq!(pc.interrupts.borrow().pic().get_pending_vector(), None);
}
