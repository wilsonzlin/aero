#![cfg(debug_assertions)]

mod tier1_common;

use aero_cpu_core::state::CpuState;
use aero_jit::abi;
use aero_jit::tier1::ir::interp::execute_block;
use aero_jit::tier1::wasm::{Tier1WasmCodegen, EXPORT_TIER1_BLOCK_FN};
use aero_jit::tier1::{discover_block, translate_block, BlockLimits};
use aero_jit::wasm::{
    IMPORT_JIT_EXIT, IMPORT_MEMORY, IMPORT_MEM_READ_U16, IMPORT_MEM_READ_U32, IMPORT_MEM_READ_U64,
    IMPORT_MEM_READ_U8, IMPORT_MEM_WRITE_U16, IMPORT_MEM_WRITE_U32, IMPORT_MEM_WRITE_U64,
    IMPORT_MEM_WRITE_U8, IMPORT_MODULE, IMPORT_PAGE_FAULT, JIT_EXIT_SENTINEL_I64,
};
use aero_jit::Tier1Bus;
use aero_types::{Gpr, Width};
use tier1_common::{write_cpu_to_wasm_bytes, write_gpr, CpuSnapshot, SimpleBus};

use wasmi::{Caller, Engine, Func, Linker, Memory, MemoryType, Module, Store, TypedFunc};

const CPU_PTR: i32 = 0x1_0000;
const JIT_CTX_PTR: i32 = CPU_PTR + aero_jit::abi::CPU_STATE_SIZE as i32;

fn validate_wasm(bytes: &[u8]) {
    let mut validator = wasmparser::Validator::new();
    validator.validate_all(bytes).unwrap();
}

fn instantiate(bytes: &[u8]) -> (Store<()>, Memory, TypedFunc<(i32, i32), i64>) {
    let engine = Engine::default();
    let module = Module::new(&engine, bytes).unwrap();

    let mut store = Store::new(&engine, ());
    let mut linker = Linker::new(&engine);

    // Guest memory in page 0, CpuState at CPU_PTR in page 1, and room for the JIT context.
    let memory = Memory::new(&mut store, MemoryType::new(4, None)).unwrap();
    linker
        .define(IMPORT_MODULE, IMPORT_MEMORY, memory.clone())
        .unwrap();

    define_mem_helpers(&mut store, &mut linker, memory.clone());

    linker
        .define(
            IMPORT_MODULE,
            IMPORT_PAGE_FAULT,
            Func::wrap(
                &mut store,
                |_caller: Caller<'_, ()>, _cpu_ptr: i32, _addr: i64| -> i64 {
                    panic!("page_fault should not be called by tier1 tests");
                },
            ),
        )
        .unwrap();

    linker
        .define(
            IMPORT_MODULE,
            IMPORT_JIT_EXIT,
            Func::wrap(
                &mut store,
                |_caller: Caller<'_, ()>, _kind: i32, _rip: i64| -> i64 { JIT_EXIT_SENTINEL_I64 },
            ),
        )
        .unwrap();

    let instance = linker.instantiate_and_start(&mut store, &module).unwrap();
    let block = instance
        .get_typed_func::<(i32, i32), i64>(&store, EXPORT_TIER1_BLOCK_FN)
        .unwrap();
    (store, memory, block)
}

