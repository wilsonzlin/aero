mod tier1_common;

use aero_jit_x86::tier2::interp::{run_trace, RunExit, RuntimeEnv, T2State};
use aero_jit_x86::tier2::ir::{Block, BlockId, Function, Instr, Terminator, TraceIr, TraceKind};
use aero_jit_x86::tier2::profile::{ProfileData, TraceConfig};
use aero_jit_x86::tier2::trace::TraceBuilder;
use tier1_common::SimpleBus;

#[test]
fn guard_code_version_invalidates_on_version_mismatch() {
    let entry_rip: u64 = 0x1000;
    let page = entry_rip >> aero_jit_x86::PAGE_SHIFT;

    let trace = TraceIr {
        prologue: vec![Instr::GuardCodeVersion {
            page,
            expected: 0,
            exit_rip: entry_rip,
        }],
        body: vec![],
        kind: TraceKind::Linear,
    };

    let env = RuntimeEnv::default();
    let mut bus = SimpleBus::new(1);

    let mut state = T2State::default();
    state.cpu.rip = entry_rip;
    let res = run_trace(&trace, &env, &mut bus, &mut state, 1);
    assert_eq!(res.exit, RunExit::Returned);

    // Simulate a guest write to the code page after compilation.
    env.page_versions.bump_write(entry_rip, 1);

    let mut state = T2State::default();
    state.cpu.rip = entry_rip;
    let res = run_trace(&trace, &env, &mut bus, &mut state, 1);
    assert_eq!(
        res.exit,
        RunExit::Invalidate {
            next_rip: entry_rip
        }
    );
}

#[test]
fn trace_builder_guards_all_code_pages_in_trace() {
    let func = Function {
        entry: BlockId(0),
        blocks: vec![
            Block {
                id: BlockId(0),
                start_rip: 0,
                code_len: 1,
                instrs: vec![],
                term: Terminator::Jump(BlockId(1)),
            },
            Block {
                id: BlockId(1),
                start_rip: 0x2000, // page 2
                code_len: 1,
                instrs: vec![],
                term: Terminator::Return,
            },
        ],
    };

    let env = RuntimeEnv::default();
    env.page_versions.set_version(0, 1);
    env.page_versions.set_version(2, 1);

    let mut profile = ProfileData::default();
    profile.block_counts.insert(BlockId(0), 10_000);

    let trace = TraceBuilder::new(
        &func,
        &profile,
        &env.page_versions,
        TraceConfig {
            hot_block_threshold: 1000,
            max_blocks: 8,
            max_instrs: 256,
        },
    )
    .build_from(BlockId(0))
    .expect("trace should be hot");

    let guarded_pages: Vec<u64> = trace
        .ir
        .prologue
        .iter()
        .filter_map(|inst| match inst {
            Instr::GuardCodeVersion { page, .. } => Some(*page),
            _ => None,
        })
        .collect();
    assert_eq!(guarded_pages, vec![0, 2]);
}

#[test]
fn trace_builder_loop_guards_only_in_body() {
    let func = Function {
        entry: BlockId(0),
        blocks: vec![
            Block {
                id: BlockId(0),
                start_rip: 0, // page 0
                code_len: 1,
                instrs: vec![Instr::Nop],
                term: Terminator::Jump(BlockId(1)),
            },
            Block {
                id: BlockId(1),
                start_rip: 0x2000, // page 2
                code_len: 1,
                instrs: vec![Instr::Nop],
                term: Terminator::Jump(BlockId(0)),
            },
        ],
    };

    let env = RuntimeEnv::default();
    env.page_versions.set_version(0, 1);
    env.page_versions.set_version(2, 1);

    let mut profile = ProfileData::default();
    profile.block_counts.insert(BlockId(0), 10_000);
    profile.hot_backedges.insert((BlockId(1), BlockId(0)));

    let trace = TraceBuilder::new(
        &func,
        &profile,
        &env.page_versions,
        TraceConfig {
            hot_block_threshold: 1000,
            max_blocks: 8,
            max_instrs: 256,
        },
    )
    .build_from(BlockId(0))
    .expect("trace should be hot");
    assert_eq!(trace.ir.kind, TraceKind::Loop);

    assert!(trace
        .ir
        .prologue
        .iter()
        .all(|inst| !matches!(inst, Instr::GuardCodeVersion { .. })));

    let guarded_pages_in_body_prefix: Vec<u64> = trace
        .ir
        .body
        .iter()
        .take_while(|inst| matches!(inst, Instr::GuardCodeVersion { .. }))
        .filter_map(|inst| match inst {
            Instr::GuardCodeVersion { page, .. } => Some(*page),
            _ => None,
        })
        .collect();
    assert_eq!(guarded_pages_in_body_prefix, vec![0, 2]);

    assert_eq!(trace.ir.body[guarded_pages_in_body_prefix.len()], Instr::Nop);
}
