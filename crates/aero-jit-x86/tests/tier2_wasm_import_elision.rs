use aero_jit_x86::tier2::ir::{Instr, Operand, TraceIr, TraceKind, ValueId};
use aero_jit_x86::tier2::opt::RegAllocPlan;
use aero_jit_x86::tier2::wasm_codegen::{Tier2WasmCodegen, Tier2WasmOptions};
use aero_jit_x86::wasm::{
    IMPORT_MEM_READ_U32, IMPORT_MEM_WRITE_U8, IMPORT_MMU_TRANSLATE, IMPORT_MODULE,
};
use aero_types::Width;
use wasmparser::{Parser, Payload, TypeRef};

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

