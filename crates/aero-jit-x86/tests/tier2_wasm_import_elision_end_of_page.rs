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

fn count_i64_gt_u(wasm: &[u8]) -> usize {
    let mut count = 0usize;
    for payload in Parser::new(0).parse_all(wasm) {
        if let Payload::CodeSectionEntry(body) = payload.expect("parse wasm") {
            let mut reader = body.get_operators_reader().expect("operators reader");
            while !reader.eof() {
                if matches!(reader.read().expect("read operator"), Operator::I64GtU) {
                    count += 1;
                }
            }
        }
    }
    count
}

#[test]
fn tier2_inline_tlb_constant_end_of_page_u32_load_imports_mmu_translate_and_uses_fast_path() {
    // For u32, `PAGE_SIZE - 4` is still same-page. This test is sensitive to the `>` vs `>=`
    // boundary when deciding whether an access can ever cross a page.
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![Instr::LoadMem {
            dst: ValueId(0),
            addr: Operand::Const(aero_jit_x86::PAGE_SIZE - 4),
            width: Width::W32,
        }],
        kind: TraceKind::Linear,
    };
    let plan = RegAllocPlan::default();
    let wasm = Tier2WasmCodegen::new().compile_trace_with_options(
        &trace,
        &plan,
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
        "expected env.mem_read_u32 import for u32 load, got {imports:?}"
    );
    assert!(
        imports
            .iter()
            .any(|(module, name)| module == IMPORT_MODULE && name == IMPORT_MMU_TRANSLATE),
        "expected end-of-page same-page u32 load to import env.mmu_translate, got {imports:?}"
    );
    assert_eq!(
        count_i64_gt_u(&wasm),
        0,
        "expected constant end-of-page same-page u32 load to not emit a cross-page check"
    );

    let mmu_translate = imported_func_index(&wasm, IMPORT_MODULE, IMPORT_MMU_TRANSLATE)
        .expect("expected env.mmu_translate import");
    assert_eq!(
        count_calls_to(&wasm, mmu_translate),
        2,
        "expected end-of-page same-page u32 load to emit inline-TLB mmu_translate call sites"
    );
}

#[test]
fn tier2_inline_tlb_constant_end_of_page_u32_store_imports_mmu_translate_and_uses_fast_path() {
    // For u32, `PAGE_SIZE - 4` is still same-page. This test is sensitive to the `>` vs `>=`
    // boundary when deciding whether an access can ever cross a page.
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![Instr::StoreMem {
            addr: Operand::Const(aero_jit_x86::PAGE_SIZE - 4),
            src: Operand::Const(0x1122_3344),
            width: Width::W32,
        }],
        kind: TraceKind::Linear,
    };
    let plan = RegAllocPlan::default();
    let wasm = Tier2WasmCodegen::new().compile_trace_with_options(
        &trace,
        &plan,
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
        "expected env.mem_write_u32 import for u32 store, got {imports:?}"
    );
    assert!(
        imports
            .iter()
            .any(|(module, name)| module == IMPORT_MODULE && name == IMPORT_MMU_TRANSLATE),
        "expected end-of-page same-page u32 store to import env.mmu_translate, got {imports:?}"
    );
    assert_eq!(
        count_i64_gt_u(&wasm),
        0,
        "expected constant end-of-page same-page u32 store to not emit a cross-page check"
    );

    let mmu_translate = imported_func_index(&wasm, IMPORT_MODULE, IMPORT_MMU_TRANSLATE)
        .expect("expected env.mmu_translate import");
    assert_eq!(
        count_calls_to(&wasm, mmu_translate),
        2,
        "expected end-of-page same-page u32 store to emit inline-TLB mmu_translate call sites"
    );
}

#[test]
fn tier2_inline_tlb_constant_end_of_page_u64_load_imports_mmu_translate_and_uses_fast_path() {
    // For u64, `PAGE_SIZE - 8` is still same-page. This test is sensitive to the `>` vs `>=`
    // boundary when deciding whether an access can ever cross a page.
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![Instr::LoadMem {
            dst: ValueId(0),
            addr: Operand::Const(aero_jit_x86::PAGE_SIZE - 8),
            width: Width::W64,
        }],
        kind: TraceKind::Linear,
    };
    let plan = RegAllocPlan::default();
    let wasm = Tier2WasmCodegen::new().compile_trace_with_options(
        &trace,
        &plan,
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
        "expected env.mem_read_u64 import for u64 load, got {imports:?}"
    );
    assert!(
        imports
            .iter()
            .any(|(module, name)| module == IMPORT_MODULE && name == IMPORT_MMU_TRANSLATE),
        "expected end-of-page same-page u64 load to import env.mmu_translate, got {imports:?}"
    );
    assert_eq!(
        count_i64_gt_u(&wasm),
        0,
        "expected constant end-of-page same-page u64 load to not emit a cross-page check"
    );

    let mmu_translate = imported_func_index(&wasm, IMPORT_MODULE, IMPORT_MMU_TRANSLATE)
        .expect("expected env.mmu_translate import");
    assert_eq!(
        count_calls_to(&wasm, mmu_translate),
        2,
        "expected end-of-page same-page u64 load to emit inline-TLB mmu_translate call sites"
    );
}

#[test]
fn tier2_inline_tlb_constant_end_of_page_u64_store_imports_mmu_translate_and_uses_fast_path() {
    // For u64, `PAGE_SIZE - 8` is still same-page. This test is sensitive to the `>` vs `>=`
    // boundary when deciding whether an access can ever cross a page.
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![Instr::StoreMem {
            addr: Operand::Const(aero_jit_x86::PAGE_SIZE - 8),
            src: Operand::Const(0x1122_3344_5566_7788),
            width: Width::W64,
        }],
        kind: TraceKind::Linear,
    };
    let plan = RegAllocPlan::default();
    let wasm = Tier2WasmCodegen::new().compile_trace_with_options(
        &trace,
        &plan,
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
        "expected env.mem_write_u64 import for u64 store, got {imports:?}"
    );
    assert!(
        imports
            .iter()
            .any(|(module, name)| module == IMPORT_MODULE && name == IMPORT_MMU_TRANSLATE),
        "expected end-of-page same-page u64 store to import env.mmu_translate, got {imports:?}"
    );
    assert_eq!(
        count_i64_gt_u(&wasm),
        0,
        "expected constant end-of-page same-page u64 store to not emit a cross-page check"
    );

    let mmu_translate = imported_func_index(&wasm, IMPORT_MODULE, IMPORT_MMU_TRANSLATE)
        .expect("expected env.mmu_translate import");
    assert_eq!(
        count_calls_to(&wasm, mmu_translate),
        2,
        "expected end-of-page same-page u64 store to emit inline-TLB mmu_translate call sites"
    );
}
