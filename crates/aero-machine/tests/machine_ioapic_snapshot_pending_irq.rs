#![cfg(not(target_arch = "wasm32"))]

use aero_machine::{Machine, MachineConfig, RunExit};
use aero_platform::interrupts::{
    InterruptController as PlatformInterruptController, InterruptInput, PlatformInterruptMode,
    PlatformInterrupts,
};
use pretty_assertions::assert_eq;

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
    cpu.set_rflags(0x202); // IF=1 (caller may override)
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
fn machine_snapshot_roundtrip_preserves_pending_ioapic_vector_until_if_is_set() {
    const RAM_SIZE: u64 = 2 * 1024 * 1024;
    const GSI: u32 = 14;
    const VECTOR: u8 = 0x60;

    let cfg = MachineConfig {
        ram_size_bytes: RAM_SIZE,
        enable_pc_platform: true,
        // Keep this test focused on IOAPIC/LAPIC + snapshot/restore.
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

    // Route the IOAPIC vector into a real-mode handler that writes a flag byte.
    let handler_addr = 0x8000u64;
    let code_base = 0x9000u64;
    let flag_addr = 0x0500u16;
    let flag_value = 0xA5_u8;

    src.write_physical_u8(u64::from(flag_addr), 0);

    install_real_mode_handler(&mut src, handler_addr, flag_addr, flag_value);
    install_hlt_loop(&mut src, code_base);
    write_ivt_entry(&mut src, VECTOR, 0x0000, handler_addr as u16);
    setup_real_mode_cpu(&mut src, code_base);

    // Clear IF so the machine must not acknowledge/enqueue the pending external interrupt.
    src.cpu_mut().set_rflags(0);

    // Halt the CPU first so any interrupt must wake it.
    assert!(matches!(src.run_slice(16), RunExit::Halted { .. }));

    // Switch to APIC mode and program the IOAPIC redirection entry.
    {
        let interrupts = src.platform_interrupts().unwrap();
        let mut ints = interrupts.borrow_mut();
        ints.set_mode(PlatformInterruptMode::Apic);
        program_ioapic_redirection_entry(&mut ints, GSI, u32::from(VECTOR), 0);
    }

    // Assert the GSI. This delivers into LAPIC IRR immediately (via IOAPIC), but must not be
    // acknowledged by the CPU while IF=0.
    src.platform_interrupts()
        .unwrap()
        .borrow_mut()
        .raise_irq(InterruptInput::Gsi(GSI));

    assert_eq!(
        PlatformInterruptController::get_pending(&*src.platform_interrupts().unwrap().borrow()),
        Some(VECTOR),
        "sanity: expected LAPIC pending vector after asserting GSI"
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
        restored.platform_interrupts().unwrap().borrow().mode(),
        PlatformInterruptMode::Apic,
        "platform interrupt mode should survive snapshot restore"
    );
    assert_eq!(
        PlatformInterruptController::get_pending(
            &*restored.platform_interrupts().unwrap().borrow()
        ),
        Some(VECTOR),
        "pending LAPIC vector should survive snapshot restore"
    );

    // While IF=0, running slices must not acknowledge the LAPIC vector.
    assert!(matches!(restored.run_slice(16), RunExit::Halted { .. }));
    assert_eq!(
        PlatformInterruptController::get_pending(
            &*restored.platform_interrupts().unwrap().borrow()
        ),
        Some(VECTOR),
        "pending LAPIC vector should remain while IF=0"
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
        "pending IOAPIC interrupt was not delivered after setting IF post-restore (flag=0x{:02x})",
        restored.read_physical_u8(u64::from(flag_addr))
    );
}
