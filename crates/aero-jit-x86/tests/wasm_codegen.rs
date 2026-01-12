#![cfg(feature = "legacy-baseline")]

use aero_jit_x86::legacy::interp::interpret_block;
use aero_jit_x86::legacy::ir::{BinOp, CmpOp, IrBlock, IrOp, MemSize, Operand, Place, Temp};
use aero_jit_x86::legacy::wasm::{WasmCodegen, EXPORT_BLOCK_FN};
use aero_jit_x86::legacy::{CpuState, Reg};
use aero_jit_x86::wasm::abi::{
    IMPORT_JIT_EXIT, IMPORT_JIT_EXIT_MMIO, IMPORT_MEMORY, IMPORT_MEM_READ_U16, IMPORT_MEM_READ_U32,
    IMPORT_MEM_READ_U64, IMPORT_MEM_READ_U8, IMPORT_MEM_WRITE_U16, IMPORT_MEM_WRITE_U32,
    IMPORT_MEM_WRITE_U64, IMPORT_MEM_WRITE_U8, IMPORT_MMU_TRANSLATE, IMPORT_MODULE,
    IMPORT_PAGE_FAULT,
};

use wasmi::{Caller, Engine, Func, Linker, Memory, MemoryType, Module, Store, TypedFunc};

#[derive(Debug, Default, Clone, Copy)]
struct HostState {
    mmu_translate_calls: u64,
    mmio_exit_calls: u64,
    slow_mem_reads: u64,
    slow_mem_writes: u64,
    ram_size: u64,
}

fn validate_wasm(bytes: &[u8]) {
    let mut validator = wasmparser::Validator::new();
    validator.validate_all(bytes).unwrap();
}

