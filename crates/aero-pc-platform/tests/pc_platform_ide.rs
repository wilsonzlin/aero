use aero_devices::pci::profile::{IDE_PIIX3, ISA_PIIX3};
use aero_devices_storage::atapi::AtapiCdrom;
use aero_devices_storage::pci_ide::{PRIMARY_PORTS, SECONDARY_PORTS};
use aero_pc_platform::PcPlatform;
use aero_platform::interrupts::{InterruptController, PlatformInterruptMode};
use aero_storage::{MemBackend, RawDisk, VirtualDisk, SECTOR_SIZE};
use aero_interrupts::apic::IOAPIC_MMIO_BASE;
use memory::MemoryBus as _;

fn program_ioapic_entry(pc: &mut PcPlatform, gsi: u32, low: u32, high: u32) {
    let redtbl_low = 0x10u32 + gsi * 2;
    let redtbl_high = redtbl_low + 1;
    pc.memory.write_u32(IOAPIC_MMIO_BASE, redtbl_low);
    pc.memory.write_u32(IOAPIC_MMIO_BASE + 0x10, low);
    pc.memory.write_u32(IOAPIC_MMIO_BASE, redtbl_high);
    pc.memory.write_u32(IOAPIC_MMIO_BASE + 0x10, high);
}

fn cfg_addr(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    0x8000_0000
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | (offset as u32 & 0xFC)
}

fn read_cfg_u32(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    pc.io.write(0xCF8, 4, cfg_addr(bus, device, function, offset));
    pc.io.read(0xCFC, 4)
}

fn read_vendor_id(pc: &mut PcPlatform, bus: u8, device: u8, function: u8) -> u16 {
    (read_cfg_u32(pc, bus, device, function, 0x00) & 0xffff) as u16
}

fn read_header_type(pc: &mut PcPlatform, bus: u8, device: u8, function: u8) -> u8 {
    ((read_cfg_u32(pc, bus, device, function, 0x0c) >> 16) & 0xff) as u8
}

fn write_cfg_u16(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, offset: u8, value: u16) {
    pc.io.write(0xCF8, 4, cfg_addr(bus, device, function, offset));
    pc.io.write(0xCFC, 2, u32::from(value));
}

fn write_cfg_u32(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, offset: u8, value: u32) {
    pc.io.write(0xCF8, 4, cfg_addr(bus, device, function, offset));
    pc.io.write(0xCFC, 4, value);
}

fn read_io_bar_base(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, bar: u8) -> u16 {
    let off = 0x10 + bar * 4;
    let val = read_cfg_u32(pc, bus, device, function, off);
    u16::try_from(val & 0xFFFF_FFFC).unwrap()
}

fn enumerate_bus0(pc: &mut PcPlatform) -> Vec<(u8, u8)> {
    let mut found = Vec::new();
    for device in 0u8..32 {
        let vendor = read_vendor_id(pc, 0, device, 0);
        if vendor == 0xffff {
            continue;
        }
        found.push((device, 0));

        let header_type = read_header_type(pc, 0, device, 0);
        let functions = if (header_type & 0x80) != 0 { 8 } else { 1 };
        for function in 1u8..functions {
            let vendor = read_vendor_id(pc, 0, device, function);
            if vendor != 0xffff {
                found.push((device, function));
            }
        }
    }
    found
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
    assert_eq!(read_io_bar_base(&mut pc, bdf.bus, bdf.device, bdf.function, 0), 0x1F0);
    assert_eq!(read_io_bar_base(&mut pc, bdf.bus, bdf.device, bdf.function, 1), 0x3F4);
    assert_eq!(read_io_bar_base(&mut pc, bdf.bus, bdf.device, bdf.function, 2), 0x170);
    assert_eq!(read_io_bar_base(&mut pc, bdf.bus, bdf.device, bdf.function, 3), 0x374);
    assert_eq!(read_io_bar_base(&mut pc, bdf.bus, bdf.device, bdf.function, 4), 0xC000);
}

#[test]
fn pc_platform_ide_io_decode_bit_gates_legacy_ports_and_bus_master_bar4() {
    let mut pc = PcPlatform::new_with_ide(2 * 1024 * 1024);
    let bdf = IDE_PIIX3.bdf;

    let bm_base = read_io_bar_base(&mut pc, bdf.bus, bdf.device, bdf.function, 4);
    assert_ne!(bm_base, 0);

    // The IDE model floats the legacy taskfile/status registers high (0xFF) when no drive is
    // present. Attach a tiny in-memory disk so the test can distinguish "I/O decoding disabled"
    // from "no device responded".
    let disk = RawDisk::create(MemBackend::new(), 8 * SECTOR_SIZE as u64).unwrap();
    pc.attach_ide_primary_master_disk(Box::new(disk)).unwrap();

    // With PCI I/O decoding enabled, legacy ports should respond and BAR4 should decode.
    let status = pc.io.read(PRIMARY_PORTS.cmd_base + 7, 1) as u8;
    assert_ne!(status, 0xFF, "expected IDE status to decode while IO is enabled");
    assert_eq!(pc.io.read(bm_base, 1), 0, "BMIDE cmd reg should decode");

    // Disable PCI I/O decoding: legacy ports and BAR4 should float high.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0000);
    assert_eq!(
        pc.io.read(PRIMARY_PORTS.cmd_base + 7, 1),
        0xFF,
        "status port should float high when IO decoding is disabled"
    );
    assert_eq!(
        pc.io.read(bm_base, 1),
        0xFF,
        "BMIDE BAR4 should not decode when IO decoding is disabled"
    );

    // Re-enable I/O decoding and ensure both regions decode again.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0001);
    let status = pc.io.read(PRIMARY_PORTS.cmd_base + 7, 1) as u8;
    assert_ne!(status, 0xFF, "status should decode again after IO is enabled");
    assert_eq!(pc.io.read(bm_base, 1), 0, "BMIDE should decode again");
}

