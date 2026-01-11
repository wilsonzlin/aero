use aero_cpu::SimpleBus;

use aero_jit::profile::{ProfileData, TraceConfig};
use aero_jit::t2_ir::{Terminator, TraceKind};
use aero_jit::tier2::{build_function_from_x86, CfgBuildConfig};
use aero_jit::trace::TraceBuilder;

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

fn build_test_func() -> aero_jit::t2_ir::Function {
    let mut bus = SimpleBus::new(64);
    bus.load(0, CODE);
    build_function_from_x86(&bus, 0, CfgBuildConfig::default())
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
        Terminator::Branch { then_bb, else_bb, .. } => {
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
    profile.code_page_versions.insert(0, 1);

    let cfg = TraceConfig {
        hot_block_threshold: 1000,
        max_blocks: 8,
        max_instrs: 256,
    };

    let builder = TraceBuilder::new(&func, &profile, cfg);
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
    profile.code_page_versions.insert(0, 1);

    let cfg = TraceConfig {
        hot_block_threshold: 1000,
        max_blocks: 8,
        max_instrs: 256,
    };

    let builder = TraceBuilder::new(&func, &profile, cfg);
    let trace = builder.build_from(entry).expect("trace should be hot");
    assert_eq!(trace.ir.kind, TraceKind::Linear);
}
