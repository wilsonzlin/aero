use aero_perf::{PerfCounters, PerfWorker};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Cpu {
    regs: [i64; 2],
    pc: usize,
}

#[derive(Debug, Clone, PartialEq)]
enum Instruction {
    Nop,
    LoadImm { reg: usize, imm: i64 },
    Dec { reg: usize },
    Jnz { reg: usize, target: usize },
    /// Simulated string/`REP` instruction.
    RepNop { iterations: u64 },
}

fn run_interpreter(cpu: &mut Cpu, program: &[Instruction], perf: &mut PerfWorker) {
    while cpu.pc < program.len() {
        match program[cpu.pc].clone() {
            Instruction::Nop => cpu.pc += 1,
            Instruction::LoadImm { reg, imm } => {
                cpu.regs[reg] = imm;
                cpu.pc += 1;
            }
            Instruction::Dec { reg } => {
                cpu.regs[reg] -= 1;
                cpu.pc += 1;
            }
            Instruction::Jnz { reg, target } => {
                if cpu.regs[reg] != 0 {
                    cpu.pc = target;
                } else {
                    cpu.pc += 1;
                }
            }
            Instruction::RepNop { iterations } => {
                perf.add_rep_iterations(iterations);
                cpu.pc += 1;
            }
        }

        // Retire one guest architectural instruction per decoded instruction.
        perf.retire_instructions(1);
    }

    perf.flush();
}

#[derive(Debug, Clone)]
struct JitBlock {
    start_pc: usize,
    instrs: Vec<Instruction>,
    instruction_count: u64,
}

impl JitBlock {
    fn compile(start_pc: usize, program: &[Instruction]) -> Self {
        let mut instrs = Vec::new();
        let mut pc = start_pc;
        while pc < program.len() {
            let inst = program[pc].clone();
            instrs.push(inst.clone());
            pc += 1;
            if matches!(inst, Instruction::Jnz { .. }) {
                break;
            }
        }
        Self {
            start_pc,
            instruction_count: instrs.len() as u64,
            instrs,
        }
    }

    fn run(&self, cpu: &mut Cpu, perf: &mut PerfWorker) -> u64 {
        debug_assert_eq!(cpu.pc, self.start_pc);
        let mut rep_iterations = 0u64;
        for inst in &self.instrs {
            match inst {
                Instruction::Nop => cpu.pc += 1,
                Instruction::LoadImm { reg, imm } => {
                    cpu.regs[*reg] = *imm;
                    cpu.pc += 1;
                }
                Instruction::Dec { reg } => {
                    cpu.regs[*reg] -= 1;
                    cpu.pc += 1;
                }
                Instruction::Jnz { reg, target } => {
                    if cpu.regs[*reg] != 0 {
                        cpu.pc = *target;
                    } else {
                        cpu.pc += 1;
                    }
                }
                Instruction::RepNop { iterations } => {
                    rep_iterations += *iterations;
                    cpu.pc += 1;
                }
            }
        }

        if rep_iterations != 0 {
            perf.add_rep_iterations(rep_iterations);
        }

        self.instruction_count
    }
}

fn run_jit(cpu: &mut Cpu, program: &[Instruction], perf: &mut PerfWorker) {
    let mut cache: HashMap<usize, JitBlock> = HashMap::new();
    while cpu.pc < program.len() {
        let pc = cpu.pc;
        let executed = {
            let block = cache
                .entry(pc)
                .or_insert_with(|| JitBlock::compile(pc, program));
            block.run(cpu, perf)
        };
        // JIT attribution at block granularity.
        perf.retire_instructions(executed);
    }
    perf.flush();
}

#[test]
fn counts_straight_line_program() {
    let n = 10_000usize;
    let program = vec![Instruction::Nop; n];

    let shared = Arc::new(PerfCounters::new());
    let mut perf = PerfWorker::new(shared.clone());
    let mut cpu = Cpu::default();
    run_interpreter(&mut cpu, &program, &mut perf);

    assert_eq!(cpu.pc, n);
    assert_eq!(perf.lifetime_snapshot().instructions_executed, n as u64);
    assert_eq!(perf.lifetime_snapshot().rep_iterations, 0);
    assert_eq!(shared.instructions_executed(), n as u64);
}

#[test]
fn counts_control_flow_loop() {
    let iterations = 7i64;
    let program = vec![
        Instruction::LoadImm { reg: 0, imm: iterations }, // 0
        Instruction::Nop,                                 // 1
        Instruction::Dec { reg: 0 },                      // 2
        Instruction::Jnz { reg: 0, target: 1 },           // 3
    ];

    let shared = Arc::new(PerfCounters::new());
    let mut perf = PerfWorker::new(shared.clone());
    let mut cpu = Cpu::default();
    run_interpreter(&mut cpu, &program, &mut perf);

    let expected = 1 + (iterations as u64) * 3;
    assert_eq!(cpu.pc, program.len());
    assert_eq!(cpu.regs[0], 0);
    assert_eq!(perf.lifetime_snapshot().instructions_executed, expected);
    assert_eq!(perf.lifetime_snapshot().rep_iterations, 0);
    assert_eq!(shared.instructions_executed(), expected);
}

#[test]
fn rep_instruction_counts_as_one_architectural_instruction() {
    let program = vec![
        Instruction::Nop,
        Instruction::RepNop { iterations: 5 },
        Instruction::Nop,
    ];

    let shared = Arc::new(PerfCounters::new());
    let mut perf = PerfWorker::new(shared);
    let mut cpu = Cpu::default();
    run_interpreter(&mut cpu, &program, &mut perf);

    assert_eq!(perf.lifetime_snapshot().instructions_executed, 3);
    assert_eq!(perf.lifetime_snapshot().rep_iterations, 5);
}

#[test]
fn jit_and_interpreter_counts_match() {
    let iterations = 3i64;
    let program = vec![
        Instruction::LoadImm { reg: 0, imm: iterations }, // 0
        Instruction::Nop,                                 // 1
        Instruction::RepNop { iterations: 10 },            // 2
        Instruction::Dec { reg: 0 },                      // 3
        Instruction::Jnz { reg: 0, target: 1 },           // 4
    ];

    let mut cpu_i = Cpu::default();
    let shared_i = Arc::new(PerfCounters::new());
    let mut perf_i = PerfWorker::new(shared_i);
    run_interpreter(&mut cpu_i, &program, &mut perf_i);

    let mut cpu_j = Cpu::default();
    let shared_j = Arc::new(PerfCounters::new());
    let mut perf_j = PerfWorker::new(shared_j);
    run_jit(&mut cpu_j, &program, &mut perf_j);

    assert_eq!(cpu_i, cpu_j);
    assert_eq!(
        perf_i.lifetime_snapshot().instructions_executed,
        perf_j.lifetime_snapshot().instructions_executed
    );
    assert_eq!(
        perf_i.lifetime_snapshot().rep_iterations,
        perf_j.lifetime_snapshot().rep_iterations
    );
}

