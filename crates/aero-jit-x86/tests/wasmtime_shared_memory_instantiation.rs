#![cfg(not(target_arch = "wasm32"))]

use aero_jit_x86::tier1::ir::{IrBuilder, IrTerminator};
use aero_jit_x86::tier1::{Tier1WasmCodegen, Tier1WasmOptions, EXPORT_BLOCK_FN};
use aero_jit_x86::tier2::ir::{Instr, TraceIr, TraceKind};
use aero_jit_x86::tier2::opt::RegAllocPlan;
use aero_jit_x86::tier2::{Tier2WasmCodegen, Tier2WasmOptions};
#[cfg(feature = "tier1-inline-tlb")]
use aero_types::Width;
use aero_jit_x86::wasm::IMPORT_MEMORY;
use aero_jit_x86::wasm::{
    IMPORT_JIT_EXIT, IMPORT_JIT_EXIT_MMIO, IMPORT_MEM_READ_U16, IMPORT_MEM_READ_U32,
    IMPORT_MEM_READ_U64, IMPORT_MEM_READ_U8, IMPORT_MEM_WRITE_U16, IMPORT_MEM_WRITE_U32,
    IMPORT_MEM_WRITE_U64, IMPORT_MEM_WRITE_U8, IMPORT_MMU_TRANSLATE, IMPORT_MODULE,
    IMPORT_PAGE_FAULT,
};
use wasmtime::{
    Caller, Config, Engine, Linker, MemoryType, Module, SharedMemory, Store, TypedFunc,
};

fn shared_engine() -> Engine {
    let mut config = Config::new();
    config.wasm_threads(true);
    config.shared_memory(true);
    Engine::new(&config).expect("create wasmtime engine with wasm threads enabled")
}

fn define_tier1_stub_imports(linker: &mut Linker<()>) {
    // Tier-1 modules always import the slow-path memory helpers even if the block does not use
    // them. Define minimal no-op stubs for instantiation.
    linker
        .func_wrap(
            IMPORT_MODULE,
            IMPORT_MEM_READ_U8,
            |_caller: Caller<'_, ()>, _cpu_ptr: i32, _addr: i64| -> i32 { 0 },
        )
        .unwrap();
    linker
        .func_wrap(
            IMPORT_MODULE,
            IMPORT_MEM_READ_U16,
            |_caller: Caller<'_, ()>, _cpu_ptr: i32, _addr: i64| -> i32 { 0 },
        )
        .unwrap();
    linker
        .func_wrap(
            IMPORT_MODULE,
            IMPORT_MEM_READ_U32,
            |_caller: Caller<'_, ()>, _cpu_ptr: i32, _addr: i64| -> i32 { 0 },
        )
        .unwrap();
    linker
        .func_wrap(
            IMPORT_MODULE,
            IMPORT_MEM_READ_U64,
            |_caller: Caller<'_, ()>, _cpu_ptr: i32, _addr: i64| -> i64 { 0 },
        )
        .unwrap();
    linker
        .func_wrap(
            IMPORT_MODULE,
            IMPORT_MEM_WRITE_U8,
            |_caller: Caller<'_, ()>, _cpu_ptr: i32, _addr: i64, _value: i32| {},
        )
        .unwrap();
    linker
        .func_wrap(
            IMPORT_MODULE,
            IMPORT_MEM_WRITE_U16,
            |_caller: Caller<'_, ()>, _cpu_ptr: i32, _addr: i64, _value: i32| {},
        )
        .unwrap();
    linker
        .func_wrap(
            IMPORT_MODULE,
            IMPORT_MEM_WRITE_U32,
            |_caller: Caller<'_, ()>, _cpu_ptr: i32, _addr: i64, _value: i32| {},
        )
        .unwrap();
    linker
        .func_wrap(
            IMPORT_MODULE,
            IMPORT_MEM_WRITE_U64,
            |_caller: Caller<'_, ()>, _cpu_ptr: i32, _addr: i64, _value: i64| {},
        )
        .unwrap();

    linker
        .func_wrap(
            IMPORT_MODULE,
            IMPORT_PAGE_FAULT,
            |_caller: Caller<'_, ()>, _cpu_ptr: i32, _addr: i64| -> i64 {
                aero_jit_x86::wasm::JIT_EXIT_SENTINEL_I64
            },
        )
        .unwrap();
    linker
        .func_wrap(
            IMPORT_MODULE,
            IMPORT_JIT_EXIT,
            |_caller: Caller<'_, ()>, _kind: i32, rip: i64| -> i64 { rip },
        )
        .unwrap();

    // Inline-TLB imports (only required when `Tier1WasmOptions.inline_tlb = true`).
    linker
        .func_wrap(
            IMPORT_MODULE,
            IMPORT_MMU_TRANSLATE,
            |_caller: Caller<'_, ()>, _cpu_ptr: i32, _jit_ctx_ptr: i32, _vaddr: i64, _access: i32| {
                0i64
            },
        )
        .unwrap();
    linker
        .func_wrap(
            IMPORT_MODULE,
            IMPORT_JIT_EXIT_MMIO,
            |_caller: Caller<'_, ()>,
             _kind: i32,
             _vaddr: i64,
             _size: i32,
             _is_write: i32,
             _value_low: i64,
             _value_high: i64| { aero_jit_x86::wasm::JIT_EXIT_SENTINEL_I64 },
        )
        .unwrap();
}