#[test]
fn pc_platform_presents_piix3_as_a_multifunction_device() {
    let mut pc = PcPlatform::new_with_ide(2 * 1024 * 1024);

    let found = enumerate_bus0(&mut pc);
    assert!(
        found.contains(&(ISA_PIIX3.bdf.device, ISA_PIIX3.bdf.function)),
        "missing {}",
        ISA_PIIX3.name
    );
    assert!(
        found.contains(&(IDE_PIIX3.bdf.device, IDE_PIIX3.bdf.function)),
        "missing {}",
        IDE_PIIX3.name
    );

    let header_type = read_header_type(
        &mut pc,
        ISA_PIIX3.bdf.bus,
        ISA_PIIX3.bdf.device,
        ISA_PIIX3.bdf.function,
    );
    assert_ne!(
        header_type & 0x80,
        0,
        "PIIX3 function 0 should advertise multi-function"
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
    pc.attach_ide_primary_master_disk(Box::new(disk)).unwrap();

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
fn pc_platform_ide_pio_write_multi_sector_round_trip() {
    let disk = RawDisk::create(MemBackend::new(), 8 * SECTOR_SIZE as u64).unwrap();

    let mut pc = PcPlatform::new_with_ide(2 * 1024 * 1024);
    pc.attach_ide_primary_master_disk(Box::new(disk)).unwrap();

    // Two-sector payload with distinct halves.
    let mut payload = vec![0u8; 2 * SECTOR_SIZE];
    payload[..SECTOR_SIZE].fill(0x11);
    payload[SECTOR_SIZE..].fill(0x22);

    // WRITE SECTORS for LBA 0, 2 sectors.
    pc.io.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0); // master + LBA
    pc.io.write(PRIMARY_PORTS.cmd_base + 2, 1, 2); // count
    pc.io.write(PRIMARY_PORTS.cmd_base + 3, 1, 0); // lba0
    pc.io.write(PRIMARY_PORTS.cmd_base + 4, 1, 0); // lba1
    pc.io.write(PRIMARY_PORTS.cmd_base + 5, 1, 0); // lba2
    pc.io.write(PRIMARY_PORTS.cmd_base + 7, 1, 0x30); // WRITE SECTORS

    for i in 0..(payload.len() / 2) {
        let w = u16::from_le_bytes([payload[i * 2], payload[i * 2 + 1]]);
        pc.io.write(PRIMARY_PORTS.cmd_base, 2, u32::from(w));
    }

    // Re-read via PIO to validate the write stuck.
    pc.io.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0); // master + LBA
    pc.io.write(PRIMARY_PORTS.cmd_base + 2, 1, 2); // count
    pc.io.write(PRIMARY_PORTS.cmd_base + 3, 1, 0); // lba0
    pc.io.write(PRIMARY_PORTS.cmd_base + 4, 1, 0); // lba1
    pc.io.write(PRIMARY_PORTS.cmd_base + 5, 1, 0); // lba2
    pc.io.write(PRIMARY_PORTS.cmd_base + 7, 1, 0x20); // READ SECTORS

    let mut readback = vec![0u8; 2 * SECTOR_SIZE];
    for i in 0..(readback.len() / 2) {
        let w = pc.io.read(PRIMARY_PORTS.cmd_base, 2) as u16;
        readback[i * 2..i * 2 + 2].copy_from_slice(&w.to_le_bytes());
    }

    assert_eq!(readback, payload);
}

#[test]
fn pc_platform_ide_dma_and_irq14_routing_work() {
    let capacity = 8 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut sector0 = vec![0u8; SECTOR_SIZE];
    sector0[0..4].copy_from_slice(b"BOOT");
    disk.write_sectors(0, &sector0).unwrap();

    let mut pc = PcPlatform::new_with_ide(2 * 1024 * 1024);
    pc.attach_ide_primary_master_disk(Box::new(disk)).unwrap();

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

#[test]
fn pc_platform_ide_respects_nien_interrupt_disable() {
    let capacity = 8 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut sector0 = vec![0u8; SECTOR_SIZE];
    sector0[0..4].copy_from_slice(b"BOOT");
    disk.write_sectors(0, &sector0).unwrap();

    let mut pc = PcPlatform::new_with_ide(2 * 1024 * 1024);
    pc.attach_ide_primary_master_disk(Box::new(disk)).unwrap();

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

    // Disable IDE interrupts (nIEN=1) via the device control register.
    pc.io.write(PRIMARY_PORTS.ctrl_base, 1, 0x02);

    // READ DMA (LBA 0, 1 sector).
    pc.io.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    pc.io.write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    pc.io.write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    pc.io.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    pc.io.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    pc.io.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xC8); // READ DMA
    pc.io.write(bm_base, 1, 0x09);

    pc.process_ide();
    pc.poll_pci_intx_lines();

    assert_eq!(
        pc.interrupts.borrow().pic().get_pending_vector(),
        None,
        "IRQ14 should be suppressed when nIEN=1"
    );

    // DMA should still succeed.
    let mut out = [0u8; 4];
    pc.memory.read_physical(read_buf, &mut out);
    assert_eq!(&out, b"BOOT");

    // Stop bus master and clear status bits to mimic driver behavior.
    pc.io.write(bm_base, 1, 0);
    pc.io.write(bm_base + 2, 1, 0x06); // clear IRQ+ERR

    // Re-enable interrupts (nIEN=0) and re-run the same DMA read; now IRQ14 should assert.
    pc.io.write(PRIMARY_PORTS.ctrl_base, 1, 0x00);
    pc.memory.write_u32(read_buf, 0);

    pc.io.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    pc.io.write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    pc.io.write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    pc.io.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    pc.io.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    pc.io.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xC8);
    pc.io.write(bm_base, 1, 0x09);

    pc.process_ide();
    pc.poll_pci_intx_lines();

    let pending = pc
        .interrupts
        .borrow()
        .pic()
        .get_pending_vector()
        .expect("IRQ14 should be pending after DMA completion with nIEN=0");
    let irq = pc
        .interrupts
        .borrow()
        .pic()
        .vector_to_irq(pending)
        .expect("pending vector should decode to an IRQ number");
    assert_eq!(irq, 14);

    // ACK+EOI so subsequent tests aren't affected by PIC latching.
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().acknowledge(pending);
        interrupts.pic_mut().eoi(pending);
    }

    // Clear the IDE device interrupt by reading the status register, then ensure the PIC no longer
    // has a pending vector.
    let _ = pc.io.read(PRIMARY_PORTS.cmd_base + 7, 1);
    pc.poll_pci_intx_lines();
    assert_eq!(pc.interrupts.borrow().pic().get_pending_vector(), None);

    // Now verify that toggling nIEN after an interrupt becomes pending suppresses delivery.
    //
    // In a real machine the interrupt would be asserted asynchronously; the platform polls the
    // device's `*_irq_pending()` flags to drive the ISA IRQ line. That poll must respect nIEN.
    pc.io.write(PRIMARY_PORTS.ctrl_base, 1, 0x00); // ensure interrupts enabled
    pc.memory.write_u32(read_buf, 0);

    // READ DMA (LBA 0, 1 sector).
    pc.io.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    pc.io.write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    pc.io.write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    pc.io.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    pc.io.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    pc.io.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xC8);
    pc.io.write(bm_base, 1, 0x09);

    pc.process_ide();

    assert!(
        pc.ide
            .as_ref()
            .expect("IDE controller should be present")
            .borrow()
            .controller
            .primary_irq_pending(),
        "DMA completion should leave a primary IDE IRQ pending before nIEN is set"
    );

    // Disable interrupts after the IRQ has become pending.
    pc.io.write(PRIMARY_PORTS.ctrl_base, 1, 0x02); // nIEN=1
    assert!(
        !pc.ide
            .as_ref()
            .expect("IDE controller should be present")
            .borrow()
            .controller
            .primary_irq_pending(),
        "primary_irq_pending() should respect nIEN gating"
    );

    pc.poll_pci_intx_lines();
    assert_eq!(
        pc.interrupts.borrow().pic().get_pending_vector(),
        None,
        "IRQ14 should be suppressed when nIEN is set after DMA completes"
    );

    // DMA should still succeed while interrupts are gated off.
    let mut out = [0u8; 4];
    pc.memory.read_physical(read_buf, &mut out);
    assert_eq!(&out, b"BOOT");

    // Clear pending IRQ by reading status, then re-enable interrupts and ensure it does not
    // retroactively deliver an IRQ.
    let _ = pc.io.read(PRIMARY_PORTS.cmd_base + 7, 1);
    pc.io.write(PRIMARY_PORTS.ctrl_base, 1, 0x00); // nIEN=0
    pc.poll_pci_intx_lines();
    assert_eq!(pc.interrupts.borrow().pic().get_pending_vector(), None);
}

