#![cfg(all(debug_assertions, feature = "tier1-inline-tlb"))]

mod tier1_common;

use aero_cpu_core::state::CpuState;
use aero_jit_x86::abi;
use aero_jit_x86::jit_ctx::JitContext;
use aero_jit_x86::tier1::ir::{GuestReg, IrBuilder, IrTerminator};
use aero_jit_x86::tier1::{Tier1WasmCodegen, Tier1WasmOptions, EXPORT_BLOCK_FN};
use aero_jit_x86::wasm::{
    IMPORT_JIT_EXIT, IMPORT_JIT_EXIT_MMIO, IMPORT_MEMORY, IMPORT_MEM_READ_U16, IMPORT_MEM_READ_U32,
    IMPORT_MEM_READ_U64, IMPORT_MEM_READ_U8, IMPORT_MEM_WRITE_U16, IMPORT_MEM_WRITE_U32,
    IMPORT_MEM_WRITE_U64, IMPORT_MEM_WRITE_U8, IMPORT_MMU_TRANSLATE, IMPORT_MODULE,
    IMPORT_PAGE_FAULT, JIT_EXIT_SENTINEL_I64,
};
use aero_jit_x86::{
    JIT_TLB_ENTRIES, JIT_TLB_ENTRY_SIZE, JIT_TLB_INDEX_MASK, PAGE_BASE_MASK, PAGE_SHIFT,
    TLB_FLAG_EXEC, TLB_FLAG_IS_RAM, TLB_FLAG_READ, TLB_FLAG_WRITE,
};
use aero_types::{Gpr, Width};
use tier1_common::{write_cpu_to_wasm_bytes, CpuSnapshot};

use wasmi::{Caller, Engine, Func, Linker, Memory, MemoryType, Module, Store, TypedFunc};

const CPU_PTR: i32 = 0;
const JIT_CTX_PTR: i32 = abi::CPU_STATE_SIZE as i32;
const TLB_SALT: u64 = 0x1234_5678_9abc_def0;

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
) -> (Store<HostState>, Memory, TypedFunc<(i32, i32), i64>) {
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

    define_mem_helpers(&mut store, &mut linker, memory);
    define_mmu_translate(&mut store, &mut linker, memory);
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
                |_caller: Caller<'_, HostState>, _kind: i32, rip: i64| -> i64 {
                    // Like `jit_exit_mmio`, return the resume RIP while the block returns the
                    // sentinel separately.
                    rip
                },
            ),
        )
        .unwrap();

    let instance = linker.instantiate_and_start(&mut store, &module).unwrap();
    let block = instance
        .get_typed_func::<(i32, i32), i64>(&store, EXPORT_BLOCK_FN)
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

    fn read_ram_base(caller: &mut Caller<'_, HostState>, memory: &Memory, cpu_ptr: i32) -> usize {
        // Slow-path helpers only receive `cpu_ptr`; in our tests, the JIT context is stored at
        // `cpu_ptr + CPU_STATE_SIZE`.
        read_u64_from_memory(
            caller,
            memory,
            cpu_ptr as usize
                + (abi::CPU_STATE_SIZE as usize)
                + (JitContext::RAM_BASE_OFFSET as usize),
        ) as usize
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
                    let ram_base = read_ram_base(&mut caller, &mem, cpu_ptr);
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
                    let ram_base = read_ram_base(&mut caller, &mem, cpu_ptr);
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
                    let ram_base = read_ram_base(&mut caller, &mem, cpu_ptr);
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
                    let ram_base = read_ram_base(&mut caller, &mem, cpu_ptr);
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
                    let ram_base = read_ram_base(&mut caller, &mem, cpu_ptr);
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
                    let ram_base = read_ram_base(&mut caller, &mem, cpu_ptr);
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
                    let ram_base = read_ram_base(&mut caller, &mem, cpu_ptr);
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
                    let ram_base = read_ram_base(&mut caller, &mem, cpu_ptr);
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
                      _cpu_ptr: i32,
                      jit_ctx_ptr: i32,
                      vaddr: i64,
                      _access: i32|
                      -> i64 {
                    caller.data_mut().mmu_translate_calls += 1;

                    let vaddr_u = vaddr as u64;
                    let vpn = vaddr_u >> PAGE_SHIFT;
                    let idx = vpn & JIT_TLB_INDEX_MASK;

                    let salt = read_u64_from_memory(
                        &mut caller,
                        &mem,
                        jit_ctx_ptr as usize + (JitContext::TLB_SALT_OFFSET as usize),
                    );

                    let tag = (vpn ^ salt) | 1;

                    let is_ram = vaddr_u < caller.data().ram_size;
                    let phys_base = vaddr_u & PAGE_BASE_MASK;
                    let flags = TLB_FLAG_READ
                        | TLB_FLAG_WRITE
                        | TLB_FLAG_EXEC
                        | if is_ram { TLB_FLAG_IS_RAM } else { 0 };
                    let data = phys_base | flags;

                    let entry_addr = (jit_ctx_ptr as u64)
                        + (JitContext::TLB_OFFSET as u64)
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
                 rip: i64|
                 -> i64 {
                    caller.data_mut().mmio_exit_calls += 1;
                    rip
                },
            ),
        )
        .unwrap();
}

