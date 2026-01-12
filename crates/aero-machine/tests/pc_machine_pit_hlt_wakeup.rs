#![cfg(not(target_arch = "wasm32"))]

use aero_devices::hpet::HPET_MMIO_BASE;
use aero_devices::pit8254::{PIT_CH0, PIT_CMD, PIT_HZ};
use aero_machine::pc::PcMachine;
use aero_machine::RunExit;
use aero_platform::interrupts::PlatformInterruptMode;

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

fn program_ioapic_entry(
    ints: &mut aero_platform::interrupts::PlatformInterrupts,
    gsi: u32,
    low: u32,
    high: u32,
) {
    let redtbl_low = 0x10u32 + gsi * 2;
    let redtbl_high = redtbl_low + 1;
    ints.ioapic_mmio_write(0x00, redtbl_low);
    ints.ioapic_mmio_write(0x10, low);
    ints.ioapic_mmio_write(0x00, redtbl_high);
    ints.ioapic_mmio_write(0x10, high);
}

#[test]
fn pc_machine_pit_irq0_wakes_hlt_cpu() {
    let mut pc = PcMachine::new(2 * 1024 * 1024);

    // Configure a real-mode IVT handler for PIC IRQ0 (vector 0x20 once offsets are set).
    let vector = 0x20u8;
    let handler_addr = 0x1000u64;
    let code_base = 0x2000u64;
    let flag_addr = 0x0500u16;
    let flag_value = 0xA5u8;

    pc.bus.platform.memory.write_u8(u64::from(flag_addr), 0x00);
    install_real_mode_handler(&mut pc, handler_addr, flag_addr, flag_value);
    install_hlt_loop(&mut pc, code_base);
    write_ivt_entry(&mut pc, vector, 0x0000, handler_addr as u16);
    setup_real_mode_cpu(&mut pc, code_base);

    // Enter HLT first so the PIT interrupt must wake the CPU.
    assert!(matches!(pc.run_slice(16), RunExit::Halted { .. }));

    // Remap/unmask PIC IRQ0 => vector 0x20.
    {
        let mut ints = pc.bus.platform.interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        ints.pic_mut().set_masked(0, false);
    }

    // Program PIT channel 0 for periodic interrupts (~1kHz).
    let divisor = (PIT_HZ / 1000) as u16;
    {
        let pit = pc.bus.platform.pit();
        let mut pit = pit.borrow_mut();
        // ch0, lobyte/hibyte, mode2, binary
        pit.port_write(PIT_CMD, 1, 0x34);
        pit.port_write(PIT_CH0, 1, (divisor & 0xFF) as u32);
        pit.port_write(PIT_CH0, 1, (divisor >> 8) as u32);
    }

    // Run until the handler writes the flag byte.
    for _ in 0..50 {
        let _ = pc.run_slice(256);
        if pc.bus.platform.memory.read_u8(u64::from(flag_addr)) == flag_value {
            return;
        }
    }

    panic!(
        "real-mode PIT IRQ0 handler did not run (flag=0x{:02x})",
        pc.bus.platform.memory.read_u8(u64::from(flag_addr))
    );
}

