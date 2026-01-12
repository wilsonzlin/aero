#![cfg(not(target_arch = "wasm32"))]

use aero_machine::{Machine, MachineConfig, RunExit};
use aero_platform::interrupts::{
    InterruptController as PlatformInterruptController, InterruptInput, PlatformInterruptMode,
    IMCR_DATA_PORT, IMCR_INDEX, IMCR_SELECT_PORT,
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
    cpu.set_rflags(0x202); // IF=1
    cpu.halted = false;
}

fn program_ioapic_entry(
    ints: &mut aero_platform::interrupts::PlatformInterrupts,
    gsi: u32,
    vector: u8,
) {
    let redtbl_low = 0x10u32 + gsi * 2;
    let redtbl_high = redtbl_low + 1;
    ints.ioapic_mmio_write(0x00, redtbl_low);
    // Match typical PCI INTx wiring: active-low, edge-triggered.
    //
    // Note: for GSI10-13, `aero_interrupts::apic::IoApic` assumes active-low board wiring; guests
    // should therefore set the redirection table polarity bit (bit13) so the IOAPIC interprets a
    // low electrical level as an asserted interrupt.
    ints.ioapic_mmio_write(0x10, u32::from(vector) | (1 << 13));
    ints.ioapic_mmio_write(0x00, redtbl_high);
    ints.ioapic_mmio_write(0x10, 0);
}

#[test]
fn machine_snapshot_roundtrip_preserves_imcr_apic_mode_and_pending_ioapic_interrupt() {
    const RAM_SIZE: u64 = 2 * 1024 * 1024;
    const GSI: u32 = 10;
    const APIC_VECTOR: u8 = 0x60;
    const PIC_VECTOR: u8 = 0x21; // IRQ1 with PIC base 0x20.

    let cfg = MachineConfig {
        ram_size_bytes: RAM_SIZE,
        enable_pc_platform: true,
        // Keep this test focused on the interrupt controller complex + snapshot restore.
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

    // Install two handlers:
    // - APIC_VECTOR (delivered via IOAPIC after IMCR switch)
    // - PIC_VECTOR (delivered via legacy PIC after switching back)
    let handler_apic = 0x8000u64;
    let handler_pic = 0x8100u64;
    let code_base = 0x9000u64;
    let flag_apic = 0x0500u16;
    let flag_pic = 0x0501u16;

    src.write_physical_u8(u64::from(flag_apic), 0);
    src.write_physical_u8(u64::from(flag_pic), 0);

    install_real_mode_handler(&mut src, handler_apic, flag_apic, 0xA5);
    install_real_mode_handler(&mut src, handler_pic, flag_pic, 0x5A);
    install_hlt_loop(&mut src, code_base);
    write_ivt_entry(&mut src, APIC_VECTOR, 0x0000, handler_apic as u16);
    write_ivt_entry(&mut src, PIC_VECTOR, 0x0000, handler_pic as u16);
    setup_real_mode_cpu(&mut src, code_base);

    // Halt the CPU first so the interrupt must wake it.
    assert!(matches!(src.run_slice(16), RunExit::Halted { .. }));

    // Program IOAPIC redirection entry while still in PIC mode (typical OS sequencing).
    {
        let interrupts = src.platform_interrupts().unwrap();
        let mut ints = interrupts.borrow_mut();
        program_ioapic_entry(&mut ints, GSI, APIC_VECTOR);
    }

    // Guest-style IMCR programming:
    //   out 0x22, 0x70; out 0x23, 0x01  => route ISA IRQs through IOAPIC/LAPIC
    src.io_write(IMCR_SELECT_PORT, 1, u32::from(IMCR_INDEX));
    src.io_write(IMCR_DATA_PORT, 1, 0x01);

    assert_eq!(
        src.platform_interrupts().unwrap().borrow().mode(),
        PlatformInterruptMode::Apic
    );

    // Assert a GSI and snapshot *before* the CPU has a chance to acknowledge it.
    src.platform_interrupts()
        .unwrap()
        .borrow_mut()
        .raise_irq(InterruptInput::Gsi(GSI));

    let snap = src.take_snapshot_full().unwrap();

    let mut restored = Machine::new(cfg).unwrap();
    restored.restore_snapshot_bytes(&snap).unwrap();

    assert!(
        restored.cpu().halted,
        "CPU should remain halted after snapshot restore"
    );
    assert_eq!(
        restored.platform_interrupts().unwrap().borrow().mode(),
        PlatformInterruptMode::Apic,
        "IMCR-selected APIC mode should survive snapshot restore"
    );

    // IMCR port state should roundtrip too.
    restored.io_write(IMCR_SELECT_PORT, 1, u32::from(IMCR_INDEX));
    assert_eq!(
        restored.io_read(IMCR_DATA_PORT, 1) as u8 & 1,
        1,
        "IMCR data port bit0 should remain set after restore"
    );

    // Sanity: the interrupt controller should still report the pending vector.
    assert_eq!(
        PlatformInterruptController::get_pending(
            &*restored.platform_interrupts().unwrap().borrow()
        ),
        Some(APIC_VECTOR)
    );

    // Run until the APIC handler writes its flag byte.
    for _ in 0..50 {
        let _ = restored.run_slice(256);
        if restored.read_physical_u8(u64::from(flag_apic)) == 0xA5 {
            break;
        }
    }
    assert_eq!(
        restored.read_physical_u8(u64::from(flag_apic)),
        0xA5,
        "expected pending IOAPIC interrupt to be delivered after restore"
    );

    // Lower the line (clean up) so switching back to PIC mode does not inherit the asserted GSI.
    restored
        .platform_interrupts()
        .unwrap()
        .borrow_mut()
        .lower_irq(InterruptInput::Gsi(GSI));

    // Switch back to legacy PIC mode via IMCR and prove PIC interrupts work after restore.
    restored.io_write(IMCR_SELECT_PORT, 1, u32::from(IMCR_INDEX));
    restored.io_write(IMCR_DATA_PORT, 1, 0x00);
    assert_eq!(
        restored.platform_interrupts().unwrap().borrow().mode(),
        PlatformInterruptMode::LegacyPic
    );

    // Configure the PIC to deliver IRQ1 on vector 0x21.
    {
        let interrupts = restored.platform_interrupts().unwrap();
        let mut ints = interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        for irq in 0..16 {
            ints.pic_mut().set_masked(irq, true);
        }
        ints.pic_mut().set_masked(1, false);
    }

    restored
        .platform_interrupts()
        .unwrap()
        .borrow_mut()
        .raise_irq(InterruptInput::IsaIrq(1));

    for _ in 0..50 {
        let _ = restored.run_slice(256);
        if restored.read_physical_u8(u64::from(flag_pic)) == 0x5A {
            return;
        }
    }

    panic!(
        "PIC interrupt was not delivered after switching back from IMCR APIC mode (flag=0x{:02x})",
        restored.read_physical_u8(u64::from(flag_pic))
    );
}