fn define_mem_helpers(store: &mut Store<()>, linker: &mut Linker<()>, memory: Memory) {
    fn read<const N: usize>(caller: &mut Caller<'_, ()>, memory: &Memory, addr: usize) -> u64 {
        let mut buf = [0u8; N];
        memory
            .read(caller, addr, &mut buf)
            .expect("memory read in bounds");
        let mut v = 0u64;
        for (i, b) in buf.iter().enumerate() {
            v |= (*b as u64) << (i * 8);
        }
        v
    }

    fn write<const N: usize>(
        caller: &mut Caller<'_, ()>,
        memory: &Memory,
        addr: usize,
        value: u64,
    ) {
        let mut buf = vec![0u8; N];
        for (i, b) in buf.iter_mut().enumerate() {
            *b = (value >> (i * 8)) as u8;
        }
        memory
            .write(caller, addr, &buf)
            .expect("memory write in bounds");
    }

    let mem = memory.clone();
    linker
        .define(
            IMPORT_MODULE,
            IMPORT_MEM_READ_U8,
            Func::wrap(
                &mut *store,
                move |mut caller: Caller<'_, ()>, _cpu_ptr: i32, addr: i64| -> i32 {
                    read::<1>(&mut caller, &mem, addr as usize) as i32
                },
            ),
        )
        .unwrap();

    let mem = memory.clone();
    linker
        .define(
            IMPORT_MODULE,
            IMPORT_MEM_READ_U16,
            Func::wrap(
                &mut *store,
                move |mut caller: Caller<'_, ()>, _cpu_ptr: i32, addr: i64| -> i32 {
                    read::<2>(&mut caller, &mem, addr as usize) as i32
                },
            ),
        )
        .unwrap();

    let mem = memory.clone();
    linker
        .define(
            IMPORT_MODULE,
            IMPORT_MEM_READ_U32,
            Func::wrap(
                &mut *store,
                move |mut caller: Caller<'_, ()>, _cpu_ptr: i32, addr: i64| -> i32 {
                    read::<4>(&mut caller, &mem, addr as usize) as i32
                },
            ),
        )
        .unwrap();

    let mem = memory.clone();
    linker
        .define(
            IMPORT_MODULE,
            IMPORT_MEM_READ_U64,
            Func::wrap(
                &mut *store,
                move |mut caller: Caller<'_, ()>, _cpu_ptr: i32, addr: i64| -> i64 {
                    read::<8>(&mut caller, &mem, addr as usize) as i64
                },
            ),
        )
        .unwrap();

    let mem = memory.clone();
    linker
        .define(
            IMPORT_MODULE,
            IMPORT_MEM_WRITE_U8,
            Func::wrap(
                &mut *store,
                move |mut caller: Caller<'_, ()>, _cpu_ptr: i32, addr: i64, value: i32| {
                    write::<1>(&mut caller, &mem, addr as usize, value as u64);
                },
            ),
        )
        .unwrap();

    let mem = memory.clone();
    linker
        .define(
            IMPORT_MODULE,
            IMPORT_MEM_WRITE_U16,
            Func::wrap(
                &mut *store,
                move |mut caller: Caller<'_, ()>, _cpu_ptr: i32, addr: i64, value: i32| {
                    write::<2>(&mut caller, &mem, addr as usize, value as u64);
                },
            ),
        )
        .unwrap();

    let mem = memory.clone();
    linker
        .define(
            IMPORT_MODULE,
            IMPORT_MEM_WRITE_U32,
            Func::wrap(
                &mut *store,
                move |mut caller: Caller<'_, ()>, _cpu_ptr: i32, addr: i64, value: i32| {
                    write::<4>(&mut caller, &mem, addr as usize, value as u64);
                },
            ),
        )
        .unwrap();

    let mem = memory;
    linker
        .define(
            IMPORT_MODULE,
            IMPORT_MEM_WRITE_U64,
            Func::wrap(
                &mut *store,
                move |mut caller: Caller<'_, ()>, _cpu_ptr: i32, addr: i64, value: i64| {
                    write::<8>(&mut caller, &mem, addr as usize, value as u64);
                },
            ),
        )
        .unwrap();
}

