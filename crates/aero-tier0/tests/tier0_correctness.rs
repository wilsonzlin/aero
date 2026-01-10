use aero_tier0::{
    CpuBus, CpuState, ExitReason, LegacyInterpreter, MemoryBus, Reg, Tier0Interpreter,
};

fn run_both(program: &[u8], cpu: CpuState) -> (CpuState, CpuState) {
    let mut bus = MemoryBus::new(0x20000);
    bus.load(0, program).unwrap();

    let mut legacy = LegacyInterpreter::new(cpu.clone());
    let legacy_exit = legacy
        .run(&mut bus.clone(), 1_000_000)
        .unwrap_or(ExitReason::InstructionLimit);
    assert_ne!(legacy_exit, ExitReason::InstructionLimit);

    let mut tier0 = Tier0Interpreter::new(cpu);
    let tier0_exit = tier0
        .run(&mut bus, 1_000_000)
        .unwrap_or(ExitReason::InstructionLimit);
    assert_ne!(tier0_exit, ExitReason::InstructionLimit);

    (legacy.cpu, tier0.cpu)
}

#[test]
fn dec_jnz_loop_matches() {
    // dec rcx; jnz -5; hlt
    let prog = [0x48, 0xFF, 0xC9, 0x75, 0xFB, 0xF4];
    let mut cpu = CpuState::default();
    cpu.set_reg(Reg::Rcx, 100);
    let (legacy_cpu, tier0_cpu) = run_both(&prog, cpu);
    assert_eq!(legacy_cpu, tier0_cpu);
    assert_eq!(legacy_cpu.reg(Reg::Rcx), 0);
}

#[test]
fn rep_movsb_matches() {
    // rep movsb; hlt
    let prog = [0xF3, 0xA4, 0xF4];
    let src_addr: u64 = 0x1000;
    let dst_addr: u64 = 0x2000;
    let len: u64 = 1024;

    let mut cpu = CpuState::default();
    cpu.set_reg(Reg::Rsi, src_addr);
    cpu.set_reg(Reg::Rdi, dst_addr);
    cpu.set_reg(Reg::Rcx, len);

    let mut init_bus = MemoryBus::new(0x8000);
    init_bus.load(0, &prog).unwrap();
    for i in 0..len {
        init_bus
            .write_u8(src_addr + i, (i as u8).wrapping_add(1))
            .unwrap();
    }

    let mut legacy = LegacyInterpreter::new(cpu.clone());
    let mut legacy_bus = init_bus.clone();
    let legacy_exit = legacy.run(&mut legacy_bus, 10_000_000).unwrap();
    assert_ne!(legacy_exit, ExitReason::InstructionLimit);

    let mut tier0 = Tier0Interpreter::new(cpu);
    let mut tier0_bus = init_bus;
    let tier0_exit = tier0.run(&mut tier0_bus, 10_000_000).unwrap();
    assert_ne!(tier0_exit, ExitReason::InstructionLimit);

    assert_eq!(legacy.cpu, tier0.cpu);
    assert_eq!(legacy.cpu.reg(Reg::Rcx), 0);

    let dst = dst_addr as usize;
    let dst_end = dst + len as usize;
    assert_eq!(
        &legacy_bus.as_slice()[dst..dst_end],
        &tier0_bus.as_slice()[dst..dst_end]
    );
}

#[test]
fn code_write_invalidates_decoded_blocks() {
    // nop; hlt
    let prog = [0x90, 0xF4];
    let mut bus = MemoryBus::new(0x10000);
    bus.load(0, &prog).unwrap();

    let mut tier0 = Tier0Interpreter::new(CpuState::default());
    let exit = tier0
        .run(&mut bus, 10)
        .unwrap_or(ExitReason::InstructionLimit);
    assert_ne!(exit, ExitReason::InstructionLimit);

    // Patch the first byte to an invalid opcode. This should increment the page
    // version and force the next run to re-decode.
    bus.write_u8(0, 0xCC).unwrap();

    tier0.cpu.rip = 0;
    let err = tier0.run(&mut bus, 10).unwrap_err();
    assert!(matches!(
        err,
        aero_tier0::interpreter::Exception::DecodeError { .. }
    ));
}

#[test]
fn sti_interrupt_shadow_delays_interrupt() {
    // sti; nop; hlt
    let prog = [0xFB, 0x90, 0xF4];
    let mut cpu = CpuState::default();
    cpu.flags.iflag = false;
    cpu.set_pending_interrupt(0x20);

    let (legacy_cpu, tier0_cpu) = run_both(&prog, cpu);
    assert_eq!(legacy_cpu, tier0_cpu);
    // Interrupt should have been consumed.
    assert_eq!(legacy_cpu.pending_interrupt, None);
    assert_eq!(legacy_cpu.rip, 2); // After NOP (STI+NOP), before HLT.
}

#[test]
fn mov_ss_interrupt_shadow_delays_interrupt() {
    // mov ss, ax; nop; hlt
    let prog = [0x8E, 0xD0, 0x90, 0xF4];
    let mut cpu = CpuState::default();
    cpu.flags.iflag = true;
    cpu.set_reg(Reg::Rax, 0x1234);
    cpu.set_pending_interrupt(0x21);

    let (legacy_cpu, tier0_cpu) = run_both(&prog, cpu);
    assert_eq!(legacy_cpu, tier0_cpu);
    assert_eq!(legacy_cpu.ss, 0x1234);
    assert_eq!(legacy_cpu.pending_interrupt, None);
    assert_eq!(legacy_cpu.rip, 3); // After NOP.
}