fn instantiate(
    bytes: &[u8],
    memory_pages: u32,
    ram_size: u64,
) -> (Store<HostState>, Memory, TypedFunc<i32, i64>) {
    let engine = Engine::default();
    let module = Module::new(&engine, bytes).unwrap();

    let mut store = Store::new(
        &engine,
        HostState {
            ram_size,
            ..Default::default()
        },
    );
    let mut linker = Linker::new(&engine);

    let memory = Memory::new(&mut store, MemoryType::new(memory_pages, None)).unwrap();
    linker.define(IMPORT_MODULE, IMPORT_MEMORY, memory).unwrap();

    // Helpers: operate directly on the imported linear memory.
    define_mem_helpers(&mut store, &mut linker, memory);

    // mmu_translate / mmio exits.
    define_mmu_translate(&mut store, &mut linker, memory);
    define_mmio_exit(&mut store, &mut linker);

    // page_fault and jit_exit are present for ABI completeness.
    linker
        .define(
            IMPORT_MODULE,
            IMPORT_PAGE_FAULT,
            Func::wrap(
                &mut store,
                |_caller: Caller<'_, HostState>, _cpu_ptr: i32, _addr: i64| -> i64 {
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
                |_caller: Caller<'_, HostState>, _kind: i32, _rip: i64| -> i64 {
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

fn read_u64_from_memory(caller: &mut Caller<'_, HostState>, memory: &Memory, addr: usize) -> u64 {
    let mut buf = [0u8; 8];
    memory
        .read(caller, addr, &mut buf)
        .expect("memory read in bounds");
    u64::from_le_bytes(buf)
}

fn write_u64_to_memory(
    caller: &mut Caller<'_, HostState>,
    memory: &Memory,
    addr: usize,
    value: u64,
) {
    memory
        .write(caller, addr, &value.to_le_bytes())
        .expect("memory write in bounds");
}

fn define_mem_helpers(
    store: &mut Store<HostState>,
    linker: &mut Linker<HostState>,
    memory: Memory,
) {
    fn read<const N: usize>(
        caller: &mut Caller<'_, HostState>,
        memory: &Memory,
        addr: usize,
    ) -> u64 {
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
        caller: &mut Caller<'_, HostState>,
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

    let mem = memory;
    linker
        .define(
            IMPORT_MODULE,
            IMPORT_MEM_READ_U8,
            Func::wrap(
                &mut *store,
                move |mut caller: Caller<'_, HostState>, cpu_ptr: i32, addr: i64| -> i32 {
                    caller.data_mut().slow_mem_reads += 1;
                    let ram_base = read_u64_from_memory(
                        &mut caller,
                        &mem,
                        cpu_ptr as usize + CpuState::RAM_BASE_OFFSET as usize,
                    ) as usize;
                    read::<1>(&mut caller, &mem, ram_base + addr as usize) as i32
                },
            ),
        )
        .unwrap();

    let mem = memory;
    linker
        .define(
            IMPORT_MODULE,
            IMPORT_MEM_READ_U16,
            Func::wrap(
                &mut *store,
                move |mut caller: Caller<'_, HostState>, cpu_ptr: i32, addr: i64| -> i32 {
                    caller.data_mut().slow_mem_reads += 1;
                    let ram_base = read_u64_from_memory(
                        &mut caller,
                        &mem,
                        cpu_ptr as usize + CpuState::RAM_BASE_OFFSET as usize,
                    ) as usize;
                    read::<2>(&mut caller, &mem, ram_base + addr as usize) as i32
                },
            ),
        )
        .unwrap();

    let mem = memory;
    linker
        .define(
            IMPORT_MODULE,
            IMPORT_MEM_READ_U32,
            Func::wrap(
                &mut *store,
                move |mut caller: Caller<'_, HostState>, cpu_ptr: i32, addr: i64| -> i32 {
                    caller.data_mut().slow_mem_reads += 1;
                    let ram_base = read_u64_from_memory(
                        &mut caller,
                        &mem,
                        cpu_ptr as usize + CpuState::RAM_BASE_OFFSET as usize,
                    ) as usize;
                    read::<4>(&mut caller, &mem, ram_base + addr as usize) as i32
                },
            ),
        )
        .unwrap();

    let mem = memory;
    linker
        .define(
            IMPORT_MODULE,
            IMPORT_MEM_READ_U64,
            Func::wrap(
                &mut *store,
                move |mut caller: Caller<'_, HostState>, cpu_ptr: i32, addr: i64| -> i64 {
                    caller.data_mut().slow_mem_reads += 1;
                    let ram_base = read_u64_from_memory(
                        &mut caller,
                        &mem,
                        cpu_ptr as usize + CpuState::RAM_BASE_OFFSET as usize,
                    ) as usize;
                    read::<8>(&mut caller, &mem, ram_base + addr as usize) as i64
                },
            ),
        )
        .unwrap();

    let mem = memory;
    linker
        .define(
            IMPORT_MODULE,
            IMPORT_MEM_WRITE_U8,
            Func::wrap(
                &mut *store,
                move |mut caller: Caller<'_, HostState>, cpu_ptr: i32, addr: i64, value: i32| {
                    caller.data_mut().slow_mem_writes += 1;
                    let ram_base = read_u64_from_memory(
                        &mut caller,
                        &mem,
                        cpu_ptr as usize + CpuState::RAM_BASE_OFFSET as usize,
                    ) as usize;
                    write::<1>(&mut caller, &mem, ram_base + addr as usize, value as u64);
                },
            ),
        )
        .unwrap();

    let mem = memory;
    linker
        .define(
            IMPORT_MODULE,
            IMPORT_MEM_WRITE_U16,
            Func::wrap(
                &mut *store,
                move |mut caller: Caller<'_, HostState>, cpu_ptr: i32, addr: i64, value: i32| {
                    caller.data_mut().slow_mem_writes += 1;
                    let ram_base = read_u64_from_memory(
                        &mut caller,
                        &mem,
                        cpu_ptr as usize + CpuState::RAM_BASE_OFFSET as usize,
                    ) as usize;
                    write::<2>(&mut caller, &mem, ram_base + addr as usize, value as u64);
                },
            ),
        )
        .unwrap();

    let mem = memory;
    linker
        .define(
            IMPORT_MODULE,
            IMPORT_MEM_WRITE_U32,
            Func::wrap(
                &mut *store,
                move |mut caller: Caller<'_, HostState>, cpu_ptr: i32, addr: i64, value: i32| {
                    caller.data_mut().slow_mem_writes += 1;
                    let ram_base = read_u64_from_memory(
                        &mut caller,
                        &mem,
                        cpu_ptr as usize + CpuState::RAM_BASE_OFFSET as usize,
                    ) as usize;
                    write::<4>(&mut caller, &mem, ram_base + addr as usize, value as u64);
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
                move |mut caller: Caller<'_, HostState>, cpu_ptr: i32, addr: i64, value: i64| {
                    caller.data_mut().slow_mem_writes += 1;
                    let ram_base = read_u64_from_memory(
                        &mut caller,
                        &mem,
                        cpu_ptr as usize + CpuState::RAM_BASE_OFFSET as usize,
                    ) as usize;
                    write::<8>(&mut caller, &mem, ram_base + addr as usize, value as u64);
                },
            ),
        )
        .unwrap();
}

