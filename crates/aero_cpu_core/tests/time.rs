use aero_cpu_core::msr;
use aero_cpu_core::system::Cpu;
use aero_cpu_core::time_insn::BasicBlockBuilder;

fn tsc_from_edx_eax(cpu: &Cpu) -> u64 {
    ((cpu.rdx as u32 as u64) << 32) | (cpu.rax as u32 as u64)
}

#[test]
fn rdtsc_is_monotonic_across_retired_instructions() {
    let mut cpu = Cpu::default();
    cpu.time.set_tsc(1234);

    let inst = cpu.exec_time_insn(&[0x0F, 0x31]).unwrap(); // RDTSC
    let tsc1 = tsc_from_edx_eax(&cpu);
    cpu.retire_cycles(inst.cycles);

    let inst = cpu.exec_time_insn(&[0x90]).unwrap(); // NOP
    cpu.retire_cycles(inst.cycles);

    cpu.exec_time_insn(&[0x0F, 0x31]).unwrap(); // RDTSC
    let tsc2 = tsc_from_edx_eax(&cpu);

    assert!(tsc2 > tsc1, "expected monotonic TSC: {tsc2} <= {tsc1}");
}

#[test]
fn rdtscp_reads_tsc_aux_into_ecx() {
    let mut cpu = Cpu::default();
    cpu.cs = 0x8; // CPL0 for WRMSR
    cpu.rcx = msr::IA32_TSC_AUX as u64;
    cpu.rax = 0xAABB_CCDD;
    cpu.rdx = 0;
    let inst = cpu.exec_time_insn(&[0x0F, 0x30]).unwrap(); // WRMSR
    cpu.retire_cycles(inst.cycles);

    // User-mode read (RDTSCP is unprivileged).
    cpu.cs = 0x23;
    let inst = cpu.exec_time_insn(&[0x0F, 0x01, 0xF9]).unwrap(); // RDTSCP
    cpu.retire_cycles(inst.cycles);

    assert_eq!(cpu.rcx as u32, 0xAABB_CCDD);
}

#[test]
fn fences_are_noops_for_register_state_and_terminate_blocks() {
    let mut cpu = Cpu::default();
    cpu.rax = 0x1111_2222_3333_4444;
    cpu.rbx = 0x5555_6666_7777_8888;
    cpu.rcx = 0x9999_AAAA_BBBB_CCCC;
    cpu.rdx = 0xDDDD_EEEE_FFFF_0000;
    let before = (cpu.rax, cpu.rbx, cpu.rcx, cpu.rdx);

    let inst = cpu.exec_time_insn(&[0x0F, 0xAE, 0xE8]).unwrap(); // LFENCE
    cpu.retire_cycles(inst.cycles);
    assert!(inst.serializing);
    assert!(inst.terminates_block);
    assert_eq!((cpu.rax, cpu.rbx, cpu.rcx, cpu.rdx), before);

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
    let mut cpu = Cpu::default();
    cpu.cs = 0x8; // CPL0

    cpu.rcx = msr::IA32_TSC as u64;
    cpu.rax = 0x9ABC_DEF0;
    cpu.rdx = 0x1234_5678;
    let inst = cpu.exec_time_insn(&[0x0F, 0x30]).unwrap(); // WRMSR
    cpu.retire_cycles(inst.cycles);

    cpu.exec_time_insn(&[0x0F, 0x31]).unwrap(); // RDTSC
    let tsc = tsc_from_edx_eax(&cpu);

    assert!(
        tsc >= 0x1234_5678_9ABC_DEF0,
        "expected RDTSC to reflect IA32_TSC write: {tsc:#x}"
    );
}
