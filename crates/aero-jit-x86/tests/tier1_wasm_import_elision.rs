use aero_jit_x86::tier1::ir::{IrBuilder, IrTerminator};
use aero_jit_x86::tier1::Tier1WasmCodegen;
use aero_jit_x86::wasm::{
    IMPORT_MEM_READ_U8, IMPORT_MEM_WRITE_U16, IMPORT_MEM_WRITE_U32, IMPORT_MEM_WRITE_U64,
    IMPORT_MEM_WRITE_U8, IMPORT_MEMORY, IMPORT_MODULE,
};
use aero_types::Width;
use wasmparser::{Parser, Payload, TypeRef};

fn import_entries(wasm: &[u8]) -> Vec<(String, String, TypeRef)> {
    let mut out = Vec::new();
    for payload in Parser::new(0).parse_all(wasm) {
        if let Payload::ImportSection(imports) = payload.expect("parse wasm") {
            for group in imports {
                let group = group.expect("parse import group");
                for import in group {
                    let (_offset, import) = import.expect("parse import");
                    out.push((import.module.to_string(), import.name.to_string(), import.ty));
                }
            }
        }
    }
    out
}

fn type_count(wasm: &[u8]) -> u32 {
    for payload in Parser::new(0).parse_all(wasm) {
        if let Payload::TypeSection(types) = payload.expect("parse wasm") {
            let mut count = 0u32;
            for ty in types.into_iter_err_on_gc_types() {
                ty.expect("parse type");
                count += 1;
            }
            return count;
        }
    }
    panic!("type section not found");
}

#[test]
fn tier1_block_without_mem_ops_imports_only_memory() {
    let b = IrBuilder::new(0x1000);
    let ir = b.finish(IrTerminator::Jump { target: 0x2000 });
    ir.validate().unwrap();

    let wasm = Tier1WasmCodegen::new().compile_block(&ir);
    let imports = import_entries(&wasm);

    assert_eq!(
        imports.len(),
        1,
        "expected Tier-1 block without mem ops to only import env.memory, got {imports:?}"
    );
    let (module, name, ty) = &imports[0];
    assert_eq!(module, IMPORT_MODULE);
    assert_eq!(name, IMPORT_MEMORY);
    assert!(
        matches!(ty, TypeRef::Memory(_)),
        "expected env.memory import to be a memory, got {ty:?}"
    );

    assert_eq!(
        type_count(&wasm),
        1,
        "expected Tier-1 block without helper imports to only define one function type (the block signature)"
    );
}

#[test]
fn tier1_block_with_load_imports_mem_read_helpers() {
    let mut b = IrBuilder::new(0x1000);
    let addr = b.const_int(Width::W64, 0);
    let _value = b.load(Width::W8, addr);
    let ir = b.finish(IrTerminator::Jump { target: 0x2000 });
    ir.validate().unwrap();

    let wasm = Tier1WasmCodegen::new().compile_block(&ir);
    let imports = import_entries(&wasm);

    let mut found_mem_read_u8 = false;
    for (module, name, _ty) in imports {
        if module == IMPORT_MODULE && name == IMPORT_MEM_READ_U8 {
            found_mem_read_u8 = true;
        }
        // Sanity: a pure load block shouldn't need any write helpers.
        assert_ne!(name, IMPORT_MEM_WRITE_U8);
        assert_ne!(name, IMPORT_MEM_WRITE_U16);
        assert_ne!(name, IMPORT_MEM_WRITE_U32);
        assert_ne!(name, IMPORT_MEM_WRITE_U64);
    }

    assert!(
        found_mem_read_u8,
        "expected Tier-1 block with a load to import env.mem_read_u8"
    );

    assert_eq!(
        type_count(&wasm),
        2,
        "expected Tier-1 block with a single load to define only the mem_read_u8 and block function types"
    );
}

#[test]
fn tier1_block_with_store_imports_mem_write_helpers() {
    let mut b = IrBuilder::new(0x1000);
    let addr = b.const_int(Width::W64, 0);
    let src = b.const_int(Width::W8, 0x12);
    b.store(Width::W8, addr, src);
    let ir = b.finish(IrTerminator::Jump { target: 0x2000 });
    ir.validate().unwrap();

    let wasm = Tier1WasmCodegen::new().compile_block(&ir);
    let imports = import_entries(&wasm);

    let mut found_mem_write_u8 = false;
    for (module, name, _ty) in imports {
        if module == IMPORT_MODULE && name == IMPORT_MEM_WRITE_U8 {
            found_mem_write_u8 = true;
        }
        // Sanity: a pure store block shouldn't need any read helpers.
        assert_ne!(name, IMPORT_MEM_READ_U8);
    }

    assert!(
        found_mem_write_u8,
        "expected Tier-1 block with a store to import env.mem_write_u8"
    );

    assert_eq!(
        type_count(&wasm),
        2,
        "expected Tier-1 block with a single store to define only the mem_write_u8 and block function types"
    );
}
