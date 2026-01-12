#![cfg(not(target_arch = "wasm32"))]

use aero_devices::pit8254::{PIT_CH0, PIT_CMD, PIT_HZ};
use aero_machine::pc::PcMachine;
use aero_machine::RunExit;

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

#[test]
fn pc_machine_pit_irq0_wakes_hlt_cpu() {
    let mut pc = PcMachine::new(2 * 1024 * 1024);

    // Configure a real-mode IVT handler for PIC IRQ0 (vector 0x20 once offsets are set).
    let vector = 0x20u8;
    let handler_addr = 0x1000u64;
    let code_base = 0x2000u64;
    let flag_addr = 0x0500u16;
    let flag_value = 0xA5u8;

    pc.bus
        .platform
        .memory
        .write_u8(u64::from(flag_addr), 0x00);
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