fn run_wasm_inner(
    block: &aero_jit_x86::tier1::ir::IrBlock,
    cpu: CpuState,
    ram: Vec<u8>,
    ram_size: u64,
    prefill_tlb: Option<(u64, u64)>,
    options: Tier1WasmOptions,
) -> (u64, CpuState, Vec<u8>, HostState) {
    let wasm = Tier1WasmCodegen::new().compile_block_with_options(block, options);
    validate_wasm(&wasm);

    let ram_base = (JIT_CTX_PTR as u64) + (JitContext::TOTAL_BYTE_SIZE as u64);
    let total_len = ram_base as usize + ram.len();

    let mut mem = vec![0u8; total_len];

    let mut cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    write_cpu_to_wasm_bytes(&cpu, &mut cpu_bytes);
    let cpu_base = CPU_PTR as usize;
    mem[cpu_base..cpu_base + cpu_bytes.len()].copy_from_slice(&cpu_bytes);

    let ctx = JitContext {
        ram_base,
        tlb_salt: TLB_SALT,
    };
    ctx.write_header_to_mem(&mut mem, JIT_CTX_PTR as usize);

    if let Some((vaddr, tlb_data)) = prefill_tlb {
        let vpn = vaddr >> PAGE_SHIFT;
        let idx = (vpn & JIT_TLB_INDEX_MASK) as usize;
        let entry_addr = (JIT_CTX_PTR as usize)
            + (JitContext::TLB_OFFSET as usize)
            + idx * (JIT_TLB_ENTRY_SIZE as usize);
        let tag = (vpn ^ TLB_SALT) | 1;
        mem[entry_addr..entry_addr + 8].copy_from_slice(&tag.to_le_bytes());
        mem[entry_addr + 8..entry_addr + 16].copy_from_slice(&tlb_data.to_le_bytes());
    }

    mem[ram_base as usize..ram_base as usize + ram.len()].copy_from_slice(&ram);

    let pages = total_len.div_ceil(65_536) as u32;
    let (mut store, memory, func) = instantiate(&wasm, pages, ram_size);
    memory.write(&mut store, 0, &mem).unwrap();

    let ret = func.call(&mut store, (CPU_PTR, JIT_CTX_PTR)).unwrap();

    let mut got_mem = vec![0u8; total_len];
    memory.read(&store, 0, &mut got_mem).unwrap();

    let cpu_base = CPU_PTR as usize;
    let snap =
        CpuSnapshot::from_wasm_bytes(&got_mem[cpu_base..cpu_base + abi::CPU_STATE_SIZE as usize]);
    let mut got_cpu = CpuState {
        gpr: snap.gpr,
        rip: snap.rip,
        ..Default::default()
    };
    got_cpu.set_rflags(snap.rflags);

    let next_rip = if ret == JIT_EXIT_SENTINEL_I64 {
        got_cpu.rip
    } else {
        ret as u64
    };

    let got_ram = got_mem[ram_base as usize..ram_base as usize + ram.len()].to_vec();
    let host_state = *store.data();
    (next_rip, got_cpu, got_ram, host_state)
}