#[test]
fn pc_machine_pit_irq0_wakes_hlt_cpu_in_apic_mode() {
    let mut pc = PcMachine::new(2 * 1024 * 1024);

    // Program IOAPIC GSI2 (ISA IRQ0 per ACPI ISO mapping) to deliver vector 0x40.
    let vector = 0x40u8;
    let pit_gsi = 2u32;

    let handler_addr = 0x1100u64;
    let code_base = 0x2100u64;
    let flag_addr = 0x0501u16;
    let flag_value = 0x5Au8;

    pc.bus.platform.memory.write_u8(u64::from(flag_addr), 0x00);
    install_real_mode_handler(&mut pc, handler_addr, flag_addr, flag_value);
    install_hlt_loop(&mut pc, code_base);
    write_ivt_entry(&mut pc, vector, 0x0000, handler_addr as u16);
    setup_real_mode_cpu(&mut pc, code_base);

    // Enter HLT first so the PIT interrupt must wake the CPU.
    assert!(matches!(pc.run_slice(16), RunExit::Halted { .. }));

    // Switch interrupt routing to APIC mode and program the IOAPIC redirection entry.
    {
        let mut ints = pc.bus.platform.interrupts.borrow_mut();
        ints.set_mode(PlatformInterruptMode::Apic);
        program_ioapic_entry(&mut ints, pit_gsi, u32::from(vector), 0);
    }

    // Program PIT channel 0 for periodic interrupts (~1kHz).
    let divisor = (PIT_HZ / 1000) as u16;
    {
        let pit = pc.bus.platform.pit();
        let mut pit = pit.borrow_mut();
        // ch0, lobyte/hibyte, mode2, binary
        pit.port_write(PIT_CMD, 1, 0x34);
        pit.port_write(PIT_CH0, 1, (divisor & 0xFF) as u32);
        pit.port_write(PIT_CH0, 1, (divisor >> 8) as u32);
    }

    for _ in 0..50 {
        let _ = pc.run_slice(256);
        if pc.bus.platform.memory.read_u8(u64::from(flag_addr)) == flag_value {
            return;
        }
    }

    panic!(
        "real-mode PIT IRQ0 IOAPIC handler did not run (flag=0x{:02x})",
        pc.bus.platform.memory.read_u8(u64::from(flag_addr))
    );
}

#[test]
fn pc_machine_hpet_timer0_wakes_hlt_cpu_in_apic_mode() {
    let mut pc = PcMachine::new(2 * 1024 * 1024);

    // HPET timer0 defaults to GSI2 in our platform + ACPI tables.
    let vector = 0x61u8;
    let hpet_gsi = 2u32;

    let handler_addr = 0x1200u64;
    let code_base = 0x2200u64;
    let flag_addr = 0x0502u16;
    let flag_value = 0xC3u8;

    pc.bus.platform.memory.write_u8(u64::from(flag_addr), 0x00);
    install_real_mode_handler(&mut pc, handler_addr, flag_addr, flag_value);
    install_hlt_loop(&mut pc, code_base);
    write_ivt_entry(&mut pc, vector, 0x0000, handler_addr as u16);
    setup_real_mode_cpu(&mut pc, code_base);

    // Enter HLT first so the HPET interrupt must wake the CPU.
    assert!(matches!(pc.run_slice(16), RunExit::Halted { .. }));

    // Enable A20 so HPET base (0xFED0_0000) doesn't alias to IOAPIC (0xFEC0_0000).
    pc.bus.platform.chipset.a20().set_enabled(true);

    // Switch to APIC mode and route HPET GSI2 to `vector`.
    {
        let mut ints = pc.bus.platform.interrupts.borrow_mut();
        ints.set_mode(PlatformInterruptMode::Apic);
        let low = u32::from(vector) | (1 << 15); // level-triggered, active-high
        program_ioapic_entry(&mut ints, hpet_gsi, low, 0);
    }

    // Program HPET timer0 via guest-visible MMIO.
    // Configure Timer0: route=2, level-triggered, interrupt enabled.
    let timer0_cfg = (2u64 << 9) | (1 << 1) | (1 << 2);
    pc.bus
        .platform
        .memory
        .write_physical(HPET_MMIO_BASE + 0x100, &timer0_cfg.to_le_bytes());
    // Comparator: 10_000 ticks at 10MHz is 1ms.
    pc.bus
        .platform
        .memory
        .write_physical(HPET_MMIO_BASE + 0x108, &10_000u64.to_le_bytes());
    // Enable HPET (general config).
    pc.bus
        .platform
        .memory
        .write_physical(HPET_MMIO_BASE + 0x010, &1u64.to_le_bytes());

    for _ in 0..50 {
        let _ = pc.run_slice(256);
        if pc.bus.platform.memory.read_u8(u64::from(flag_addr)) == flag_value {
            return;
        }
    }

    panic!(
        "real-mode HPET timer0 interrupt did not wake HLT CPU (flag=0x{:02x})",
        pc.bus.platform.memory.read_u8(u64::from(flag_addr))
    );
}