fn define_mmu_translate(
    store: &mut Store<HostState>,
    linker: &mut Linker<HostState>,
    memory: Memory,
) {
    let mem = memory;
    linker
        .define(
            IMPORT_MODULE,
            IMPORT_MMU_TRANSLATE,
            Func::wrap(
                &mut *store,
                move |mut caller: Caller<'_, HostState>,
                      cpu_ptr: i32,
                      vaddr: i64,
                      _access: i32|
                      -> i64 {
                    caller.data_mut().mmu_translate_calls += 1;

                    let vaddr_u = vaddr as u64;
                    let vpn = vaddr_u >> aero_jit_x86::legacy::PAGE_SHIFT;
                    let idx = vpn & aero_jit_x86::legacy::JIT_TLB_INDEX_MASK;

                    let salt = read_u64_from_memory(
                        &mut caller,
                        &mem,
                        cpu_ptr as usize + CpuState::TLB_SALT_OFFSET as usize,
                    );

                    let tag = vpn ^ salt;
                    // Keep tag 0 reserved for invalidation.
                    let tag = tag | 1;

                    let is_ram = vaddr_u < caller.data().ram_size;
                    let phys_base = vaddr_u & aero_jit_x86::legacy::PAGE_BASE_MASK;
                    let flags = aero_jit_x86::legacy::TLB_FLAG_READ
                        | aero_jit_x86::legacy::TLB_FLAG_WRITE
                        | aero_jit_x86::legacy::TLB_FLAG_EXEC
                        | if is_ram {
                            aero_jit_x86::legacy::TLB_FLAG_IS_RAM
                        } else {
                            0
                        };
                    let data = phys_base | flags;

                    let entry_addr = (cpu_ptr as u64)
                        + (CpuState::TLB_OFFSET as u64)
                        + idx * (aero_jit_x86::legacy::JIT_TLB_ENTRY_SIZE as u64);

                    write_u64_to_memory(&mut caller, &mem, entry_addr as usize, tag);
                    write_u64_to_memory(&mut caller, &mem, (entry_addr + 8) as usize, data);

                    data as i64
                },
            ),
        )
        .unwrap();
}