fn run_wasm(
    block: &aero_jit_x86::tier1::ir::IrBlock,
    cpu: CpuState,
    ram: Vec<u8>,
    ram_size: u64,
) -> (u64, CpuState, Vec<u8>, HostState) {
    run_wasm_inner(
        block,
        cpu,
        ram,
        ram_size,
        None,
        Tier1WasmOptions {
            inline_tlb: true,
            ..Default::default()
        },
    )
}

fn run_wasm_with_prefilled_tlb(
    block: &aero_jit_x86::tier1::ir::IrBlock,
    cpu: CpuState,
    ram: Vec<u8>,
    ram_size: u64,
    vaddr: u64,
    tlb_data: u64,
) -> (u64, CpuState, Vec<u8>, HostState) {
    run_wasm_inner(
        block,
        cpu,
        ram,
        ram_size,
        Some((vaddr, tlb_data)),
        Tier1WasmOptions {
            inline_tlb: true,
            ..Default::default()
        },
    )
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

    let cpu = CpuState {
        rip: 0x1000,
        ..Default::default()
    };

    let ram = vec![0u8; 0x10000];
    let (next_rip, got_cpu, got_ram, host_state) = run_wasm(&block, cpu, ram, 0x10000);

    assert_eq!(next_rip, 0x3000);
    assert_eq!(got_cpu.rip, 0x3000);
    assert_eq!(got_cpu.gpr[Gpr::Rax.as_u8() as usize], 0x1234_5678);
    assert_eq!(got_cpu.gpr[Gpr::Rbx.as_u8() as usize] & 0xff, 0xAB);

    assert_eq!(got_ram[0x1000], 0xAB);
    assert_eq!(&got_ram[0x1004..0x1008], &0x1234_5678u32.to_le_bytes(),);

    assert_eq!(host_state.mmu_translate_calls, 1);
    assert_eq!(host_state.mmio_exit_calls, 0);
    assert_eq!(host_state.slow_mem_reads, 0);
    assert_eq!(host_state.slow_mem_writes, 0);
}

#[test]
fn tier1_inline_tlb_can_disable_store_fastpath() {
    let mut b = IrBuilder::new(0x1000);

    let addr = b.const_int(Width::W64, 0x1000);
    let v = b.const_int(Width::W32, 0x1234_5678);
    b.store(Width::W32, addr, v);

    let ld = b.load(Width::W32, addr);
    b.write_reg(
        GuestReg::Gpr {
            reg: Gpr::Rax,
            width: Width::W32,
            high8: false,
        },
        ld,
    );

    let block = b.finish(IrTerminator::Jump { target: 0x3000 });
    block.validate().unwrap();

    let cpu = CpuState {
        rip: 0x1000,
        ..Default::default()
    };
    let ram = vec![0u8; 0x10000];

    let (next_rip, got_cpu, got_ram, host_state) = run_wasm_inner(
        &block,
        cpu,
        ram,
        0x10000,
        None,
        Tier1WasmOptions {
            inline_tlb: true,
            inline_tlb_stores: false,
            ..Default::default()
        },
    );

    assert_eq!(next_rip, 0x3000);
    assert_eq!(got_cpu.rip, 0x3000);
    assert_eq!(got_cpu.gpr[Gpr::Rax.as_u8() as usize], 0x1234_5678);

    assert_eq!(&got_ram[0x1000..0x1004], &0x1234_5678u32.to_le_bytes(),);

    // Store goes through the helper path.
    assert_eq!(host_state.slow_mem_writes, 1);
    // Load still uses the inline-TLB fast-path.
    assert_eq!(host_state.slow_mem_reads, 0);
    assert_eq!(host_state.mmu_translate_calls, 1);
    assert_eq!(host_state.mmio_exit_calls, 0);
}

