use aero_jit_x86::jit_ctx;
use aero_jit_x86::tier2::ir::{Instr, Operand, TraceIr, TraceKind, ValueId};
use aero_jit_x86::tier2::opt::RegAllocPlan;
use aero_jit_x86::tier2::wasm_codegen::{Tier2WasmCodegen, Tier2WasmOptions};
use aero_jit_x86::wasm::{
    IMPORT_MEM_READ_U32, IMPORT_MEM_READ_U64, IMPORT_MEM_WRITE_U32, IMPORT_MEM_WRITE_U64,
    IMPORT_MEM_WRITE_U8, IMPORT_MMU_TRANSLATE, IMPORT_MODULE,
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

fn loads_code_version_table_locals(wasm: &[u8]) -> bool {
    for payload in Parser::new(0).parse_all(wasm) {
        if let Payload::CodeSectionEntry(body) = payload.expect("parse wasm") {
            let mut reader = body.get_operators_reader().expect("operators reader");
            while !reader.eof() {
                if let Operator::I32Load { memarg } = reader.read().expect("read operator") {
                    if memarg.offset == u64::from(jit_ctx::CODE_VERSION_TABLE_PTR_OFFSET)
                        || memarg.offset == u64::from(jit_ctx::CODE_VERSION_TABLE_LEN_OFFSET)
                    {
                        return true;
                    }
                }
            }
        }
    }
    false
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
fn tier2_inline_tlb_cross_page_only_trace_elides_mmu_translate_import() {
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![Instr::LoadMem {
            dst: ValueId(0),
            addr: Operand::Const(aero_jit_x86::PAGE_SIZE - 2),
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
fn tier2_inline_tlb_cross_page_only_u64_load_elides_mmu_translate_import() {
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![Instr::LoadMem {
            dst: ValueId(0),
            addr: Operand::Const(aero_jit_x86::PAGE_SIZE - 2),
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
fn tier2_inline_tlb_cross_page_only_trace_with_value_const_elides_mmu_translate_import() {
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![
            Instr::Const {
                dst: ValueId(0),
                value: aero_jit_x86::PAGE_SIZE - 2,
            },
            Instr::LoadMem {
                dst: ValueId(1),
                addr: Operand::Value(ValueId(0)),
                width: Width::W32,
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
fn tier2_inline_tlb_cross_page_only_trace_with_addr_const_elides_mmu_translate_import() {
    // Same as the `Value(Const)` variant, but ensure we also propagate constant addresses through
    // `Instr::Addr`.
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![
            Instr::Const {
                dst: ValueId(0),
                value: aero_jit_x86::PAGE_SIZE,
            },
            Instr::Const {
                dst: ValueId(1),
                value: 0,
            },
            Instr::Addr {
                dst: ValueId(2),
                base: Operand::Value(ValueId(0)),
                index: Operand::Value(ValueId(1)),
                scale: 1,
                disp: -2,
            },
            Instr::LoadMem {
                dst: ValueId(3),
                addr: Operand::Value(ValueId(2)),
                width: Width::W32,
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
            .any(|(module, name)| module == IMPORT_MODULE && name == IMPORT_MEM_READ_U32),
        "expected env.mem_read_u32 import for cross-page load, got {imports:?}"
    );
    assert!(
        !imports
            .iter()
            .any(|(module, name)| module == IMPORT_MODULE && name == IMPORT_MMU_TRANSLATE),
        "expected Addr-constant cross-page-only trace to not import env.mmu_translate, got {imports:?}"
    );
}

#[test]
fn tier2_inline_tlb_cross_page_only_trace_with_binop_const_elides_mmu_translate_import() {
    // Ensure constant folding for `Instr::BinOp` feeds into the always-cross-page detection used
    // by inline-TLB import elision.
    //
    // Compute `PAGE_SIZE + (-2)` via wrapping addition to get `PAGE_SIZE - 2`, which is a constant
    // cross-page `u32` load address.
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![
            Instr::Const {
                dst: ValueId(0),
                value: aero_jit_x86::PAGE_SIZE,
            },
            Instr::Const {
                dst: ValueId(1),
                value: u64::MAX - 1, // -2 (wrapping)
            },
            Instr::BinOp {
                dst: ValueId(2),
                op: aero_jit_x86::tier2::ir::BinOp::Add,
                lhs: Operand::Value(ValueId(0)),
                rhs: Operand::Value(ValueId(1)),
                flags: aero_types::FlagSet::EMPTY,
            },
            Instr::LoadMem {
                dst: ValueId(3),
                addr: Operand::Value(ValueId(2)),
                width: Width::W32,
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
            .any(|(module, name)| module == IMPORT_MODULE && name == IMPORT_MEM_READ_U32),
        "expected env.mem_read_u32 import for cross-page load, got {imports:?}"
    );
    assert!(
        !imports
            .iter()
            .any(|(module, name)| module == IMPORT_MODULE && name == IMPORT_MMU_TRANSLATE),
        "expected BinOp-constant cross-page-only trace to not import env.mmu_translate, got {imports:?}"
    );
}

#[test]
fn tier2_inline_tlb_cross_page_only_store_elides_mmu_translate_import() {
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![Instr::StoreMem {
            addr: Operand::Const(aero_jit_x86::PAGE_SIZE - 2),
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
        "expected env.mem_write_u32 import for cross-page store, got {imports:?}"
    );
    assert!(
        !imports
            .iter()
            .any(|(module, name)| module == IMPORT_MODULE && name == IMPORT_MMU_TRANSLATE),
        "expected cross-page-only store trace to not import env.mmu_translate, got {imports:?}"
    );
}

#[test]
fn tier2_inline_tlb_cross_page_only_u64_store_elides_mmu_translate_import() {
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![Instr::StoreMem {
            addr: Operand::Const(aero_jit_x86::PAGE_SIZE - 2),
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
fn tier2_inline_tlb_cross_page_only_store_with_value_const_elides_mmu_translate_import() {
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![
            Instr::Const {
                dst: ValueId(0),
                value: aero_jit_x86::PAGE_SIZE - 2,
            },
            Instr::StoreMem {
                addr: Operand::Value(ValueId(0)),
                src: Operand::Const(0x1122_3344),
                width: Width::W32,
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
            .any(|(module, name)| module == IMPORT_MODULE && name == IMPORT_MEM_WRITE_U32),
        "expected env.mem_write_u32 import for cross-page store, got {imports:?}"
    );
    assert!(
        !imports
            .iter()
            .any(|(module, name)| module == IMPORT_MODULE && name == IMPORT_MMU_TRANSLATE),
        "expected cross-page-only store trace to not import env.mmu_translate, got {imports:?}"
    );
}

#[test]
fn tier2_inline_tlb_cross_page_only_store_with_addr_const_elides_mmu_translate_import() {
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![
            Instr::Const {
                dst: ValueId(0),
                value: aero_jit_x86::PAGE_SIZE,
            },
            Instr::Const {
                dst: ValueId(1),
                value: 0,
            },
            Instr::Addr {
                dst: ValueId(2),
                base: Operand::Value(ValueId(0)),
                index: Operand::Value(ValueId(1)),
                scale: 1,
                disp: -2,
            },
            Instr::StoreMem {
                addr: Operand::Value(ValueId(2)),
                src: Operand::Const(0x1122_3344),
                width: Width::W32,
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
            .any(|(module, name)| module == IMPORT_MODULE && name == IMPORT_MEM_WRITE_U32),
        "expected env.mem_write_u32 import for cross-page store, got {imports:?}"
    );
    assert!(
        !imports
            .iter()
            .any(|(module, name)| module == IMPORT_MODULE && name == IMPORT_MMU_TRANSLATE),
        "expected Addr-constant cross-page-only store trace to not import env.mmu_translate, got {imports:?}"
    );
}

#[test]
fn tier2_inline_tlb_cross_page_only_store_with_binop_const_elides_mmu_translate_import() {
    // Ensure constant folding for `Instr::BinOp` feeds into the always-cross-page detection used
    // by inline-TLB import elision for stores.
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![
            Instr::Const {
                dst: ValueId(0),
                value: aero_jit_x86::PAGE_SIZE,
            },
            Instr::Const {
                dst: ValueId(1),
                value: u64::MAX - 1, // -2 (wrapping)
            },
            Instr::BinOp {
                dst: ValueId(2),
                op: aero_jit_x86::tier2::ir::BinOp::Add,
                lhs: Operand::Value(ValueId(0)),
                rhs: Operand::Value(ValueId(1)),
                flags: aero_types::FlagSet::EMPTY,
            },
            Instr::StoreMem {
                addr: Operand::Value(ValueId(2)),
                src: Operand::Const(0x1122_3344),
                width: Width::W32,
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
            .any(|(module, name)| module == IMPORT_MODULE && name == IMPORT_MEM_WRITE_U32),
        "expected env.mem_write_u32 import for cross-page store, got {imports:?}"
    );
    assert!(
        !imports
            .iter()
            .any(|(module, name)| module == IMPORT_MODULE && name == IMPORT_MMU_TRANSLATE),
        "expected BinOp-constant cross-page-only store trace to not import env.mmu_translate, got {imports:?}"
    );
}

#[test]
fn tier2_inline_tlb_same_page_trace_imports_mmu_translate() {
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![Instr::StoreMem {
            addr: Operand::Const(0),
            src: Operand::Const(0xAA),
            width: Width::W8,
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
            .any(|(module, name)| module == IMPORT_MODULE && name == IMPORT_MEM_WRITE_U8),
        "expected env.mem_write_u8 import for MMIO fallback, got {imports:?}"
    );
    assert!(
        imports
            .iter()
            .any(|(module, name)| module == IMPORT_MODULE && name == IMPORT_MMU_TRANSLATE),
        "expected same-page trace to import env.mmu_translate, got {imports:?}"
    );
}

#[test]
fn tier2_inline_tlb_u8_access_elides_cross_page_check() {
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![Instr::StoreMem {
            addr: Operand::Const(0),
            src: Operand::Const(0xAA),
            width: Width::W8,
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

    assert_eq!(
        count_i64_gt_u(&wasm),
        0,
        "expected u8 access to not emit a cross-page check"
    );
}

#[test]
fn tier2_inline_tlb_constant_same_page_store_elides_cross_page_check() {
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![Instr::StoreMem {
            addr: Operand::Const(0),
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

    assert_eq!(
        count_i64_gt_u(&wasm),
        0,
        "expected constant same-page store to not emit a cross-page check"
    );
}

#[test]
fn tier2_inline_tlb_constant_same_page_store_value_address_elides_cross_page_check() {
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![
            Instr::Const {
                dst: ValueId(0),
                value: 0,
            },
            Instr::StoreMem {
                addr: Operand::Value(ValueId(0)),
                src: Operand::Const(0x1122_3344),
                width: Width::W32,
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
        0,
        "expected constant same-page store to not emit a cross-page check"
    );
}

#[test]
fn tier2_inline_tlb_value_address_load_emits_cross_page_check() {
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
                width: Width::W32,
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
        "expected non-constant multi-byte load to emit exactly one cross-page check"
    );
}

#[test]
fn tier2_inline_tlb_value_address_store_emits_cross_page_check() {
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
                src: Operand::Const(0x1122_3344),
                width: Width::W32,
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
        "expected non-constant multi-byte store to emit exactly one cross-page check"
    );
}

#[test]
fn tier2_inline_tlb_constant_same_page_access_elides_cross_page_check() {
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![Instr::LoadMem {
            dst: ValueId(0),
            addr: Operand::Const(0),
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

    assert_eq!(
        count_i64_gt_u(&wasm),
        0,
        "expected constant same-page access to not emit a cross-page check"
    );
}

#[test]
fn tier2_inline_tlb_constant_same_page_u64_load_imports_mmu_translate_and_elides_cross_page_check()
{
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![Instr::LoadMem {
            dst: ValueId(0),
            addr: Operand::Const(0),
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
        "expected same-page u64 load to import env.mmu_translate, got {imports:?}"
    );
    assert_eq!(
        count_i64_gt_u(&wasm),
        0,
        "expected constant same-page u64 load to not emit a cross-page check"
    );
}

#[test]
fn tier2_inline_tlb_constant_same_page_value_address_elides_cross_page_check() {
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![
            Instr::Const {
                dst: ValueId(0),
                value: 0,
            },
            Instr::LoadMem {
                dst: ValueId(1),
                addr: Operand::Value(ValueId(0)),
                width: Width::W32,
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
        0,
        "expected constant same-page access to not emit a cross-page check"
    );
}

#[test]
fn tier2_inline_tlb_constant_same_page_u64_store_imports_mmu_translate_and_elides_cross_page_check()
{
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![Instr::StoreMem {
            addr: Operand::Const(0),
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
        "expected same-page u64 store to import env.mmu_translate, got {imports:?}"
    );
    assert_eq!(
        count_i64_gt_u(&wasm),
        0,
        "expected constant same-page u64 store to not emit a cross-page check"
    );
}

#[test]
fn tier2_inline_tlb_constant_cross_page_access_skips_unreachable_mmu_translate_calls() {
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![
            // Keep inline-TLB enabled by including a same-page access.
            Instr::LoadMem {
                dst: ValueId(0),
                addr: Operand::Const(0),
                width: Width::W8,
            },
            // A constant cross-page load always takes the slow helper path, so it should not emit
            // inline-TLB scaffolding (including calls to `env.mmu_translate`).
            Instr::LoadMem {
                dst: ValueId(1),
                addr: Operand::Const(aero_jit_x86::PAGE_SIZE - 2),
                width: Width::W32,
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
fn tier2_inline_tlb_constant_cross_page_u64_load_skips_unreachable_mmu_translate_calls() {
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![
            // Keep inline-TLB enabled by including a same-page access.
            Instr::LoadMem {
                dst: ValueId(0),
                addr: Operand::Const(0),
                width: Width::W8,
            },
            // A constant cross-page load always takes the slow helper path, so it should not emit
            // inline-TLB scaffolding (including calls to `env.mmu_translate`).
            Instr::LoadMem {
                dst: ValueId(1),
                addr: Operand::Const(aero_jit_x86::PAGE_SIZE - 4),
                width: Width::W64,
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
fn tier2_inline_tlb_constant_cross_page_value_address_skips_unreachable_mmu_translate_calls() {
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![
            // Keep inline-TLB enabled by including a same-page access.
            Instr::LoadMem {
                dst: ValueId(0),
                addr: Operand::Const(0),
                width: Width::W8,
            },
            Instr::Const {
                dst: ValueId(1),
                value: aero_jit_x86::PAGE_SIZE - 2,
            },
            // A constant cross-page load always takes the slow helper path, so it should not emit
            // inline-TLB scaffolding (including calls to `env.mmu_translate`).
            Instr::LoadMem {
                dst: ValueId(2),
                addr: Operand::Value(ValueId(1)),
                width: Width::W32,
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
fn tier2_inline_tlb_constant_cross_page_addr_value_skips_unreachable_mmu_translate_calls() {
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![
            // Keep inline-TLB enabled by including a same-page access.
            Instr::LoadMem {
                dst: ValueId(0),
                addr: Operand::Const(0),
                width: Width::W8,
            },
            Instr::Const {
                dst: ValueId(1),
                value: aero_jit_x86::PAGE_SIZE,
            },
            Instr::Const {
                dst: ValueId(2),
                value: 0,
            },
            Instr::Addr {
                dst: ValueId(3),
                base: Operand::Value(ValueId(1)),
                index: Operand::Value(ValueId(2)),
                scale: 1,
                disp: -2,
            },
            // A constant cross-page load always takes the slow helper path, so it should not emit
            // inline-TLB scaffolding (including calls to `env.mmu_translate`).
            Instr::LoadMem {
                dst: ValueId(4),
                addr: Operand::Value(ValueId(3)),
                width: Width::W32,
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
fn tier2_inline_tlb_constant_cross_page_binop_value_skips_unreachable_mmu_translate_calls() {
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![
            // Keep inline-TLB enabled by including a same-page access.
            Instr::LoadMem {
                dst: ValueId(0),
                addr: Operand::Const(0),
                width: Width::W8,
            },
            Instr::Const {
                dst: ValueId(1),
                value: aero_jit_x86::PAGE_SIZE,
            },
            Instr::Const {
                dst: ValueId(2),
                value: u64::MAX - 1, // -2 (wrapping)
            },
            Instr::BinOp {
                dst: ValueId(3),
                op: aero_jit_x86::tier2::ir::BinOp::Add,
                lhs: Operand::Value(ValueId(1)),
                rhs: Operand::Value(ValueId(2)),
                flags: aero_types::FlagSet::EMPTY,
            },
            // A constant cross-page load always takes the slow helper path, so it should not emit
            // inline-TLB scaffolding (including calls to `env.mmu_translate`).
            Instr::LoadMem {
                dst: ValueId(4),
                addr: Operand::Value(ValueId(3)),
                width: Width::W32,
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
fn tier2_inline_tlb_constant_cross_page_store_skips_unreachable_mmu_translate_calls() {
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![
            // Keep inline-TLB enabled by including a same-page access.
            Instr::LoadMem {
                dst: ValueId(0),
                addr: Operand::Const(0),
                width: Width::W8,
            },
            // A constant cross-page store always takes the slow helper path, so it should not emit
            // inline-TLB scaffolding (including calls to `env.mmu_translate`).
            Instr::StoreMem {
                addr: Operand::Const(aero_jit_x86::PAGE_SIZE - 2),
                src: Operand::Const(0x1122_3344),
                width: Width::W32,
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
fn tier2_inline_tlb_constant_cross_page_u64_store_skips_unreachable_mmu_translate_calls() {
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![
            // Keep inline-TLB enabled by including a same-page access.
            Instr::LoadMem {
                dst: ValueId(0),
                addr: Operand::Const(0),
                width: Width::W8,
            },
            // A constant cross-page store always takes the slow helper path, so it should not emit
            // inline-TLB scaffolding (including calls to `env.mmu_translate`).
            Instr::StoreMem {
                addr: Operand::Const(aero_jit_x86::PAGE_SIZE - 4),
                src: Operand::Const(0x1122_3344_5566_7788),
                width: Width::W64,
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
fn tier2_inline_tlb_constant_cross_page_store_value_address_skips_unreachable_mmu_translate_calls()
{
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![
            // Keep inline-TLB enabled by including a same-page access.
            Instr::LoadMem {
                dst: ValueId(0),
                addr: Operand::Const(0),
                width: Width::W8,
            },
            Instr::Const {
                dst: ValueId(1),
                value: aero_jit_x86::PAGE_SIZE - 2,
            },
            // A constant cross-page store always takes the slow helper path, so it should not emit
            // inline-TLB scaffolding (including calls to `env.mmu_translate`).
            Instr::StoreMem {
                addr: Operand::Value(ValueId(1)),
                src: Operand::Const(0x1122_3344),
                width: Width::W32,
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
fn tier2_inline_tlb_constant_cross_page_store_addr_value_skips_unreachable_mmu_translate_calls() {
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![
            // Keep inline-TLB enabled by including a same-page access.
            Instr::LoadMem {
                dst: ValueId(0),
                addr: Operand::Const(0),
                width: Width::W8,
            },
            Instr::Const {
                dst: ValueId(1),
                value: aero_jit_x86::PAGE_SIZE,
            },
            Instr::Const {
                dst: ValueId(2),
                value: 0,
            },
            Instr::Addr {
                dst: ValueId(3),
                base: Operand::Value(ValueId(1)),
                index: Operand::Value(ValueId(2)),
                scale: 1,
                disp: -2,
            },
            // A constant cross-page store always takes the slow helper path, so it should not emit
            // inline-TLB scaffolding (including calls to `env.mmu_translate`).
            Instr::StoreMem {
                addr: Operand::Value(ValueId(3)),
                src: Operand::Const(0x1122_3344),
                width: Width::W32,
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
fn tier2_inline_tlb_constant_cross_page_store_binop_value_skips_unreachable_mmu_translate_calls() {
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![
            // Keep inline-TLB enabled by including a same-page access.
            Instr::LoadMem {
                dst: ValueId(0),
                addr: Operand::Const(0),
                width: Width::W8,
            },
            Instr::Const {
                dst: ValueId(1),
                value: aero_jit_x86::PAGE_SIZE,
            },
            Instr::Const {
                dst: ValueId(2),
                value: u64::MAX - 1, // -2 (wrapping)
            },
            Instr::BinOp {
                dst: ValueId(3),
                op: aero_jit_x86::tier2::ir::BinOp::Add,
                lhs: Operand::Value(ValueId(1)),
                rhs: Operand::Value(ValueId(2)),
                flags: aero_types::FlagSet::EMPTY,
            },
            // A constant cross-page store always takes the slow helper path, so it should not emit
            // inline-TLB scaffolding (including calls to `env.mmu_translate`).
            Instr::StoreMem {
                addr: Operand::Value(ValueId(3)),
                src: Operand::Const(0x1122_3344),
                width: Width::W32,
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
fn tier2_inline_tlb_skips_code_version_table_locals_when_stores_are_always_cross_page() {
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![
            // Keep inline-TLB enabled by including a same-page access.
            Instr::LoadMem {
                dst: ValueId(0),
                addr: Operand::Const(0),
                width: Width::W8,
            },
            // A constant cross-page store always takes the slow helper path and therefore does not
            // need the code-version table locals.
            Instr::StoreMem {
                addr: Operand::Const(aero_jit_x86::PAGE_SIZE - 2),
                src: Operand::Const(0x1122_3344),
                width: Width::W32,
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
            code_version_guard_import: true,
            ..Default::default()
        },
    );

    assert!(
        !loads_code_version_table_locals(&wasm),
        "expected trace with only cross-page stores to not load code-version table ptr/len locals"
    );
}

#[test]
fn tier2_inline_tlb_skips_code_version_table_locals_when_cross_page_store_uses_value_address() {
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![
            // Keep inline-TLB enabled by including a same-page access.
            Instr::LoadMem {
                dst: ValueId(0),
                addr: Operand::Const(0),
                width: Width::W8,
            },
            Instr::Const {
                dst: ValueId(1),
                value: aero_jit_x86::PAGE_SIZE - 2,
            },
            // A constant cross-page store always takes the slow helper path and therefore does not
            // need the code-version table locals, even when expressed via a value.
            Instr::StoreMem {
                addr: Operand::Value(ValueId(1)),
                src: Operand::Const(0x1122_3344),
                width: Width::W32,
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
            code_version_guard_import: true,
            ..Default::default()
        },
    );

    assert!(
        !loads_code_version_table_locals(&wasm),
        "expected trace with only cross-page stores to not load code-version table ptr/len locals"
    );
}

#[test]
fn tier2_inline_tlb_loads_code_version_table_locals_for_same_page_store_fast_path() {
    // The inline-TLB store fast-path bumps the code-version table; ensure we load the table
    // pointer/length locals when a store might take the fast path.
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![Instr::StoreMem {
            addr: Operand::Const(0),
            src: Operand::Const(0xAA),
            width: Width::W8,
        }],
        kind: TraceKind::Linear,
    };
    let plan = RegAllocPlan::default();
    let wasm = Tier2WasmCodegen::new().compile_trace_with_options(
        &trace,
        &plan,
        Tier2WasmOptions {
            inline_tlb: true,
            code_version_guard_import: true,
            ..Default::default()
        },
    );
    assert!(
        loads_code_version_table_locals(&wasm),
        "expected same-page store trace to load code-version table ptr/len locals"
    );
}

#[test]
fn tier2_inline_tlb_skips_code_version_table_locals_for_load_only_traces() {
    // Load fast paths do not bump the code-version table; ensure we don't load the table ptr/len
    // locals for traces that only perform loads.
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![Instr::LoadMem {
            dst: ValueId(0),
            addr: Operand::Const(0),
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
            code_version_guard_import: true,
            ..Default::default()
        },
    );
    assert!(
        !loads_code_version_table_locals(&wasm),
        "expected load-only trace to not load code-version table ptr/len locals"
    );
}
