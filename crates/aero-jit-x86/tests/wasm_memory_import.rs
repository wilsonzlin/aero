use aero_jit_x86::tier1::ir::{IrBlock, IrTerminator};
use aero_jit_x86::tier1::{Tier1WasmCodegen, Tier1WasmOptions};
use aero_jit_x86::tier2::ir::{Instr, TraceIr, TraceKind};
use aero_jit_x86::tier2::opt::RegAllocPlan;
use aero_jit_x86::tier2::{Tier2WasmCodegen, Tier2WasmOptions};
use aero_jit_x86::wasm::{IMPORT_MEMORY, IMPORT_MODULE};
use wasmparser::{Parser, Payload, TypeRef};

fn imported_memory_type(wasm: &[u8]) -> wasmparser::MemoryType {
    for payload in Parser::new(0).parse_all(wasm) {
        match payload.expect("parse wasm") {
            Payload::ImportSection(imports) => {
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
            _ => {}
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

#[test]
fn tier1_wasm_memory_import_defaults_to_unshared() {
    let wasm = Tier1WasmCodegen::new().compile_block_with_options(
        &trivial_tier1_block(),
        Tier1WasmOptions::default(),
    );
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
fn tier2_wasm_memory_import_defaults_to_unshared() {
    let trace = trivial_tier2_trace();
    let plan = RegAllocPlan::default();
    let wasm =
        Tier2WasmCodegen::new().compile_trace_with_options(&trace, &plan, Tier2WasmOptions::default());
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
    assert_eq!(mem.maximum, Some(65_536));
    assert_eq!(mem.page_size_log2, None);
}
