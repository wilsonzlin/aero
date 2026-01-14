#![cfg(not(target_arch = "wasm32"))]

use aero_jit_x86::tier1::ir::{IrBuilder, IrTerminator};
use aero_jit_x86::tier1::{Tier1WasmCodegen, Tier1WasmOptions, EXPORT_BLOCK_FN};
use aero_jit_x86::tier2::ir::{Instr, TraceIr, TraceKind};
use aero_jit_x86::tier2::opt::RegAllocPlan;
use aero_jit_x86::tier2::{Tier2WasmCodegen, Tier2WasmOptions};
use aero_jit_x86::wasm::IMPORT_MEMORY;
use aero_jit_x86::wasm::IMPORT_MODULE;
use wasmtime::{Config, Engine, Linker, MemoryType, Module, SharedMemory, Store, TypedFunc};

fn shared_engine() -> Engine {
    let mut config = Config::new();
    config.wasm_threads(true);
    config.shared_memory(true);
    Engine::new(&config).expect("create wasmtime engine with wasm threads enabled")
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