fn run_wasm(
    ir: &aero_jit::tier1::ir::IrBlock,
    cpu: &CpuState,
    bus: &SimpleBus,
) -> (u64, CpuSnapshot, Vec<u8>) {
    let wasm = Tier1WasmCodegen::new().compile_block(ir);
    validate_wasm(&wasm);

    let (mut store, memory, func) = instantiate(&wasm);

    // Initialize guest memory.
    memory.write(&mut store, 0, bus.mem()).unwrap();

    // Initialize CpuState.
    let mut cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    write_cpu_to_wasm_bytes(cpu, &mut cpu_bytes);
    memory
        .write(&mut store, CPU_PTR as usize, &cpu_bytes)
        .unwrap();

    let ret = func.call(&mut store, (CPU_PTR, JIT_CTX_PTR)).unwrap();

    // Read back guest memory region (page 0).
    let mut out_mem = vec![0u8; bus.mem().len()];
    memory.read(&store, 0, &mut out_mem).unwrap();

    // Read back CpuState.
    let mut out_cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    memory
        .read(&store, CPU_PTR as usize, &mut out_cpu_bytes)
        .unwrap();
    let out_cpu = CpuSnapshot::from_wasm_bytes(&out_cpu_bytes);

    let next_rip = if ret == JIT_EXIT_SENTINEL_I64 {
        out_cpu.rip
    } else {
        ret as u64
    };

    (next_rip, out_cpu, out_mem)
}

fn assert_ir_wasm_matches_interp(code: &[u8], entry_rip: u64, cpu: CpuState, mut bus: SimpleBus) {
    bus.load(entry_rip, code);

    let block = discover_block(&bus, entry_rip, BlockLimits::default());
    let ir = translate_block(&block);

    let mut interp_bus = bus.clone();
    let mut interp_cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    write_cpu_to_wasm_bytes(&cpu, &mut interp_cpu_bytes);
    let _ = execute_block(&ir, &mut interp_cpu_bytes, &mut interp_bus);
    let interp_cpu = CpuSnapshot::from_wasm_bytes(&interp_cpu_bytes);

    let (next_rip, out_cpu, out_mem) = run_wasm(&ir, &cpu, &bus);

    assert_eq!(next_rip, interp_cpu.rip);
    assert_eq!(out_cpu, interp_cpu);
    assert_eq!(out_mem, interp_bus.mem());
}

#[test]
fn wasm_tier1_mov_add_cmp_sete_ret() {
    let code = [
        0xb8, 0x05, 0x00, 0x00, 0x00, // mov eax, 5
        0x83, 0xc0, 0x07, // add eax, 7
        0x83, 0xf8, 0x0c, // cmp eax, 12
        0x0f, 0x94, 0xc0, // sete al
        0xc3, // ret
    ];

    let entry = 0x1000u64;
    let mut cpu = CpuState::default();
    cpu.rip = entry;
    write_gpr(&mut cpu, Gpr::Rsp, 0x8000);

    let mut bus = SimpleBus::new(0x10000);
    bus.write(0x8000, Width::W64, 0x2000);

    assert_ir_wasm_matches_interp(&code, entry, cpu, bus);
}

#[test]
fn wasm_tier1_cmp_jne_not_taken() {
    let code = [
        0xb8, 0x00, 0x00, 0x00, 0x00, // mov eax, 0
        0x83, 0xf8, 0x00, // cmp eax, 0
        0x75, 0x05, // jne +5
    ];
    let entry = 0x3000u64;
    let mut cpu = CpuState::default();
    cpu.rip = entry;

    let bus = SimpleBus::new(0x10000);
    assert_ir_wasm_matches_interp(&code, entry, cpu, bus);
}

#[test]
fn wasm_tier1_lea_sib_ret() {
    let code = [
        0x48, 0x8d, 0x44, 0x91, 0x10, // lea rax, [rcx + rdx*4 + 0x10]
        0xc3, // ret
    ];
    let entry = 0x4000u64;

    let mut cpu = CpuState::default();
    cpu.rip = entry;
    write_gpr(&mut cpu, Gpr::Rsp, 0x8800);
    write_gpr(&mut cpu, Gpr::Rcx, 0x100);
    write_gpr(&mut cpu, Gpr::Rdx, 0x2);

    let mut bus = SimpleBus::new(0x10000);
    bus.write(0x8800, Width::W64, 0x5000);

    assert_ir_wasm_matches_interp(&code, entry, cpu, bus);
}
