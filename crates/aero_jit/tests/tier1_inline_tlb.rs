#![cfg(debug_assertions)]

use aero_cpu::CpuState;
use aero_jit::abi::{
    CPU_AND_JIT_CTX_BYTE_SIZE, JIT_CTX_RAM_BASE_OFFSET, JIT_CTX_TLB_OFFSET, JIT_CTX_TLB_SALT_OFFSET,
};
use aero_jit::tier1_ir::{GuestReg, IrBuilder, IrTerminator};
use aero_jit::wasm::tier1::{Tier1WasmCodegen, Tier1WasmOptions};
use aero_jit::wasm::{
    EXPORT_BLOCK_FN, IMPORT_JIT_EXIT, IMPORT_JIT_EXIT_MMIO, IMPORT_MEMORY, IMPORT_MEM_READ_U16,
    IMPORT_MEM_READ_U32, IMPORT_MEM_READ_U64, IMPORT_MEM_READ_U8, IMPORT_MEM_WRITE_U16,
    IMPORT_MEM_WRITE_U32, IMPORT_MEM_WRITE_U64, IMPORT_MEM_WRITE_U8, IMPORT_MMU_TRANSLATE,
    IMPORT_MODULE, IMPORT_PAGE_FAULT,
};
use aero_jit::{
    JIT_TLB_ENTRY_SIZE, JIT_TLB_INDEX_MASK, PAGE_BASE_MASK, PAGE_SHIFT, TLB_FLAG_EXEC,
    TLB_FLAG_IS_RAM, TLB_FLAG_READ, TLB_FLAG_WRITE,
};
use aero_types::{Gpr, Width};

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
    linker
        .define(IMPORT_MODULE, IMPORT_MEMORY, memory.clone())
        .unwrap();

    define_mem_helpers(&mut store, &mut linker, memory.clone());
    define_mmu_translate(&mut store, &mut linker, memory.clone());
    define_mmio_exit(&mut store, &mut linker);

    linker
        .define(
            IMPORT_MODULE,
            IMPORT_PAGE_FAULT,
            Func::wrap(
                &mut store,
                |_caller: Caller<'_, HostState>, _cpu_ptr: i32, _addr: i64| -> i64 {
                    panic!("page_fault should not be called by tier1 inline-tlb tests");
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
                    // Sentinel mirrors `u64::MAX`.
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

fn define_mem_helpers(store: &mut Store<HostState>, linker: &mut Linker<HostState>, memory: Memory) {
    fn read<const N: usize>(caller: &mut Caller<'_, HostState>, memory: &Memory, addr: usize) -> u64 {
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

    let mem = memory.clone();
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
                        cpu_ptr as usize + JIT_CTX_RAM_BASE_OFFSET as usize,
                    ) as usize;
                    read::<1>(&mut caller, &mem, ram_base + addr as usize) as i32
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
                move |mut caller: Caller<'_, HostState>, cpu_ptr: i32, addr: i64| -> i32 {
                    caller.data_mut().slow_mem_reads += 1;
                    let ram_base = read_u64_from_memory(
                        &mut caller,
                        &mem,
                        cpu_ptr as usize + JIT_CTX_RAM_BASE_OFFSET as usize,
                    ) as usize;
                    read::<2>(&mut caller, &mem, ram_base + addr as usize) as i32
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
                move |mut caller: Caller<'_, HostState>, cpu_ptr: i32, addr: i64| -> i32 {
                    caller.data_mut().slow_mem_reads += 1;
                    let ram_base = read_u64_from_memory(
                        &mut caller,
                        &mem,
                        cpu_ptr as usize + JIT_CTX_RAM_BASE_OFFSET as usize,
                    ) as usize;
                    read::<4>(&mut caller, &mem, ram_base + addr as usize) as i32
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
                move |mut caller: Caller<'_, HostState>, cpu_ptr: i32, addr: i64| -> i64 {
                    caller.data_mut().slow_mem_reads += 1;
                    let ram_base = read_u64_from_memory(
                        &mut caller,
                        &mem,
                        cpu_ptr as usize + JIT_CTX_RAM_BASE_OFFSET as usize,
                    ) as usize;
                    read::<8>(&mut caller, &mem, ram_base + addr as usize) as i64
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
                move |mut caller: Caller<'_, HostState>, cpu_ptr: i32, addr: i64, value: i32| {
                    caller.data_mut().slow_mem_writes += 1;
                    let ram_base = read_u64_from_memory(
                        &mut caller,
                        &mem,
                        cpu_ptr as usize + JIT_CTX_RAM_BASE_OFFSET as usize,
                    ) as usize;
                    write::<1>(&mut caller, &mem, ram_base + addr as usize, value as u64);
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
                move |mut caller: Caller<'_, HostState>, cpu_ptr: i32, addr: i64, value: i32| {
                    caller.data_mut().slow_mem_writes += 1;
                    let ram_base = read_u64_from_memory(
                        &mut caller,
                        &mem,
                        cpu_ptr as usize + JIT_CTX_RAM_BASE_OFFSET as usize,
                    ) as usize;
                    write::<2>(&mut caller, &mem, ram_base + addr as usize, value as u64);
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
                move |mut caller: Caller<'_, HostState>, cpu_ptr: i32, addr: i64, value: i32| {
                    caller.data_mut().slow_mem_writes += 1;
                    let ram_base = read_u64_from_memory(
                        &mut caller,
                        &mem,
                        cpu_ptr as usize + JIT_CTX_RAM_BASE_OFFSET as usize,
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
                        cpu_ptr as usize + JIT_CTX_RAM_BASE_OFFSET as usize,
                    ) as usize;
                    write::<8>(&mut caller, &mem, ram_base + addr as usize, value as u64);
                },
            ),
        )
        .unwrap();
}

