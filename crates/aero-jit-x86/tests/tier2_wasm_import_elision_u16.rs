use aero_jit_x86::tier2::ir::{Instr, Operand, TraceIr, TraceKind, ValueId};
use aero_jit_x86::tier2::opt::RegAllocPlan;
use aero_jit_x86::tier2::wasm_codegen::{Tier2WasmCodegen, Tier2WasmOptions};
use aero_jit_x86::wasm::{
    IMPORT_MEM_READ_U16, IMPORT_MEM_WRITE_U16, IMPORT_MMU_TRANSLATE, IMPORT_MODULE,
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
fn tier2_inline_tlb_cross_page_only_u16_load_elides_mmu_translate_import() {
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![Instr::LoadMem {
            dst: ValueId(0),
            addr: Operand::Const(aero_jit_x86::PAGE_SIZE - 1),
            width: Width::W16,
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
            .any(|(module, name)| module == IMPORT_MODULE && name == IMPORT_MEM_READ_U16),
        "expected env.mem_read_u16 import for cross-page load, got {imports:?}"
    );
    assert!(
        !imports
            .iter()
            .any(|(module, name)| module == IMPORT_MODULE && name == IMPORT_MMU_TRANSLATE),
        "expected cross-page-only u16 load trace to not import env.mmu_translate, got {imports:?}"
    );
}

#[test]
fn tier2_inline_tlb_cross_page_only_u16_store_elides_mmu_translate_import() {
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![Instr::StoreMem {
            addr: Operand::Const(aero_jit_x86::PAGE_SIZE - 1),
            src: Operand::Const(0xabcd),
            width: Width::W16,
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
            .any(|(module, name)| module == IMPORT_MODULE && name == IMPORT_MEM_WRITE_U16),
        "expected env.mem_write_u16 import for cross-page store, got {imports:?}"
    );
    assert!(
        !imports
            .iter()
            .any(|(module, name)| module == IMPORT_MODULE && name == IMPORT_MMU_TRANSLATE),
        "expected cross-page-only u16 store trace to not import env.mmu_translate, got {imports:?}"
    );
}

#[test]
fn tier2_inline_tlb_cross_page_only_u16_load_with_value_const_elides_mmu_translate_import() {
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![
            Instr::Const {
                dst: ValueId(0),
                value: aero_jit_x86::PAGE_SIZE - 1,
            },
            Instr::LoadMem {
                dst: ValueId(1),
                addr: Operand::Value(ValueId(0)),
                width: Width::W16,
            },
        ],
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
            .any(|(module, name)| module == IMPORT_MODULE && name == IMPORT_MEM_READ_U16),
        "expected env.mem_read_u16 import for cross-page load, got {imports:?}"
    );
    assert!(
        !imports
            .iter()
            .any(|(module, name)| module == IMPORT_MODULE && name == IMPORT_MMU_TRANSLATE),
        "expected cross-page-only u16 load trace to not import env.mmu_translate, got {imports:?}"
    );
}

#[test]
fn tier2_inline_tlb_cross_page_only_u16_store_with_value_const_elides_mmu_translate_import() {
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![
            Instr::Const {
                dst: ValueId(0),
                value: aero_jit_x86::PAGE_SIZE - 1,
            },
            Instr::StoreMem {
                addr: Operand::Value(ValueId(0)),
                src: Operand::Const(0xabcd),
                width: Width::W16,
            },
        ],
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
            .any(|(module, name)| module == IMPORT_MODULE && name == IMPORT_MEM_WRITE_U16),
        "expected env.mem_write_u16 import for cross-page store, got {imports:?}"
    );
    assert!(
        !imports
            .iter()
            .any(|(module, name)| module == IMPORT_MODULE && name == IMPORT_MMU_TRANSLATE),
        "expected cross-page-only u16 store trace to not import env.mmu_translate, got {imports:?}"
    );
}

#[test]
fn tier2_inline_tlb_constant_end_of_page_u16_load_imports_mmu_translate_and_elides_cross_page_check(
) {
    // For u16, `PAGE_SIZE - 2` is still same-page. This test is sensitive to the `>` vs `>=`
    // boundary when deciding whether an access can ever cross a page.
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![Instr::LoadMem {
            dst: ValueId(0),
            addr: Operand::Const(aero_jit_x86::PAGE_SIZE - 2),
            width: Width::W16,
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
            .any(|(module, name)| module == IMPORT_MODULE && name == IMPORT_MEM_READ_U16),
        "expected env.mem_read_u16 import for u16 load, got {imports:?}"
    );
    assert!(
        imports
            .iter()
            .any(|(module, name)| module == IMPORT_MODULE && name == IMPORT_MMU_TRANSLATE),
        "expected end-of-page same-page u16 load to import env.mmu_translate, got {imports:?}"
    );
    assert_eq!(
        count_i64_gt_u(&wasm),
        0,
        "expected constant end-of-page same-page u16 load to not emit a cross-page check"
    );

    let mmu_translate = imported_func_index(&wasm, IMPORT_MODULE, IMPORT_MMU_TRANSLATE)
        .expect("expected env.mmu_translate import");
    assert_eq!(
        count_calls_to(&wasm, mmu_translate),
        2,
        "expected end-of-page same-page u16 load to emit inline-TLB mmu_translate call sites"
    );
}

#[test]
fn tier2_inline_tlb_constant_end_of_page_u16_store_imports_mmu_translate_and_elides_cross_page_check(
) {
    // For u16, `PAGE_SIZE - 2` is still same-page. This test is sensitive to the `>` vs `>=`
    // boundary when deciding whether an access can ever cross a page.
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![Instr::StoreMem {
            addr: Operand::Const(aero_jit_x86::PAGE_SIZE - 2),
            src: Operand::Const(0xabcd),
            width: Width::W16,
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
            .any(|(module, name)| module == IMPORT_MODULE && name == IMPORT_MEM_WRITE_U16),
        "expected env.mem_write_u16 import for u16 store, got {imports:?}"
    );
    assert!(
        imports
            .iter()
            .any(|(module, name)| module == IMPORT_MODULE && name == IMPORT_MMU_TRANSLATE),
        "expected end-of-page same-page u16 store to import env.mmu_translate, got {imports:?}"
    );
    assert_eq!(
        count_i64_gt_u(&wasm),
        0,
        "expected constant end-of-page same-page u16 store to not emit a cross-page check"
    );

    let mmu_translate = imported_func_index(&wasm, IMPORT_MODULE, IMPORT_MMU_TRANSLATE)
        .expect("expected env.mmu_translate import");
    assert_eq!(
        count_calls_to(&wasm, mmu_translate),
        2,
        "expected end-of-page same-page u16 store to emit inline-TLB mmu_translate call sites"
    );
}

#[test]
fn tier2_inline_tlb_constant_cross_page_u16_load_skips_unreachable_mmu_translate_calls() {
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![
            // Keep inline-TLB enabled by including a same-page access.
            Instr::LoadMem {
                dst: ValueId(0),
                addr: Operand::Const(0),
                width: Width::W16,
            },
            Instr::Const {
                dst: ValueId(1),
                value: aero_jit_x86::PAGE_SIZE - 1,
            },
            // A constant cross-page load always takes the slow helper path, so it should not emit
            // inline-TLB scaffolding (including calls to `env.mmu_translate`).
            Instr::LoadMem {
                dst: ValueId(2),
                addr: Operand::Value(ValueId(1)),
                width: Width::W16,
            },
        ],
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

    let mmu_translate = imported_func_index(&wasm, IMPORT_MODULE, IMPORT_MMU_TRANSLATE)
        .expect("expected env.mmu_translate import");
    assert_eq!(
        count_calls_to(&wasm, mmu_translate),
        2,
        "expected only the same-page access to emit mmu_translate call sites"
    );
}

#[test]
fn tier2_inline_tlb_constant_cross_page_u16_store_skips_unreachable_mmu_translate_calls() {
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![
            // Keep inline-TLB enabled by including a same-page access.
            Instr::StoreMem {
                addr: Operand::Const(0),
                src: Operand::Const(0xabcd),
                width: Width::W16,
            },
            Instr::Const {
                dst: ValueId(0),
                value: aero_jit_x86::PAGE_SIZE - 1,
            },
            // A constant cross-page store always takes the slow helper path, so it should not emit
            // inline-TLB scaffolding (including calls to `env.mmu_translate`).
            Instr::StoreMem {
                addr: Operand::Value(ValueId(0)),
                src: Operand::Const(0xabcd),
                width: Width::W16,
            },
        ],
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

    let mmu_translate = imported_func_index(&wasm, IMPORT_MODULE, IMPORT_MMU_TRANSLATE)
        .expect("expected env.mmu_translate import");
    assert_eq!(
        count_calls_to(&wasm, mmu_translate),
        2,
        "expected only the same-page access to emit mmu_translate call sites"
    );
}

#[test]
fn tier2_inline_tlb_value_address_u16_load_emits_cross_page_check() {
    // For multi-byte accesses with non-constant addresses, we must emit a runtime cross-page check.
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![
            Instr::LoadReg {
                dst: ValueId(0),
                reg: aero_types::Gpr::Rax,
            },
            Instr::LoadMem {
                dst: ValueId(1),
                addr: Operand::Value(ValueId(0)),
                width: Width::W16,
            },
        ],
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

    assert_eq!(
        count_i64_gt_u(&wasm),
        1,
        "expected non-constant u16 load to emit exactly one cross-page check"
    );
}

#[test]
fn tier2_inline_tlb_value_address_u16_store_emits_cross_page_check() {
    // For multi-byte accesses with non-constant addresses, we must emit a runtime cross-page check.
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![
            Instr::LoadReg {
                dst: ValueId(0),
                reg: aero_types::Gpr::Rax,
            },
            Instr::StoreMem {
                addr: Operand::Value(ValueId(0)),
                src: Operand::Const(0xabcd),
                width: Width::W16,
            },
        ],
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

    assert_eq!(
        count_i64_gt_u(&wasm),
        1,
        "expected non-constant u16 store to emit exactly one cross-page check"
    );
}