#[test]
fn tier1_module_instantiates_with_shared_imported_memory_in_wasmtime() {
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

    let engine = shared_engine();
    let module = Module::new(&engine, &wasm).expect("compile Tier-1 wasm module");

    let mut store = Store::new(&engine, ());
    let mut linker = Linker::new(&engine);

    let memory =
        SharedMemory::new(&engine, MemoryType::shared(1, 2)).expect("create shared memory");
    linker
        .define(&mut store, IMPORT_MODULE, IMPORT_MEMORY, memory)
        .expect("define env.memory import");
    define_tier1_stub_imports(&mut linker);

    let instance = linker
        .instantiate(&mut store, &module)
        .expect("instantiate Tier-1 module");
    let block: TypedFunc<(i32, i32), i64> = instance
        .get_typed_func(&mut store, EXPORT_BLOCK_FN)
        .expect("get exported Tier-1 block function");

    let ret = block.call(&mut store, (0, 0)).expect("call Tier-1 block");
    assert_eq!(ret as u64, 0x2000);
}

#[test]
fn tier2_module_instantiates_with_shared_imported_memory_in_wasmtime() {
    let trace = TraceIr {
        prologue: vec![],
        body: vec![Instr::SideExit { exit_rip: 0x3000 }],
        kind: TraceKind::Linear,
    };
    let plan = RegAllocPlan::default();

    let wasm = Tier2WasmCodegen::new().compile_trace_with_options(
        &trace,
        &plan,
        Tier2WasmOptions {
            memory_shared: true,
            ..Default::default()
        },
    );

    let engine = shared_engine();
    let module = Module::new(&engine, &wasm).expect("compile Tier-2 wasm module");

    let mut store = Store::new(&engine, ());
    let mut linker = Linker::new(&engine);

    // Define a small shared memory. The generated module declares max=65536 when shared, so any
    // smaller host memory max should be compatible.
    let memory =
        SharedMemory::new(&engine, MemoryType::shared(1, 2)).expect("create shared memory");
    linker
        .define(&mut store, IMPORT_MODULE, IMPORT_MEMORY, memory)
        .expect("define env.memory import");

    let instance = linker
        .instantiate(&mut store, &module)
        .expect("instantiate Tier-2 module");
    let trace_fn: TypedFunc<(i32, i32), i64> = instance
        .get_typed_func(
            &mut store,
            aero_jit_x86::tier2::wasm_codegen::EXPORT_TRACE_FN,
        )
        .expect("get exported Tier-2 trace function");

    let ret = trace_fn
        .call(&mut store, (0, 0))
        .expect("call Tier-2 trace");
    assert_eq!(ret as u64, 0x3000);
}

