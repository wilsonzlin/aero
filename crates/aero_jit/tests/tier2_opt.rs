use rand::{seq::SliceRandom, Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

mod tier1_common;

use aero_types::{Flag, Gpr, Width};
use tier1_common::SimpleBus;

use aero_jit::profile::{ProfileData, TraceConfig};
use aero_jit::tier2::exec::{
    run_function, run_function_from_block, run_trace, run_trace_with_cached_regs, RunExit,
    RuntimeEnv, T2State,
};
use aero_jit::tier2::ir::{
    BinOp, Block, BlockId, FlagMask, Function, Instr, Operand, Terminator, TraceIr, TraceKind,
    ValueId, REG_COUNT,
};
use aero_jit::tier2::opt::{optimize_trace, passes, OptConfig};
use aero_jit::tier2::trace::TraceBuilder;

const ALL_REGS: [Gpr; REG_COUNT] = [
    Gpr::Rax,
    Gpr::Rcx,
    Gpr::Rdx,
    Gpr::Rbx,
    Gpr::Rsp,
    Gpr::Rbp,
    Gpr::Rsi,
    Gpr::Rdi,
    Gpr::R8,
    Gpr::R9,
    Gpr::R10,
    Gpr::R11,
    Gpr::R12,
    Gpr::R13,
    Gpr::R14,
    Gpr::R15,
];

fn v(idx: u32) -> ValueId {
    ValueId(idx)
}

fn make_random_state(rng: &mut ChaCha8Rng) -> T2State {
    let mut state = T2State::default();
    for r in ALL_REGS {
        state.cpu.gpr[r.as_u8() as usize] = rng.gen();
    }
    state.cpu.rflags = aero_jit::abi::RFLAGS_RESERVED1;
    for flag in [Flag::Cf, Flag::Pf, Flag::Af, Flag::Zf, Flag::Sf, Flag::Of] {
        if rng.gen() {
            state.cpu.rflags |= 1u64 << flag.rflags_bit();
        }
    }
    state
}

fn gen_operand(rng: &mut ChaCha8Rng, values: &[ValueId]) -> Operand {
    if !values.is_empty() && rng.gen_bool(0.7) {
        Operand::Value(values[rng.gen_range(0..values.len())])
    } else {
        Operand::Const(rng.gen())
    }
}

fn gen_random_trace(rng: &mut ChaCha8Rng, max_instrs: usize) -> TraceIr {
    let mut next_value: u32 = 0;
    let mut values: Vec<ValueId> = Vec::new();
    let mut body: Vec<Instr> = Vec::new();

    for _ in 0..max_instrs {
        match rng.gen_range(0..100u32) {
            0..=15 => {
                let dst = v(next_value);
                next_value += 1;
                let value = rng.gen();
                body.push(Instr::Const { dst, value });
                values.push(dst);
            }
            16..=35 => {
                let dst = v(next_value);
                next_value += 1;
                let reg = *ALL_REGS.choose(rng).unwrap();
                body.push(Instr::LoadReg { dst, reg });
                values.push(dst);
            }
            36..=75 => {
                if values.is_empty() {
                    continue;
                }
                let dst = v(next_value);
                next_value += 1;
                let op = match rng.gen_range(0..10u32) {
                    0 => BinOp::Add,
                    1 => BinOp::Sub,
                    2 => BinOp::Mul,
                    3 => BinOp::And,
                    4 => BinOp::Or,
                    5 => BinOp::Xor,
                    6 => BinOp::Shl,
                    7 => BinOp::Shr,
                    8 => BinOp::Eq,
                    _ => BinOp::LtU,
                };
                let lhs = gen_operand(rng, &values);
                let rhs = gen_operand(rng, &values);
                let flags = if rng.gen_bool(0.3) {
                    FlagMask::ALL
                } else {
                    FlagMask::EMPTY
                };
                body.push(Instr::BinOp {
                    dst,
                    op,
                    lhs,
                    rhs,
                    flags,
                });
                values.push(dst);
            }
            76..=85 => {
                let dst = v(next_value);
                next_value += 1;
                let base = gen_operand(rng, &values);
                let index = gen_operand(rng, &values);
                let scale = *[1u8, 2, 4, 8].choose(rng).unwrap();
                let disp = rng.gen::<i32>() as i64;
                body.push(Instr::Addr {
                    dst,
                    base,
                    index,
                    scale,
                    disp,
                });
                values.push(dst);
            }
            _ => {
                if values.is_empty() {
                    continue;
                }
                let reg = *ALL_REGS.choose(rng).unwrap();
                let src = gen_operand(rng, &values);
                body.push(Instr::StoreReg { reg, src });
            }
        }
    }

    TraceIr {
        prologue: Vec::new(),
        body,
        kind: TraceKind::Linear,
    }
}

#[test]
fn random_traces_match_after_optimization_and_cached_reg_exec() {
    let env = RuntimeEnv::default();
    let mut rng = ChaCha8Rng::seed_from_u64(0x5EED);

    for _ in 0..250 {
        let trace = gen_random_trace(&mut rng, 50);
        let mut baseline = make_random_state(&mut rng);
        let mut optimized_state = baseline.clone();
        let mut bus0 = SimpleBus::new(65536);
        let mut bus1 = bus0.clone();

        let baseline_res = run_trace(&trace, &env, &mut bus0, &mut baseline, 1);

        let mut optimized = trace.clone();
        let out = optimize_trace(&mut optimized, &OptConfig::default());
        let opt_res = run_trace_with_cached_regs(
            &optimized,
            &env,
            &mut bus1,
            &mut optimized_state,
            1,
            &out.regalloc.cached,
        );

        assert_eq!(baseline_res.exit, opt_res.exit);
        assert_eq!(baseline, optimized_state);
    }
}

#[test]
fn flag_elimination_clears_overwritten_flags() {
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
                flags: FlagMask::ALL,
            },
            Instr::StoreReg {
                reg: Gpr::Rax,
                src: Operand::Value(v(2)),
            },
            Instr::BinOp {
                dst: v(3),
                op: BinOp::Add,
                lhs: Operand::Value(v(2)),
                rhs: Operand::Value(v(1)),
                flags: FlagMask::ALL,
            },
            Instr::StoreReg {
                reg: Gpr::Rbx,
                src: Operand::Value(v(3)),
            },
        ],
        kind: TraceKind::Linear,
    };

    optimize_trace(&mut trace, &OptConfig::default());

    let flags: Vec<FlagMask> = trace
        .iter_instrs()
        .filter_map(|i| match i {
            Instr::BinOp { flags, .. } => Some(*flags),
            _ => None,
        })
        .collect();

    assert!(flags.len() >= 2);
    assert!(flags[0].is_empty());
    assert_eq!(flags[1], FlagMask::ALL);
}

