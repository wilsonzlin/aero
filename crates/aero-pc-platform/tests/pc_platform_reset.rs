use aero_devices::pci::profile::{IDE_PIIX3, NVME_CONTROLLER, SATA_AHCI_ICH9, USB_UHCI_PIIX3};
use aero_devices::pci::{PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT};
use aero_devices::reset_ctrl::{RESET_CTRL_PORT, RESET_CTRL_RESET_VALUE};
use aero_devices_storage::pci_ide::PRIMARY_PORTS;
use aero_pc_platform::{PcPlatform, ResetEvent};
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
    pc.io.write(
        PCI_CFG_ADDR_PORT,
        4,
        cfg_addr(bus, device, function, offset),
    );
    pc.io.read(PCI_CFG_DATA_PORT, 4)
}

fn read_io_bar_base(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, bar: u8) -> u16 {
    let off = 0x10 + bar * 4;
    let val = read_cfg_u32(pc, bus, device, function, off);
    u16::try_from(val & 0xFFFF_FFFC).unwrap()
}

fn write_cfg_u16(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, offset: u8, value: u16) {
    pc.io.write(
        PCI_CFG_ADDR_PORT,
        4,
        cfg_addr(bus, device, function, offset),
    );
    pc.io.write(PCI_CFG_DATA_PORT, 2, u32::from(value));
}

fn write_cfg_u32(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, offset: u8, value: u32) {
    pc.io.write(
        PCI_CFG_ADDR_PORT,
        4,
        cfg_addr(bus, device, function, offset),
    );
    pc.io.write(PCI_CFG_DATA_PORT, 4, value);
}

fn read_ahci_bar5_base(pc: &mut PcPlatform) -> u64 {
    let bdf = SATA_AHCI_ICH9.bdf;
    let bar5 = read_cfg_u32(pc, bdf.bus, bdf.device, bdf.function, 0x24);
    u64::from(bar5 & 0xffff_fff0)
}

fn read_nvme_bar0_base(pc: &mut PcPlatform) -> u64 {
    let bdf = NVME_CONTROLLER.bdf;
    let bar0_lo = read_cfg_u32(pc, bdf.bus, bdf.device, bdf.function, 0x10);
    let bar0_hi = read_cfg_u32(pc, bdf.bus, bdf.device, bdf.function, 0x14);
    (u64::from(bar0_hi) << 32) | u64::from(bar0_lo & 0xffff_fff0)
}

#[test]
fn pc_platform_reset_restores_deterministic_power_on_state() {
    let mut pc = PcPlatform::new(2 * 1024 * 1024);

    // Capture initial PCI state so we can verify it's restored deterministically.
    let bar5_base_before = read_ahci_bar5_base(&mut pc);
    let uhci_bdf = USB_UHCI_PIIX3.bdf;
    let uhci_bar4_before = read_cfg_u32(
        &mut pc,
        uhci_bdf.bus,
        uhci_bdf.device,
        uhci_bdf.function,
        0x20,
    );

    // Mutate some state:
    // - Enable A20.
    pc.io.write_u8(0x92, 0x02);
    assert!(pc.chipset.a20().enabled());

    // - Touch the PCI config address latch (PCI config mechanism #1).
    pc.io.write(PCI_CFG_ADDR_PORT, 4, 0x8000_0000);
    assert_eq!(pc.io.read(PCI_CFG_ADDR_PORT, 4), 0x8000_0000);

    // - Relocate UHCI BAR4 to a different base (to ensure PCI resources are reset deterministically).
    write_cfg_u32(
        &mut pc,
        uhci_bdf.bus,
        uhci_bdf.device,
        uhci_bdf.function,
        0x20,
        0xD000,
    );
    let uhci_bar4_after = read_cfg_u32(
        &mut pc,
        uhci_bdf.bus,
        uhci_bdf.device,
        uhci_bdf.function,
        0x20,
    );
    assert_ne!(uhci_bar4_after, uhci_bar4_before);

    // - Queue a reset event.
    pc.io.write_u8(RESET_CTRL_PORT, RESET_CTRL_RESET_VALUE);
    assert_eq!(pc.take_reset_events(), vec![ResetEvent::System]);
    pc.io.write_u8(RESET_CTRL_PORT, RESET_CTRL_RESET_VALUE);

    // - Disable PCI memory decoding for AHCI and move BAR5.
    let bdf = SATA_AHCI_ICH9.bdf;
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0);
    write_cfg_u32(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        0x24,
        (bar5_base_before + 0x10_0000) as u32,
    );

    // Now reset back to baseline.
    pc.reset();

    // A20 must be disabled.
    assert!(!pc.chipset.a20().enabled());

    // Reset should clear any pending reset events.
    assert!(pc.take_reset_events().is_empty());

    // PCI config address latch should be cleared.
    assert_eq!(pc.io.read(PCI_CFG_ADDR_PORT, 4), 0);

    // UHCI BAR4 should be restored to its initial BIOS-assigned value.
    let uhci_bar4_after_reset = read_cfg_u32(
        &mut pc,
        uhci_bdf.bus,
        uhci_bdf.device,
        uhci_bdf.function,
        0x20,
    );
    assert_eq!(uhci_bar4_after_reset, uhci_bar4_before);

    // BIOS POST should deterministically reassign AHCI BAR5 to its original base.
    let bar5_base_after = read_ahci_bar5_base(&mut pc);
    assert_eq!(bar5_base_after, bar5_base_before);

    // Enable A20 so the AHCI MMIO base doesn't alias across the 1MiB boundary (A20 gate).
    pc.io.write_u8(0x92, 0x02);

    // AHCI CAP register must be readable again after reset (i.e. MMIO decoding was restored).
    let cap = pc.memory.read_u32(bar5_base_after);
    assert_ne!(cap, 0xFFFF_FFFF);
    assert_ne!(cap & 0x8000_0000, 0);
}

