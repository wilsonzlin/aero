#![cfg(not(target_arch = "wasm32"))]

use aero_jit_x86::tier1::ir::{IrBuilder, IrTerminator};
use aero_jit_x86::tier1::{Tier1WasmCodegen, Tier1WasmOptions, EXPORT_BLOCK_FN};
use aero_jit_x86::{abi, jit_ctx};
use aero_jit_x86::tier2::ir::{Instr, TraceIr, TraceKind};
use aero_jit_x86::tier2::opt::RegAllocPlan;
use aero_jit_x86::tier2::{Tier2WasmCodegen, Tier2WasmOptions};
use aero_jit_x86::tier2::wasm_codegen::EXPORT_TRACE_FN;
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

fn write_bytes(mem: &SharedMemory, addr: usize, src: &[u8]) {
    let data = mem.data();
    assert!(
        addr.checked_add(src.len()).is_some_and(|end| end <= data.len()),
        "write_bytes out of bounds: addr={addr} len={} mem_len={}",
        src.len(),
        data.len()
    );
    for (i, b) in src.iter().copied().enumerate() {
        unsafe {
            *data[addr + i].get() = b;
        }
    }
}

fn read_bytes(mem: &SharedMemory, addr: usize, dst: &mut [u8]) {
    let data = mem.data();
    assert!(
        addr.checked_add(dst.len()).is_some_and(|end| end <= data.len()),
        "read_bytes out of bounds: addr={addr} len={} mem_len={}",
        dst.len(),
        data.len()
    );
    for (i, out) in dst.iter_mut().enumerate() {
        unsafe {
            *out = *data[addr + i].get();
        }
    }
}

fn write_u32(mem: &SharedMemory, addr: usize, value: u32) {
    write_bytes(mem, addr, &value.to_le_bytes());
}

fn write_u64(mem: &SharedMemory, addr: usize, value: u64) {
    write_bytes(mem, addr, &value.to_le_bytes());
}

fn read_u32(mem: &SharedMemory, addr: usize) -> u32 {
    let mut buf = [0u8; 4];
    read_bytes(mem, addr, &mut buf);
    u32::from_le_bytes(buf)
}

#[test]
fn tier2_inline_code_version_guards_work_with_shared_memory_in_wasmtime() {
    // Execute a Tier-2 trace using inline code-version table reads (`code_version_guard_import =
    // false`) with `memory_shared = true`. This forces the guard to use atomic loads, which
    // requires Wasmtime's wasm-threads/shared-memory support to be enabled.
    let trace = TraceIr {
        prologue: vec![Instr::GuardCodeVersion {
            page: 0,
            expected: 1,
            exit_rip: 0x9999,
        }],
        body: vec![],
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
        SharedMemory::new(&engine, MemoryType::shared(1, 1)).expect("create shared memory");
    linker
        .define(&mut store, IMPORT_MODULE, IMPORT_MEMORY, memory.clone())
        .expect("define env.memory import");

    let instance = linker
        .instantiate(&mut store, &module)
        .expect("instantiate Tier-2 module");
    let trace_fn: TypedFunc<(i32, i32), i64> = instance
        .get_typed_func(&mut store, EXPORT_TRACE_FN)
        .expect("get exported Tier-2 trace function");

    // CPU/JIT context at 0, code-version table at 0x1000.
    let cpu_ptr = 0usize;
    let table_ptr = 0x1000usize;
    let init_rip = 0x1111u64;

    // Install a 1-entry code-version table: [1].
    write_u32(
        &memory,
        cpu_ptr + jit_ctx::CODE_VERSION_TABLE_PTR_OFFSET as usize,
        table_ptr as u32,
    );
    write_u32(
        &memory,
        cpu_ptr + jit_ctx::CODE_VERSION_TABLE_LEN_OFFSET as usize,
        1,
    );
    write_u32(&memory, table_ptr, 1);

    // Initialize CPU state.
    write_u64(
        &memory,
        cpu_ptr + abi::CPU_RIP_OFF as usize,
        init_rip,
    );
    write_u64(
        &memory,
        cpu_ptr + abi::CPU_RFLAGS_OFF as usize,
        abi::RFLAGS_RESERVED1,
    );

    // First run: guard passes.
    let ret = trace_fn
        .call(&mut store, (cpu_ptr as i32, 0))
        .expect("call Tier-2 trace");
    assert_eq!(ret as u64, init_rip);
    assert_eq!(
        read_u32(
            &memory,
            cpu_ptr + jit_ctx::TRACE_EXIT_REASON_OFFSET as usize
        ),
        jit_ctx::TRACE_EXIT_REASON_NONE
    );

    // Mutate the table entry: guard should invalidate.
    write_u32(&memory, table_ptr, 2);
    write_u64(
        &memory,
        cpu_ptr + abi::CPU_RIP_OFF as usize,
        init_rip,
    );
    let ret = trace_fn
        .call(&mut store, (cpu_ptr as i32, 0))
        .expect("call Tier-2 trace");
    assert_eq!(ret as u64, 0x9999);
    assert_eq!(
        read_u32(
            &memory,
            cpu_ptr + jit_ctx::TRACE_EXIT_REASON_OFFSET as usize
        ),
        jit_ctx::TRACE_EXIT_REASON_CODE_INVALIDATION
    );
}