#[test]
fn cse_removes_duplicate_expressions() {
    let mut trace = TraceIr {
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
                op: BinOp::Add,
                lhs: Operand::Value(v(0)),
                rhs: Operand::Value(v(1)),
                flags: FlagMask::EMPTY,
            },
            Instr::BinOp {
                dst: v(3),
                op: BinOp::Add,
                lhs: Operand::Value(v(0)),
                rhs: Operand::Value(v(1)),
                flags: FlagMask::EMPTY,
            },
            Instr::StoreReg {
                reg: Gpr::Rcx,
                src: Operand::Value(v(2)),
            },
            Instr::StoreReg {
                reg: Gpr::Rdx,
                src: Operand::Value(v(3)),
            },
        ],
        kind: TraceKind::Linear,
    };

    optimize_trace(&mut trace, &OptConfig::default());

    let adds = trace
        .iter_instrs()
        .filter(|i| matches!(i, Instr::BinOp { op: BinOp::Add, .. }))
        .count();
    assert_eq!(adds, 1);
}

#[test]
fn addr_simplify_folds_nested_displacements() {
    let mut trace = TraceIr {
        prologue: vec![],
        body: vec![
            Instr::LoadReg {
                dst: v(0),
                reg: Gpr::Rax,
            },
            Instr::Addr {
                dst: v(1),
                base: Operand::Value(v(0)),
                index: Operand::Const(0),
                scale: 1,
                disp: 8,
            },
            Instr::Addr {
                dst: v(2),
                base: Operand::Value(v(1)),
                index: Operand::Const(0),
                scale: 1,
                disp: 4,
            },
            Instr::StoreReg {
                reg: Gpr::Rbx,
                src: Operand::Value(v(2)),
            },
        ],
        kind: TraceKind::Linear,
    };

    passes::addr_simplify::run(&mut trace);

    let inst = trace
        .body
        .iter()
        .find(|i| matches!(i, Instr::Addr { dst, .. } if *dst == v(2)))
        .unwrap();
    match inst {
        Instr::Addr { base, disp, .. } => {
            assert_eq!(*base, Operand::Value(v(0)));
            assert_eq!(*disp, 12);
        }
        _ => unreachable!(),
    }
}

