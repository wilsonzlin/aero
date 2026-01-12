mod tier1_common;

use aero_cpu_core::jit::runtime::PageVersionTracker;
use aero_types::{Flag, FlagSet, Width};
use tier1_common::SimpleBus;

use aero_jit_x86::tier2::ir::{Function, Instr, Terminator, TraceKind};
use aero_jit_x86::tier2::profile::{ProfileData, TraceConfig};
use aero_jit_x86::tier2::trace::TraceBuilder;
use aero_jit_x86::tier2::{build_function_from_x86, CfgBuildConfig};

// Tiny x86 program:
//
//   0x00: dec rcx
//   0x03: jne 0x00
//   0x05: int3  (decoded as Invalid by the Tier-1 front-end => exit-to-interpreter)
//
// This yields a 2-block CFG:
//   block0: branch (backedge to 0x00, fallthrough to 0x05)
//   block1: side-exit/return
const CODE: &[u8] = &[
    0x48, 0xff, 0xc9, // dec rcx
    0x75, 0xfb, // jne -5 (to 0x00)
    0xcc, // int3
];

// Tiny x86 program that performs a memory store and load:
//
//   0x00: mov byte ptr [rax], 0x12
//   0x03: mov al, byte ptr [rax]
//   0x05: int3
const MEM_CODE: &[u8] = &[
    0xc6, 0x00, 0x12, // mov byte ptr [rax], 0x12
    0x8a, 0x00, // mov al, byte ptr [rax]
    0xcc, // int3
];

// Tiny x86 program that uses a parity conditional jump (JP):
//
//   0x00: xor eax, eax
//   0x02: jp 0x00
//   0x04: int3
const JP_CODE: &[u8] = &[
    0x31, 0xc0, // xor eax, eax
    0x7a, 0xfc, // jp -4 (to 0x00)
    0xcc, // int3
];

// Same as `JP_CODE`, but with JNP.
const JNP_CODE: &[u8] = &[
    0x31, 0xc0, // xor eax, eax
    0x7b, 0xfc, // jnp -4 (to 0x00)
    0xcc, // int3
];

fn build_test_func() -> Function {
    let mut bus = SimpleBus::new(64);
    bus.load(0, CODE);
    build_function_from_x86(&bus, 0, 64, CfgBuildConfig::default())
}

fn build_func(code: &[u8]) -> Function {
    let mut bus = SimpleBus::new(64);
    bus.load(0, code);
    build_function_from_x86(&bus, 0, 64, CfgBuildConfig::default())
}

#[test]
fn builds_cfg_from_x86_bytes() {
    let func = build_test_func();

    assert_eq!(func.block(func.entry).start_rip, 0);
    assert!(func.find_block_by_rip(0).is_some());
    assert!(func.find_block_by_rip(5).is_some());

    let entry = func.entry;
    let exit = func.find_block_by_rip(5).unwrap();

    match &func.block(entry).term {
        Terminator::Branch {
            then_bb, else_bb, ..
        } => {
            assert_eq!(*then_bb, entry);
            assert_eq!(*else_bb, exit);
        }
        other => panic!("expected entry block to end in Branch, got {other:?}"),
    }
}

#[test]
fn trace_builder_classifies_hot_backedge_as_loop() {
    let func = build_test_func();
    let entry = func.entry;
    let exit = func.find_block_by_rip(5).unwrap();

    let mut profile = ProfileData::default();
    profile.block_counts.insert(entry, 10_000);
    profile.block_counts.insert(exit, 10);
    profile.edge_counts.insert((entry, entry), 9_000);
    profile.edge_counts.insert((entry, exit), 1_000);
    profile.hot_backedges.insert((entry, entry));
    let mut page_versions = PageVersionTracker::default();
    page_versions.set_version(0, 1);

    let cfg = TraceConfig {
        hot_block_threshold: 1000,
        max_blocks: 8,
        max_instrs: 256,
    };

    let builder = TraceBuilder::new(&func, &profile, &page_versions, cfg);
    let trace = builder.build_from(entry).expect("trace should be hot");
    assert_eq!(trace.ir.kind, TraceKind::Loop);
}

#[test]
fn trace_builder_falls_back_to_linear_without_hot_backedge() {
    let func = build_test_func();
    let entry = func.entry;
    let exit = func.find_block_by_rip(5).unwrap();

    let mut profile = ProfileData::default();
    profile.block_counts.insert(entry, 10_000);
    profile.block_counts.insert(exit, 10);
    profile.edge_counts.insert((entry, entry), 9_000);
    profile.edge_counts.insert((entry, exit), 1_000);
    // Do not mark (entry -> entry) as a hot backedge.
    let mut page_versions = PageVersionTracker::default();
    page_versions.set_version(0, 1);

    let cfg = TraceConfig {
        hot_block_threshold: 1000,
        max_blocks: 8,
        max_instrs: 256,
    };

    let builder = TraceBuilder::new(&func, &profile, &page_versions, cfg);
    let trace = builder.build_from(entry).expect("trace should be hot");
    assert_eq!(trace.ir.kind, TraceKind::Linear);
}

#[test]
fn lowers_tier1_load_store_into_tier2_memory_ops() {
    let func = build_func(MEM_CODE);
    let entry = func.entry;

    let instrs = &func.block(entry).instrs;
    assert!(instrs.iter().any(|i| matches!(
        i,
        Instr::StoreMem {
            width: Width::W8,
            ..
        }
    )));
    assert!(instrs.iter().any(|i| matches!(
        i,
        Instr::LoadMem {
            width: Width::W8,
            ..
        }
    )));

    // Historically, Tier-2 would bail on Tier-1 memory ops by emitting an unconditional
    // side-exit at the *entry* RIP. We should only side-exit for the final `int3`
    // terminator (exit RIP = 0x6).
    assert!(!instrs
        .iter()
        .any(|i| matches!(i, Instr::SideExit { exit_rip } if *exit_rip == 0)));
}

#[test]
fn supports_parity_conditions_jp_and_jnp() {
    for code in [JP_CODE, JNP_CODE] {
        let func = build_func(code);

        assert_eq!(func.block(func.entry).start_rip, 0);
        assert!(func.find_block_by_rip(0).is_some());
        assert!(func.find_block_by_rip(4).is_some());

        let entry = func.entry;
        let exit = func.find_block_by_rip(4).unwrap();

        match &func.block(entry).term {
            Terminator::Branch {
                then_bb, else_bb, ..
            } => {
                assert_eq!(*then_bb, entry);
                assert_eq!(*else_bb, exit);
            }
            other => panic!("expected entry block to end in Branch, got {other:?}"),
        }

        let instrs = &func.block(entry).instrs;
        assert!(instrs
            .iter()
            .any(|i| matches!(i, Instr::LoadFlag { flag: Flag::Pf, .. })));
        assert!(instrs
            .iter()
            .any(|i| matches!(i, Instr::BinOp { flags, .. } if flags.contains(FlagSet::PF))));
        assert!(instrs
            .iter()
            .any(|i| matches!(i, Instr::BinOp { flags, .. } if flags.contains(FlagSet::AF))));

        // Ensure we didn't deopt due to missing parity lowering.
        assert!(!instrs
            .iter()
            .any(|i| matches!(i, Instr::SideExit { exit_rip } if *exit_rip == 0)));
    }
}
