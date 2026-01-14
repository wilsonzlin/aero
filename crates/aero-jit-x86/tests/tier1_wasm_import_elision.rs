use aero_jit_x86::tier1::ir::{IrBuilder, IrTerminator};
use aero_jit_x86::tier1::Tier1WasmCodegen;
#[cfg(feature = "tier1-inline-tlb")]
use aero_jit_x86::tier1::Tier1WasmOptions;
use aero_jit_x86::wasm::{
    IMPORT_JIT_EXIT, IMPORT_MEMORY, IMPORT_MEM_READ_U16, IMPORT_MEM_READ_U64, IMPORT_MEM_READ_U8,
    IMPORT_MEM_WRITE_U16, IMPORT_MEM_WRITE_U32, IMPORT_MEM_WRITE_U64, IMPORT_MEM_WRITE_U8,
    IMPORT_MODULE,
};
#[cfg(feature = "tier1-inline-tlb")]
use aero_jit_x86::wasm::{IMPORT_JIT_EXIT_MMIO, IMPORT_MEM_READ_U32, IMPORT_MMU_TRANSLATE};
use aero_types::Width;
use wasmparser::{Parser, Payload, TypeRef, ValType};

fn import_entries(wasm: &[u8]) -> Vec<(String, String, TypeRef)> {
    let mut out = Vec::new();
    for payload in Parser::new(0).parse_all(wasm) {
        if let Payload::ImportSection(imports) = payload.expect("parse wasm") {
            for group in imports {
                let group = group.expect("parse import group");
                for import in group {
                    let (_offset, import) = import.expect("parse import");
                    out.push((
                        import.module.to_string(),
                        import.name.to_string(),
                        import.ty,
                    ));
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

fn func_types(wasm: &[u8]) -> Vec<(Vec<ValType>, Vec<ValType>)> {
    for payload in Parser::new(0).parse_all(wasm) {
        if let Payload::TypeSection(types) = payload.expect("parse wasm") {
            let mut out = Vec::new();
            for ty in types.into_iter_err_on_gc_types() {
                let ty = ty.expect("parse type");
                out.push((ty.params().to_vec(), ty.results().to_vec()));
            }
            return out;
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
fn tier1_block_with_multiple_load_widths_reuses_mem_read_type() {
    let mut b = IrBuilder::new(0x1000);
    let addr = b.const_int(Width::W64, 0);
    let _v0 = b.load(Width::W8, addr);
    let _v1 = b.load(Width::W16, addr);
    let ir = b.finish(IrTerminator::Jump { target: 0x2000 });
    ir.validate().unwrap();

    let wasm = Tier1WasmCodegen::new().compile_block(&ir);
    let imports = import_entries(&wasm);

    let mut found_u8: Option<u32> = None;
    let mut found_u16: Option<u32> = None;
    for (module, name, ty) in imports {
        if module == IMPORT_MODULE && name == IMPORT_MEM_READ_U8 {
            let TypeRef::Func(idx) = ty else {
                panic!("expected env.mem_read_u8 import to be a function, got {ty:?}");
            };
            found_u8 = Some(idx);
        }
        if module == IMPORT_MODULE && name == IMPORT_MEM_READ_U16 {
            let TypeRef::Func(idx) = ty else {
                panic!("expected env.mem_read_u16 import to be a function, got {ty:?}");
            };
            found_u16 = Some(idx);
        }
        // Sanity: a pure load block shouldn't need any write helpers.
        assert_ne!(name, IMPORT_MEM_WRITE_U8);
        assert_ne!(name, IMPORT_MEM_WRITE_U16);
        assert_ne!(name, IMPORT_MEM_WRITE_U32);
        assert_ne!(name, IMPORT_MEM_WRITE_U64);
    }

    let ty_u8 = found_u8.expect("expected env.mem_read_u8 import");
    let ty_u16 = found_u16.expect("expected env.mem_read_u16 import");
    assert_eq!(
        ty_u8, ty_u16,
        "expected env.mem_read_u8 and env.mem_read_u16 to reference the same type index"
    );
    let tys = func_types(&wasm);
    assert_eq!(
        tys[ty_u8 as usize],
        (vec![ValType::I32, ValType::I64], vec![ValType::I32]),
        "expected shared mem_read type to have signature (i32, i64) -> i32"
    );

    assert_eq!(
        type_count(&wasm),
        2,
        "expected mem_read_u8/u16 to reuse a single (i32,i64)->i32 type plus the block signature"
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

#[test]
fn tier1_block_with_multiple_store_widths_reuses_mem_write_type() {
    let mut b = IrBuilder::new(0x1000);
    let addr = b.const_int(Width::W64, 0);
    let src8 = b.const_int(Width::W8, 0x12);
    b.store(Width::W8, addr, src8);
    let src16 = b.const_int(Width::W16, 0x1234);
    b.store(Width::W16, addr, src16);
    let ir = b.finish(IrTerminator::Jump { target: 0x2000 });
    ir.validate().unwrap();

    let wasm = Tier1WasmCodegen::new().compile_block(&ir);
    let imports = import_entries(&wasm);

    let mut found_u8: Option<u32> = None;
    let mut found_u16: Option<u32> = None;
    for (module, name, ty) in imports {
        if module == IMPORT_MODULE && name == IMPORT_MEM_WRITE_U8 {
            let TypeRef::Func(idx) = ty else {
                panic!("expected env.mem_write_u8 import to be a function, got {ty:?}");
            };
            found_u8 = Some(idx);
        }
        if module == IMPORT_MODULE && name == IMPORT_MEM_WRITE_U16 {
            let TypeRef::Func(idx) = ty else {
                panic!("expected env.mem_write_u16 import to be a function, got {ty:?}");
            };
            found_u16 = Some(idx);
        }
        // Sanity: a pure store block shouldn't need any read helpers.
        assert_ne!(name, IMPORT_MEM_READ_U8);
        assert_ne!(name, IMPORT_MEM_READ_U16);
        assert_ne!(name, IMPORT_MEM_READ_U64);
    }

    let ty_u8 = found_u8.expect("expected env.mem_write_u8 import");
    let ty_u16 = found_u16.expect("expected env.mem_write_u16 import");
    assert_eq!(
        ty_u8, ty_u16,
        "expected env.mem_write_u8 and env.mem_write_u16 to reference the same type index"
    );
    let tys = func_types(&wasm);
    assert_eq!(
        tys[ty_u8 as usize],
        (vec![ValType::I32, ValType::I64, ValType::I32], Vec::new()),
        "expected shared mem_write type to have signature (i32, i64, i32) -> ()"
    );

    assert_eq!(
        type_count(&wasm),
        2,
        "expected mem_write_u8/u16 to reuse a single (i32,i64,i32)->() type plus the block signature"
    );
}

#[test]
fn tier1_block_with_call_helper_imports_jit_exit() {
    let mut b = IrBuilder::new(0x1000);
    b.call_helper("dummy", Vec::new(), None);
    let ir = b.finish(IrTerminator::Jump { target: 0x2000 });
    ir.validate().unwrap();

    let wasm = Tier1WasmCodegen::new().compile_block(&ir);
    let imports = import_entries(&wasm);

    assert_eq!(
        imports.len(),
        2,
        "expected Tier-1 block with CallHelper to only import env.memory + env.jit_exit, got {imports:?}"
    );

    let mut found_jit_exit = false;
    for (module, name, _ty) in imports {
        if module == IMPORT_MODULE && name == IMPORT_JIT_EXIT {
            found_jit_exit = true;
        }
        assert_ne!(name, IMPORT_MEM_READ_U8);
        assert_ne!(name, IMPORT_MEM_WRITE_U8);
    }

    assert!(
        found_jit_exit,
        "expected env.jit_exit import for CallHelper block"
    );
    assert_eq!(
        type_count(&wasm),
        2,
        "expected Tier-1 CallHelper-only block to define only the jit_exit and block function types"
    );
}

#[test]
fn tier1_block_with_u64_load_and_call_helper_reuses_i64_return_type() {
    let mut b = IrBuilder::new(0x1000);
    let addr = b.const_int(Width::W64, 0);
    let _value = b.load(Width::W64, addr);
    b.call_helper("dummy", Vec::new(), None);
    let ir = b.finish(IrTerminator::Jump { target: 0x2000 });
    ir.validate().unwrap();

    let wasm = Tier1WasmCodegen::new().compile_block(&ir);
    let imports = import_entries(&wasm);

    let mut found_mem_read_u64: Option<u32> = None;
    let mut found_jit_exit: Option<u32> = None;
    for (module, name, ty) in imports {
        if module == IMPORT_MODULE && name == IMPORT_MEM_READ_U64 {
            let TypeRef::Func(idx) = ty else {
                panic!("expected env.mem_read_u64 import to be a function, got {ty:?}");
            };
            found_mem_read_u64 = Some(idx);
        }
        if module == IMPORT_MODULE && name == IMPORT_JIT_EXIT {
            let TypeRef::Func(idx) = ty else {
                panic!("expected env.jit_exit import to be a function, got {ty:?}");
            };
            found_jit_exit = Some(idx);
        }
        // Sanity: this block shouldn't need any write helpers.
        assert_ne!(name, IMPORT_MEM_WRITE_U8);
        assert_ne!(name, IMPORT_MEM_WRITE_U16);
        assert_ne!(name, IMPORT_MEM_WRITE_U32);
        assert_ne!(name, IMPORT_MEM_WRITE_U64);
    }

    let ty_mem_read_u64 = found_mem_read_u64.expect("expected env.mem_read_u64 import");
    let ty_jit_exit = found_jit_exit.expect("expected env.jit_exit import");
    assert_eq!(
        ty_mem_read_u64, ty_jit_exit,
        "expected env.mem_read_u64 and env.jit_exit to reference the same type index"
    );
    let tys = func_types(&wasm);
    assert_eq!(
        tys[ty_mem_read_u64 as usize],
        (vec![ValType::I32, ValType::I64], vec![ValType::I64]),
        "expected shared (mem_read_u64/jit_exit) type to have signature (i32, i64) -> i64"
    );

    assert_eq!(
        type_count(&wasm),
        2,
        "expected mem_read_u64 and jit_exit to share the (i32,i64)->i64 type plus the block signature"
    );
}

#[cfg(feature = "tier1-inline-tlb")]
#[test]
fn tier1_inline_tlb_mmio_fallback_does_not_import_jit_exit_mmio() {
    let mut b = IrBuilder::new(0x1000);
    let addr = b.const_int(Width::W64, 0);
    let _value = b.load(Width::W32, addr);
    let ir = b.finish(IrTerminator::Jump { target: 0x2000 });
    ir.validate().unwrap();

    let wasm = Tier1WasmCodegen::new().compile_block_with_options(
        &ir,
        Tier1WasmOptions {
            inline_tlb: true,
            inline_tlb_mmio_exit: false,
            ..Default::default()
        },
    );
    let imports = import_entries(&wasm);

    let mut found_translate = false;
    let mut found_mem_read_u32 = false;
    for (module, name, _ty) in &imports {
        if module == IMPORT_MODULE && name == IMPORT_MMU_TRANSLATE {
            found_translate = true;
        }
        if module == IMPORT_MODULE && name == IMPORT_MEM_READ_U32 {
            found_mem_read_u32 = true;
        }
        assert_ne!(
            name, IMPORT_JIT_EXIT_MMIO,
            "expected Tier-1 block with inline_tlb_mmio_exit=false to not import env.jit_exit_mmio, got {imports:?}"
        );
    }

    assert!(
        found_translate,
        "expected env.mmu_translate import when inline_tlb=true"
    );
    assert!(
        found_mem_read_u32,
        "expected env.mem_read_u32 import for MMIO fallback slow path"
    );
}