#[test]
fn licm_hoists_invariant_loads_out_of_loop_body() {
    let mut trace = TraceIr {
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
                op: BinOp::Add,
                lhs: Operand::Value(v(0)),
                rhs: Operand::Value(v(1)),
                flags: FlagMask::EMPTY,
            },
            Instr::StoreReg {
                reg: Gpr::Rbx,
                src: Operand::Value(v(2)),
            },
        ],
        kind: TraceKind::Loop,
    };

    passes::licm::run(&mut trace);

    assert!(trace
        .prologue
        .iter()
        .any(|i| matches!(i, Instr::LoadReg { reg, .. } if *reg == Gpr::Rax)));
    assert!(!trace
        .body
        .iter()
        .any(|i| matches!(i, Instr::LoadReg { reg, .. } if *reg == Gpr::Rax)));
}

#[test]
fn regalloc_cached_exec_reduces_cpu_state_traffic() {
    let trace = TraceIr {
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
                flags: FlagMask::EMPTY,
            },
            Instr::StoreReg {
                reg: Gpr::Rax,
                src: Operand::Value(v(2)),
            },
            Instr::LoadReg {
                dst: v(3),
                reg: Gpr::Rax,
            },
            Instr::BinOp {
                dst: v(4),
                op: BinOp::Add,
                lhs: Operand::Value(v(3)),
                rhs: Operand::Value(v(1)),
                flags: FlagMask::EMPTY,
            },
            Instr::StoreReg {
                reg: Gpr::Rax,
                src: Operand::Value(v(4)),
            },
        ],
        kind: TraceKind::Linear,
    };

    let plan = passes::regalloc::run(&trace);
    assert!(plan.is_cached(Gpr::Rax));

    let env = RuntimeEnv::default();
    let mut cpu0 = T2State::default();
    cpu0.cpu.gpr[Gpr::Rax.as_u8() as usize] = 10;
    let mut cpu1 = cpu0.clone();
    let mut bus0 = SimpleBus::new(65536);
    let mut bus1 = bus0.clone();

    let baseline = run_trace(&trace, &env, &mut bus0, &mut cpu0, 1);
    let cached = run_trace_with_cached_regs(&trace, &env, &mut bus1, &mut cpu1, 1, &plan.cached);

    assert_eq!(baseline.exit, RunExit::Returned);
    assert_eq!(baseline.exit, cached.exit);
    assert_eq!(cpu0, cpu1);

    let base_traffic = baseline.stats.reg_loads + baseline.stats.reg_stores;
    let cached_traffic = cached.stats.reg_loads + cached.stats.reg_stores;
    assert!(cached_traffic < base_traffic);
}

