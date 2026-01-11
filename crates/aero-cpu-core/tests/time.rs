use aero_cpu_core::assist::AssistContext;
use aero_cpu_core::interp::tier0::exec::{run_batch_with_assists, BatchExit};
use aero_cpu_core::mem::FlatTestBus;
use aero_cpu_core::msr;
use aero_cpu_core::state::{CpuMode, CpuState, CR0_PE};
use aero_cpu_core::time_insn::{decode_instruction, BasicBlockBuilder};
use aero_x86::Register;

const BUS_SIZE: usize = 0x2000;
const CODE_BASE: u64 = 0x1000;

fn tsc_from_edx_eax(state: &CpuState) -> u64 {
    let lo = state.read_reg(Register::EAX) as u32 as u64;
    let hi = state.read_reg(Register::EDX) as u32 as u64;
    (hi << 32) | lo
}

fn exec_one(ctx: &mut AssistContext, state: &mut CpuState, bus: &mut FlatTestBus) {
    let res = run_batch_with_assists(ctx, state, bus, 1);
    assert_eq!(res.executed, 1);
    assert_eq!(res.exit, BatchExit::Completed);
}

#[test]
fn rdtsc_is_monotonic_across_retired_instructions() {
    let code = [0x0F, 0x31, 0x90, 0x0F, 0x31]; // rdtsc; nop; rdtsc
    let mut bus = FlatTestBus::new(BUS_SIZE);
    bus.load(CODE_BASE, &code);

    let mut state = CpuState::new(CpuMode::Bit32);
    state.set_rip(CODE_BASE);
    state.msr.tsc = 1234;

    let mut ctx = AssistContext::default();
    ctx.tsc_step = 1;

    exec_one(&mut ctx, &mut state, &mut bus);
    let tsc1 = tsc_from_edx_eax(&state);

    exec_one(&mut ctx, &mut state, &mut bus); // NOP
    exec_one(&mut ctx, &mut state, &mut bus);
    let tsc2 = tsc_from_edx_eax(&state);

    assert!(tsc2 > tsc1, "expected monotonic TSC: {tsc2} <= {tsc1}");
}

#[test]
fn rdtscp_reads_tsc_aux_into_ecx() {
    let mut bus = FlatTestBus::new(BUS_SIZE);
    let mut ctx = AssistContext::default();

    let mut state = CpuState::new(CpuMode::Bit32);
    state.control.cr0 |= CR0_PE;
    state.segments.cs.selector = 0x08; // CPL0 for WRMSR.

    state.write_reg(Register::ECX, msr::IA32_TSC_AUX as u64);
    state.write_reg(Register::EAX, 0xAABB_CCDD);
    state.write_reg(Register::EDX, 0);
    state.set_rip(CODE_BASE);
    bus.load(CODE_BASE, &[0x0F, 0x30]); // WRMSR
    exec_one(&mut ctx, &mut state, &mut bus);

    // User-mode read (RDTSCP is unprivileged).
    state.segments.cs.selector = 0x23;
    state.set_rip(CODE_BASE);
    bus.load(CODE_BASE, &[0x0F, 0x01, 0xF9]); // RDTSCP
    exec_one(&mut ctx, &mut state, &mut bus);

    assert_eq!(state.read_reg(Register::ECX) as u32, 0xAABB_CCDD);
}

#[test]
fn fences_are_noops_for_register_state_and_terminate_blocks() {
    let inst = decode_instruction(&[0x0F, 0xAE, 0xE8]).unwrap(); // LFENCE
    assert!(inst.serializing);
    assert!(inst.terminates_block);

    let mut bus = FlatTestBus::new(BUS_SIZE);
    let mut ctx = AssistContext::default();
    let mut state = CpuState::new(CpuMode::Bit32);

    state.write_reg(Register::RAX, 0x1111_2222_3333_4444);
    state.write_reg(Register::RBX, 0x5555_6666_7777_8888);
    state.write_reg(Register::RCX, 0x9999_AAAA_BBBB_CCCC);
    state.write_reg(Register::RDX, 0xDDDD_EEEE_FFFF_0000);
    let before = (
        state.read_reg(Register::RAX),
        state.read_reg(Register::RBX),
        state.read_reg(Register::RCX),
        state.read_reg(Register::RDX),
    );

    state.set_rip(CODE_BASE);
    bus.load(CODE_BASE, &[0x0F, 0xAE, 0xE8]); // LFENCE
    exec_one(&mut ctx, &mut state, &mut bus);

    assert_eq!(
        (
            state.read_reg(Register::RAX),
            state.read_reg(Register::RBX),
            state.read_reg(Register::RCX),
            state.read_reg(Register::RDX),
        ),
        before
    );

    let code = [
        0x90, // NOP
        0x0F, 0xAE, 0xE8, // LFENCE
        0x90, // NOP
    ];

    let block1 = BasicBlockBuilder::decode_block(&code, 0, 16).unwrap();
    assert_eq!(block1.instructions.len(), 2);
    assert_eq!(block1.len, 4);
    assert_eq!(block1.end(), 4);

    let block2 = BasicBlockBuilder::decode_block(&code, block1.end(), 16).unwrap();
    assert_eq!(block2.instructions.len(), 1);
    assert_eq!(block2.len, 1);
}

#[test]
fn wrmsr_ia32_tsc_updates_subsequent_rdtsc() {
    let mut bus = FlatTestBus::new(BUS_SIZE);
    let mut ctx = AssistContext::default();

    let mut state = CpuState::new(CpuMode::Bit32);
    state.control.cr0 |= CR0_PE;
    state.segments.cs.selector = 0x08; // CPL0

    state.write_reg(Register::ECX, msr::IA32_TSC as u64);
    state.write_reg(Register::EAX, 0x9ABC_DEF0);
    state.write_reg(Register::EDX, 0x1234_5678);
    state.set_rip(CODE_BASE);
    bus.load(CODE_BASE, &[0x0F, 0x30]); // WRMSR
    exec_one(&mut ctx, &mut state, &mut bus);

    state.set_rip(CODE_BASE);
    bus.load(CODE_BASE, &[0x0F, 0x31]); // RDTSC
    exec_one(&mut ctx, &mut state, &mut bus);
    let tsc = tsc_from_edx_eax(&state);

    assert!(
        tsc >= 0x1234_5678_9ABC_DEF0,
        "expected RDTSC to reflect IA32_TSC write: {tsc:#x}"
    );
}
