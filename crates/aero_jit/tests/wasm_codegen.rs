use aero_jit::interp::interpret_block;
use aero_jit::ir::{BinOp, CmpOp, IrBlock, IrOp, MemSize, Operand, Place, Temp};
use aero_jit::wasm::{
    WasmCodegen, EXPORT_BLOCK_FN, IMPORT_JIT_EXIT, IMPORT_MEMORY, IMPORT_MEM_READ_U16,
    IMPORT_MEM_READ_U32, IMPORT_MEM_READ_U64, IMPORT_MEM_READ_U8, IMPORT_MEM_WRITE_U16,
    IMPORT_MEM_WRITE_U32, IMPORT_MEM_WRITE_U64, IMPORT_MEM_WRITE_U8, IMPORT_MODULE,
    IMPORT_PAGE_FAULT,
};
use aero_jit::{CpuState, Reg};

use wasmi::{Caller, Engine, Func, Linker, Memory, MemoryType, Module, Store, TypedFunc};

fn validate_wasm(bytes: &[u8]) {
    let mut validator = wasmparser::Validator::new();
    validator.validate_all(bytes).unwrap();
}

fn instantiate(bytes: &[u8]) -> (Store<()>, Memory, TypedFunc<i32, i64>) {
    let engine = Engine::default();
    let module = Module::new(&engine, bytes).unwrap();

    let mut store = Store::new(&engine, ());
    let mut linker = Linker::new(&engine);

    let memory = Memory::new(&mut store, MemoryType::new(1, None)).unwrap();
    linker
        .define(IMPORT_MODULE, IMPORT_MEMORY, memory.clone())
        .unwrap();

    // Helpers: operate directly on the imported linear memory.
    define_mem_helpers(&mut store, &mut linker, memory.clone());

    // page_fault and jit_exit are present for ABI completeness.
    linker
        .define(
            IMPORT_MODULE,
            IMPORT_PAGE_FAULT,
            Func::wrap(
                &mut store,
                |_caller: Caller<'_, ()>, _cpu_ptr: i32, _addr: i64| -> i64 {
                    panic!("page_fault should not be called by baseline tests");
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
                |_caller: Caller<'_, ()>, _kind: i32, _rip: i64| -> i64 {
                    // Sentinel mirrors `interp::JIT_EXIT_SENTINEL` (u64::MAX).
                    -1i64
                },
            ),
        )
        .unwrap();

    let instance = linker.instantiate_and_start(&mut store, &module).unwrap();
    let block = instance
        .get_typed_func::<i32, i64>(&store, EXPORT_BLOCK_FN)
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

fn run_case(block: &IrBlock, cpu: CpuState, mut mem: Vec<u8>) -> (u64, CpuState, Vec<u8>) {
    let mut expected_cpu = cpu;
    let expected_rip = interpret_block(block, &mut expected_cpu, &mut mem);
    (expected_rip, expected_cpu, mem)
}

fn run_wasm(block: &IrBlock, cpu: CpuState, mut mem: Vec<u8>) -> (u64, CpuState, Vec<u8>) {
    let wasm = WasmCodegen::new().compile_block(block);
    validate_wasm(&wasm);

    cpu.write_to_mem(&mut mem, 0);

    let (mut store, memory, func) = instantiate(&wasm);
    memory.write(&mut store, 0, &mem).unwrap();

    let got_rip = func.call(&mut store, 0).unwrap() as u64;

    let mut got_mem = vec![0u8; mem.len()];
    memory.read(&store, 0, &mut got_mem).unwrap();
    let got_cpu = CpuState::read_from_mem(&got_mem, 0);
    (got_rip, got_cpu, got_mem)
}