#[test]
fn trace_builder_builds_loop_trace_and_deopts_with_precise_rip() {
    let func = Function {
        entry: BlockId(0),
        blocks: vec![
            Block {
                id: BlockId(0),
                start_rip: 0,
                instrs: vec![
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
                        flags: FlagMask::ALL,
                    },
                    Instr::StoreReg {
                        reg: Gpr::Rax,
                        src: Operand::Value(v(2)),
                    },
                    Instr::Const {
                        dst: v(3),
                        value: 10,
                    },
                    Instr::BinOp {
                        dst: v(4),
                        op: BinOp::LtU,
                        lhs: Operand::Value(v(2)),
                        rhs: Operand::Value(v(3)),
                        flags: FlagMask::EMPTY,
                    },
                ],
                term: Terminator::Branch {
                    cond: Operand::Value(v(4)),
                    then_bb: BlockId(0),
                    else_bb: BlockId(1),
                },
            },
            Block {
                id: BlockId(1),
                start_rip: 100,
                instrs: vec![],
                term: Terminator::Return,
            },
        ],
    };

    let mut profile = ProfileData::default();
    profile.block_counts.insert(BlockId(0), 10_000);
    profile.block_counts.insert(BlockId(1), 100);
    profile.edge_counts.insert((BlockId(0), BlockId(0)), 9_000);
    profile.edge_counts.insert((BlockId(0), BlockId(1)), 1_000);
    profile.hot_backedges.insert((BlockId(0), BlockId(0)));
    profile.code_page_versions.insert(0, 7);

    let cfg = TraceConfig {
        hot_block_threshold: 1000,
        max_blocks: 8,
        max_instrs: 256,
    };

    let builder = TraceBuilder::new(&func, &profile, cfg);
    let mut trace = builder.build_from(BlockId(0)).expect("trace");
    assert_eq!(trace.ir.kind, TraceKind::Loop);
    assert_eq!(trace.entry_block, BlockId(0));
    assert_eq!(trace.side_exits.len(), 1);
    assert_eq!(trace.side_exits[0].next_rip, 100);

    let mut env = RuntimeEnv::default();
    env.code_page_versions.insert(0, 7);
    let mut cpu_interp = T2State::default();
    cpu_interp.cpu.gpr[Gpr::Rax.as_u8() as usize] = 0;
    assert_eq!(
        run_function(
            &func,
            &env,
            &mut SimpleBus::new(65536),
            &mut cpu_interp,
            1_000_000
        ),
        RunExit::Returned
    );

    let mut cpu_trace = T2State::default();
    cpu_trace.cpu.gpr[Gpr::Rax.as_u8() as usize] = 0;
    let opt = optimize_trace(&mut trace.ir, &OptConfig::default());
    let mut bus = SimpleBus::new(65536);
    let exit = run_trace_with_cached_regs(
        &trace.ir,
        &env,
        &mut bus,
        &mut cpu_trace,
        1_000_000,
        &opt.regalloc.cached,
    );
    assert_eq!(exit.exit, RunExit::SideExit { next_rip: 100 });

    let block1 = func.find_block_by_rip(100).unwrap();
    assert_eq!(
        run_function_from_block(&func, &env, &mut bus, &mut cpu_trace, block1, 1_000_000),
        RunExit::Returned
    );
    assert_eq!(cpu_interp, cpu_trace);
}

