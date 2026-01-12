#![cfg(not(target_arch = "wasm32"))]

use aero_devices::pci::profile::IDE_PIIX3;
use aero_devices::pci::{PciBdf, PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT};
use aero_devices_storage::pci_ide::PRIMARY_PORTS;
use aero_machine::{Machine, MachineConfig, RunExit};
use aero_platform::interrupts::InterruptController as PlatformInterruptController;
use aero_storage::{MemBackend, RawDisk, SECTOR_SIZE};
use pretty_assertions::assert_eq;

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
    cpu.set_rflags(0x202); // IF=1 (caller can override)
    cpu.halted = false;
}

#[test]
fn machine_snapshot_roundtrip_preserves_pending_ide_irq14_in_pic_mode_until_if_is_set() {
    const RAM_SIZE: u64 = 2 * 1024 * 1024;
    const VECTOR: u8 = 0x2E; // PIC slave base 0x28 + (IRQ14-8)

    let cfg = MachineConfig {
        ram_size_bytes: RAM_SIZE,
        enable_pc_platform: true,
        enable_ide: true,
        // Keep this test focused on IDE IRQ14 + PIC interrupt controller snapshot/restore.
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

    // Attach a disk so ATA IDENTIFY has a target.
    let capacity = 8 * SECTOR_SIZE as u64;
    let disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    src.attach_ide_primary_master_disk(Box::new(disk)).unwrap();

    // Install an IRQ14 handler that writes a flag byte.
    let handler_addr = 0x8000u64;
    let code_base = 0x9000u64;
    let flag_addr = 0x0500u16;
    let flag_value = 0xA5_u8;

    src.write_physical_u8(u64::from(flag_addr), 0);

    install_real_mode_handler(&mut src, handler_addr, flag_addr, flag_value);
    install_hlt_loop(&mut src, code_base);
    write_ivt_entry(&mut src, VECTOR, 0x0000, handler_addr as u16);
    setup_real_mode_cpu(&mut src, code_base);

    // Clear IF so the interrupt controller can accumulate a pending vector without the CPU
    // acknowledging it.
    src.cpu_mut().set_rflags(0);

    // Run until the CPU executes HLT.
    assert!(matches!(src.run_slice(16), RunExit::Halted { .. }));

    // Configure the PIC and unmask cascade + IRQ14.
    {
        let interrupts = src.platform_interrupts().unwrap();
        let mut ints = interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        for irq in 0..16 {
            ints.pic_mut().set_masked(irq, true);
        }
        ints.pic_mut().set_masked(2, false);
        ints.pic_mut().set_masked(14, false);
    }

    // Enable PCI I/O decode so legacy ports are active.
    let bdf = IDE_PIIX3.bdf;
    write_cfg_u16(&mut src, bdf, 0x04, 0x0001);

    // Issue ATA IDENTIFY DEVICE (0xEC) via legacy ports.
    src.io_write(PRIMARY_PORTS.cmd_base + 6, 1, 0xA0);
    src.io_write(PRIMARY_PORTS.cmd_base + 7, 1, 0xEC);

    // Verify that IDENTIFY data is reachable via the data port (0x1F0). This should not clear the
    // IRQ latch; only STATUS reads do.
    let word0 = src.io_read(PRIMARY_PORTS.cmd_base, 2) as u16;
    assert_eq!(word0, 0x0040);

    // Drive the IDE IRQ line into the interrupt controller without acknowledging.
    src.poll_pci_intx_lines();

    // Sanity: the PIC sees a pending vector even though the CPU cannot accept it (IF=0).
    assert_eq!(
        PlatformInterruptController::get_pending(&*src.platform_interrupts().unwrap().borrow()),
        Some(VECTOR)
    );

    let snap = src.take_snapshot_full().unwrap();

    let mut restored = Machine::new(cfg).unwrap();
    restored.restore_snapshot_bytes(&snap).unwrap();

    assert!(
        restored.cpu().halted,
        "CPU should remain halted after restore"
    );
    assert_eq!(
        restored.cpu().rflags() & 0x200,
        0,
        "IF should remain cleared after restore"
    );
    assert_eq!(
        PlatformInterruptController::get_pending(
            &*restored.platform_interrupts().unwrap().borrow()
        ),
        Some(VECTOR),
        "pending PIC vector should survive snapshot restore"
    );

    // While IF=0, running slices must not acknowledge the PIC.
    assert!(matches!(restored.run_slice(16), RunExit::Halted { .. }));
    assert_eq!(
        PlatformInterruptController::get_pending(
            &*restored.platform_interrupts().unwrap().borrow()
        ),
        Some(VECTOR),
        "PIC vector should remain pending while IF=0"
    );

    // Re-enable interrupts and run until the handler writes the flag byte.
    restored.cpu_mut().set_rflags(0x202);
    for _ in 0..50 {
        let _ = restored.run_slice(256);
        if restored.read_physical_u8(u64::from(flag_addr)) == flag_value {
            return;
        }
    }

    panic!(
        "pending IRQ14 was not delivered after setting IF post-restore (flag=0x{:02x})",
        restored.read_physical_u8(u64::from(flag_addr))
    );
}
