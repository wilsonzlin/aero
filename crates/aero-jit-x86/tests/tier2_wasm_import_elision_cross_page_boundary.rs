use aero_jit_x86::tier2::ir::{Instr, Operand, TraceIr, TraceKind, ValueId};
use aero_jit_x86::tier2::opt::RegAllocPlan;
use aero_jit_x86::tier2::wasm_codegen::{Tier2WasmCodegen, Tier2WasmOptions};
use aero_jit_x86::wasm::{
    IMPORT_MEM_READ_U32, IMPORT_MEM_READ_U64, IMPORT_MEM_WRITE_U32, IMPORT_MEM_WRITE_U64,
    IMPORT_MMU_TRANSLATE, IMPORT_MODULE,
};
use aero_types::Width;
use wasmparser::{Operator, Parser, Payload, TypeRef};

fn import_names(wasm: &[u8]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for payload in Parser::new(0).parse_all(wasm) {
        if let Payload::ImportSection(imports) = payload.expect("parse wasm") {
            for group in imports {
                let group = group.expect("parse import group");
                for import in group {
                    let (_offset, import) = import.expect("parse import");
                    // Only record named imports (functions, memories, etc).
                    match import.ty {
                        TypeRef::Func(_) | TypeRef::Memory(_) => {
                            out.push((import.module.to_string(), import.name.to_string()));
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    out
}

fn imported_func_index(wasm: &[u8], module_name: &str, import_name: &str) -> Option<u32> {
    let mut func_index = 0u32;
    for payload in Parser::new(0).parse_all(wasm) {
        if let Payload::ImportSection(imports) = payload.expect("parse wasm") {
            for group in imports {
                let group = group.expect("parse import group");
                for import in group {
                    let (_offset, import) = import.expect("parse import");
                    let TypeRef::Func(_) = import.ty else {
                        continue;
                    };
                    if import.module == module_name && import.name == import_name {
                        return Some(func_index);
                    }
                    func_index += 1;
                }
            }
        }
    }
    None
}

fn count_calls_to(wasm: &[u8], func_index: u32) -> usize {
    let mut count = 0usize;
    for payload in Parser::new(0).parse_all(wasm) {
        if let Payload::CodeSectionEntry(body) = payload.expect("parse wasm") {
            let mut reader = body.get_operators_reader().expect("operators reader");
            while !reader.eof() {
                match reader.read().expect("read operator") {
                    Operator::Call { function_index } if function_index == func_index => {
                        count += 1;
                    }
                    _ => {}
                }
            }
        }
    }
    count
}

#[test]
fn tier2_inline_tlb_cross_page_boundary_u32_load_elides_mmu_translate_import() {
    // For u32, `PAGE_SIZE - 3` is the first cross-page start offset. This catches off-by-one bugs
    // where `PAGE_SIZE - 3` is incorrectly treated as same-page.
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![Instr::LoadMem {
            dst: ValueId(0),
            addr: Operand::Const(aero_jit_x86::PAGE_SIZE - 3),
            width: Width::W32,
        }],
        kind: TraceKind::Linear,
    };
    let wasm = Tier2WasmCodegen::new().compile_trace_with_options(
        &trace,
        &RegAllocPlan::default(),
        Tier2WasmOptions {
            inline_tlb: true,
            ..Default::default()
        },
    );

    let imports = import_names(&wasm);
    assert!(
        imports
            .iter()
            .any(|(module, name)| module == IMPORT_MODULE && name == IMPORT_MEM_READ_U32),
        "expected env.mem_read_u32 import for cross-page load, got {imports:?}"
    );
    assert!(
        !imports
            .iter()
            .any(|(module, name)| module == IMPORT_MODULE && name == IMPORT_MMU_TRANSLATE),
        "expected cross-page-only trace to not import env.mmu_translate, got {imports:?}"
    );
}

#[test]
fn tier2_inline_tlb_cross_page_boundary_u32_store_elides_mmu_translate_import() {
    // For u32, `PAGE_SIZE - 3` is the first cross-page start offset.
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![Instr::StoreMem {
            addr: Operand::Const(aero_jit_x86::PAGE_SIZE - 3),
            src: Operand::Const(0x1122_3344),
            width: Width::W32,
        }],
        kind: TraceKind::Linear,
    };
    let wasm = Tier2WasmCodegen::new().compile_trace_with_options(
        &trace,
        &RegAllocPlan::default(),
        Tier2WasmOptions {
            inline_tlb: true,
            ..Default::default()
        },
    );

    let imports = import_names(&wasm);
    assert!(
        imports
            .iter()
            .any(|(module, name)| module == IMPORT_MODULE && name == IMPORT_MEM_WRITE_U32),
        "expected env.mem_write_u32 import for cross-page store, got {imports:?}"
    );
    assert!(
        !imports
            .iter()
            .any(|(module, name)| module == IMPORT_MODULE && name == IMPORT_MMU_TRANSLATE),
        "expected cross-page-only trace to not import env.mmu_translate, got {imports:?}"
    );
}

#[test]
fn tier2_inline_tlb_cross_page_boundary_u64_load_elides_mmu_translate_import() {
    // For u64, `PAGE_SIZE - 7` is the first cross-page start offset.
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![Instr::LoadMem {
            dst: ValueId(0),
            addr: Operand::Const(aero_jit_x86::PAGE_SIZE - 7),
            width: Width::W64,
        }],
        kind: TraceKind::Linear,
    };
    let wasm = Tier2WasmCodegen::new().compile_trace_with_options(
        &trace,
        &RegAllocPlan::default(),
        Tier2WasmOptions {
            inline_tlb: true,
            ..Default::default()
        },
    );

    let imports = import_names(&wasm);
    assert!(
        imports
            .iter()
            .any(|(module, name)| module == IMPORT_MODULE && name == IMPORT_MEM_READ_U64),
        "expected env.mem_read_u64 import for cross-page load, got {imports:?}"
    );
    assert!(
        !imports
            .iter()
            .any(|(module, name)| module == IMPORT_MODULE && name == IMPORT_MMU_TRANSLATE),
        "expected cross-page-only u64 load trace to not import env.mmu_translate, got {imports:?}"
    );
}

#[test]
fn tier2_inline_tlb_cross_page_boundary_u64_store_elides_mmu_translate_import() {
    // For u64, `PAGE_SIZE - 7` is the first cross-page start offset.
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![Instr::StoreMem {
            addr: Operand::Const(aero_jit_x86::PAGE_SIZE - 7),
            src: Operand::Const(0x1122_3344_5566_7788),
            width: Width::W64,
        }],
        kind: TraceKind::Linear,
    };
    let wasm = Tier2WasmCodegen::new().compile_trace_with_options(
        &trace,
        &RegAllocPlan::default(),
        Tier2WasmOptions {
            inline_tlb: true,
            ..Default::default()
        },
    );

    let imports = import_names(&wasm);
    assert!(
        imports
            .iter()
            .any(|(module, name)| module == IMPORT_MODULE && name == IMPORT_MEM_WRITE_U64),
        "expected env.mem_write_u64 import for cross-page store, got {imports:?}"
    );
    assert!(
        !imports
            .iter()
            .any(|(module, name)| module == IMPORT_MODULE && name == IMPORT_MMU_TRANSLATE),
        "expected cross-page-only u64 store trace to not import env.mmu_translate, got {imports:?}"
    );
}

#[test]
fn tier2_inline_tlb_cross_page_boundary_u32_load_skips_unreachable_mmu_translate_calls() {
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![
            // Keep inline-TLB enabled by including a same-page access.
            Instr::LoadMem {
                dst: ValueId(0),
                addr: Operand::Const(0),
                width: Width::W8,
            },
            // Constant cross-page u32 load should use the slow helper directly.
            Instr::LoadMem {
                dst: ValueId(1),
                addr: Operand::Const(aero_jit_x86::PAGE_SIZE - 3),
                width: Width::W32,
            },
        ],
        kind: TraceKind::Linear,
    };
    let wasm = Tier2WasmCodegen::new().compile_trace_with_options(
        &trace,
        &RegAllocPlan::default(),
        Tier2WasmOptions {
            inline_tlb: true,
            ..Default::default()
        },
    );
    let mmu_translate = imported_func_index(&wasm, IMPORT_MODULE, IMPORT_MMU_TRANSLATE)
        .expect("expected env.mmu_translate import");
    assert_eq!(
        count_calls_to(&wasm, mmu_translate),
        2,
        "expected only the same-page access to emit mmu_translate call sites"
    );
}

#[test]
fn tier2_inline_tlb_cross_page_boundary_u32_store_skips_unreachable_mmu_translate_calls() {
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![
            // Keep inline-TLB enabled by including a same-page access.
            Instr::LoadMem {
                dst: ValueId(0),
                addr: Operand::Const(0),
                width: Width::W8,
            },
            // Constant cross-page u32 store should use the slow helper directly.
            Instr::StoreMem {
                addr: Operand::Const(aero_jit_x86::PAGE_SIZE - 3),
                src: Operand::Const(0x1122_3344),
                width: Width::W32,
            },
        ],
        kind: TraceKind::Linear,
    };
    let wasm = Tier2WasmCodegen::new().compile_trace_with_options(
        &trace,
        &RegAllocPlan::default(),
        Tier2WasmOptions {
            inline_tlb: true,
            ..Default::default()
        },
    );
    let mmu_translate = imported_func_index(&wasm, IMPORT_MODULE, IMPORT_MMU_TRANSLATE)
        .expect("expected env.mmu_translate import");
    assert_eq!(
        count_calls_to(&wasm, mmu_translate),
        2,
        "expected only the same-page access to emit mmu_translate call sites"
    );
}