#[test]
fn memory_load_store_roundtrip() {
    let trace = TraceIr {
        prologue: vec![],
        body: vec![
            Instr::Const {
                dst: v(0),
                value: 0x100,
            },
            Instr::Const {
                dst: v(1),
                value: 0x1122_3344_5566_7788,
            },
            Instr::StoreMem {
                addr: Operand::Value(v(0)),
                src: Operand::Value(v(1)),
                width: Width::W64,
            },
            Instr::LoadMem {
                dst: v(2),
                addr: Operand::Value(v(0)),
                width: Width::W64,
            },
            Instr::StoreReg {
                reg: Gpr::Rax,
                src: Operand::Value(v(2)),
            },
        ],
        kind: TraceKind::Linear,
    };

    let env = RuntimeEnv::default();
    let mut bus = SimpleBus::new(65536);
    let mut state = T2State::default();
    let res = run_trace(&trace, &env, &mut bus, &mut state, 1);
    assert_eq!(res.exit, RunExit::Returned);
    assert_eq!(
        state.cpu.gpr[Gpr::Rax.as_u8() as usize],
        0x1122_3344_5566_7788
    );

    let got_mem = u64::from_le_bytes(bus.mem()[0x100..0x108].try_into().expect("u64 bytes"));
    assert_eq!(got_mem, 0x1122_3344_5566_7788);
}

#[test]
fn memory_ops_not_misoptimized_across_store() {
    let trace = TraceIr {
        prologue: vec![],
        body: vec![
            Instr::Const {
                dst: v(0),
                value: 0x200,
            },
            Instr::Const {
                dst: v(1),
                value: 1,
            },
            Instr::StoreMem {
                addr: Operand::Value(v(0)),
                src: Operand::Value(v(1)),
                width: Width::W64,
            },
            Instr::LoadMem {
                dst: v(2),
                addr: Operand::Value(v(0)),
                width: Width::W64,
            },
            Instr::Const {
                dst: v(3),
                value: 2,
            },
            Instr::StoreMem {
                addr: Operand::Value(v(0)),
                src: Operand::Value(v(3)),
                width: Width::W64,
            },
            Instr::LoadMem {
                dst: v(4),
                addr: Operand::Value(v(0)),
                width: Width::W64,
            },
            Instr::StoreReg {
                reg: Gpr::Rcx,
                src: Operand::Value(v(2)),
            },
            Instr::StoreReg {
                reg: Gpr::Rdx,
                src: Operand::Value(v(4)),
            },
        ],
        kind: TraceKind::Linear,
    };

    let env = RuntimeEnv::default();
    let mut base_state = T2State::default();
    let mut opt_state = base_state.clone();
    let mut bus0 = SimpleBus::new(65536);
    let mut bus1 = bus0.clone();

    let base = run_trace(&trace, &env, &mut bus0, &mut base_state, 1);

    let mut optimized = trace.clone();
    let out = optimize_trace(&mut optimized, &OptConfig::default());
    let opt = run_trace_with_cached_regs(
        &optimized,
        &env,
        &mut bus1,
        &mut opt_state,
        1,
        &out.regalloc.cached,
    );

    assert_eq!(base.exit, opt.exit);
    assert_eq!(base_state, opt_state);
    assert_eq!(bus0.mem(), bus1.mem());
    assert_eq!(opt_state.cpu.gpr[Gpr::Rcx.as_u8() as usize], 1);
    assert_eq!(opt_state.cpu.gpr[Gpr::Rdx.as_u8() as usize], 2);
}

#[test]
fn dce_keeps_storemem_even_if_value_is_unused() {
    let mut trace = TraceIr {
        prologue: vec![],
        body: vec![
            Instr::Const {
                dst: v(0),
                value: 0x300,
            },
            Instr::StoreMem {
                addr: Operand::Value(v(0)),
                src: Operand::Const(0xAA),
                width: Width::W8,
            },
        ],
        kind: TraceKind::Linear,
    };

    optimize_trace(&mut trace, &OptConfig::default());
    assert!(trace
        .iter_instrs()
        .any(|inst| matches!(inst, Instr::StoreMem { .. })));

    let env = RuntimeEnv::default();
    let mut bus = SimpleBus::new(65536);
    let mut state = T2State::default();
    let res = run_trace(&trace, &env, &mut bus, &mut state, 1);
    assert_eq!(res.exit, RunExit::Returned);
    assert_eq!(bus.mem()[0x300], 0xAA);
}
