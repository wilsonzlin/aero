#[cfg(feature = "legacy-baseline")]
use aero_jit_x86::legacy::{
    ir::{IrBlock as LegacyIrBlock, IrOp as LegacyIrOp, Operand as LegacyOperand},
    wasm::{LegacyWasmOptions, WasmCodegen as LegacyWasmCodegen},
};
use aero_jit_x86::tier1::ir::{IrBlock, IrTerminator};
use aero_jit_x86::tier1::{Tier1WasmCodegen, Tier1WasmOptions};
use aero_jit_x86::tier2::ir::{Instr, TraceIr, TraceKind};
use aero_jit_x86::tier2::opt::RegAllocPlan;
use aero_jit_x86::tier2::{Tier2WasmCodegen, Tier2WasmOptions};
use aero_jit_x86::wasm::{IMPORT_MEMORY, IMPORT_MODULE, WASM32_MAX_PAGES};
use wasmparser::{Parser, Payload, TypeRef};

fn imported_memory_type(wasm: &[u8]) -> wasmparser::MemoryType {
    for payload in Parser::new(0).parse_all(wasm) {
        if let Payload::ImportSection(imports) = payload.expect("parse wasm") {
            for group in imports {
                let group = group.expect("parse import group");
                for import in group {
                    let (_offset, import) = import.expect("parse import");
                    if import.module == IMPORT_MODULE && import.name == IMPORT_MEMORY {
                        match import.ty {
                            TypeRef::Memory(mem) => return mem,
                            other => panic!("env.memory import was not a memory: {other:?}"),
                        }
                    }
                }
            }
        }
    }
    panic!("env.memory import not found");
}

fn trivial_tier1_block() -> IrBlock {
    IrBlock {
        entry_rip: 0x1000,
        insts: vec![],
        terminator: IrTerminator::Jump { target: 0x1000 },
        value_types: vec![],
    }
}

fn trivial_tier2_trace() -> TraceIr {
    TraceIr {
        prologue: vec![],
        body: vec![Instr::SideExit { exit_rip: 0x1000 }],
        kind: TraceKind::Linear,
    }
}

#[cfg(feature = "legacy-baseline")]
fn trivial_legacy_block() -> LegacyIrBlock {
    LegacyIrBlock::new(vec![LegacyIrOp::Exit {
        next_rip: LegacyOperand::Imm(0),
    }])
}

#[test]
fn tier1_wasm_memory_import_defaults_to_unshared() {
    let wasm = Tier1WasmCodegen::new()
        .compile_block_with_options(&trivial_tier1_block(), Tier1WasmOptions::default());
    let mem = imported_memory_type(&wasm);
    assert!(!mem.memory64);
    assert!(!mem.shared);
    assert_eq!(mem.initial, 1);
    assert_eq!(mem.maximum, None);
    assert_eq!(mem.page_size_log2, None);
}

#[test]
fn tier1_wasm_memory_import_can_be_shared() {
    let wasm = Tier1WasmCodegen::new().compile_block_with_options(
        &trivial_tier1_block(),
        Tier1WasmOptions {
            memory_shared: true,
            memory_min_pages: 3,
            memory_max_pages: Some(5),
            ..Default::default()
        },
    );
    let mem = imported_memory_type(&wasm);
    assert!(!mem.memory64);
    assert!(mem.shared);
    assert_eq!(mem.initial, 3);
    assert_eq!(mem.maximum, Some(5));
    assert_eq!(mem.page_size_log2, None);
}

#[test]
fn tier1_wasm_memory_import_shared_defaults_to_4gib_maximum_and_respects_minimum() {
    let wasm = Tier1WasmCodegen::new().compile_block_with_options(
        &trivial_tier1_block(),
        Tier1WasmOptions {
            memory_shared: true,
            memory_min_pages: 3,
            ..Default::default()
        },
    );
    let mem = imported_memory_type(&wasm);
    assert!(!mem.memory64);
    assert!(mem.shared);
    assert_eq!(mem.initial, 3);
    assert_eq!(mem.maximum, Some(u64::from(WASM32_MAX_PAGES)));
    assert_eq!(mem.page_size_log2, None);
}

#[test]
fn tier2_wasm_memory_import_defaults_to_unshared() {
    let trace = trivial_tier2_trace();
    let plan = RegAllocPlan::default();
    let wasm = Tier2WasmCodegen::new().compile_trace_with_options(
        &trace,
        &plan,
        Tier2WasmOptions::default(),
    );
    let mem = imported_memory_type(&wasm);
    assert!(!mem.memory64);
    assert!(!mem.shared);
    assert_eq!(mem.initial, 1);
    assert_eq!(mem.maximum, None);
    assert_eq!(mem.page_size_log2, None);
}