#[test]
fn pc_platform_ide_dma_requires_pci_bus_master_enable() {
    let capacity = 4 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut sector0 = vec![0u8; SECTOR_SIZE];
    sector0[..4].copy_from_slice(b"DMA!");
    disk.write_sectors(0, &sector0).unwrap();

    let mut pc = PcPlatform::new_with_ide(2 * 1024 * 1024);
    pc.attach_ide_primary_master_disk(Box::new(disk)).unwrap();

    let bdf = IDE_PIIX3.bdf;
    let bm_base = read_io_bar_base(&mut pc, bdf.bus, bdf.device, bdf.function, 4);

    // PRD table at 0x1000: one entry, end-of-table, 512 bytes.
    let prd_addr = 0x1000u64;
    let read_buf = 0x2000u64;
    pc.memory.write_u32(prd_addr, read_buf as u32);
    pc.memory.write_u16(prd_addr + 4, SECTOR_SIZE as u16);
    pc.memory.write_u16(prd_addr + 6, 0x8000);
    pc.io.write(bm_base + 4, 4, prd_addr as u32);

    // Ensure IO decode is enabled but bus mastering is disabled.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0001);

    // Issue READ DMA (LBA 0, 1 sector) and start bus master.
    pc.io.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    pc.io.write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    pc.io.write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    pc.io.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    pc.io.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    pc.io.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xC8); // READ DMA
    pc.io.write(bm_base, 1, 0x09);

    pc.process_ide();
    assert_eq!(pc.memory.read_u8(read_buf), 0, "DMA should be gated off");

    // Now enable bus mastering and retry.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0005);
    pc.process_ide();

    let mut out = [0u8; 4];
    pc.memory.read_physical(read_buf, &mut out);
    assert_eq!(&out, b"DMA!");
}

#[test]
fn pc_platform_ide_dma_scatter_gather_prd_splits_transfer() {
    let capacity = 4 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut sector0 = vec![0u8; SECTOR_SIZE];
    sector0[..256].fill(0xAA);
    sector0[256..].fill(0xBB);
    disk.write_sectors(0, &sector0).unwrap();

    let mut pc = PcPlatform::new_with_ide(2 * 1024 * 1024);
    pc.attach_ide_primary_master_disk(Box::new(disk)).unwrap();

    let bdf = IDE_PIIX3.bdf;
    let bm_base = read_io_bar_base(&mut pc, bdf.bus, bdf.device, bdf.function, 4);

    // Enable IO decode + bus mastering.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0005);

    // PRD table at 0x1000: two 256-byte segments, end-of-table on second.
    let prd_addr = 0x1000u64;
    let buf1 = 0x2000u64;
    let buf2 = 0x2100u64;
    // Entry 0.
    pc.memory.write_u32(prd_addr, buf1 as u32);
    pc.memory.write_u16(prd_addr + 4, 256);
    pc.memory.write_u16(prd_addr + 6, 0);
    // Entry 1 (EOT).
    pc.memory.write_u32(prd_addr + 8, buf2 as u32);
    pc.memory.write_u16(prd_addr + 12, 256);
    pc.memory.write_u16(prd_addr + 14, 0x8000);

    pc.io.write(bm_base + 4, 4, prd_addr as u32);

    // READ DMA for LBA 0, 1 sector.
    pc.io.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    pc.io.write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    pc.io.write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    pc.io.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    pc.io.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    pc.io.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xC8);

    pc.io.write(bm_base, 1, 0x09);
    pc.process_ide();

    let mut out1 = vec![0u8; 256];
    let mut out2 = vec![0u8; 256];
    pc.memory.read_physical(buf1, &mut out1);
    pc.memory.read_physical(buf2, &mut out2);
    assert!(out1.iter().all(|b| *b == 0xAA));
    assert!(out2.iter().all(|b| *b == 0xBB));
}