#[test]
fn pc_platform_reset_resets_nvme_controller_state() {
    let mut pc = PcPlatform::new_with_nvme(2 * 1024 * 1024);
    let bdf = NVME_CONTROLLER.bdf;
    let bar0_base = read_nvme_bar0_base(&mut pc);

    // Enable the controller and mutate a few registers so we can detect that reset cleared them.
    let asq = 0x10000u64;
    let acq = 0x20000u64;

    pc.memory.write_u32(bar0_base + 0x0024, 0x000f_000f); // AQA
    pc.memory.write_u64(bar0_base + 0x0028, asq); // ASQ
    pc.memory.write_u64(bar0_base + 0x0030, acq); // ACQ
    pc.memory.write_u32(bar0_base + 0x0014, 1); // CC.EN
    assert_eq!(pc.memory.read_u32(bar0_base + 0x001c) & 1, 1);

    pc.memory.write_u32(bar0_base + 0x000c, 1); // INTMS
    assert_eq!(pc.memory.read_u32(bar0_base + 0x000c) & 1, 1);

    pc.reset();

    // Re-enable memory decoding in case the post-reset BIOS chose a different policy.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0002);
    let bar0_base_after = read_nvme_bar0_base(&mut pc);

    assert_eq!(
        pc.memory.read_u32(bar0_base_after + 0x0014),
        0,
        "reset should clear NVMe CC register"
    );
    assert_eq!(
        pc.memory.read_u32(bar0_base_after + 0x001c),
        0,
        "reset should clear NVMe CSTS register"
    );
    assert_eq!(
        pc.memory.read_u32(bar0_base_after + 0x000c),
        0,
        "reset should clear NVMe interrupt mask register"
    );
}

