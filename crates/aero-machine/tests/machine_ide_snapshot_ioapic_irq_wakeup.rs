#![cfg(not(target_arch = "wasm32"))]

use aero_devices::pci::profile::IDE_PIIX3;
use aero_devices::pci::{PciBdf, PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT};
use aero_devices_storage::pci_ide::PRIMARY_PORTS;
use aero_machine::{Machine, MachineConfig, RunExit};
use aero_platform::interrupts::{PlatformInterruptMode, PlatformInterrupts};
use aero_storage::{MemBackend, RawDisk, VirtualDisk as _, SECTOR_SIZE};
use pretty_assertions::{assert_eq, assert_ne};

fn cfg_addr(bdf: PciBdf, offset: u8) -> u32 {
    0x8000_0000
        | ((u32::from(bdf.bus)) << 16)
        | ((u32::from(bdf.device & 0x1F)) << 11)
        | ((u32::from(bdf.function & 0x07)) << 8)
        | (u32::from(offset) & 0xFC)
}

fn write_cfg_u16(m: &mut Machine, bdf: PciBdf, offset: u8, value: u16) {
    m.io_write(PCI_CFG_ADDR_PORT, 4, cfg_addr(bdf, offset));
    m.io_write(PCI_CFG_DATA_PORT, 2, u32::from(value));
}

fn read_cfg_u32(m: &mut Machine, bdf: PciBdf, offset: u8) -> u32 {
    m.io_write(PCI_CFG_ADDR_PORT, 4, cfg_addr(bdf, offset));
    m.io_read(PCI_CFG_DATA_PORT, 4)
}

fn write_ivt_entry(m: &mut Machine, vector: u8, segment: u16, offset: u16) {
    let base = u64::from(vector) * 4;
    m.write_physical_u16(base, offset);
    m.write_physical_u16(base + 2, segment);
}

fn install_real_mode_handler(m: &mut Machine, handler_addr: u64, flag_addr: u16, value: u8) {
    let flag_addr_bytes = flag_addr.to_le_bytes();

    // mov byte ptr [imm16], imm8
    // iret
    let code = [
        0xC6,
        0x06,
        flag_addr_bytes[0],
        flag_addr_bytes[1],
        value,
        0xCF,
    ];
    m.write_physical(handler_addr, &code);
}

fn install_hlt_loop(m: &mut Machine, code_base: u64) {
    // hlt; jmp short $-3 (back to hlt)
    let code = [0xF4u8, 0xEB, 0xFD];
    m.write_physical(code_base, &code);
}

fn setup_real_mode_cpu(m: &mut Machine, entry_ip: u64) {
    let cpu = m.cpu_mut();

    // Real-mode segments: base = selector<<4, limit = 0xFFFF.
    for seg in [
        &mut cpu.segments.cs,
        &mut cpu.segments.ds,
        &mut cpu.segments.es,
        &mut cpu.segments.ss,
        &mut cpu.segments.fs,
        &mut cpu.segments.gs,
    ] {
        seg.selector = 0;
        seg.base = 0;
        seg.limit = 0xFFFF;
        seg.access = 0;
    }

    cpu.set_stack_ptr(0x7000);
    cpu.set_rip(entry_ip);
    cpu.set_rflags(0x202); // IF=1
    cpu.halted = false;
}

fn program_ioapic_redirection_entry(ints: &mut PlatformInterrupts, gsi: u32, low: u32, high: u32) {
    let redtbl_low = 0x10u32 + gsi * 2;
    let redtbl_high = redtbl_low + 1;
    ints.ioapic_mmio_write(0x00, redtbl_low);
    ints.ioapic_mmio_write(0x10, low);
    ints.ioapic_mmio_write(0x00, redtbl_high);
    ints.ioapic_mmio_write(0x10, high);
}