fn define_mmio_exit(store: &mut Store<HostState>, linker: &mut Linker<HostState>) {
    linker
        .define(
            IMPORT_MODULE,
            IMPORT_JIT_EXIT_MMIO,
            Func::wrap(
                &mut *store,
                |mut caller: Caller<'_, HostState>,
                 _cpu_ptr: i32,
                 _vaddr: i64,
                 _size: i32,
                 _is_write: i32,
                 _value: i64,
                 _rip: i64|
                 -> i64 {
                    caller.data_mut().mmio_exit_calls += 1;
                    -1i64
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

fn run_wasm(
    block: &IrBlock,
    cpu: CpuState,
    mut mem: Vec<u8>,
) -> (u64, CpuState, Vec<u8>, HostState) {
    let wasm = WasmCodegen::new().compile_block(block);
    validate_wasm(&wasm);

    cpu.write_to_mem(&mut mem, 0);

    let pages = mem.len().div_ceil(65_536) as u32;
    let ram_size = mem.len() as u64 - cpu.ram_base;
    let (mut store, memory, func) = instantiate(&wasm, pages, ram_size);
    memory.write(&mut store, 0, &mem).unwrap();

    let got_rip = func.call(&mut store, 0).unwrap() as u64;

    let mut got_mem = vec![0u8; mem.len()];
    memory.read(&store, 0, &mut got_mem).unwrap();
    let got_cpu = CpuState::read_from_mem(&got_mem, 0);
    let host_state = *store.data();
    (got_rip, got_cpu, got_mem, host_state)
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
    cpu.ram_base = CpuState::TOTAL_BYTE_SIZE as u64;

    let mem = vec![0u8; 65536];
    let (expected_rip, expected_cpu, _expected_mem) = run_case(&block, cpu, mem.clone());
    let (got_rip, got_cpu, _got_mem, _host_state) = run_wasm(&block, cpu, mem);

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
        cpu.ram_base = CpuState::TOTAL_BYTE_SIZE as u64;

        let mem = vec![0u8; 65536];
        let (expected_rip, expected_cpu, _expected_mem) = run_case(&block, cpu, mem.clone());
        let (got_rip, got_cpu, _got_mem, _host_state) = run_wasm(&block, cpu, mem);

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

    let mut cpu = CpuState::default();
    cpu.ram_base = CpuState::TOTAL_BYTE_SIZE as u64;
    let mem = vec![0u8; 65536];

    let (expected_rip, expected_cpu, expected_mem) = run_case(&block, cpu, mem.clone());
    let (got_rip, got_cpu, got_mem, host_state) = run_wasm(&block, cpu, mem);

    assert_eq!(got_rip, expected_rip);
    assert_eq!(got_cpu, expected_cpu);

    // Also validate the memory mutations (stores).
    let base = cpu.ram_base as usize;
    assert_eq!(
        &got_mem[base + 0x1000..base + 0x1001],
        &expected_mem[base + 0x1000..base + 0x1001]
    );
    assert_eq!(
        &got_mem[base + 0x1004..base + 0x1008],
        &expected_mem[base + 0x1004..base + 0x1008]
    );

    // Sanity: for same-page accesses we should see one translation miss and no slow helpers.
    assert_eq!(host_state.mmu_translate_calls, 1);
    assert_eq!(host_state.mmio_exit_calls, 0);
    assert_eq!(host_state.slow_mem_reads, 0);
    assert_eq!(host_state.slow_mem_writes, 0);
}

#[test]
fn wasm_codegen_tlb_collision_forces_retranslate() {
    // The baseline Tier-1 TLB is direct-mapped. Two virtual pages that share the same index must
    // evict each other, forcing a re-translate when revisiting the older page.
    let collide_addr =
        (aero_jit_x86::legacy::JIT_TLB_ENTRIES as u64) << aero_jit_x86::legacy::PAGE_SHIFT;

    let block = IrBlock::new(vec![
        IrOp::Load {
            dst: Place::Reg(Reg::Rax),
            addr: Operand::Imm(0),
            size: MemSize::U32,
        },
        IrOp::Load {
            dst: Place::Reg(Reg::Rbx),
            addr: Operand::Imm(collide_addr as i64),
            size: MemSize::U32,
        },
        IrOp::Load {
            dst: Place::Reg(Reg::Rcx),
            addr: Operand::Imm(4),
            size: MemSize::U32,
        },
        IrOp::Exit {
            next_rip: Operand::Imm(0x3000),
        },
    ]);

    let mut cpu = CpuState::default();
    cpu.ram_base = CpuState::TOTAL_BYTE_SIZE as u64;

    // Ensure guest RAM covers `collide_addr` and a few bytes past it.
    let total_len = cpu.ram_base as usize + collide_addr as usize + 0x1000;
    let mut mem = vec![0u8; total_len];

    // Seed some data at the load sites.
    let base = cpu.ram_base as usize;
    mem[base..base + 4].copy_from_slice(&0x1122_3344u32.to_le_bytes());
    mem[base + 4..base + 8].copy_from_slice(&0x5566_7788u32.to_le_bytes());
    mem[base + collide_addr as usize..base + collide_addr as usize + 4]
        .copy_from_slice(&0x99aa_bbccu32.to_le_bytes());

    let (expected_rip, expected_cpu, _expected_mem) = run_case(&block, cpu, mem.clone());
    let (got_rip, got_cpu, _got_mem, host_state) = run_wasm(&block, cpu, mem);

    assert_eq!(got_rip, expected_rip);
    assert_eq!(got_cpu, expected_cpu);

    // We should translate:
    // - page 0 once
    // - collide page once (evicts page 0)
    // - page 0 again (tag mismatch -> retranslate)
    assert_eq!(host_state.mmu_translate_calls, 3);
    assert_eq!(host_state.mmio_exit_calls, 0);
    assert_eq!(host_state.slow_mem_reads, 0);
    assert_eq!(host_state.slow_mem_writes, 0);
}

#[test]
fn wasm_codegen_mmio_access_exits_to_runtime() {
    // Any access beyond the configured guest RAM size should be classified as non-RAM by the
    // `mmu_translate` helper and cause a `jit_exit_mmio` exit.
    let block = IrBlock::new(vec![
        IrOp::Load {
            dst: Place::Reg(Reg::Rax),
            addr: Operand::Imm(0xF000),
            size: MemSize::U32,
        },
        IrOp::Exit {
            next_rip: Operand::Imm(0x3000),
        },
    ]);

    let mut cpu = CpuState::default();
    cpu.ram_base = CpuState::TOTAL_BYTE_SIZE as u64;
    let mem = vec![0u8; 65536];

    let (got_rip, got_cpu, _got_mem, host_state) = run_wasm(&block, cpu, mem);

    assert_eq!(got_rip, u64::MAX);
    assert_eq!(got_cpu.rip, u64::MAX);
    assert_eq!(host_state.mmio_exit_calls, 1);
    assert_eq!(host_state.mmu_translate_calls, 1);
    assert_eq!(host_state.slow_mem_reads, 0);
    assert_eq!(host_state.slow_mem_writes, 0);
}

#[test]
fn wasm_codegen_cross_page_load_uses_slow_helper() {
    // Cross-page accesses are handled by the slow `mem_read_*` helpers for correctness.
    let addr = 0xFF9; // U64 at 0xFF9 crosses the 4KiB boundary.
    let block = IrBlock::new(vec![
        IrOp::Load {
            dst: Place::Reg(Reg::Rax),
            addr: Operand::Imm(addr),
            size: MemSize::U64,
        },
        IrOp::Exit {
            next_rip: Operand::Imm(0x3000),
        },
    ]);

    let mut cpu = CpuState::default();
    cpu.ram_base = CpuState::TOTAL_BYTE_SIZE as u64;

    let mut mem = vec![0u8; 65536];
    let base = cpu.ram_base as usize;
    mem[base + addr as usize..base + addr as usize + 8]
        .copy_from_slice(&0x1122_3344_5566_7788u64.to_le_bytes());

    let (expected_rip, expected_cpu, _expected_mem) = run_case(&block, cpu, mem.clone());
    let (got_rip, got_cpu, _got_mem, host_state) = run_wasm(&block, cpu, mem);

    assert_eq!(got_rip, expected_rip);
    assert_eq!(got_cpu, expected_cpu);
    assert_eq!(host_state.mmu_translate_calls, 0);
    assert_eq!(host_state.mmio_exit_calls, 0);
    assert_eq!(host_state.slow_mem_reads, 1);
}

#[test]
fn wasm_codegen_mmio_store_exits_to_runtime() {
    let block = IrBlock::new(vec![
        IrOp::Store {
            addr: Operand::Imm(0xF000),
            value: Operand::Imm(0x1234_5678),
            size: MemSize::U32,
        },
        IrOp::Exit {
            next_rip: Operand::Imm(0x3000),
        },
    ]);

    let mut cpu = CpuState::default();
    cpu.ram_base = CpuState::TOTAL_BYTE_SIZE as u64;
    let mem = vec![0u8; 65536];

    let (got_rip, got_cpu, _got_mem, host_state) = run_wasm(&block, cpu, mem);

    assert_eq!(got_rip, u64::MAX);
    assert_eq!(got_cpu.rip, u64::MAX);
    assert_eq!(host_state.mmio_exit_calls, 1);
    assert_eq!(host_state.mmu_translate_calls, 1);
    assert_eq!(host_state.slow_mem_reads, 0);
    assert_eq!(host_state.slow_mem_writes, 0);
}

#[test]
fn wasm_codegen_cross_page_store_uses_slow_helper() {
    let addr = 0xFFD; // U32 store at 0xFFD crosses the 4KiB boundary.
    let value = 0xDEAD_BEEFu32;
    let block = IrBlock::new(vec![
        IrOp::Store {
            addr: Operand::Imm(addr),
            value: Operand::Imm(value as i64),
            size: MemSize::U32,
        },
        IrOp::Exit {
            next_rip: Operand::Imm(0x3000),
        },
    ]);

    let mut cpu = CpuState::default();
    cpu.ram_base = CpuState::TOTAL_BYTE_SIZE as u64;
    let mem = vec![0u8; 65536];

    let base = cpu.ram_base as usize;
    let (expected_rip, expected_cpu, expected_mem) = run_case(&block, cpu, mem.clone());
    let (got_rip, got_cpu, got_mem, host_state) = run_wasm(&block, cpu, mem);

    assert_eq!(got_rip, expected_rip);
    assert_eq!(got_cpu, expected_cpu);
    assert_eq!(
        &got_mem[base + addr as usize..base + addr as usize + 4],
        &expected_mem[base + addr as usize..base + addr as usize + 4]
    );
    let got_val = u32::from_le_bytes(
        got_mem[base + addr as usize..base + addr as usize + 4]
            .try_into()
            .unwrap(),
    );
    assert_eq!(got_val, value);
    assert_eq!(host_state.mmu_translate_calls, 0);
    assert_eq!(host_state.mmio_exit_calls, 0);
    assert_eq!(host_state.slow_mem_reads, 0);
    assert_eq!(host_state.slow_mem_writes, 1);
}

#[test]
fn wasm_codegen_high_ram_remap_uses_contiguous_ram_offset() {
    // Q35 layout:
    // - low RAM:  [0x0000_0000 .. 0xB000_0000)
    // - hole:     [0xB000_0000 .. 0x1_0000_0000)
    // - high RAM: [0x1_0000_0000 .. ...] remapped to start at 0xB000_0000 in the contiguous RAM
    //             backing store.
    const HIGH_RAM_BASE: u64 = 0x1_0000_0000;

    // We'll point `CpuState.ram_base` at a value that causes the correct high-RAM remap to wrap the
    // final wasm32 address into a small in-bounds offset, while the buggy identity-mapped
    // computation stays huge and traps.
    //
    // With the expected remap:
    //   wasm_addr = (ram_base + 0xB000_0000 + (paddr - 4GiB)) mod 2^32
    // For paddr == 4GiB, this becomes:
    //   wasm_addr = (ram_base + 0xB000_0000) mod 2^32
    //
    // Choose ram_base = 0x5000_0000 + desired_offset, so:
    //   (ram_base + 0xB000_0000) mod 2^32 == desired_offset
    //   (ram_base + 4GiB)        mod 2^32 == 0x5000_0000 + desired_offset   (OOB)
    let desired_offset: usize = 0x10000;
    let ram_base: u64 = 0x5000_0000 + desired_offset as u64;

    let block = IrBlock::new(vec![
        IrOp::Store {
            addr: Operand::Imm(i64::try_from(HIGH_RAM_BASE).unwrap()),
            value: Operand::Imm(0xAB),
            size: MemSize::U8,
        },
        IrOp::Load {
            dst: Place::Reg(Reg::Rax),
            addr: Operand::Imm(i64::try_from(HIGH_RAM_BASE).unwrap()),
            size: MemSize::U8,
        },
        IrOp::Exit {
            next_rip: Operand::Imm(0x3000),
        },
    ]);

    let mut cpu = CpuState::default();
    cpu.rip = 0x1000;
    cpu.ram_base = ram_base;

    let wasm = WasmCodegen::new().compile_block(&block);
    validate_wasm(&wasm);

    // Memory layout: keep it tiny, but large enough to hold CpuState + the desired test byte at
    // `desired_offset`.
    let mem_len = CpuState::TOTAL_BYTE_SIZE.max(desired_offset + 16);
    let mut mem = vec![0u8; mem_len];
    cpu.write_to_mem(&mut mem, 0);

    // Seed the target location; the store should overwrite it.
    mem[desired_offset] = 0x7f;

    let pages = mem.len().div_ceil(65_536) as u32;
    // Make `mmu_translate` classify 4GiB as RAM so the fast-path is taken.
    let ram_size = HIGH_RAM_BASE + 0x1000;
    let (mut store, memory, func) = instantiate(&wasm, pages, ram_size);
    memory.write(&mut store, 0, &mem).unwrap();

    let got_rip = func.call(&mut store, 0).unwrap() as u64;

    let mut got_mem = vec![0u8; mem.len()];
    memory.read(&store, 0, &mut got_mem).unwrap();
    let got_cpu = CpuState::read_from_mem(&got_mem, 0);
    let host_state = *store.data();

    assert_eq!(got_rip, 0x3000);
    assert_eq!(got_cpu.rip, 0x3000);
    assert_eq!(got_cpu.get_reg(Reg::Rax) & 0xff, 0xAB);
    assert_eq!(got_mem[desired_offset], 0xAB);

    // The store/load are same-page, so we should see one translation miss and no slow helpers.
    assert_eq!(host_state.mmu_translate_calls, 1);
    assert_eq!(host_state.mmio_exit_calls, 0);
    assert_eq!(host_state.slow_mem_reads, 0);
    assert_eq!(host_state.slow_mem_writes, 0);
}

#[test]
fn wasm_codegen_exit_if_matches_interpreter() {
    let t_cond = Temp(0);
    let block = IrBlock::new(vec![
        IrOp::Cmp {
            dst: Place::Temp(t_cond),
            op: CmpOp::Eq,
            lhs: Operand::Reg(Reg::Rax),
            rhs: Operand::Reg(Reg::Rbx),
        },
        IrOp::ExitIf {
            cond: Operand::Temp(t_cond),
            next_rip: Operand::Imm(0x1111),
        },
        IrOp::Bin {
            dst: Place::Reg(Reg::Rcx),
            op: BinOp::Add,
            lhs: Operand::Reg(Reg::Rcx),
            rhs: Operand::Imm(1),
        },
        IrOp::Exit {
            next_rip: Operand::Imm(0x2222),
        },
    ]);

    for (rax, rbx) in [(5u64, 5u64), (5u64, 7u64)] {
        let mut cpu = CpuState::default();
        cpu.set_reg(Reg::Rax, rax);
        cpu.set_reg(Reg::Rbx, rbx);
        cpu.set_reg(Reg::Rcx, 123);
        cpu.rip = 0x3333;
        cpu.ram_base = CpuState::TOTAL_BYTE_SIZE as u64;

        let mem = vec![0u8; 65536];
        let (expected_rip, expected_cpu, _expected_mem) = run_case(&block, cpu, mem.clone());
        let (got_rip, got_cpu, _got_mem, _host_state) = run_wasm(&block, cpu, mem);

        assert_eq!(got_rip, expected_rip);
        assert_eq!(got_cpu, expected_cpu);
    }
}

#[test]
fn wasm_codegen_bailout_matches_interpreter() {
    let block = IrBlock::new(vec![
        IrOp::Bin {
            dst: Place::Reg(Reg::Rax),
            op: BinOp::Add,
            lhs: Operand::Reg(Reg::Rax),
            rhs: Operand::Imm(5),
        },
        IrOp::Bailout {
            kind: 7,
            rip: Operand::Imm(0x9999),
        },
        IrOp::Bin {
            dst: Place::Reg(Reg::Rbx),
            op: BinOp::Add,
            lhs: Operand::Reg(Reg::Rbx),
            rhs: Operand::Imm(1),
        },
        IrOp::Exit {
            next_rip: Operand::Imm(0x2222),
        },
    ]);

    let mut cpu = CpuState::default();
    cpu.set_reg(Reg::Rax, 10);
    cpu.set_reg(Reg::Rbx, 20);
    cpu.rip = 0x1234;
    cpu.ram_base = CpuState::TOTAL_BYTE_SIZE as u64;

    let mem = vec![0u8; 65536];
    let (expected_rip, expected_cpu, _expected_mem) = run_case(&block, cpu, mem.clone());
    let (got_rip, got_cpu, _got_mem, _host_state) = run_wasm(&block, cpu, mem);

    assert_eq!(got_rip, expected_rip);
    assert_eq!(got_cpu, expected_cpu);
}