#[test]
fn pc_platform_ide_dma_writes_mark_dirty_pages_when_enabled() {
    let capacity = 8 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut sector0 = vec![0u8; SECTOR_SIZE];
    sector0[0..4].copy_from_slice(b"BOOT");
    disk.write_sectors(0, &sector0).unwrap();

    let mut pc = PcPlatform::new_with_ide_dirty_tracking(2 * 1024 * 1024);
    pc.attach_ide_primary_master_disk(Box::new(disk)).unwrap();

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

    // Clear dirty tracking for CPU-initiated setup writes. We want to observe only the DMA
    // writes performed by the device model.
    pc.memory.clear_dirty();

    pc.process_ide();

    let mut out = [0u8; 4];
    pc.memory.read_physical(read_buf, &mut out);
    assert_eq!(&out, b"BOOT");

    let page_size = u64::from(pc.memory.dirty_page_size());
    let expected_page = read_buf / page_size;

    let dirty = pc
        .memory
        .take_dirty_pages()
        .expect("dirty tracking enabled");
    assert!(
        dirty.contains(&expected_page),
        "dirty pages should include IDE DMA buffer page (got {dirty:?})"
    );
}

#[test]
fn pc_platform_ide_dma_works_after_bus_master_bar4_relocation() {
    let capacity = 8 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut sector0 = vec![0u8; SECTOR_SIZE];
    sector0[0..4].copy_from_slice(b"BOOT");
    disk.write_sectors(0, &sector0).unwrap();

    let mut pc = PcPlatform::new_with_ide(2 * 1024 * 1024);
    pc.attach_ide_primary_master_disk(Box::new(disk)).unwrap();

    // Unmask IRQ2 (cascade) and IRQ14 so we can observe primary IDE IRQ delivery via the PIC.
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().set_offsets(0x20, 0x28);
        interrupts.pic_mut().set_masked(2, false);
        interrupts.pic_mut().set_masked(14, false);
    }

    let bdf = IDE_PIIX3.bdf;
    let old_bm_base = read_io_bar_base(&mut pc, bdf.bus, bdf.device, bdf.function, 4);
    assert_ne!(old_bm_base, 0);

    // Enable bus mastering for DMA (keep I/O decoding enabled).
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0005);

    // Relocate BAR4 to a new base within the platform's PCI I/O window. BAR4 is a 16-byte I/O BAR,
    // so ensure 16-byte alignment.
    let new_bm_base: u16 = old_bm_base.wrapping_add(0x1000);
    assert_eq!(new_bm_base % 0x10, 0);
    write_cfg_u32(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        0x20,
        u32::from(new_bm_base),
    );
    assert_eq!(
        read_io_bar_base(&mut pc, bdf.bus, bdf.device, bdf.function, 4),
        new_bm_base
    );

    // Old base should float high after relocation.
    assert_eq!(pc.io.read(old_bm_base, 1), 0xFF);

    // PRD table at 0x1000: one entry, end-of-table, 512 bytes.
    let prd_addr = 0x1000u64;
    let read_buf = 0x2000u64;
    pc.memory.write_u32(prd_addr, read_buf as u32);
    pc.memory.write_u16(prd_addr + 4, SECTOR_SIZE as u16);
    pc.memory.write_u16(prd_addr + 6, 0x8000);

    pc.io.write(new_bm_base + 4, 4, prd_addr as u32);

    // Issue READ DMA (LBA 0, 1 sector).
    pc.io.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    pc.io.write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    pc.io.write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    pc.io.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    pc.io.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    pc.io.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xC8); // READ DMA

    // Start bus master (direction = to memory).
    pc.io.write(new_bm_base, 1, 0x09);
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

    // Consume and EOI the interrupt.
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().acknowledge(pending);
        interrupts.pic_mut().eoi(pending);
    }

    let mut out = [0u8; 4];
    pc.memory.read_physical(read_buf, &mut out);
    assert_eq!(&out, b"BOOT");

    // Clear device interrupt and ensure the PIC no longer sees a pending vector.
    let _ = pc.io.read(PRIMARY_PORTS.cmd_base + 7, 1);
    pc.poll_pci_intx_lines();
    assert_eq!(pc.interrupts.borrow().pic().get_pending_vector(), None);
}

fn send_atapi_packet(pc: &mut PcPlatform, base: u16, features: u8, pkt: &[u8; 12], byte_count: u16) {
    pc.io.write(base + 1, 1, features as u32);
    pc.io.write(base + 4, 1, (byte_count & 0xFF) as u32);
    pc.io.write(base + 5, 1, (byte_count >> 8) as u32);
    pc.io.write(base + 7, 1, 0xA0); // PACKET
    for i in 0..6 {
        let w = u16::from_le_bytes([pkt[i * 2], pkt[i * 2 + 1]]);
        pc.io.write(base, 2, w as u32);
    }
}

