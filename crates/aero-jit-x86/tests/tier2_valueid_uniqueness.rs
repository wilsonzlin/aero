mod tier1_common;

use std::collections::HashMap;

use aero_cpu_core::jit::runtime::PageVersionTracker;
use aero_jit_x86::tier2::ir::{Operand, ValueId};
use aero_jit_x86::tier2::opt::{optimize_trace, OptConfig};
use aero_jit_x86::tier2::profile::{ProfileData, TraceConfig};
use aero_jit_x86::tier2::trace::TraceBuilder;
use aero_jit_x86::tier2::wasm_codegen::Tier2WasmCodegen;
use aero_jit_x86::tier2::{build_function_from_x86, CfgBuildConfig};
use tier1_common::SimpleBus;

use wasmparser::Validator;

#[test]
fn tier2_traces_have_globally_unique_value_ids_across_blocks() {
    // Build a small 3-block CFG:
    //
    //   b0: mov eax, 1; jmp b1
    //   b1: add eax, 2; jmp b2
    //   b2: add eax, 3; int3  (=> side exit)
    //
    // The trace builder concatenates block instruction streams without renaming ValueIds, so the
    // CFG builder must ensure global uniqueness across blocks.
    let code: &[u8] = &[
        0xb8, 0x01, 0x00, 0x00, 0x00, // mov eax, 1
        0xeb, 0x00, // jmp +0 (to 0x7)
        0x83, 0xc0, 0x02, // add eax, 2
        0xeb, 0x00, // jmp +0 (to 0xc)
        0x83, 0xc0, 0x03, // add eax, 3
        0xcc, // int3
    ];

    let mut bus = SimpleBus::new(64);
    bus.load(0, code);

    let func = build_function_from_x86(&bus, 0, 64, CfgBuildConfig::default());
    assert!(
        func.blocks.len() >= 3,
        "expected at least 3 blocks, got {}",
        func.blocks.len()
    );

    let mut profile = ProfileData::default();
    profile.block_counts.insert(func.entry, 10_000);

    let page_versions = PageVersionTracker::default();
    let builder = TraceBuilder::new(
        &func,
        &profile,
        &page_versions,
        TraceConfig {
            hot_block_threshold: 1000,
            max_blocks: 8,
            max_instrs: 256,
        },
    );

    let mut trace = builder.build_from(func.entry).expect("trace should be hot");
    assert!(
        trace.blocks.len() >= 3,
        "expected trace to span multiple blocks, got {}",
        trace.blocks.len()
    );

    let mut defs: HashMap<ValueId, usize> = HashMap::new();
    for inst in trace.ir.iter_instrs() {
        if let Some(dst) = inst.dst() {
            *defs.entry(dst).or_insert(0) += 1;
        }
    }

    for (v, count) in &defs {
        assert_eq!(
            *count, 1,
            "ValueId collision: {v:?} is defined {count} times in a single trace"
        );
    }

    for inst in trace.ir.iter_instrs() {
        inst.for_each_operand(|op| {
            let Operand::Value(v) = op else { return };
            assert_eq!(
                defs.get(&v).copied(),
                Some(1),
                "use of {v:?} does not resolve to exactly one definition"
            );
        });
    }

    let opt = optimize_trace(&mut trace.ir, &OptConfig::default());
    let wasm = Tier2WasmCodegen::new().compile_trace(&trace.ir, &opt.regalloc);

    let mut validator = Validator::new();
    validator
        .validate_all(&wasm)
        .expect("generated wasm is valid");
}
