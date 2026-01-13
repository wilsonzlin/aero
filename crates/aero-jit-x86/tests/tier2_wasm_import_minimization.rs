use std::collections::HashSet;

use aero_jit_x86::tier2::ir::{Instr, Operand, TraceIr, TraceKind, ValueId};
use aero_jit_x86::tier2::opt::RegAllocPlan;
use aero_jit_x86::tier2::wasm_codegen::Tier2WasmCodegen;
use aero_jit_x86::wasm::{
    IMPORT_MEM_READ_U16, IMPORT_MEM_READ_U32, IMPORT_MEM_READ_U64, IMPORT_MEM_READ_U8,
    IMPORT_MEM_WRITE_U16, IMPORT_MEM_WRITE_U32, IMPORT_MEM_WRITE_U64, IMPORT_MEM_WRITE_U8,
    IMPORT_MODULE,
};
use aero_types::{Gpr, Width};
use wasmparser::{Parser, Payload};

fn env_import_names(wasm: &[u8]) -> HashSet<String> {
    let mut names = HashSet::new();
    for payload in Parser::new(0).parse_all(wasm) {
        if let Payload::ImportSection(imports) = payload.expect("parse wasm") {
            for group in imports {
                let group = group.expect("parse import group");
                for import in group {
                    let (_offset, import) = import.expect("parse import");
                    if import.module == IMPORT_MODULE {
                        names.insert(import.name.to_string());
                    }
                }
            }
        }
    }
    names
}

#[test]
fn tier2_wasm_imports_only_used_mem_helpers_by_width() {
    // Trace contains only 8-bit memory accesses; the module should only import the u8 helpers.
    let trace = TraceIr {
        prologue: vec![],
        body: vec![
            Instr::StoreMem {
                addr: Operand::Const(0x1000),
                src: Operand::Const(0xab),
                width: Width::W8,
            },
            Instr::LoadMem {
                dst: ValueId(0),
                addr: Operand::Const(0x1000),
                width: Width::W8,
            },
            Instr::StoreReg {
                reg: Gpr::Rax,
                src: Operand::Value(ValueId(0)),
            },
        ],
        kind: TraceKind::Linear,
    };

    let wasm = Tier2WasmCodegen::new().compile_trace(&trace, &RegAllocPlan::default());
    wasmparser::Validator::new()
        .validate_all(&wasm)
        .expect("generated wasm should validate");

    let imports = env_import_names(&wasm);
    assert!(
        imports.contains(IMPORT_MEM_READ_U8),
        "expected {IMPORT_MEM_READ_U8} import"
    );
    assert!(
        imports.contains(IMPORT_MEM_WRITE_U8),
        "expected {IMPORT_MEM_WRITE_U8} import"
    );

    for &name in &[
        IMPORT_MEM_READ_U16,
        IMPORT_MEM_READ_U32,
        IMPORT_MEM_READ_U64,
        IMPORT_MEM_WRITE_U16,
        IMPORT_MEM_WRITE_U32,
        IMPORT_MEM_WRITE_U64,
    ] {
        assert!(
            !imports.contains(name),
            "unexpected import {IMPORT_MODULE}.{name} for u8-only trace"
        );
    }
}