#[test]
fn machine_snapshot_roundtrip_preserves_inflight_ide_dma_read_and_wakes_hlt_via_ioapic() {
    const RAM_SIZE: u64 = 2 * 1024 * 1024;
    const VECTOR: u8 = 0x60;

    let cfg = MachineConfig {
        ram_size_bytes: RAM_SIZE,
        enable_pc_platform: true,
        enable_ide: true,
        // Keep this test focused on IDE + snapshot/restore + IOAPIC delivery.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    };

    let mut src = Machine::new(cfg.clone()).unwrap();

    // Attach a small disk to IDE primary master and seed it with a known prefix.
    let capacity = 4 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut sector0 = vec![0u8; SECTOR_SIZE];
    sector0[0..8].copy_from_slice(b"SNAPDMA!");
    disk.write_sectors(0, &sector0).unwrap();
    src.attach_ide_primary_master_disk(Box::new(disk)).unwrap();

    // Route IOAPIC vector into a real-mode handler that writes a flag byte.
    let handler_addr = 0x8000u64;
    let code_base = 0x9000u64;
    let flag_addr = 0x0500u16;
    let flag_value = 0xA9_u8;

    // Keep the initial flag deterministic even if BIOS touched this region.
    src.write_physical_u8(u64::from(flag_addr), 0);

    install_real_mode_handler(&mut src, handler_addr, flag_addr, flag_value);
    install_hlt_loop(&mut src, code_base);
    write_ivt_entry(&mut src, VECTOR, 0x0000, handler_addr as u16);
    setup_real_mode_cpu(&mut src, code_base);

    // Halt the CPU first so the interrupt must wake it.
    assert!(matches!(src.run_slice(16), RunExit::Halted { .. }));

    // Switch to APIC mode and program IOAPIC redirection entry for GSI14 -> VECTOR.
    {
        let interrupts = src
            .platform_interrupts()
            .expect("pc platform should provide interrupts");
        let mut ints = interrupts.borrow_mut();
        ints.set_mode(PlatformInterruptMode::Apic);
        program_ioapic_redirection_entry(&mut ints, 14, u32::from(VECTOR), 0);
    }

    let bdf = IDE_PIIX3.bdf;

    // Enable PCI I/O decode + bus mastering.
    write_cfg_u16(&mut src, bdf, 0x04, 0x0005);

    // Resolve the BMIDE BAR4 base assigned by BIOS POST.
    let bar4_raw = read_cfg_u32(&mut src, bdf, 0x20);
    let bm_base = (bar4_raw & 0xFFFF_FFFC) as u16;
    assert_ne!(bm_base, 0, "expected BMIDE BAR4 base to be programmed");

    // Guest physical addresses for PRD table + DMA buffer.
    let prd_addr = 0x1000u64;
    let data_buf = 0x2000u64;

    // PRD entry: 512 bytes, end-of-table.
    src.write_physical_u32(prd_addr, data_buf as u32);
    src.write_physical_u16(prd_addr + 4, SECTOR_SIZE as u16);
    src.write_physical_u16(prd_addr + 6, 0x8000);

    // Clear destination buffer so the DMA result is observable.
    src.write_physical(data_buf, &[0u8; 8]);

    // Program BMIDE and start the engine (direction=to-memory).
    src.io_write(bm_base + 4, 4, prd_addr as u32);
    src.io_write(bm_base + 2, 1, 0x06); // clear error/irq bits (defensive)
    src.io_write(bm_base, 1, 0x09); // start + direction=to-memory

    // Issue ATA READ DMA (0xC8) for LBA 0, count 1, primary master.
    src.io_write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    src.io_write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    src.io_write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    src.io_write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    src.io_write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    src.io_write(PRIMARY_PORTS.cmd_base + 7, 1, 0xC8);

    // Ensure the DMA has not run yet (we have not ticked the controller).
    assert_eq!(src.read_physical_bytes(data_buf, 8), vec![0u8; 8]);
    assert_eq!(
        src.io_read(bm_base + 2, 1) as u8 & 0x04,
        0,
        "BMIDE IRQ bit should not be set before DMA execution"
    );

    let snap = src.take_snapshot_full().unwrap();

    let mut restored = Machine::new(cfg).unwrap();
    restored.restore_snapshot_bytes(&snap).unwrap();

    // Host contract: controller restore drops attached disks; reattach after restoring state.
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    disk.write_sectors(0, &sector0).unwrap();
    restored
        .attach_ide_primary_master_disk(Box::new(disk))
        .unwrap();

    let bm_base2 = (read_cfg_u32(&mut restored, bdf, 0x20) & 0xFFFF_FFFC) as u16;
    assert_eq!(
        bm_base2, bm_base,
        "BMIDE BAR4 base should survive snapshot/restore"
    );

    // Run until the interrupt handler writes the flag byte.
    for _ in 0..50 {
        let exit = restored.run_slice(256);
        match exit {
            RunExit::Completed { .. } | RunExit::Halted { .. } => {}
            other => panic!("unexpected exit: {other:?}"),
        }

        if restored.read_physical_u8(u64::from(flag_addr)) == flag_value {
            assert_eq!(restored.read_physical_bytes(data_buf, 8), b"SNAPDMA!");
            assert_ne!(
                restored.io_read(bm_base + 2, 1) as u8 & 0x04,
                0,
                "BMIDE IRQ bit should be set after DMA completion"
            );
            return;
        }
    }

    panic!(
        "IDE DMA completion interrupt was not delivered after snapshot restore (flag=0x{:02x})",
        restored.read_physical_u8(u64::from(flag_addr))
    );
}
