use std::time::Instant;

use aero_jit::Tier1Bus;
use aero_types::{FlagSet, Gpr};

use aero_jit::tier2::exec::{run_trace, run_trace_with_cached_regs, RuntimeEnv, T2State};
use aero_jit::tier2::ir::{BinOp, Instr, Operand, TraceIr, TraceKind, ValueId};
use aero_jit::tier2::opt::{optimize_trace, OptConfig};

#[derive(Clone, Debug)]
struct SimpleBus {
    mem: Vec<u8>,
}

impl SimpleBus {
    fn new(size: usize) -> Self {
        Self { mem: vec![0; size] }
    }
}

impl Tier1Bus for SimpleBus {
    fn read_u8(&self, addr: u64) -> u8 {
        self.mem[addr as usize]
    }

    fn write_u8(&mut self, addr: u64, value: u8) {
        self.mem[addr as usize] = value;
    }
}

fn v(i: u32) -> ValueId {
    ValueId(i)
}

fn main() {
    // A tiny loop: rax += 1 while rax < 10_000.
    //
    // This example isn't a stable benchmark; it is a quick manual sanity check
    // that the Tier-2 pipeline reduces instruction count and `CpuState` traffic.
    let mut trace = TraceIr {
        prologue: vec![],
        body: vec![
            Instr::LoadReg {
                dst: v(0),
                reg: Gpr::Rax,
            },
            Instr::Const {
                dst: v(1),
                value: 1,
            },
            Instr::BinOp {
                dst: v(2),
                op: BinOp::Add,
                lhs: Operand::Value(v(0)),
                rhs: Operand::Value(v(1)),
                flags: FlagSet::ALU,
            },
            Instr::StoreReg {
                reg: Gpr::Rax,
                src: Operand::Value(v(2)),
            },
            Instr::Const {
                dst: v(3),
                value: 10_000,
            },
            Instr::BinOp {
                dst: v(4),
                op: BinOp::LtU,
                lhs: Operand::Value(v(2)),
                rhs: Operand::Value(v(3)),
                flags: FlagSet::EMPTY,
            },
            Instr::Guard {
                cond: Operand::Value(v(4)),
                expected: true,
                exit_rip: 0,
            },
        ],
        kind: TraceKind::Loop,
    };

    let env = RuntimeEnv::default();
    let mut bus0 = SimpleBus::new(65536);
    let mut bus1 = bus0.clone();
    let mut base = T2State::default();
    let mut opt_state = T2State::default();

    let start = Instant::now();
    let base_res = run_trace(&trace, &env, &mut bus0, &mut base, 100_000);
    let base_time = start.elapsed();

    let opt = optimize_trace(&mut trace, &OptConfig::default());
    let start = Instant::now();
    let opt_res = run_trace_with_cached_regs(
        &trace,
        &env,
        &mut bus1,
        &mut opt_state,
        100_000,
        &opt.regalloc.cached,
    );
    let opt_time = start.elapsed();

    eprintln!(
        "baseline:  exit={:?} loads={} stores={} time={:?}",
        base_res.exit, base_res.stats.reg_loads, base_res.stats.reg_stores, base_time
    );
    eprintln!(
        "optimized: exit={:?} loads={} stores={} time={:?}",
        opt_res.exit, opt_res.stats.reg_loads, opt_res.stats.reg_stores, opt_time
    );
    eprintln!(
        "final rax baseline={} optimized={}",
        base.cpu.gpr[Gpr::Rax.as_u8() as usize],
        opt_state.cpu.gpr[Gpr::Rax.as_u8() as usize],
    );
}