#[test]
fn tier1_inline_tlb_collision_forces_retranslate() {
    let collide_addr = (JIT_TLB_ENTRIES as u64) << PAGE_SHIFT;

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

    let cpu = CpuState {
        rip: 0x1000,
        ..Default::default()
    };

    let ram_len = collide_addr as usize + 0x2000;
    let mut ram = vec![0u8; ram_len];
    ram[0..4].copy_from_slice(&0x1122_3344u32.to_le_bytes());
    ram[4..8].copy_from_slice(&0x5566_7788u32.to_le_bytes());
    ram[collide_addr as usize..collide_addr as usize + 4]
        .copy_from_slice(&0x99aa_bbccu32.to_le_bytes());

    let (next_rip, got_cpu, _got_ram, host_state) = run_wasm(&block, cpu, ram, ram_len as u64);

    assert_eq!(next_rip, 0x3000);
    assert_eq!(got_cpu.rip, 0x3000);

    // page 0, collide page, page 0 again.
    assert_eq!(host_state.mmu_translate_calls, 3);
    assert_eq!(host_state.mmio_exit_calls, 0);
    assert_eq!(host_state.slow_mem_reads, 0);
    assert_eq!(host_state.slow_mem_writes, 0);
}

#[test]
fn tier1_inline_tlb_permission_miss_read_calls_translate() {
    let addr = 0x1000u64;

    let mut b = IrBuilder::new(0x1000);
    let a0 = b.const_int(Width::W64, addr);
    let v0 = b.load(Width::W8, a0);
    b.write_reg(
        GuestReg::Gpr {
            reg: Gpr::Rax,
            width: Width::W8,
            high8: false,
        },
        v0,
    );
    let block = b.finish(IrTerminator::Jump { target: 0x3000 });
    block.validate().unwrap();

    let cpu = CpuState {
        rip: 0x1000,
        ..Default::default()
    };

    let mut ram = vec![0u8; 0x10000];
    ram[addr as usize] = 0x7f;

    // Pre-fill a matching TLB entry, but intentionally omit READ permission to force a slow
    // `mmu_translate` call.
    let tlb_data = (addr & PAGE_BASE_MASK) | (TLB_FLAG_WRITE | TLB_FLAG_EXEC | TLB_FLAG_IS_RAM);

    let (next_rip, got_cpu, _got_ram, host_state) =
        run_wasm_with_prefilled_tlb(&block, cpu, ram, 0x10000, addr, tlb_data);

    assert_eq!(next_rip, 0x3000);
    assert_eq!(got_cpu.rip, 0x3000);
    assert_eq!(got_cpu.gpr[Gpr::Rax.as_u8() as usize] & 0xff, 0x7f);

    assert_eq!(host_state.mmu_translate_calls, 1);
    assert_eq!(host_state.mmio_exit_calls, 0);
    assert_eq!(host_state.slow_mem_reads, 0);
    assert_eq!(host_state.slow_mem_writes, 0);
}

