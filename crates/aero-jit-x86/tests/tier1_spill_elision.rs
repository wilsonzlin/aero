mod tier1_common;

use aero_cpu_core::state::CpuState;
use aero_jit_x86::abi;
use aero_jit_x86::tier1::ir::{GuestReg, IrBuilder, IrTerminator};
use aero_jit_x86::tier1::{Tier1WasmCodegen, EXPORT_TIER1_BLOCK_FN};
use aero_jit_x86::wasm::{
    IMPORT_JIT_EXIT, IMPORT_MEMORY, IMPORT_MEM_READ_U16, IMPORT_MEM_READ_U32, IMPORT_MEM_READ_U64,
    IMPORT_MEM_READ_U8, IMPORT_MEM_WRITE_U16, IMPORT_MEM_WRITE_U32, IMPORT_MEM_WRITE_U64,
    IMPORT_MEM_WRITE_U8, IMPORT_MODULE, IMPORT_PAGE_FAULT,
};
use aero_types::{Gpr, Width};
use tier1_common::{write_cpu_to_wasm_bytes, CpuSnapshot};
use wasmi::{Caller, Engine, Func, Linker, Memory, MemoryType, Module, Store, TypedFunc};

const CPU_PTR: i32 = 0x1_0000;
const JIT_CTX_PTR: i32 = CPU_PTR + abi::CPU_STATE_SIZE as i32;

fn validate_wasm(bytes: &[u8]) {
    let mut validator = wasmparser::Validator::new();
    validator.validate_all(bytes).unwrap();
}

fn instantiate(bytes: &[u8]) -> (Store<()>, Memory, TypedFunc<(i32, i32), i64>) {
    let engine = Engine::default();
    let module = Module::new(&engine, bytes).unwrap();

    let mut store = Store::new(&engine, ());
    let mut linker = Linker::new(&engine);

    // Guest memory at page 0, CpuState at CPU_PTR in page 1.
    let memory = Memory::new(&mut store, MemoryType::new(4, None)).unwrap();
    linker.define(IMPORT_MODULE, IMPORT_MEMORY, memory).unwrap();

    // The spill-elision test never performs memory accesses, but the Tier-1 block ABI always
    // imports these helpers.
    define_stub_mem_helpers(&mut store, &mut linker);

    linker
        .define(
            IMPORT_MODULE,
            IMPORT_PAGE_FAULT,
            Func::wrap(
                &mut store,
                |_caller: Caller<'_, ()>, _cpu_ptr: i32, _addr: i64| -> i64 {
                    panic!("page_fault should not be called by spill-elision test");
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
                |_caller: Caller<'_, ()>, _kind: i32, rip: i64| -> i64 { rip },
            ),
        )
        .unwrap();

    let instance = linker.instantiate_and_start(&mut store, &module).unwrap();
    let block = instance
        .get_typed_func::<(i32, i32), i64>(&store, EXPORT_TIER1_BLOCK_FN)
        .unwrap();
    (store, memory, block)
}