#[test]
fn tier2_inline_tlb_cross_page_boundary_u64_load_skips_unreachable_mmu_translate_calls() {
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![
            // Keep inline-TLB enabled by including a same-page access.
            Instr::LoadMem {
                dst: ValueId(0),
                addr: Operand::Const(0),
                width: Width::W8,
            },
            // Constant cross-page u64 load should use the slow helper directly.
            Instr::LoadMem {
                dst: ValueId(1),
                addr: Operand::Const(aero_jit_x86::PAGE_SIZE - 7),
                width: Width::W64,
            },
        ],
        kind: TraceKind::Linear,
    };
    let wasm = Tier2WasmCodegen::new().compile_trace_with_options(
        &trace,
        &RegAllocPlan::default(),
        Tier2WasmOptions {
            inline_tlb: true,
            ..Default::default()
        },
    );
    let mmu_translate = imported_func_index(&wasm, IMPORT_MODULE, IMPORT_MMU_TRANSLATE)
        .expect("expected env.mmu_translate import");
    assert_eq!(
        count_calls_to(&wasm, mmu_translate),
        2,
        "expected only the same-page access to emit mmu_translate call sites"
    );
}

#[test]
fn tier2_inline_tlb_cross_page_boundary_u64_store_skips_unreachable_mmu_translate_calls() {
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![
            // Keep inline-TLB enabled by including a same-page access.
            Instr::LoadMem {
                dst: ValueId(0),
                addr: Operand::Const(0),
                width: Width::W8,
            },
            // Constant cross-page u64 store should use the slow helper directly.
            Instr::StoreMem {
                addr: Operand::Const(aero_jit_x86::PAGE_SIZE - 7),
                src: Operand::Const(0x1122_3344_5566_7788),
                width: Width::W64,
            },
        ],
        kind: TraceKind::Linear,
    };
    let wasm = Tier2WasmCodegen::new().compile_trace_with_options(
        &trace,
        &RegAllocPlan::default(),
        Tier2WasmOptions {
            inline_tlb: true,
            ..Default::default()
        },
    );
    let mmu_translate = imported_func_index(&wasm, IMPORT_MODULE, IMPORT_MMU_TRANSLATE)
        .expect("expected env.mmu_translate import");
    assert_eq!(
        count_calls_to(&wasm, mmu_translate),
        2,
        "expected only the same-page access to emit mmu_translate call sites"
    );
}