#[test]
fn tier1_inline_tlb_permission_miss_write_calls_translate() {
    let addr = 0x1000u64;

    let mut b = IrBuilder::new(0x1000);
    let a0 = b.const_int(Width::W64, addr);
    let v0 = b.const_int(Width::W16, 0xdead);
    b.store(Width::W16, a0, v0);
    let block = b.finish(IrTerminator::Jump { target: 0x3000 });
    block.validate().unwrap();

    let cpu = CpuState {
        rip: 0x1000,
        ..Default::default()
    };

    let ram = vec![0u8; 0x10000];

    // Pre-fill a matching TLB entry, but intentionally omit WRITE permission.
    let tlb_data = (addr & PAGE_BASE_MASK) | (TLB_FLAG_READ | TLB_FLAG_EXEC | TLB_FLAG_IS_RAM);

    let (next_rip, got_cpu, got_ram, host_state) =
        run_wasm_with_prefilled_tlb(&block, cpu, ram, 0x10000, addr, tlb_data);

    assert_eq!(next_rip, 0x3000);
    assert_eq!(got_cpu.rip, 0x3000);
    assert_eq!(
        &got_ram[addr as usize..addr as usize + 2],
        &0xdead_u16.to_le_bytes()
    );

    assert_eq!(host_state.mmu_translate_calls, 1);
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

    let cpu = CpuState {
        rip: 0x1000,
        ..Default::default()
    };

    let mut ram = vec![0u8; 0x10000];
    ram[addr as usize..addr as usize + 8].copy_from_slice(&0x1122_3344_5566_7788u64.to_le_bytes());

    let (next_rip, got_cpu, _got_ram, host_state) = run_wasm(&block, cpu, ram, 0x10000);

    assert_eq!(next_rip, 0x3000);
    assert_eq!(got_cpu.rip, 0x3000);
    assert_eq!(
        got_cpu.gpr[Gpr::Rax.as_u8() as usize],
        0x1122_3344_5566_7788
    );
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

    let cpu = CpuState {
        rip: 0x1000,
        ..Default::default()
    };

    let ram = vec![0u8; 0x10000];
    let (next_rip, got_cpu, _got_ram, host_state) = run_wasm(&block, cpu, ram, 0x8000);

    assert_eq!(next_rip, 0x1000);
    assert_eq!(got_cpu.rip, 0x1000);
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

    let cpu = CpuState {
        rip: 0x1000,
        ..Default::default()
    };

    let ram = vec![0u8; 0x10000];
    let (next_rip, got_cpu, _got_ram, host_state) = run_wasm(&block, cpu, ram, 0x8000);

    assert_eq!(next_rip, 0x1000);
    assert_eq!(got_cpu.rip, 0x1000);
    assert_eq!(host_state.mmio_exit_calls, 1);
    assert_eq!(host_state.mmu_translate_calls, 1);
    assert_eq!(host_state.slow_mem_reads, 0);
    assert_eq!(host_state.slow_mem_writes, 0);
}

#[test]
fn tier1_inline_tlb_high_ram_remap_load_uses_contiguous_ram_offset() {
    // Q35 layout:
    // - low RAM:  [0x0000_0000 .. 0xB000_0000)
    // - hole:     [0xB000_0000 .. 0x1_0000_0000)
    // - high RAM: [0x1_0000_0000 .. ...] remapped to start at 0xB000_0000 in the contiguous RAM
    //             backing store.
    const HIGH_RAM_BASE: u64 = 0x1_0000_0000;

    // We'll point `JitContext.ram_base` at a value that causes the correct high-RAM remap to wrap
    // the final wasm32 address into a small in-bounds offset, while the buggy identity-mapped
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

    let mut b = IrBuilder::new(0x1000);
    let addr = b.const_int(Width::W64, HIGH_RAM_BASE);
    let v0 = b.load(Width::W8, addr);
    b.write_reg(
        GuestReg::Gpr {
            reg: Gpr::Rax,
            width: Width::W8,
            high8: false,
        },
        v0,
    );
    let block = b.finish(IrTerminator::Jump { target: 0x3000 });
    block.validate().unwrap();

    let cpu = CpuState {
        rip: 0x1000,
        ..Default::default()
    };

    let wasm = Tier1WasmCodegen::new().compile_block_with_options(
        &block,
        Tier1WasmOptions {
            inline_tlb: true,
            ..Default::default()
        },
    );
    validate_wasm(&wasm);

    // Memory layout: keep it tiny, but large enough to hold CPU + JitContext + the desired test
    // byte at `desired_offset`.
    let mut mem = vec![0u8; desired_offset + 16];

    let mut cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    write_cpu_to_wasm_bytes(&cpu, &mut cpu_bytes);
    mem[CPU_PTR as usize..CPU_PTR as usize + cpu_bytes.len()].copy_from_slice(&cpu_bytes);

    let ctx = JitContext {
        ram_base,
        tlb_salt: TLB_SALT,
    };
    ctx.write_header_to_mem(&mut mem, JIT_CTX_PTR as usize);

    // Place the known byte at the address we expect the *correct* high-RAM remap to compute.
    mem[desired_offset] = 0x7f;

    let pages = mem.len().div_ceil(65_536) as u32;
    // Make `mmu_translate` classify 4GiB as RAM so the inline-TLB fast-path is taken.
    let ram_size = HIGH_RAM_BASE + 0x1000;
    let (mut store, memory, func) = instantiate(&wasm, pages, ram_size);
    memory.write(&mut store, 0, &mem).unwrap();

    let ret = func.call(&mut store, (CPU_PTR, JIT_CTX_PTR)).unwrap();
    assert_eq!(ret, 0x3000);

    let mut got_mem = vec![0u8; mem.len()];
    memory.read(&store, 0, &mut got_mem).unwrap();

    let snap = CpuSnapshot::from_wasm_bytes(&got_mem[0..abi::CPU_STATE_SIZE as usize]);
    assert_eq!(snap.rip, 0x3000);
    assert_eq!(snap.gpr[Gpr::Rax.as_u8() as usize] & 0xff, 0x7f);

    let host_state = *store.data();
    assert_eq!(host_state.mmu_translate_calls, 1);
    assert_eq!(host_state.mmio_exit_calls, 0);
    assert_eq!(host_state.slow_mem_reads, 0);
    assert_eq!(host_state.slow_mem_writes, 0);
}