#[test]
fn pc_platform_enumerates_piix3_ide_at_canonical_bdf_and_atapi_works_on_secondary_master() {
    let mut pc = PcPlatform::new_with_ide(2 * 1024 * 1024);

    // Confirm the PCI function exists at the canonical BDF.
    let bdf = IDE_PIIX3.bdf;
    let id = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x00);
    assert_eq!(id & 0xffff, u32::from(IDE_PIIX3.vendor_id));
    assert_eq!((id >> 16) & 0xffff, u32::from(IDE_PIIX3.device_id));

    // Attach an ISO-backed CD-ROM as "secondary master" (canonical Win7 install media slot).
    // Use the platform helper that adapts a byte-addressed `VirtualDisk` into ATAPI 2048-byte sectors.
    let mut iso_disk =
        RawDisk::create(MemBackend::new(), 2 * AtapiCdrom::SECTOR_SIZE as u64).unwrap();
    iso_disk
        .write_at(AtapiCdrom::SECTOR_SIZE as u64, b"WORLD")
        .unwrap();
    pc.attach_ide_secondary_master_iso(Box::new(iso_disk)).unwrap();

    // Select master on secondary channel.
    pc.io.write(SECONDARY_PORTS.cmd_base + 6, 1, 0xA0);

    // INQUIRY (alloc 36).
    let mut inquiry = [0u8; 12];
    inquiry[0] = 0x12;
    inquiry[4] = 36;
    send_atapi_packet(&mut pc, SECONDARY_PORTS.cmd_base, 0, &inquiry, 36);

    let mut inq_buf = [0u8; 36];
    for i in 0..(36 / 2) {
        let w = pc.io.read(SECONDARY_PORTS.cmd_base, 2) as u16;
        inq_buf[i * 2..i * 2 + 2].copy_from_slice(&w.to_le_bytes());
    }
    assert_eq!(&inq_buf[8..12], b"AERO");

    // Clear initial UNIT ATTENTION: TEST UNIT READY then REQUEST SENSE.
    let tur = [0u8; 12];
    send_atapi_packet(&mut pc, SECONDARY_PORTS.cmd_base, 0, &tur, 0);
    let _ = pc.io.read(SECONDARY_PORTS.cmd_base + 7, 1);

    let mut req_sense = [0u8; 12];
    req_sense[0] = 0x03;
    req_sense[4] = 18;
    send_atapi_packet(&mut pc, SECONDARY_PORTS.cmd_base, 0, &req_sense, 18);
    for _ in 0..(18 / 2) {
        let _ = pc.io.read(SECONDARY_PORTS.cmd_base, 2);
    }

    // READ(10) for LBA=1, blocks=1 (should start with "WORLD").
    let mut read10 = [0u8; 12];
    read10[0] = 0x28;
    read10[2..6].copy_from_slice(&1u32.to_be_bytes());
    read10[7..9].copy_from_slice(&1u16.to_be_bytes());
    send_atapi_packet(&mut pc, SECONDARY_PORTS.cmd_base, 0, &read10, 2048);

    let mut out = vec![0u8; 2048];
    for i in 0..(2048 / 2) {
        let w = pc.io.read(SECONDARY_PORTS.cmd_base, 2) as u16;
        out[i * 2..i * 2 + 2].copy_from_slice(&w.to_le_bytes());
    }
    assert_eq!(&out[..5], b"WORLD");
}

#[test]
fn pc_platform_ide_atapi_dma_raises_secondary_irq15() {
    let mut pc = PcPlatform::new_with_ide(2 * 1024 * 1024);

    // Attach ISO with recognizable bytes at sector 0.
    let mut iso_disk =
        RawDisk::create(MemBackend::new(), AtapiCdrom::SECTOR_SIZE as u64).unwrap();
    iso_disk.write_at(0, b"DMATEST!").unwrap();
    pc.attach_ide_secondary_master_iso(Box::new(iso_disk)).unwrap();

    let bdf = IDE_PIIX3.bdf;

    // Program PIC offsets and unmask IRQ2 (cascade) + IRQ15 so we can observe the interrupt.
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().set_offsets(0x20, 0x28);
        interrupts.pic_mut().set_masked(2, false);
        interrupts.pic_mut().set_masked(15, false);
    }

    // Enable IO decode + Bus Mastering so BMIDE DMA is allowed.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0005);

    // Read BAR4 (bus master I/O base).
    let bar4 = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x20);
    let bm_base = (bar4 & 0xffff_fffc) as u16;
    assert_ne!(bm_base, 0, "BAR4 should be programmed by BIOS POST");

    // Select master on secondary channel.
    pc.io.write(SECONDARY_PORTS.cmd_base + 6, 1, 0xA0);

    // Clear initial UNIT ATTENTION: TEST UNIT READY then REQUEST SENSE.
    let tur = [0u8; 12];
    send_atapi_packet(&mut pc, SECONDARY_PORTS.cmd_base, 0, &tur, 0);
    let _ = pc.io.read(SECONDARY_PORTS.cmd_base + 7, 1);

    let mut req_sense = [0u8; 12];
    req_sense[0] = 0x03;
    req_sense[4] = 18;
    send_atapi_packet(&mut pc, SECONDARY_PORTS.cmd_base, 0, &req_sense, 18);
    for _ in 0..(18 / 2) {
        let _ = pc.io.read(SECONDARY_PORTS.cmd_base, 2);
    }

    // PRD table and DMA buffer in guest RAM.
    let prd_addr = 0x1000u64;
    let dma_buf = 0x3000u64;

    // One PRD entry: 2048 bytes, EOT.
    pc.memory.write_u32(prd_addr, dma_buf as u32);
    pc.memory.write_u16(prd_addr + 4, 2048);
    pc.memory.write_u16(prd_addr + 6, 0x8000);

    // Program secondary PRD pointer (BMIDE base + 8 + 4).
    pc.io.write(bm_base + 8 + 4, 4, prd_addr as u32);

    // READ(10) for LBA=0, blocks=1 with DMA enabled (FEATURES bit0).
    let mut read10 = [0u8; 12];
    read10[0] = 0x28;
    read10[2..6].copy_from_slice(&0u32.to_be_bytes());
    read10[7..9].copy_from_slice(&1u16.to_be_bytes());
    send_atapi_packet(&mut pc, SECONDARY_PORTS.cmd_base, 0x01, &read10, 2048);

    // Start secondary bus master, direction=read (device -> memory).
    pc.io.write(bm_base + 8, 1, 0x09);
    pc.process_ide();
    pc.poll_pci_intx_lines();

    let mut out = [0u8; 8];
    pc.memory.read_physical(dma_buf, &mut out);
    assert_eq!(&out, b"DMATEST!");

    let pending = pc
        .interrupts
        .borrow()
        .pic()
        .get_pending_vector()
        .expect("IRQ15 should be pending after ATAPI DMA completes");
    let irq = pc
        .interrupts
        .borrow()
        .pic()
        .vector_to_irq(pending)
        .expect("pending vector should decode to an IRQ number");
    assert_eq!(irq, 15);

    // ACK+EOI so the PIC isn't left with stale state if this test is extended.
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().acknowledge(pending);
        interrupts.pic_mut().eoi(pending);
    }
}