#[cfg(feature = "tier1-inline-tlb")]
#[test]
fn tier1_inline_tlb_store_module_with_shared_memory_and_atomics_instantiates_in_wasmtime() {
    let mut b = IrBuilder::new(0x1000);
    // Pick an address that will always take the cross-page slow path for an 8-byte store, to avoid
    // needing to set up a valid inline-TLB entry. (The atomic bump code is still present in the
    // module; it just won't execute on this input.)
    let addr = b.const_int(Width::W64, 0xfff);
    let value = b.const_int(Width::W64, 0x1234);
    b.store(Width::W64, addr, value);
    let ir = b.finish(IrTerminator::Jump { target: 0x2000 });
    ir.validate().unwrap();

    let wasm = Tier1WasmCodegen::new().compile_block_with_options(
        &ir,
        Tier1WasmOptions {
            inline_tlb: true,
            inline_tlb_stores: true,
            memory_shared: true,
            ..Default::default()
        },
    );

    let engine = shared_engine();
    let module = Module::new(&engine, &wasm).expect("compile Tier-1 wasm module");

    let mut store = Store::new(&engine, ());
    let mut linker = Linker::new(&engine);

    let memory =
        SharedMemory::new(&engine, MemoryType::shared(1, 2)).expect("create shared memory");
    linker
        .define(&mut store, IMPORT_MODULE, IMPORT_MEMORY, memory)
        .expect("define env.memory import");
    define_tier1_stub_imports(&mut linker);

    let instance = linker
        .instantiate(&mut store, &module)
        .expect("instantiate Tier-1 module");
    let block: TypedFunc<(i32, i32), i64> = instance
        .get_typed_func(&mut store, EXPORT_BLOCK_FN)
        .expect("get exported Tier-1 block function");

    let ret = block.call(&mut store, (0, 0)).expect("call Tier-1 block");
    assert_eq!(ret as u64, 0x2000);
}

#[test]
fn tier2_inline_code_version_guard_table_module_with_shared_memory_and_atomics_instantiates_in_wasmtime(
) {
    let trace = TraceIr {
        prologue: vec![],
        body: vec![
            Instr::GuardCodeVersion {
                page: 0,
                expected: 0,
                exit_rip: 0x3000,
            },
            Instr::SideExit { exit_rip: 0x4000 },
        ],
        kind: TraceKind::Linear,
    };
    let plan = RegAllocPlan::default();

    let wasm = Tier2WasmCodegen::new().compile_trace_with_options(
        &trace,
        &plan,
        Tier2WasmOptions {
            memory_shared: true,
            code_version_guard_import: false,
            ..Default::default()
        },
    );

    let engine = shared_engine();
    let module = Module::new(&engine, &wasm).expect("compile Tier-2 wasm module");

    let mut store = Store::new(&engine, ());
    let mut linker = Linker::new(&engine);

    let memory =
        SharedMemory::new(&engine, MemoryType::shared(1, 2)).expect("create shared memory");
    linker
        .define(&mut store, IMPORT_MODULE, IMPORT_MEMORY, memory)
        .expect("define env.memory import");

    let instance = linker
        .instantiate(&mut store, &module)
        .expect("instantiate Tier-2 module");
    let trace_fn: TypedFunc<(i32, i32), i64> = instance
        .get_typed_func(&mut store, aero_jit_x86::tier2::wasm_codegen::EXPORT_TRACE_FN)
        .expect("get exported Tier-2 trace function");

    let ret = trace_fn
        .call(&mut store, (0, 0))
        .expect("call Tier-2 trace");
    assert_eq!(ret as u64, 0x4000);
}