fn define_stub_mem_helpers(store: &mut Store<()>, linker: &mut Linker<()>) {
    linker
        .define(
            IMPORT_MODULE,
            IMPORT_MEM_READ_U8,
            Func::wrap(
                &mut *store,
                |_caller: Caller<'_, ()>, _cpu_ptr: i32, _addr: i64| -> i32 {
                    panic!("mem_read_u8 should not be called by spill-elision test");
                },
            ),
        )
        .unwrap();
    linker
        .define(
            IMPORT_MODULE,
            IMPORT_MEM_READ_U16,
            Func::wrap(
                &mut *store,
                |_caller: Caller<'_, ()>, _cpu_ptr: i32, _addr: i64| -> i32 {
                    panic!("mem_read_u16 should not be called by spill-elision test");
                },
            ),
        )
        .unwrap();
    linker
        .define(
            IMPORT_MODULE,
            IMPORT_MEM_READ_U32,
            Func::wrap(
                &mut *store,
                |_caller: Caller<'_, ()>, _cpu_ptr: i32, _addr: i64| -> i32 {
                    panic!("mem_read_u32 should not be called by spill-elision test");
                },
            ),
        )
        .unwrap();
    linker
        .define(
            IMPORT_MODULE,
            IMPORT_MEM_READ_U64,
            Func::wrap(
                &mut *store,
                |_caller: Caller<'_, ()>, _cpu_ptr: i32, _addr: i64| -> i64 {
                    panic!("mem_read_u64 should not be called by spill-elision test");
                },
            ),
        )
        .unwrap();

    linker
        .define(
            IMPORT_MODULE,
            IMPORT_MEM_WRITE_U8,
            Func::wrap(
                &mut *store,
                |_caller: Caller<'_, ()>, _cpu_ptr: i32, _addr: i64, _value: i32| -> () {
                    panic!("mem_write_u8 should not be called by spill-elision test");
                },
            ),
        )
        .unwrap();
    linker
        .define(
            IMPORT_MODULE,
            IMPORT_MEM_WRITE_U16,
            Func::wrap(
                &mut *store,
                |_caller: Caller<'_, ()>, _cpu_ptr: i32, _addr: i64, _value: i32| -> () {
                    panic!("mem_write_u16 should not be called by spill-elision test");
                },
            ),
        )
        .unwrap();
    linker
        .define(
            IMPORT_MODULE,
            IMPORT_MEM_WRITE_U32,
            Func::wrap(
                &mut *store,
                |_caller: Caller<'_, ()>, _cpu_ptr: i32, _addr: i64, _value: i32| -> () {
                    panic!("mem_write_u32 should not be called by spill-elision test");
                },
            ),
        )
        .unwrap();
    linker
        .define(
            IMPORT_MODULE,
            IMPORT_MEM_WRITE_U64,
            Func::wrap(
                &mut *store,
                |_caller: Caller<'_, ()>, _cpu_ptr: i32, _addr: i64, _value: i64| -> () {
                    panic!("mem_write_u64 should not be called by spill-elision test");
                },
            ),
        )
        .unwrap();
}

#[test]
fn tier1_spill_elides_unused_gprs() {
    let entry = 0x1000u64;

    // IR block that only writes RAX.
    let mut b = IrBuilder::new(entry);
    let new_rax = 0x1234_5678_9abc_def0u64;
    let v = b.const_int(Width::W64, new_rax);
    b.write_reg(
        GuestReg::Gpr {
            reg: Gpr::Rax,
            width: Width::W64,
            high8: false,
        },
        v,
    );
    let ir = b.finish(IrTerminator::Jump { target: entry + 4 });
    ir.validate().unwrap();

    let wasm = Tier1WasmCodegen::new().compile_block(&ir);
    validate_wasm(&wasm);

    let (mut store, memory, func) = instantiate(&wasm);

    // Initialize CpuState with sentinel values in all registers.
    let mut cpu = CpuState::default();
    cpu.rip = entry;
    for (i, slot) in cpu.gpr.iter_mut().enumerate() {
        // Unique non-zero sentinel per GPR.
        *slot = 0x1111_1111_1111_1111u64.wrapping_mul((i as u64) + 1);
    }
    let before = CpuSnapshot::from_cpu(&cpu);

    let mut cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    write_cpu_to_wasm_bytes(&cpu, &mut cpu_bytes);
    memory
        .write(&mut store, CPU_PTR as usize, &cpu_bytes)
        .unwrap();

    let _ret = func.call(&mut store, (CPU_PTR, JIT_CTX_PTR)).unwrap();

    memory
        .read(&store, CPU_PTR as usize, &mut cpu_bytes)
        .unwrap();
    let after = CpuSnapshot::from_wasm_bytes(&cpu_bytes);

    for reg in [
        Gpr::Rax,
        Gpr::Rcx,
        Gpr::Rdx,
        Gpr::Rbx,
        Gpr::Rsp,
        Gpr::Rbp,
        Gpr::Rsi,
        Gpr::Rdi,
        Gpr::R8,
        Gpr::R9,
        Gpr::R10,
        Gpr::R11,
        Gpr::R12,
        Gpr::R13,
        Gpr::R14,
        Gpr::R15,
    ] {
        let idx = reg.as_u8() as usize;
        if reg == Gpr::Rax {
            assert_eq!(after.gpr[idx], new_rax);
        } else {
            assert_eq!(
                after.gpr[idx], before.gpr[idx],
                "unexpected clobber of {reg:?}"
            );
        }
    }
}