#[test]
fn pc_platform_ide_atapi_dma_requires_pci_bus_master_enable() {
    let mut pc = PcPlatform::new_with_ide(2 * 1024 * 1024);

    // Attach ISO with recognizable bytes at sector 0.
    let mut iso_disk = RawDisk::create(MemBackend::new(), AtapiCdrom::SECTOR_SIZE as u64).unwrap();
    iso_disk.write_at(0, b"DMATEST!").unwrap();
    pc.attach_ide_secondary_master_iso(Box::new(iso_disk)).unwrap();

    let bdf = IDE_PIIX3.bdf;

    // Program PIC offsets and unmask IRQ2 (cascade) + IRQ15 so we can observe the interrupt.
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().set_offsets(0x20, 0x28);
        interrupts.pic_mut().set_masked(2, false);
        interrupts.pic_mut().set_masked(15, false);
    }

    let bm_base = read_io_bar_base(&mut pc, bdf.bus, bdf.device, bdf.function, 4);
    assert_ne!(bm_base, 0, "BAR4 should be programmed by BIOS POST");

    // Ensure IO decode is enabled but bus mastering is disabled.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0001);

    // Select master on secondary channel.
    pc.io.write(SECONDARY_PORTS.cmd_base + 6, 1, 0xA0);

    // Clear initial UNIT ATTENTION: TEST UNIT READY then REQUEST SENSE.
    let tur = [0u8; 12];
    send_atapi_packet(&mut pc, SECONDARY_PORTS.cmd_base, 0, &tur, 0);
    let _ = pc.io.read(SECONDARY_PORTS.cmd_base + 7, 1);

    let mut req_sense = [0u8; 12];
    req_sense[0] = 0x03;
    req_sense[4] = 18;
    send_atapi_packet(&mut pc, SECONDARY_PORTS.cmd_base, 0, &req_sense, 18);
    for _ in 0..(18 / 2) {
        let _ = pc.io.read(SECONDARY_PORTS.cmd_base, 2);
    }
    // Clear any pending IRQ from REQUEST SENSE.
    let _ = pc.io.read(SECONDARY_PORTS.cmd_base + 7, 1);
    pc.poll_pci_intx_lines();
    assert_eq!(pc.interrupts.borrow().pic().get_pending_vector(), None);

    // PRD table and DMA buffer in guest RAM.
    let prd_addr = 0x1000u64;
    let dma_buf = 0x3000u64;

    // One PRD entry: 2048 bytes, EOT.
    pc.memory.write_u32(prd_addr, dma_buf as u32);
    pc.memory.write_u16(prd_addr + 4, 2048);
    pc.memory.write_u16(prd_addr + 6, 0x8000);

    // Program secondary PRD pointer (BMIDE base + 8 + 4).
    pc.io.write(bm_base + 8 + 4, 4, prd_addr as u32);

    // READ(10) for LBA=0, blocks=1 with DMA enabled (FEATURES bit0).
    let mut read10 = [0u8; 12];
    read10[0] = 0x28;
    read10[2..6].copy_from_slice(&0u32.to_be_bytes());
    read10[7..9].copy_from_slice(&1u16.to_be_bytes());
    send_atapi_packet(&mut pc, SECONDARY_PORTS.cmd_base, 0x01, &read10, 2048);

    // The PACKET command enters a "packet-out" phase and raises an interrupt to request the
    // 12-byte command packet. The helper above writes the packet synchronously, so clear that
    // interrupt before checking DMA completion behavior.
    let _ = pc.io.read(SECONDARY_PORTS.cmd_base + 7, 1);
    pc.poll_pci_intx_lines();
    assert_eq!(pc.interrupts.borrow().pic().get_pending_vector(), None);

    // Start secondary bus master, direction=read (device -> memory).
    pc.io.write(bm_base + 8, 1, 0x09);

    pc.process_ide();
    pc.poll_pci_intx_lines();

    assert_eq!(pc.memory.read_u8(dma_buf), 0, "DMA should be gated off");
    assert_eq!(
        pc.interrupts.borrow().pic().get_pending_vector(),
        None,
        "IRQ15 should not be delivered when bus mastering is disabled"
    );

    // Now enable bus mastering and retry processing; the pending DMA request should complete.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0005);
    pc.process_ide();
    pc.poll_pci_intx_lines();

    let mut out = [0u8; 8];
    pc.memory.read_physical(dma_buf, &mut out);
    assert_eq!(&out, b"DMATEST!");

    let pending = pc
        .interrupts
        .borrow()
        .pic()
        .get_pending_vector()
        .expect("IRQ15 should be pending after ATAPI DMA completes");
    let irq = pc
        .interrupts
        .borrow()
        .pic()
        .vector_to_irq(pending)
        .expect("pending vector should decode to an IRQ number");
    assert_eq!(irq, 15);

    // ACK+EOI so the PIC isn't left with stale state if this test is extended.
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().acknowledge(pending);
        interrupts.pic_mut().eoi(pending);
    }

    // Clear device interrupt and ensure we don't leave the PIC with stale state.
    let _ = pc.io.read(SECONDARY_PORTS.cmd_base + 7, 1);
    pc.poll_pci_intx_lines();
    assert_eq!(pc.interrupts.borrow().pic().get_pending_vector(), None);
}

