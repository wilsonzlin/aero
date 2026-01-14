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
fn mul_by_pow2_with_const_on_lhs_is_strength_reduced_to_shift() {
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
                lhs: Operand::Const(8),
                rhs: Operand::Value(v(0)),
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
fn nested_and_const_masks_are_collapsed() {
    // Lowering frequently produces nested masks like:
    //   tmp = x & 0xff
    //   dst = tmp & 0x0f
    // which can be collapsed to a single `x & 0x0f`.
    let trace = TraceIr {
        prologue: vec![],
        body: vec![
            Instr::LoadReg {
                dst: v(0),
                reg: Gpr::Rax,
            },
            Instr::BinOp {
                dst: v(1),
                op: BinOp::And,
                lhs: Operand::Value(v(0)),
                rhs: Operand::Const(0xff),
                flags: FlagSet::EMPTY,
            },
            Instr::BinOp {
                dst: v(2),
                op: BinOp::And,
                lhs: Operand::Value(v(1)),
                rhs: Operand::Const(0x0f),
                flags: FlagSet::EMPTY,
            },
            Instr::StoreReg {
                reg: Gpr::Rbx,
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
    base_state.cpu.gpr[Gpr::Rax.as_u8() as usize] = 0x1234_5678_9abc_def0;
    let mut opt_state = base_state.clone();

    let base = run_trace(&trace, &env, &mut bus0, &mut base_state, 1);

    let mut optimized = trace.clone();
    optimize_trace(&mut optimized, &OptConfig::default());

    let ands: Vec<(Operand, Operand)> = optimized
        .iter_instrs()
        .filter_map(|i| match i {
            Instr::BinOp {
                op: BinOp::And,
                lhs,
                rhs,
                flags,
                ..
            } if flags.is_empty() => Some((*lhs, *rhs)),
            _ => None,
        })
        .collect();
    assert_eq!(
        ands.len(),
        1,
        "expected nested masks to collapse to one And"
    );
    assert!(
        matches!(
            ands[0],
            (Operand::Const(0x0f), _) | (_, Operand::Const(0x0f))
        ),
        "expected collapsed mask of 0x0f, got {ands:?}"
    );
    assert!(
        !ands.iter().any(|(l, r)| matches!(
            (l, r),
            (Operand::Const(0xff), _) | (_, Operand::Const(0xff))
        )),
        "unexpected remaining 0xff mask: {ands:?}"
    );

    let opt = run_trace(&optimized, &env, &mut bus1, &mut opt_state, 1);
    assert_eq!(base.exit, RunExit::Returned);
    assert_eq!(opt.exit, RunExit::Returned);
    assert_eq!(base_state, opt_state);
    assert_eq!(bus0.mem(), bus1.mem());
}

#[test]
fn mul_of_boolean_values_is_strength_reduced_to_and() {
    let trace = TraceIr {
        prologue: vec![],
        body: vec![
            Instr::LoadReg {
                dst: v(0),
                reg: Gpr::Rax,
            },
            Instr::BinOp {
                dst: v(1),
                op: BinOp::Eq,
                lhs: Operand::Value(v(0)),
                rhs: Operand::Const(0),
                flags: FlagSet::EMPTY,
            },
            Instr::LoadReg {
                dst: v(2),
                reg: Gpr::Rbx,
            },
            Instr::BinOp {
                dst: v(3),
                op: BinOp::Eq,
                lhs: Operand::Value(v(2)),
                rhs: Operand::Const(0),
                flags: FlagSet::EMPTY,
            },
            Instr::BinOp {
                dst: v(4),
                op: BinOp::Mul,
                lhs: Operand::Value(v(1)),
                rhs: Operand::Value(v(3)),
                flags: FlagSet::EMPTY,
            },
            Instr::StoreReg {
                reg: Gpr::Rcx,
                src: Operand::Value(v(4)),
            },
        ],
        kind: TraceKind::Linear,
    };

    let env = RuntimeEnv::default();
    let mut bus0 = SimpleBus::new(64);
    let mut bus1 = bus0.clone();

    let mut base_state = T2State::default();
    base_state.cpu.rflags = aero_jit_x86::abi::RFLAGS_RESERVED1;
    base_state.cpu.gpr[Gpr::Rax.as_u8() as usize] = 0;
    base_state.cpu.gpr[Gpr::Rbx.as_u8() as usize] = 5;
    let mut opt_state = base_state.clone();

    let base = run_trace(&trace, &env, &mut bus0, &mut base_state, 1);

    let mut optimized = trace.clone();
    optimize_trace(&mut optimized, &OptConfig::default());

    assert!(
        optimized
            .iter_instrs()
            .any(|i| matches!(i, Instr::BinOp { op: BinOp::And, .. })),
        "expected boolean mul to be reduced to And"
    );
    assert!(
        !optimized
            .iter_instrs()
            .any(|i| matches!(i, Instr::BinOp { op: BinOp::Mul, .. })),
        "unexpected Mul remaining after boolean strength reduction"
    );

    let opt = run_trace(&optimized, &env, &mut bus1, &mut opt_state, 1);
    assert_eq!(base.exit, RunExit::Returned);
    assert_eq!(opt.exit, RunExit::Returned);
    assert_eq!(base_state, opt_state);
    assert_eq!(bus0.mem(), bus1.mem());
}

#[test]
fn select_lowering_mul_add_is_strength_reduced() {
    // Mimic the Tier-2 lowering for `Select`:
    //   cond_is_zero = (cond == 0)
    //   cond_bool    = (cond_is_zero == 0)
    //   result       = if_true * cond_bool + if_false * cond_is_zero
    let trace = TraceIr {
        prologue: vec![],
        body: vec![
            // Values to select between.
            Instr::LoadReg {
                dst: v(0),
                reg: Gpr::Rbx, // if_true
            },
            Instr::LoadReg {
                dst: v(1),
                reg: Gpr::Rcx, // if_false
            },
            // Condition.
            Instr::LoadReg {
                dst: v(2),
                reg: Gpr::Rax,
            },
            Instr::BinOp {
                dst: v(3),
                op: BinOp::Eq,
                lhs: Operand::Value(v(2)),
                rhs: Operand::Const(0),
                flags: FlagSet::EMPTY,
            },
            Instr::BinOp {
                dst: v(4),
                op: BinOp::Eq,
                lhs: Operand::Value(v(3)),
                rhs: Operand::Const(0),
                flags: FlagSet::EMPTY,
            },
            Instr::BinOp {
                dst: v(5),
                op: BinOp::Mul,
                lhs: Operand::Value(v(0)),
                rhs: Operand::Value(v(4)),
                flags: FlagSet::EMPTY,
            },
            Instr::BinOp {
                dst: v(6),
                op: BinOp::Mul,
                lhs: Operand::Value(v(1)),
                rhs: Operand::Value(v(3)),
                flags: FlagSet::EMPTY,
            },
            Instr::BinOp {
                dst: v(7),
                op: BinOp::Add,
                lhs: Operand::Value(v(5)),
                rhs: Operand::Value(v(6)),
                flags: FlagSet::EMPTY,
            },
            Instr::StoreReg {
                reg: Gpr::Rdx,
                src: Operand::Value(v(7)),
            },
        ],
        kind: TraceKind::Linear,
    };

    let env = RuntimeEnv::default();
    let mut bus0 = SimpleBus::new(64);
    let mut bus1 = bus0.clone();

    let mut base_state = T2State::default();
    base_state.cpu.rflags = aero_jit_x86::abi::RFLAGS_RESERVED1;
    base_state.cpu.gpr[Gpr::Rbx.as_u8() as usize] = 0xAAAA;
    base_state.cpu.gpr[Gpr::Rcx.as_u8() as usize] = 0xBBBB;
    let mut opt_state = base_state.clone();

    // Test both cond==0 and cond!=0.
    for cond in [0u64, 5u64] {
        base_state.cpu.gpr[Gpr::Rax.as_u8() as usize] = cond;
        opt_state.cpu.gpr[Gpr::Rax.as_u8() as usize] = cond;

        let base = run_trace(&trace, &env, &mut bus0, &mut base_state, 1);

        let mut optimized = trace.clone();
        optimize_trace(&mut optimized, &OptConfig::default());

        // The strength reduction should remove both muls from the select lowering.
        assert!(
            !optimized
                .iter_instrs()
                .any(|i| matches!(i, Instr::BinOp { op: BinOp::Mul, .. })),
            "unexpected Mul remaining after select lowering strength reduction"
        );

        let opt = run_trace(&optimized, &env, &mut bus1, &mut opt_state, 1);
        assert_eq!(base.exit, RunExit::Returned);
        assert_eq!(opt.exit, RunExit::Returned);
        assert_eq!(base_state, opt_state);
        assert_eq!(bus0.mem(), bus1.mem());
        assert_eq!(
            opt_state.cpu.gpr[Gpr::Rdx.as_u8() as usize],
            if cond != 0 { 0xAAAA } else { 0xBBBB }
        );
    }
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

#[test]
fn mul_with_live_flags_is_not_strength_reduced() {
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
                flags: FlagSet::ALU,
            },
            // Consume a flag so the mul's flags remain live.
            Instr::LoadFlag {
                dst: v(2),
                flag: aero_types::Flag::Zf,
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
    base_state.cpu.gpr[Gpr::Rax.as_u8() as usize] = 7;
    let mut opt_state = base_state.clone();

    let base = run_trace(&trace, &env, &mut bus0, &mut base_state, 1);

    let mut optimized = trace.clone();
    optimize_trace(&mut optimized, &OptConfig::default());

    // Strength reduction should not touch muls that write flags.
    assert!(
        optimized
            .iter_instrs()
            .any(|i| matches!(i, Instr::BinOp { op: BinOp::Mul, flags, .. } if !flags.is_empty())),
        "expected mul with flags to remain a mul"
    );
    assert!(
        !optimized
            .iter_instrs()
            .any(|i| matches!(i, Instr::BinOp { op: BinOp::Shl, .. })),
        "unexpected shl introduced for mul-with-flags"
    );

    let opt = run_trace(&optimized, &env, &mut bus1, &mut opt_state, 1);
    assert_eq!(base.exit, RunExit::Returned);
    assert_eq!(opt.exit, RunExit::Returned);
    assert_eq!(base_state, opt_state);
    assert_eq!(bus0.mem(), bus1.mem());
}

#[test]
fn add_with_live_flags_is_not_strength_reduced_to_addr() {
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
                flags: FlagSet::ALU,
            },
            // Consume a flag so the add's flags remain live.
            Instr::LoadFlag {
                dst: v(2),
                flag: aero_types::Flag::Zf,
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
    base_state.cpu.gpr[Gpr::Rax.as_u8() as usize] = 0x1234;
    let mut opt_state = base_state.clone();

    let base = run_trace(&trace, &env, &mut bus0, &mut base_state, 1);

    let mut optimized = trace.clone();
    optimize_trace(&mut optimized, &OptConfig::default());

    // The add should not become an Addr, because its flags remain live.
    assert!(
        optimized
            .iter_instrs()
            .any(|i| matches!(i, Instr::BinOp { op: BinOp::Add, flags, .. } if !flags.is_empty())),
        "expected add with flags to remain a BinOp::Add"
    );
    assert!(
        !optimized
            .iter_instrs()
            .any(|i| matches!(i, Instr::Addr { .. })),
        "unexpected Addr introduced for add-with-flags"
    );

    let opt = run_trace(&optimized, &env, &mut bus1, &mut opt_state, 1);
    assert_eq!(base.exit, RunExit::Returned);
    assert_eq!(opt.exit, RunExit::Returned);
    assert_eq!(base_state, opt_state);
    assert_eq!(bus0.mem(), bus1.mem());
}

#[test]
fn add_const_folds_into_existing_addr_disp() {
    // (rax + 4) + 8 should become a single Addr with disp=12.
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
                rhs: Operand::Const(4),
                flags: FlagSet::EMPTY,
            },
            Instr::BinOp {
                dst: v(2),
                op: BinOp::Add,
                lhs: Operand::Value(v(1)),
                rhs: Operand::Const(8),
                flags: FlagSet::EMPTY,
            },
            Instr::StoreReg {
                reg: Gpr::Rbx,
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
    base_state.cpu.gpr[Gpr::Rax.as_u8() as usize] = 0x1000;
    let mut opt_state = base_state.clone();

    let base = run_trace(&trace, &env, &mut bus0, &mut base_state, 1);

    let mut optimized = trace.clone();
    optimize_trace(&mut optimized, &OptConfig::default());

    let add_count = optimized
        .iter_instrs()
        .filter(|i| matches!(i, Instr::BinOp { op: BinOp::Add, .. }))
        .count();
    assert_eq!(add_count, 0, "expected all adds to be strength-reduced");

    let addrs: Vec<_> = optimized
        .iter_instrs()
        .filter_map(|i| match i {
            Instr::Addr {
                base,
                index,
                scale,
                disp,
                ..
            } => Some((*base, *index, *scale, *disp)),
            _ => None,
        })
        .collect();
    assert_eq!(
        addrs.len(),
        1,
        "expected chained adds to fold into one Addr"
    );
    assert_eq!(
        addrs[0],
        (Operand::Value(v(0)), Operand::Const(0), 1, 12),
        "expected folded displacement of 12"
    );

    let opt = run_trace(&optimized, &env, &mut bus1, &mut opt_state, 1);
    assert_eq!(base.exit, RunExit::Returned);
    assert_eq!(opt.exit, RunExit::Returned);
    assert_eq!(base_state, opt_state);
    assert_eq!(bus0.mem(), bus1.mem());
}

#[test]
fn sub_const_folds_into_existing_addr_disp() {
    // (rax + 4) - 8 should become a single Addr with disp=-4.
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
                rhs: Operand::Const(4),
                flags: FlagSet::EMPTY,
            },
            Instr::BinOp {
                dst: v(2),
                op: BinOp::Sub,
                lhs: Operand::Value(v(1)),
                rhs: Operand::Const(8),
                flags: FlagSet::EMPTY,
            },
            Instr::StoreReg {
                reg: Gpr::Rbx,
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
    base_state.cpu.gpr[Gpr::Rax.as_u8() as usize] = 0x1000;
    let mut opt_state = base_state.clone();

    let base = run_trace(&trace, &env, &mut bus0, &mut base_state, 1);

    let mut optimized = trace.clone();
    optimize_trace(&mut optimized, &OptConfig::default());

    let sub_count = optimized
        .iter_instrs()
        .filter(|i| matches!(i, Instr::BinOp { op: BinOp::Sub, .. }))
        .count();
    assert_eq!(sub_count, 0, "expected sub to be strength-reduced");

    let addrs: Vec<_> = optimized
        .iter_instrs()
        .filter_map(|i| match i {
            Instr::Addr {
                base,
                index,
                scale,
                disp,
                ..
            } => Some((*base, *index, *scale, *disp)),
            _ => None,
        })
        .collect();
    assert_eq!(addrs.len(), 1, "expected chained ops to fold into one Addr");
    assert_eq!(
        addrs[0],
        (Operand::Value(v(0)), Operand::Const(0), 1, -4),
        "expected folded displacement of -4"
    );

    let opt = run_trace(&optimized, &env, &mut bus1, &mut opt_state, 1);
    assert_eq!(base.exit, RunExit::Returned);
    assert_eq!(opt.exit, RunExit::Returned);
    assert_eq!(base_state, opt_state);
    assert_eq!(bus0.mem(), bus1.mem());
}

#[test]
fn add_of_scaled_value_is_strength_reduced_to_indexed_addr() {
    // rbx + (rax << 3) should become Addr { base: rbx, index: rax, scale: 8, disp: 0 } and the
    // shift should be DCE'd.
    let trace = TraceIr {
        prologue: vec![],
        body: vec![
            Instr::LoadReg {
                dst: v(0),
                reg: Gpr::Rax,
            },
            Instr::LoadReg {
                dst: v(1),
                reg: Gpr::Rbx,
            },
            Instr::BinOp {
                dst: v(2),
                op: BinOp::Shl,
                lhs: Operand::Value(v(0)),
                rhs: Operand::Const(3),
                flags: FlagSet::EMPTY,
            },
            Instr::BinOp {
                dst: v(3),
                op: BinOp::Add,
                lhs: Operand::Value(v(1)),
                rhs: Operand::Value(v(2)),
                flags: FlagSet::EMPTY,
            },
            Instr::StoreReg {
                reg: Gpr::Rcx,
                src: Operand::Value(v(3)),
            },
        ],
        kind: TraceKind::Linear,
    };

    let env = RuntimeEnv::default();
    let mut bus0 = SimpleBus::new(64);
    let mut bus1 = bus0.clone();

    let mut base_state = T2State::default();
    base_state.cpu.rflags = aero_jit_x86::abi::RFLAGS_RESERVED1;
    base_state.cpu.gpr[Gpr::Rax.as_u8() as usize] = 0x10;
    base_state.cpu.gpr[Gpr::Rbx.as_u8() as usize] = 0x20;
    let mut opt_state = base_state.clone();

    let base = run_trace(&trace, &env, &mut bus0, &mut base_state, 1);

    let mut optimized = trace.clone();
    optimize_trace(&mut optimized, &OptConfig::default());

    assert!(
        optimized.iter_instrs().any(|i| matches!(
            i,
            Instr::Addr {
                base: Operand::Value(b),
                index: Operand::Value(idx),
                scale: 8,
                disp: 0,
                ..
            } if *b == v(1) && *idx == v(0)
        )),
        "expected base + (x<<3) to be reduced to indexed Addr"
    );
    assert!(
        !optimized
            .iter_instrs()
            .any(|i| matches!(i, Instr::BinOp { op: BinOp::Shl, .. })),
        "expected the shift to be eliminated after folding into Addr"
    );

    let opt = run_trace(&optimized, &env, &mut bus1, &mut opt_state, 1);
    assert_eq!(base.exit, RunExit::Returned);
    assert_eq!(opt.exit, RunExit::Returned);
    assert_eq!(base_state, opt_state);
    assert_eq!(bus0.mem(), bus1.mem());
}