#[test]
fn wasm_codegen_arithmetic_block_matches_interpreter() {
    let block = IrBlock::new(vec![
        IrOp::Bin {
            dst: Place::Reg(Reg::Rax),
            op: BinOp::Add,
            lhs: Operand::Reg(Reg::Rax),
            rhs: Operand::Imm(5),
        },
        IrOp::Bin {
            dst: Place::Reg(Reg::Rbx),
            op: BinOp::Xor,
            lhs: Operand::Reg(Reg::Rbx),
            rhs: Operand::Reg(Reg::Rax),
        },
        IrOp::Exit {
            next_rip: Operand::Imm(0x1000),
        },
    ]);

    let mut cpu = CpuState::default();
    cpu.set_reg(Reg::Rax, 0x10);
    cpu.set_reg(Reg::Rbx, 0x55);
    cpu.rip = 0x2000;

    let mem = vec![0u8; 65536];
    let (expected_rip, expected_cpu, _expected_mem) = run_case(&block, cpu, mem.clone());
    let (got_rip, got_cpu, _got_mem) = run_wasm(&block, cpu, mem);

    assert_eq!(got_rip, expected_rip);
    assert_eq!(got_cpu, expected_cpu);
}

#[test]
fn wasm_codegen_select_matches_interpreter() {
    let t_cond = Temp(0);
    let t_next = Temp(1);
    let block = IrBlock::new(vec![
        IrOp::Cmp {
            dst: Place::Temp(t_cond),
            op: CmpOp::Eq,
            lhs: Operand::Reg(Reg::Rax),
            rhs: Operand::Reg(Reg::Rbx),
        },
        IrOp::Select {
            dst: Place::Reg(Reg::Rcx),
            cond: Operand::Temp(t_cond),
            if_true: Operand::Imm(1),
            if_false: Operand::Imm(0),
        },
        IrOp::Select {
            dst: Place::Temp(t_next),
            cond: Operand::Temp(t_cond),
            if_true: Operand::Imm(0x1111),
            if_false: Operand::Imm(0x2222),
        },
        IrOp::Exit {
            next_rip: Operand::Temp(t_next),
        },
    ]);

    for (rax, rbx) in [(5u64, 5u64), (5u64, 7u64)] {
        let mut cpu = CpuState::default();
        cpu.set_reg(Reg::Rax, rax);
        cpu.set_reg(Reg::Rbx, rbx);
        cpu.rip = 0x1234;

        let mem = vec![0u8; 65536];
        let (expected_rip, expected_cpu, _expected_mem) = run_case(&block, cpu, mem.clone());
        let (got_rip, got_cpu, _got_mem) = run_wasm(&block, cpu, mem);

        assert_eq!(got_rip, expected_rip);
        assert_eq!(got_cpu, expected_cpu);
    }
}

#[test]
fn wasm_codegen_load_store_helpers_match_interpreter() {
    let block = IrBlock::new(vec![
        IrOp::Store {
            addr: Operand::Imm(0x1000),
            value: Operand::Imm(0xAB),
            size: MemSize::U8,
        },
        IrOp::Store {
            addr: Operand::Imm(0x1004),
            value: Operand::Imm(0x1234_5678),
            size: MemSize::U32,
        },
        IrOp::Load {
            dst: Place::Reg(Reg::Rdx),
            addr: Operand::Imm(0x1000),
            size: MemSize::U8,
        },
        IrOp::Load {
            dst: Place::Reg(Reg::Rsi),
            addr: Operand::Imm(0x1004),
            size: MemSize::U32,
        },
        IrOp::Bin {
            dst: Place::Reg(Reg::Rdx),
            op: BinOp::Add,
            lhs: Operand::Reg(Reg::Rdx),
            rhs: Operand::Imm(1),
        },
        IrOp::Exit {
            next_rip: Operand::Imm(0x3000),
        },
    ]);

    let cpu = CpuState::default();
    let mem = vec![0u8; 65536];

    let (expected_rip, expected_cpu, expected_mem) = run_case(&block, cpu, mem.clone());
    let (got_rip, got_cpu, got_mem) = run_wasm(&block, cpu, mem);

    assert_eq!(got_rip, expected_rip);
    assert_eq!(got_cpu, expected_cpu);

    // Also validate the memory mutations (stores).
    assert_eq!(&got_mem[0x1000..0x1001], &expected_mem[0x1000..0x1001]);
    assert_eq!(&got_mem[0x1004..0x1008], &expected_mem[0x1004..0x1008]);
}