#[test]
fn pc_platform_ide_secondary_nien_suppresses_irq15_for_atapi_dma() {
    let mut pc = PcPlatform::new_with_ide(2 * 1024 * 1024);

    // Attach ISO with recognizable bytes at sector 0.
    let mut iso_disk = RawDisk::create(MemBackend::new(), AtapiCdrom::SECTOR_SIZE as u64).unwrap();
    iso_disk.write_at(0, b"DMATEST!").unwrap();
    pc.attach_ide_secondary_master_iso(Box::new(iso_disk)).unwrap();

    let bdf = IDE_PIIX3.bdf;

    // Program PIC offsets and unmask IRQ2 (cascade) + IRQ15 so we can observe the interrupt.
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().set_offsets(0x20, 0x28);
        interrupts.pic_mut().set_masked(2, false);
        interrupts.pic_mut().set_masked(15, false);
    }

    // Enable IO decode + Bus Mastering so BMIDE DMA is allowed.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0005);

    let bm_base = read_io_bar_base(&mut pc, bdf.bus, bdf.device, bdf.function, 4);
    assert_ne!(bm_base, 0, "BAR4 should be programmed by BIOS POST");

    // Select master on secondary channel.
    pc.io.write(SECONDARY_PORTS.cmd_base + 6, 1, 0xA0);

    // Clear initial UNIT ATTENTION: TEST UNIT READY then REQUEST SENSE.
    let tur = [0u8; 12];
    send_atapi_packet(&mut pc, SECONDARY_PORTS.cmd_base, 0, &tur, 0);
    let _ = pc.io.read(SECONDARY_PORTS.cmd_base + 7, 1);

    let mut req_sense = [0u8; 12];
    req_sense[0] = 0x03;
    req_sense[4] = 18;
    send_atapi_packet(&mut pc, SECONDARY_PORTS.cmd_base, 0, &req_sense, 18);
    for _ in 0..(18 / 2) {
        let _ = pc.io.read(SECONDARY_PORTS.cmd_base, 2);
    }
    // Clear any pending IRQ from REQUEST SENSE.
    let _ = pc.io.read(SECONDARY_PORTS.cmd_base + 7, 1);
    pc.poll_pci_intx_lines();
    assert_eq!(pc.interrupts.borrow().pic().get_pending_vector(), None);

    // PRD table and DMA buffer in guest RAM.
    let prd_addr = 0x1000u64;
    let dma_buf = 0x3000u64;

    // One PRD entry: 2048 bytes, EOT.
    pc.memory.write_u32(prd_addr, dma_buf as u32);
    pc.memory.write_u16(prd_addr + 4, 2048);
    pc.memory.write_u16(prd_addr + 6, 0x8000);

    // Program secondary PRD pointer (BMIDE base + 8 + 4).
    pc.io.write(bm_base + 8 + 4, 4, prd_addr as u32);

    // Disable interrupts on the secondary channel (nIEN=1).
    pc.io.write(SECONDARY_PORTS.ctrl_base, 1, 0x02);

    // READ(10) for LBA=0, blocks=1 with DMA enabled (FEATURES bit0).
    let mut read10 = [0u8; 12];
    read10[0] = 0x28;
    read10[2..6].copy_from_slice(&0u32.to_be_bytes());
    read10[7..9].copy_from_slice(&1u16.to_be_bytes());
    send_atapi_packet(&mut pc, SECONDARY_PORTS.cmd_base, 0x01, &read10, 2048);

    // Start secondary bus master, direction=read (device -> memory).
    pc.io.write(bm_base + 8, 1, 0x09);
    pc.process_ide();
    pc.poll_pci_intx_lines();

    let mut out = [0u8; 8];
    pc.memory.read_physical(dma_buf, &mut out);
    assert_eq!(&out, b"DMATEST!");

    assert_eq!(
        pc.interrupts.borrow().pic().get_pending_vector(),
        None,
        "IRQ15 should be suppressed when nIEN=1 on secondary channel"
    );

    // Re-enable interrupts and repeat the transfer; now IRQ15 should be delivered.
    pc.io.write(SECONDARY_PORTS.ctrl_base, 1, 0x00);
    // Clear bus master status bits and any stale state.
    pc.io.write(bm_base + 8, 1, 0);
    pc.io.write(bm_base + 8 + 2, 1, 0x06); // clear IRQ+ERR
    let _ = pc.io.read(SECONDARY_PORTS.cmd_base + 7, 1);
    pc.poll_pci_intx_lines();
    assert_eq!(pc.interrupts.borrow().pic().get_pending_vector(), None);

    // Reuse the same PRD pointer and issue another READ(10).
    pc.memory.write_u32(dma_buf, 0);
    send_atapi_packet(&mut pc, SECONDARY_PORTS.cmd_base, 0x01, &read10, 2048);
    pc.io.write(bm_base + 8, 1, 0x09);
    pc.process_ide();
    pc.poll_pci_intx_lines();

    let pending = pc
        .interrupts
        .borrow()
        .pic()
        .get_pending_vector()
        .expect("IRQ15 should be pending after ATAPI DMA completes with nIEN=0");
    let irq = pc
        .interrupts
        .borrow()
        .pic()
        .vector_to_irq(pending)
        .expect("pending vector should decode to an IRQ number");
    assert_eq!(irq, 15);

    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().acknowledge(pending);
        interrupts.pic_mut().eoi(pending);
    }

    // Clear device interrupt and ensure we don't leave the PIC with stale state.
    let _ = pc.io.read(SECONDARY_PORTS.cmd_base + 7, 1);
    pc.poll_pci_intx_lines();
    assert_eq!(pc.interrupts.borrow().pic().get_pending_vector(), None);
}