fn define_mmu_translate(store: &mut Store<HostState>, linker: &mut Linker<HostState>, memory: Memory) {
    let mem = memory;
    linker
        .define(
            IMPORT_MODULE,
            IMPORT_MMU_TRANSLATE,
            Func::wrap(
                &mut *store,
                move |mut caller: Caller<'_, HostState>, cpu_ptr: i32, vaddr: i64, _access: i32| -> i64 {
                    caller.data_mut().mmu_translate_calls += 1;

                    let vaddr_u = vaddr as u64;
                    let vpn = vaddr_u >> PAGE_SHIFT;
                    let idx = (vpn & JIT_TLB_INDEX_MASK) as u64;

                    let salt = read_u64_from_memory(
                        &mut caller,
                        &mem,
                        cpu_ptr as usize + JIT_CTX_TLB_SALT_OFFSET as usize,
                    );

                    let tag = (vpn ^ salt) | 1;

                    let is_ram = vaddr_u < caller.data().ram_size;
                    let phys_base = vaddr_u & PAGE_BASE_MASK;
                    let flags =
                        TLB_FLAG_READ | TLB_FLAG_WRITE | TLB_FLAG_EXEC | if is_ram { TLB_FLAG_IS_RAM } else { 0 };
                    let data = phys_base | flags;

                    let entry_addr = (cpu_ptr as u64)
                        + (JIT_CTX_TLB_OFFSET as u64)
                        + idx * (JIT_TLB_ENTRY_SIZE as u64);

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

fn run_wasm(block: &aero_jit::tier1_ir::IrBlock, cpu: CpuState, ram: Vec<u8>, ram_size: u64) -> (u64, CpuState, Vec<u8>, HostState) {
    let wasm = Tier1WasmCodegen::new().compile_block_with_options(
        block,
        Tier1WasmOptions {
            inline_tlb: true,
        },
    );
    validate_wasm(&wasm);

    let ram_base = CPU_AND_JIT_CTX_BYTE_SIZE as u64;
    let total_len = ram_base as usize + ram.len();

    let mut mem = vec![0u8; total_len];
    cpu.write_to_mem(&mut mem, 0);
    mem[JIT_CTX_RAM_BASE_OFFSET as usize..JIT_CTX_RAM_BASE_OFFSET as usize + 8]
        .copy_from_slice(&ram_base.to_le_bytes());
    mem[JIT_CTX_TLB_SALT_OFFSET as usize..JIT_CTX_TLB_SALT_OFFSET as usize + 8]
        .copy_from_slice(&0x1234_5678_9abc_def0u64.to_le_bytes());

    mem[ram_base as usize..ram_base as usize + ram.len()].copy_from_slice(&ram);

    let pages = ((total_len + 65535) / 65536) as u32;
    let (mut store, memory, func) = instantiate(&wasm, pages, ram_size);
    memory.write(&mut store, 0, &mem).unwrap();

    let got_rip = func.call(&mut store, 0).unwrap() as u64;

    let mut got_mem = vec![0u8; total_len];
    memory.read(&store, 0, &mut got_mem).unwrap();
    let got_cpu = CpuState::read_from_mem(&got_mem, 0);
    let got_ram = got_mem[ram_base as usize..ram_base as usize + ram.len()].to_vec();
    let host_state = *store.data();
    (got_rip, got_cpu, got_ram, host_state)
}

#[test]
fn tier1_inline_tlb_same_page_access_hits_and_caches() {
    let mut b = IrBuilder::new(0x1000);

    let addr0 = b.const_int(Width::W64, 0x1000);
    let v0 = b.const_int(Width::W8, 0xAB);
    b.store(Width::W8, addr0, v0);

    let addr1 = b.const_int(Width::W64, 0x1004);
    let v1 = b.const_int(Width::W32, 0x1234_5678);
    b.store(Width::W32, addr1, v1);

    let ld0 = b.load(Width::W8, addr0);
    let ld1 = b.load(Width::W32, addr1);

    b.write_reg(
        GuestReg::Gpr {
            reg: Gpr::Rbx,
            width: Width::W8,
            high8: false,
        },
        ld0,
    );
    b.write_reg(
        GuestReg::Gpr {
            reg: Gpr::Rax,
            width: Width::W32,
            high8: false,
        },
        ld1,
    );

    let block = b.finish(IrTerminator::Jump { target: 0x3000 });
    block.validate().unwrap();

    let mut cpu = CpuState::default();
    cpu.rip = 0x1000;

    let ram = vec![0u8; 0x10000];
    let (got_rip, got_cpu, got_ram, host_state) = run_wasm(&block, cpu, ram, 0x10000);

    assert_eq!(got_rip, 0x3000);
    assert_eq!(got_cpu.rip, 0x3000);
    assert_eq!(got_cpu.gpr[Gpr::Rax.as_u8() as usize], 0x1234_5678);
    assert_eq!(got_cpu.gpr[Gpr::Rbx.as_u8() as usize] & 0xff, 0xAB);

    assert_eq!(got_ram[0x1000], 0xAB);
    assert_eq!(
        &got_ram[0x1004..0x1008],
        &0x1234_5678u32.to_le_bytes(),
    );

    assert_eq!(host_state.mmu_translate_calls, 1);
    assert_eq!(host_state.mmio_exit_calls, 0);
    assert_eq!(host_state.slow_mem_reads, 0);
    assert_eq!(host_state.slow_mem_writes, 0);
}

#[test]
fn tier1_inline_tlb_collision_forces_retranslate() {
    let collide_addr = (aero_jit::JIT_TLB_ENTRIES as u64) << aero_jit::PAGE_SHIFT;

    let mut b = IrBuilder::new(0x1000);

    let a0 = b.const_int(Width::W64, 0);
    let a1 = b.const_int(Width::W64, collide_addr);
    let a2 = b.const_int(Width::W64, 4);

    let v0 = b.load(Width::W32, a0);
    let v1 = b.load(Width::W32, a1);
    let v2 = b.load(Width::W32, a2);

    b.write_reg(
        GuestReg::Gpr {
            reg: Gpr::Rax,
            width: Width::W32,
            high8: false,
        },
        v0,
    );
    b.write_reg(
        GuestReg::Gpr {
            reg: Gpr::Rbx,
            width: Width::W32,
            high8: false,
        },
        v1,
    );
    b.write_reg(
        GuestReg::Gpr {
            reg: Gpr::Rcx,
            width: Width::W32,
            high8: false,
        },
        v2,
    );

    let block = b.finish(IrTerminator::Jump { target: 0x3000 });
    block.validate().unwrap();

    let mut cpu = CpuState::default();
    cpu.rip = 0x1000;

    let ram_len = collide_addr as usize + 0x2000;
    let mut ram = vec![0u8; ram_len];
    ram[0..4].copy_from_slice(&0x1122_3344u32.to_le_bytes());
    ram[4..8].copy_from_slice(&0x5566_7788u32.to_le_bytes());
    ram[collide_addr as usize..collide_addr as usize + 4]
        .copy_from_slice(&0x99aa_bbccu32.to_le_bytes());

    let (got_rip, got_cpu, _got_ram, host_state) = run_wasm(&block, cpu, ram, ram_len as u64);

    assert_eq!(got_rip, 0x3000);
    assert_eq!(got_cpu.rip, 0x3000);

    // page 0, collide page, page 0 again.
    assert_eq!(host_state.mmu_translate_calls, 3);
    assert_eq!(host_state.mmio_exit_calls, 0);
    assert_eq!(host_state.slow_mem_reads, 0);
    assert_eq!(host_state.slow_mem_writes, 0);
}

#[test]
fn tier1_inline_tlb_cross_page_load_uses_slow_helper() {
    let addr = 0xFF9u64;

    let mut b = IrBuilder::new(0x1000);
    let a0 = b.const_int(Width::W64, addr);
    let v0 = b.load(Width::W64, a0);
    b.write_reg(
        GuestReg::Gpr {
            reg: Gpr::Rax,
            width: Width::W64,
            high8: false,
        },
        v0,
    );
    let block = b.finish(IrTerminator::Jump { target: 0x3000 });
    block.validate().unwrap();

    let mut cpu = CpuState::default();
    cpu.rip = 0x1000;

    let mut ram = vec![0u8; 0x10000];
    ram[addr as usize..addr as usize + 8]
        .copy_from_slice(&0x1122_3344_5566_7788u64.to_le_bytes());

    let (got_rip, got_cpu, _got_ram, host_state) = run_wasm(&block, cpu, ram, 0x10000);

    assert_eq!(got_rip, 0x3000);
    assert_eq!(got_cpu.rip, 0x3000);
    assert_eq!(got_cpu.gpr[Gpr::Rax.as_u8() as usize], 0x1122_3344_5566_7788);
    assert_eq!(host_state.mmu_translate_calls, 0);
    assert_eq!(host_state.mmio_exit_calls, 0);
    assert_eq!(host_state.slow_mem_reads, 1);
    assert_eq!(host_state.slow_mem_writes, 0);
}

#[test]
fn tier1_inline_tlb_mmio_load_exits_to_runtime() {
    let mut b = IrBuilder::new(0x1000);
    let addr = b.const_int(Width::W64, 0xF000);
    let _ = b.load(Width::W32, addr);
    let block = b.finish(IrTerminator::Jump { target: 0x3000 });
    block.validate().unwrap();

    let mut cpu = CpuState::default();
    cpu.rip = 0x1000;

    let ram = vec![0u8; 0x10000];
    let (got_rip, got_cpu, _got_ram, host_state) = run_wasm(&block, cpu, ram, 0x8000);

    assert_eq!(got_rip, u64::MAX);
    assert_eq!(got_cpu.rip, u64::MAX);
    assert_eq!(host_state.mmio_exit_calls, 1);
    assert_eq!(host_state.mmu_translate_calls, 1);
    assert_eq!(host_state.slow_mem_reads, 0);
    assert_eq!(host_state.slow_mem_writes, 0);
}

#[test]
fn tier1_inline_tlb_mmio_store_exits_to_runtime() {
    let mut b = IrBuilder::new(0x1000);
    let addr = b.const_int(Width::W64, 0xF000);
    let value = b.const_int(Width::W32, 0xDEAD_BEEF);
    b.store(Width::W32, addr, value);
    let block = b.finish(IrTerminator::Jump { target: 0x3000 });
    block.validate().unwrap();

    let mut cpu = CpuState::default();
    cpu.rip = 0x1000;

    let ram = vec![0u8; 0x10000];
    let (got_rip, got_cpu, _got_ram, host_state) = run_wasm(&block, cpu, ram, 0x8000);

    assert_eq!(got_rip, u64::MAX);
    assert_eq!(got_cpu.rip, u64::MAX);
    assert_eq!(host_state.mmio_exit_calls, 1);
    assert_eq!(host_state.mmu_translate_calls, 1);
    assert_eq!(host_state.slow_mem_reads, 0);
    assert_eq!(host_state.slow_mem_writes, 0);
}