#[test]
fn pc_platform_reset_resets_ide_controller_state() {
    let mut pc = PcPlatform::new_with_ide(2 * 1024 * 1024);
    let bdf = IDE_PIIX3.bdf;

    // Attach a disk so status reads are driven by the selected device.
    let disk = RawDisk::create(MemBackend::new(), 8 * SECTOR_SIZE as u64).unwrap();
    pc.attach_ide_primary_master_disk(Box::new(disk)).unwrap();

    // Ensure I/O decoding is enabled so legacy ports + BAR4 are accessible.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0001);

    let bm_base = read_io_bar_base(&mut pc, bdf.bus, bdf.device, bdf.function, 4);
    assert_ne!(bm_base, 0);

    let status_before = pc.io.read(PRIMARY_PORTS.cmd_base + 7, 1) as u8;
    assert_ne!(
        status_before, 0xFF,
        "IDE status should decode with a drive present"
    );

    // Mutate Bus Master IDE registers so we can verify they're cleared by reset.
    pc.io.write(bm_base, 1, 0x09);
    pc.io.write(bm_base + 4, 4, 0x1234_5678);
    assert_eq!(pc.io.read(bm_base, 1), 0x09);
    assert_eq!(pc.io.read(bm_base + 4, 4), 0x1234_5678);

    pc.reset();

    // Re-enable I/O decoding in case the post-reset BIOS chose a different policy.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0001);
    let bm_base_after = read_io_bar_base(&mut pc, bdf.bus, bdf.device, bdf.function, 4);
    assert_ne!(bm_base_after, 0);

    let status_after = pc.io.read(PRIMARY_PORTS.cmd_base + 7, 1) as u8;
    assert_ne!(
        status_after, 0xFF,
        "IDE drive presence should survive platform reset"
    );

    assert_eq!(
        pc.io.read(bm_base_after, 1),
        0,
        "Bus Master IDE command register should be cleared on reset"
    );
    assert_eq!(
        pc.io.read(bm_base_after + 4, 4),
        0,
        "Bus Master IDE PRD pointer should be cleared on reset"
    );
}

#[test]
fn pc_platform_reset_clears_ide_nien_and_allows_irq14_delivery() {
    let capacity = 8 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut sector0 = vec![0u8; SECTOR_SIZE];
    sector0[0..4].copy_from_slice(b"BOOT");
    disk.write_sectors(0, &sector0).unwrap();

    let mut pc = PcPlatform::new_with_ide(2 * 1024 * 1024);
    pc.attach_ide_primary_master_disk(Box::new(disk)).unwrap();

    // Enable legacy I/O decoding so we can program the device control register (nIEN).
    let bdf = IDE_PIIX3.bdf;
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0001);
    let bm_base = read_io_bar_base(&mut pc, bdf.bus, bdf.device, bdf.function, 4);
    assert_ne!(bm_base, 0);

    // Mask IDE interrupts via the device control register.
    pc.io.write(PRIMARY_PORTS.ctrl_base, 1, 0x02);

    pc.reset();

    // Re-enable bus mastering and I/O decode in case the post-reset BIOS chose a different policy.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0005);
    let bm_base_after = read_io_bar_base(&mut pc, bdf.bus, bdf.device, bdf.function, 4);
    assert_ne!(bm_base_after, 0);

    // Unmask IRQ2 (cascade) and IRQ14 so we can observe primary IDE IRQ delivery via the PIC.
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().set_offsets(0x20, 0x28);
        interrupts.pic_mut().set_masked(2, false);
        interrupts.pic_mut().set_masked(14, false);
    }

    // PRD table at 0x1000: one entry, end-of-table, 512 bytes.
    let prd_addr = 0x1000u64;
    let read_buf = 0x2000u64;
    pc.memory.write_u32(prd_addr, read_buf as u32);
    pc.memory.write_u16(prd_addr + 4, SECTOR_SIZE as u16);
    pc.memory.write_u16(prd_addr + 6, 0x8000);
    pc.io.write(bm_base_after + 4, 4, prd_addr as u32);

    // Issue READ DMA (LBA 0, 1 sector) and start bus master.
    pc.io.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    pc.io.write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    pc.io.write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    pc.io.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    pc.io.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    pc.io.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xC8); // READ DMA
    pc.io.write(bm_base_after, 1, 0x09);

    pc.process_ide();
    pc.poll_pci_intx_lines();

    let mut out = [0u8; 4];
    pc.memory.read_physical(read_buf, &mut out);
    assert_eq!(&out, b"BOOT");

    let pending = pc
        .interrupts
        .borrow()
        .pic()
        .get_pending_vector()
        .expect("IRQ14 should be pending after reset clears nIEN");
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

    // Clear the IDE device interrupt by reading the status register.
    let _ = pc.io.read(PRIMARY_PORTS.cmd_base + 7, 1);
    pc.poll_pci_intx_lines();
    assert_eq!(pc.interrupts.borrow().pic().get_pending_vector(), None);
}