#[test]
fn pc_platform_routes_ide_irq14_via_ioapic_in_apic_mode() {
    let capacity = 8 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut sector0 = vec![0u8; SECTOR_SIZE];
    sector0[0..4].copy_from_slice(b"BOOT");
    disk.write_sectors(0, &sector0).unwrap();

    let mut pc = PcPlatform::new_with_ide(2 * 1024 * 1024);
    pc.attach_ide_primary_master_disk(Box::new(disk)).unwrap();

    // Switch the platform into APIC mode via IMCR (0x22/0x23).
    pc.io.write_u8(0x22, 0x70);
    pc.io.write_u8(0x23, 0x01);
    assert_eq!(pc.interrupts.borrow().mode(), PlatformInterruptMode::Apic);

    // Program IOAPIC entry for ISA IRQ14 (GSI14) to vector 0x60, edge-triggered, active-high.
    let vector = 0x60u32;
    program_ioapic_entry(&mut pc, 14, vector, 0);

    let bdf = IDE_PIIX3.bdf;
    let bm_base = read_io_bar_base(&mut pc, bdf.bus, bdf.device, bdf.function, 4);
    assert_ne!(bm_base, 0, "BAR4 should be programmed by BIOS POST");

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

    // IOAPIC should have delivered the vector through the LAPIC.
    assert_eq!(pc.interrupts.borrow().get_pending(), Some(vector as u8));

    // Acknowledge the interrupt (vector in service).
    pc.interrupts.borrow_mut().acknowledge(vector as u8);

    let mut out = [0u8; 4];
    pc.memory.read_physical(read_buf, &mut out);
    assert_eq!(&out, b"BOOT");

    // Clear the IDE device interrupt by reading the status register and propagating the deassertion
    // before EOI, to avoid immediately retriggering in level-triggered configurations.
    let _ = pc.io.read(PRIMARY_PORTS.cmd_base + 7, 1);
    pc.poll_pci_intx_lines();

    pc.interrupts.borrow_mut().eoi(vector as u8);
    assert_eq!(pc.interrupts.borrow().get_pending(), None);
}

#[test]
fn pc_platform_routes_ide_irq15_via_ioapic_in_apic_mode() {
    let mut pc = PcPlatform::new_with_ide(2 * 1024 * 1024);

    // Attach ISO with recognizable bytes at sector 0.
    let mut iso_disk = RawDisk::create(MemBackend::new(), AtapiCdrom::SECTOR_SIZE as u64).unwrap();
    iso_disk.write_at(0, b"DMATEST!").unwrap();
    pc.attach_ide_secondary_master_iso(Box::new(iso_disk)).unwrap();

    // Switch the platform into APIC mode via IMCR (0x22/0x23).
    pc.io.write_u8(0x22, 0x70);
    pc.io.write_u8(0x23, 0x01);
    assert_eq!(pc.interrupts.borrow().mode(), PlatformInterruptMode::Apic);

    // Program IOAPIC entry for ISA IRQ15 (GSI15) to vector 0x61, edge-triggered, active-high.
    let vector = 0x61u32;
    program_ioapic_entry(&mut pc, 15, vector, 0);

    let bdf = IDE_PIIX3.bdf;

    // Enable IO decode + Bus Mastering so BMIDE DMA is allowed.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0005);

    let bm_base = read_io_bar_base(&mut pc, bdf.bus, bdf.device, bdf.function, 4);
    assert_ne!(bm_base, 0, "BAR4 should be programmed by BIOS POST");

    // Select master on secondary channel.
    pc.io.write(SECONDARY_PORTS.cmd_base + 6, 1, 0xA0);

    // Clear initial UNIT ATTENTION: TEST UNIT READY then REQUEST SENSE.
    let tur = [0u8; 12];
    send_atapi_packet(&mut pc, SECONDARY_PORTS.cmd_base, 0, &tur, 0);
    let _ = pc.io.read(SECONDARY_PORTS.cmd_base + 7, 1);

    let mut req_sense = [0u8; 12];
    req_sense[0] = 0x03;
    req_sense[4] = 18;
    send_atapi_packet(&mut pc, SECONDARY_PORTS.cmd_base, 0, &req_sense, 18);
    for _ in 0..(18 / 2) {
        let _ = pc.io.read(SECONDARY_PORTS.cmd_base, 2);
    }

    // Clear any pending device interrupt from REQUEST SENSE before starting the DMA read.
    let _ = pc.io.read(SECONDARY_PORTS.cmd_base + 7, 1);
    pc.poll_pci_intx_lines();
    assert_eq!(pc.interrupts.borrow().get_pending(), None);

    // PRD table and DMA buffer in guest RAM.
    let prd_addr = 0x1000u64;
    let dma_buf = 0x3000u64;

    // One PRD entry: 2048 bytes, EOT.
    pc.memory.write_u32(prd_addr, dma_buf as u32);
    pc.memory.write_u16(prd_addr + 4, 2048);
    pc.memory.write_u16(prd_addr + 6, 0x8000);

    // Program secondary PRD pointer (BMIDE base + 8 + 4).
    pc.io.write(bm_base + 8 + 4, 4, prd_addr as u32);

    // READ(10) for LBA=0, blocks=1 with DMA enabled (FEATURES bit0).
    let mut read10 = [0u8; 12];
    read10[0] = 0x28;
    read10[2..6].copy_from_slice(&0u32.to_be_bytes());
    read10[7..9].copy_from_slice(&1u16.to_be_bytes());
    send_atapi_packet(&mut pc, SECONDARY_PORTS.cmd_base, 0x01, &read10, 2048);

    // The PACKET command enters a "packet-out" phase and raises an interrupt to request the
    // 12-byte command packet. The helper above writes the packet synchronously, so clear that
    // interrupt before checking DMA completion behavior.
    let _ = pc.io.read(SECONDARY_PORTS.cmd_base + 7, 1);
    pc.poll_pci_intx_lines();
    assert_eq!(pc.interrupts.borrow().get_pending(), None);

    // Start secondary bus master, direction=read (device -> memory).
    pc.io.write(bm_base + 8, 1, 0x09);
    pc.process_ide();
    pc.poll_pci_intx_lines();

    let mut out = [0u8; 8];
    pc.memory.read_physical(dma_buf, &mut out);
    assert_eq!(&out, b"DMATEST!");

    // IOAPIC should have delivered the vector through the LAPIC.
    assert_eq!(pc.interrupts.borrow().get_pending(), Some(vector as u8));

    pc.interrupts.borrow_mut().acknowledge(vector as u8);

    // Clear the device interrupt so the IRQ line deasserts (and won't retrigger in
    // level-triggered configurations).
    let _ = pc.io.read(SECONDARY_PORTS.cmd_base + 7, 1);
    pc.poll_pci_intx_lines();

    pc.interrupts.borrow_mut().eoi(vector as u8);
    assert_eq!(pc.interrupts.borrow().get_pending(), None);
}
