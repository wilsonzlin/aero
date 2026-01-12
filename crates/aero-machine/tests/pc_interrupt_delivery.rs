#![cfg(not(target_arch = "wasm32"))]

use aero_machine::pc::PcMachine;
use aero_machine::RunExit;
use aero_platform::interrupts::{InterruptInput, PlatformInterruptMode};

fn write_u16_le(pc: &mut PcMachine, paddr: u64, value: u16) {
    pc.bus
        .platform
        .memory
        .write_physical(paddr, &value.to_le_bytes());
}

fn write_ivt_entry(pc: &mut PcMachine, vector: u8, segment: u16, offset: u16) {
    let base = u64::from(vector) * 4;
    write_u16_le(pc, base, offset);
    write_u16_le(pc, base + 2, segment);
}

fn install_real_mode_handler(pc: &mut PcMachine, handler_addr: u64, flag_addr: u16, value: u8) {
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
    pc.bus.platform.memory.write_physical(handler_addr, &code);
}

fn install_hlt_loop(pc: &mut PcMachine, code_base: u64) {
    // hlt; jmp short $-3 (back to hlt)
    let code = [0xF4u8, 0xEB, 0xFD];
    pc.bus.platform.memory.write_physical(code_base, &code);
}

fn setup_real_mode_cpu(pc: &mut PcMachine, entry_ip: u64) {
    pc.cpu = aero_cpu_core::CpuCore::new(aero_cpu_core::state::CpuMode::Real);

    // Real-mode segments: base = selector<<4, limit = 0xFFFF.
    for seg in [
        &mut pc.cpu.state.segments.cs,
        &mut pc.cpu.state.segments.ds,
        &mut pc.cpu.state.segments.es,
        &mut pc.cpu.state.segments.ss,
        &mut pc.cpu.state.segments.fs,
        &mut pc.cpu.state.segments.gs,
    ] {
        seg.selector = 0;
        seg.base = 0;
        seg.limit = 0xFFFF;
        seg.access = 0;
    }

    pc.cpu.state.set_stack_ptr(0x8000);
    pc.cpu.state.set_rip(entry_ip);
    pc.cpu.state.set_rflags(0x202); // IF=1
    pc.cpu.state.halted = false;
}

fn program_ioapic_entry(ints: &mut aero_platform::interrupts::PlatformInterrupts, gsi: u32, low: u32, high: u32) {
    let redtbl_low = 0x10u32 + gsi * 2;
    let redtbl_high = redtbl_low + 1;
    ints.ioapic_mmio_write(0x00, redtbl_low);
    ints.ioapic_mmio_write(0x10, low);
    ints.ioapic_mmio_write(0x00, redtbl_high);
    ints.ioapic_mmio_write(0x10, high);
}

#[test]
fn pc_machine_delivers_pic_interrupt_to_real_mode_ivt_handler() {
    let mut pc = PcMachine::new(2 * 1024 * 1024);

    let vector = 0x21u8; // IRQ1 with PIC base 0x20
    let handler_addr = 0x1000u64;
    let code_base = 0x2000u64;
    let flag_addr = 0x0500u16;
    let flag_value = 0x5Au8;

    install_real_mode_handler(&mut pc, handler_addr, flag_addr, flag_value);
    install_hlt_loop(&mut pc, code_base);
    write_ivt_entry(&mut pc, vector, 0x0000, handler_addr as u16);

    setup_real_mode_cpu(&mut pc, code_base);

    // Run until the CPU executes HLT.
    assert!(matches!(pc.run_slice(16), RunExit::Halted { .. }));

    // Configure the PIC and raise IRQ1.
    {
        let mut ints = pc.bus.platform.interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        ints.pic_mut().set_masked(1, false);
        ints.raise_irq(InterruptInput::IsaIrq(1));
    }

    // Run until the handler writes the flag byte.
    for _ in 0..10 {
        let _ = pc.run_slice(128);
        if pc.bus.platform.memory.read_u8(u64::from(flag_addr)) == flag_value {
            return;
        }
    }

    panic!(
        "real-mode PIC interrupt handler did not run (flag=0x{:02x})",
        pc.bus.platform.memory.read_u8(u64::from(flag_addr))
    );
}

#[test]
fn pc_machine_delivers_ioapic_interrupt_to_real_mode_ivt_handler() {
    let mut pc = PcMachine::new(2 * 1024 * 1024);

    let vector = 0x60u8;
    let gsi = 10u32;
    let handler_addr = 0x1100u64;
    let code_base = 0x2100u64;
    let flag_addr = 0x0501u16;
    let flag_value = 0xA5u8;

    install_real_mode_handler(&mut pc, handler_addr, flag_addr, flag_value);
    install_hlt_loop(&mut pc, code_base);
    write_ivt_entry(&mut pc, vector, 0x0000, handler_addr as u16);

    setup_real_mode_cpu(&mut pc, code_base);

    // Halt the CPU first so the interrupt must wake it.
    assert!(matches!(pc.run_slice(16), RunExit::Halted { .. }));

    // Switch to APIC mode and route GSI10 to `vector` (level-triggered, active-low).
    {
        let mut ints = pc.bus.platform.interrupts.borrow_mut();
        ints.set_mode(PlatformInterruptMode::Apic);
        let low = u32::from(vector) | (1 << 13) | (1 << 15); // polarity_low + level-triggered
        program_ioapic_entry(&mut *ints, gsi, low, 0);
        ints.raise_irq(InterruptInput::Gsi(gsi));
    }

    for _ in 0..10 {
        let _ = pc.run_slice(256);
        if pc.bus.platform.memory.read_u8(u64::from(flag_addr)) == flag_value {
            return;
        }
    }

    panic!(
        "real-mode IOAPIC interrupt handler did not run (flag=0x{:02x})",
        pc.bus.platform.memory.read_u8(u64::from(flag_addr))
    );
}
