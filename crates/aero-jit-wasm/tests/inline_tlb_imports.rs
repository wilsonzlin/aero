use aero_jit_x86::tier1::ir::{IrBuilder, IrTerminator};
use aero_jit_x86::tier1::{Tier1WasmCodegen, Tier1WasmOptions};
use aero_jit_x86::wasm::{IMPORT_JIT_EXIT_MMIO, IMPORT_MMU_TRANSLATE, IMPORT_MODULE};
use aero_types::Width;
use wasmparser::{Parser, Payload};

fn imports(bytes: &[u8]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for payload in Parser::new(0).parse_all(bytes) {
        if let Payload::ImportSection(reader) = payload.expect("valid wasm") {
            for group in reader {
                let group = group.expect("valid import group");
                for import in group {
                    let (_idx, import) = import.expect("valid import");
                    out.push((import.module.to_string(), import.name.to_string()));
                }
            }
        }
    }
    out
}

#[test]
fn inline_tlb_controls_import_set() {
    // Ensure the block has a memory op; Tier-1 codegen intentionally disables inline-TLB for
    // blocks with no loads/stores.
    let mut b = IrBuilder::new(0);
    let addr = b.const_int(Width::W64, 0);
    let value = b.const_int(Width::W32, 0x1234_5678);
    b.store(Width::W32, addr, value);
    let block = b.finish(IrTerminator::ExitToInterpreter { next_rip: 0 });

    let inline_opts = Tier1WasmOptions {
        inline_tlb: true,
        ..Default::default()
    };
    let wasm_inline = Tier1WasmCodegen::new().compile_block_with_options(&block, inline_opts);

    let baseline_opts = Tier1WasmOptions {
        inline_tlb: false,
        ..Default::default()
    };
    let wasm_baseline = Tier1WasmCodegen::new().compile_block_with_options(&block, baseline_opts);

    let inline_imports = imports(&wasm_inline);
    assert!(
        inline_imports.contains(&(IMPORT_MODULE.to_string(), IMPORT_MMU_TRANSLATE.to_string())),
        "expected env.{IMPORT_MMU_TRANSLATE} import when inline_tlb=true; got {inline_imports:?}"
    );
    assert!(
        inline_imports.contains(&(IMPORT_MODULE.to_string(), IMPORT_JIT_EXIT_MMIO.to_string())),
        "expected env.{IMPORT_JIT_EXIT_MMIO} import when inline_tlb=true; got {inline_imports:?}"
    );

    let baseline_imports = imports(&wasm_baseline);
    assert!(
        !baseline_imports.contains(&(IMPORT_MODULE.to_string(), IMPORT_MMU_TRANSLATE.to_string())),
        "did not expect env.{IMPORT_MMU_TRANSLATE} import when inline_tlb=false; got {baseline_imports:?}"
    );
    assert!(
        !baseline_imports.contains(&(IMPORT_MODULE.to_string(), IMPORT_JIT_EXIT_MMIO.to_string())),
        "did not expect env.{IMPORT_JIT_EXIT_MMIO} import when inline_tlb=false; got {baseline_imports:?}"
    );
}