#[test]
fn tier1_inline_tlb_high_ram_remap_store_uses_contiguous_ram_offset() {
    const HIGH_RAM_BASE: u64 = 0x1_0000_0000;

    let desired_offset: usize = 0x10000;
    let ram_base: u64 = 0x5000_0000 + desired_offset as u64;

    let mut b = IrBuilder::new(0x1000);
    let addr = b.const_int(Width::W64, HIGH_RAM_BASE);
    let v0 = b.const_int(Width::W8, 0xab);
    b.store(Width::W8, addr, v0);
    let block = b.finish(IrTerminator::Jump { target: 0x3000 });
    block.validate().unwrap();

    let cpu = CpuState {
        rip: 0x1000,
        ..Default::default()
    };

    let wasm = Tier1WasmCodegen::new().compile_block_with_options(
        &block,
        Tier1WasmOptions {
            inline_tlb: true,
            ..Default::default()
        },
    );
    validate_wasm(&wasm);

    let mut mem = vec![0u8; desired_offset + 16];

    let mut cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    write_cpu_to_wasm_bytes(&cpu, &mut cpu_bytes);
    mem[CPU_PTR as usize..CPU_PTR as usize + cpu_bytes.len()].copy_from_slice(&cpu_bytes);

    let ctx = JitContext {
        ram_base,
        tlb_salt: TLB_SALT,
    };
    ctx.write_header_to_mem(&mut mem, JIT_CTX_PTR as usize);

    let pages = mem.len().div_ceil(65_536) as u32;
    let ram_size = HIGH_RAM_BASE + 0x1000;
    let (mut store, memory, func) = instantiate(&wasm, pages, ram_size);
    memory.write(&mut store, 0, &mem).unwrap();

    let ret = func.call(&mut store, (CPU_PTR, JIT_CTX_PTR)).unwrap();
    assert_eq!(ret, 0x3000);

    let mut got_mem = vec![0u8; mem.len()];
    memory.read(&store, 0, &mut got_mem).unwrap();

    assert_eq!(got_mem[desired_offset], 0xab);

    let host_state = *store.data();
    assert_eq!(host_state.mmu_translate_calls, 1);
    assert_eq!(host_state.mmio_exit_calls, 0);
    assert_eq!(host_state.slow_mem_reads, 0);
    assert_eq!(host_state.slow_mem_writes, 0);
}
