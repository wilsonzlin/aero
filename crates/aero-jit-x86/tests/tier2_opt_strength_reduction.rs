#![cfg(not(target_arch = "wasm32"))]

mod tier1_common;

use aero_types::{FlagSet, Gpr};
use tier1_common::SimpleBus;

use aero_jit_x86::tier2::interp::{run_trace, RunExit, RuntimeEnv, T2State};
use aero_jit_x86::tier2::ir::{BinOp, Instr, Operand, TraceIr, TraceKind, ValueId};
use aero_jit_x86::tier2::opt::{optimize_trace, OptConfig};

fn v(idx: u32) -> ValueId {
    ValueId(idx)
}

#[test]
fn mul_by_pow2_is_strength_reduced_to_shift() {
    let trace = TraceIr {
        prologue: vec![],
        body: vec![
            Instr::LoadReg {
                dst: v(0),
                reg: Gpr::Rax,
            },
            Instr::BinOp {
                dst: v(1),
                op: BinOp::Mul,
                lhs: Operand::Value(v(0)),
                rhs: Operand::Const(8),
                flags: FlagSet::EMPTY,
            },
            Instr::StoreReg {
                reg: Gpr::Rbx,
                src: Operand::Value(v(1)),
            },
        ],
        kind: TraceKind::Linear,
    };

    let env = RuntimeEnv::default();
    let mut bus0 = SimpleBus::new(64);
    let mut bus1 = bus0.clone();

    let mut base_state = T2State::default();
    base_state.cpu.rflags = aero_jit_x86::abi::RFLAGS_RESERVED1;
    base_state.cpu.gpr[Gpr::Rax.as_u8() as usize] = 7;
    let mut opt_state = base_state.clone();

    let base = run_trace(&trace, &env, &mut bus0, &mut base_state, 1);

    let mut optimized = trace.clone();
    optimize_trace(&mut optimized, &OptConfig::default());

    assert!(
        optimized
            .iter_instrs()
            .any(|i| matches!(i, Instr::BinOp { op: BinOp::Shl, .. })),
        "expected mul-by-8 to be reduced to shl"
    );
    assert!(
        !optimized
            .iter_instrs()
            .any(|i| matches!(i, Instr::BinOp { op: BinOp::Mul, .. })),
        "unexpected mul remaining after strength reduction"
    );

    let opt = run_trace(&optimized, &env, &mut bus1, &mut opt_state, 1);
    assert_eq!(base.exit, RunExit::Returned);
    assert_eq!(opt.exit, RunExit::Returned);
    assert_eq!(base_state, opt_state);
    assert_eq!(bus0.mem(), bus1.mem());
}

#[test]
fn add_sub_const_is_strength_reduced_to_addr() {
    let trace = TraceIr {
        prologue: vec![],
        body: vec![
            Instr::LoadReg {
                dst: v(0),
                reg: Gpr::Rax,
            },
            Instr::BinOp {
                dst: v(1),
                op: BinOp::Add,
                lhs: Operand::Value(v(0)),
                rhs: Operand::Const(16),
                flags: FlagSet::EMPTY,
            },
            Instr::BinOp {
                dst: v(2),
                op: BinOp::Sub,
                lhs: Operand::Value(v(0)),
                rhs: Operand::Const(3),
                flags: FlagSet::EMPTY,
            },
            Instr::StoreReg {
                reg: Gpr::Rbx,
                src: Operand::Value(v(1)),
            },
            Instr::StoreReg {
                reg: Gpr::Rcx,
                src: Operand::Value(v(2)),
            },
        ],
        kind: TraceKind::Linear,
    };

    let env = RuntimeEnv::default();
    let mut bus0 = SimpleBus::new(64);
    let mut bus1 = bus0.clone();

    let mut base_state = T2State::default();
    base_state.cpu.rflags = aero_jit_x86::abi::RFLAGS_RESERVED1;
    base_state.cpu.gpr[Gpr::Rax.as_u8() as usize] = 0x1122_3344_5566_7788;
    let mut opt_state = base_state.clone();

    let base = run_trace(&trace, &env, &mut bus0, &mut base_state, 1);

    let mut optimized = trace.clone();
    optimize_trace(&mut optimized, &OptConfig::default());

    assert!(
        optimized
            .iter_instrs()
            .any(|i| matches!(i, Instr::Addr { .. })),
        "expected add/sub with const to be reduced to Addr"
    );
    assert!(
        !optimized.iter_instrs().any(|i| matches!(
            i,
            Instr::BinOp {
                op: BinOp::Add | BinOp::Sub,
                ..
            }
        )),
        "unexpected add/sub BinOp remaining after strength reduction"
    );

    let opt = run_trace(&optimized, &env, &mut bus1, &mut opt_state, 1);
    assert_eq!(base.exit, RunExit::Returned);
    assert_eq!(opt.exit, RunExit::Returned);
    assert_eq!(base_state, opt_state);
    assert_eq!(bus0.mem(), bus1.mem());
}
