use aero_jit_x86::tier1::ir::{IrBuilder, IrTerminator};
use aero_jit_x86::tier1::{Tier1WasmCodegen, Tier1WasmOptions};
use aero_jit_x86::wasm::{IMPORT_MEMORY, IMPORT_MODULE, WASM32_MAX_PAGES};

#[test]
fn tier1_wasm_codegen_can_import_shared_memory_with_maximum() {
    let b = IrBuilder::new(0x1000);
    let ir = b.finish(IrTerminator::Jump { target: 0x2000 });
    ir.validate().unwrap();

    let wasm = Tier1WasmCodegen::new().compile_block_with_options(
        &ir,
        Tier1WasmOptions {
            memory_shared: true,
            ..Default::default()
        },
    );

    wasmparser::Validator::new()
        .validate_all(&wasm)
        .expect("generated WASM should validate");

    let mut found_mem_import = false;
    for payload in wasmparser::Parser::new(0).parse_all(&wasm) {
        if let wasmparser::Payload::ImportSection(imports) = payload.expect("parse wasm section") {
            for group in imports {
                let group = group.expect("parse import group");
                for import in group {
                    let (_idx, import) = import.expect("parse import");
                    if import.module == IMPORT_MODULE && import.name == IMPORT_MEMORY {
                        let wasmparser::TypeRef::Memory(ty) = import.ty else {
                            panic!("env.memory import was not a memory");
                        };
                        assert!(
                            ty.shared,
                            "expected generated env.memory import to be shared"
                        );
                        assert_eq!(
                            ty.maximum,
                            Some(u64::from(WASM32_MAX_PAGES)),
                            "expected shared env.memory import to default to a 4GiB maximum"
                        );
                        found_mem_import = true;
                    }
                }
            }
        }
    }

    assert!(found_mem_import, "did not find env.memory import");
}