#[test]
fn tier2_wasm_memory_import_can_be_shared() {
    let trace = trivial_tier2_trace();
    let plan = RegAllocPlan::default();
    let wasm = Tier2WasmCodegen::new().compile_trace_with_options(
        &trace,
        &plan,
        Tier2WasmOptions {
            memory_shared: true,
            memory_min_pages: 2,
            memory_max_pages: Some(4),
            ..Default::default()
        },
    );
    let mem = imported_memory_type(&wasm);
    assert!(!mem.memory64);
    assert!(mem.shared);
    assert_eq!(mem.initial, 2);
    assert_eq!(mem.maximum, Some(4));
    assert_eq!(mem.page_size_log2, None);
}

#[test]
fn tier2_wasm_memory_import_shared_defaults_to_4gib_maximum() {
    let trace = trivial_tier2_trace();
    let plan = RegAllocPlan::default();
    let wasm = Tier2WasmCodegen::new().compile_trace_with_options(
        &trace,
        &plan,
        Tier2WasmOptions {
            memory_shared: true,
            ..Default::default()
        },
    );
    let mem = imported_memory_type(&wasm);
    assert!(!mem.memory64);
    assert!(mem.shared);
    assert_eq!(mem.initial, 1);
    assert_eq!(mem.maximum, Some(u64::from(WASM32_MAX_PAGES)));
    assert_eq!(mem.page_size_log2, None);
}

#[test]
fn tier2_wasm_memory_import_shared_defaults_to_4gib_maximum_and_respects_minimum() {
    let trace = trivial_tier2_trace();
    let plan = RegAllocPlan::default();
    let wasm = Tier2WasmCodegen::new().compile_trace_with_options(
        &trace,
        &plan,
        Tier2WasmOptions {
            memory_shared: true,
            memory_min_pages: 7,
            ..Default::default()
        },
    );
    let mem = imported_memory_type(&wasm);
    assert!(!mem.memory64);
    assert!(mem.shared);
    assert_eq!(mem.initial, 7);
    assert_eq!(mem.maximum, Some(u64::from(WASM32_MAX_PAGES)));
    assert_eq!(mem.page_size_log2, None);
}

#[test]
#[should_panic(expected = "exceeds wasm32 max pages")]
fn tier1_wasm_memory_import_rejects_maximum_above_wasm32_limit() {
    let _ = Tier1WasmCodegen::new().compile_block_with_options(
        &trivial_tier1_block(),
        Tier1WasmOptions {
            memory_max_pages: Some(WASM32_MAX_PAGES + 1),
            ..Default::default()
        },
    );
}

#[test]
#[should_panic(expected = "exceeds wasm32 max pages")]
fn tier2_wasm_memory_import_rejects_maximum_above_wasm32_limit() {
    let trace = trivial_tier2_trace();
    let plan = RegAllocPlan::default();
    let _ = Tier2WasmCodegen::new().compile_trace_with_options(
        &trace,
        &plan,
        Tier2WasmOptions {
            memory_max_pages: Some(WASM32_MAX_PAGES + 1),
            ..Default::default()
        },
    );
}

#[cfg(feature = "legacy-baseline")]
#[test]
fn legacy_wasm_memory_import_shared_defaults_to_4gib_maximum() {
    // The legacy baseline codegen is feature-gated. This test ensures it can also generate modules
    // that import shared memories (SharedArrayBuffer) in browser workers.
    let block = trivial_legacy_block();
    let wasm = LegacyWasmCodegen::new().compile_block_with_options(
        &block,
        LegacyWasmOptions {
            memory_shared: true,
            ..Default::default()
        },
    );
    let mem = imported_memory_type(&wasm);
    assert!(!mem.memory64);
    assert!(mem.shared);
    assert_eq!(mem.initial, 1);
    assert_eq!(mem.maximum, Some(u64::from(WASM32_MAX_PAGES)));
    assert_eq!(mem.page_size_log2, None);
}

#[cfg(feature = "legacy-baseline")]
#[test]
fn legacy_wasm_memory_import_shared_defaults_to_4gib_maximum_and_respects_minimum() {
    let block = trivial_legacy_block();
    let wasm = LegacyWasmCodegen::new().compile_block_with_options(
        &block,
        LegacyWasmOptions {
            memory_shared: true,
            memory_min_pages: 9,
            ..Default::default()
        },
    );
    let mem = imported_memory_type(&wasm);
    assert!(!mem.memory64);
    assert!(mem.shared);
    assert_eq!(mem.initial, 9);
    assert_eq!(mem.maximum, Some(u64::from(WASM32_MAX_PAGES)));
    assert_eq!(mem.page_size_log2, None);
}

#[cfg(feature = "legacy-baseline")]
#[test]
#[should_panic(expected = "exceeds wasm32 max pages")]
fn legacy_wasm_memory_import_rejects_maximum_above_wasm32_limit() {
    let block = trivial_legacy_block();
    let _ = LegacyWasmCodegen::new().compile_block_with_options(
        &block,
        LegacyWasmOptions {
            memory_max_pages: Some(WASM32_MAX_PAGES + 1),
            ..Default::default()
        },
    );
}
