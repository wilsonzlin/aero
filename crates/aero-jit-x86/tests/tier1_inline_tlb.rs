#![cfg(all(debug_assertions, feature = "tier1-inline-tlb"))]

mod tier1_common;

use aero_cpu_core::state::CpuState;
use aero_jit_x86::abi;
use aero_jit_x86::jit_ctx::{self, JitContext};
use aero_jit_x86::tier1::ir::{GuestReg, IrBlock, IrBuilder, IrInst, IrTerminator};
use aero_jit_x86::tier1::{
    discover_block_mode, translate_block, BlockLimits, Tier1WasmCodegen, Tier1WasmOptions,
    EXPORT_BLOCK_FN,
};
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
use tier1_common::{write_cpu_to_wasm_bytes, CpuSnapshot, SimpleBus};

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
    last_mmio: Option<MmioExit>,
    // Test-only override used to simulate non-identity page mappings.
    override_vpn: Option<u64>,
    override_phys_base: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MmioExit {
    vaddr: u64,
    size: u32,
    is_write: bool,
    value: u64,
    rip: u64,
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
                    let phys_base = match caller.data().override_vpn {
                        Some(override_vpn) if override_vpn == vpn => {
                            caller.data().override_phys_base & PAGE_BASE_MASK
                        }
                        _ => vaddr_u & PAGE_BASE_MASK,
                    };
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
                 vaddr: i64,
                 size: i32,
                 is_write: i32,
                 value: i64,
                 rip: i64|
                 -> i64 {
                    caller.data_mut().mmio_exit_calls += 1;
                    caller.data_mut().last_mmio = Some(MmioExit {
                        vaddr: vaddr as u64,
                        size: size as u32,
                        is_write: is_write != 0,
                        value: value as u64,
                        rip: rip as u64,
                    });
                    rip
                },
            ),
        )
        .unwrap();
}

fn run_wasm_inner_with_prefilled_tlbs(
    block: &aero_jit_x86::tier1::ir::IrBlock,
    cpu: CpuState,
    ram: Vec<u8>,
    ram_size: u64,
    prefill_tlbs: &[(u64, u64)],
    options: Tier1WasmOptions,
) -> (u64, CpuState, Vec<u8>, HostState) {
    let wasm = Tier1WasmCodegen::new().compile_block_with_options(block, options);
    validate_wasm(&wasm);

    // Match the real runtime layout: reserve the Tier-2 ctx region (which contains the code-version
    // table pointer/length) between the Tier-1 `JitContext` and the guest RAM backing store.
    let ram_base = (JIT_CTX_PTR as u64)
        + (JitContext::TOTAL_BYTE_SIZE as u64)
        + u64::from(jit_ctx::TIER2_CTX_SIZE);
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

    for &(vaddr, tlb_data) in prefill_tlbs {
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

fn run_wasm_inner(
    block: &aero_jit_x86::tier1::ir::IrBlock,
    cpu: CpuState,
    ram: Vec<u8>,
    ram_size: u64,
    prefill_tlb: Option<(u64, u64)>,
    options: Tier1WasmOptions,
) -> (u64, CpuState, Vec<u8>, HostState) {
    match prefill_tlb {
        Some(entry) => {
            run_wasm_inner_with_prefilled_tlbs(block, cpu, ram, ram_size, &[entry], options)
        }
        None => run_wasm_inner_with_prefilled_tlbs(block, cpu, ram, ram_size, &[], options),
    }
}

fn run_wasm_inner_with_code_version_table(
    block: &aero_jit_x86::tier1::ir::IrBlock,
    cpu: CpuState,
    ram: Vec<u8>,
    ram_size: u64,
    options: Tier1WasmOptions,
    code_version_table_len: u32,
) -> (u64, CpuState, Vec<u8>, HostState, Vec<u32>) {
    let wasm = Tier1WasmCodegen::new().compile_block_with_options(block, options);
    validate_wasm(&wasm);

    // Place the Tier-2 context and code-version table between the Tier-1 `JitContext` and the RAM
    // backing store (matching the real runtime layout).
    let table_ptr = u64::from(jit_ctx::TIER2_CTX_OFFSET + jit_ctx::TIER2_CTX_SIZE);
    let table_bytes = usize::try_from(code_version_table_len)
        .unwrap()
        .checked_mul(4)
        .unwrap();
    let ram_base = table_ptr + (table_bytes as u64);

    let total_len = ram_base as usize + ram.len();
    let mut mem = vec![0u8; total_len];

    let mut cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    write_cpu_to_wasm_bytes(&cpu, &mut cpu_bytes);
    let cpu_base = CPU_PTR as usize;
    mem[cpu_base..cpu_base + cpu_bytes.len()].copy_from_slice(&cpu_bytes);

    // Configure Tier-1 JIT context.
    let ctx = JitContext {
        ram_base,
        tlb_salt: TLB_SALT,
    };
    ctx.write_header_to_mem(&mut mem, JIT_CTX_PTR as usize);

    // Configure code-version table.
    mem[jit_ctx::CODE_VERSION_TABLE_PTR_OFFSET as usize
        ..jit_ctx::CODE_VERSION_TABLE_PTR_OFFSET as usize + 4]
        .copy_from_slice(&(table_ptr as u32).to_le_bytes());
    mem[jit_ctx::CODE_VERSION_TABLE_LEN_OFFSET as usize
        ..jit_ctx::CODE_VERSION_TABLE_LEN_OFFSET as usize + 4]
        .copy_from_slice(&code_version_table_len.to_le_bytes());

    // RAM backing store.
    mem[ram_base as usize..ram_base as usize + ram.len()].copy_from_slice(&ram);

    let pages = total_len.div_ceil(65_536) as u32;
    let (mut store, memory, func) = instantiate(&wasm, pages, ram_size);
    memory.write(&mut store, 0, &mem).unwrap();

    let ret = func.call(&mut store, (CPU_PTR, JIT_CTX_PTR)).unwrap();

    let mut got_mem = vec![0u8; total_len];
    memory.read(&store, 0, &mut got_mem).unwrap();

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

    let mut table = Vec::new();
    for i in 0..code_version_table_len {
        let off = table_ptr as usize + (i as usize) * 4;
        table.push(u32::from_le_bytes(
            got_mem[off..off + 4].try_into().unwrap(),
        ));
    }

    let host_state = *store.data();
    (next_rip, got_cpu, got_ram, host_state, table)
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
fn tier1_inline_tlb_cross_page_load_can_use_fastpath_when_enabled() {
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

    let (next_rip, got_cpu, _got_ram, host_state) = run_wasm_inner(
        &block,
        cpu,
        ram,
        0x10000,
        None,
        Tier1WasmOptions {
            inline_tlb: true,
            inline_tlb_cross_page_fastpath: true,
            ..Default::default()
        },
    );

    assert_eq!(next_rip, 0x3000);
    assert_eq!(got_cpu.rip, 0x3000);
    assert_eq!(
        got_cpu.gpr[Gpr::Rax.as_u8() as usize],
        0x1122_3344_5566_7788
    );

    assert!(host_state.mmu_translate_calls <= 2);
    assert_eq!(host_state.mmio_exit_calls, 0);
    assert_eq!(host_state.slow_mem_reads, 0);
    assert_eq!(host_state.slow_mem_writes, 0);
}

#[test]
fn tier1_inline_tlb_cross_page_store_can_use_fastpath_when_enabled() {
    let addr = 0xFF9u64;

    let mut b = IrBuilder::new(0x1000);
    let a0 = b.const_int(Width::W64, addr);
    let v0 = b.const_int(Width::W64, 0x1122_3344_5566_7788);
    b.store(Width::W64, a0, v0);
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
            inline_tlb_cross_page_fastpath: true,
            ..Default::default()
        },
    );

    assert_eq!(next_rip, 0x3000);
    assert_eq!(got_cpu.rip, 0x3000);
    assert_eq!(
        &got_ram[addr as usize..addr as usize + 8],
        &0x1122_3344_5566_7788u64.to_le_bytes(),
    );

    assert!(host_state.mmu_translate_calls <= 2);
    assert_eq!(host_state.mmio_exit_calls, 0);
    assert_eq!(host_state.slow_mem_reads, 0);
    assert_eq!(host_state.slow_mem_writes, 0);
}

#[test]
fn tier1_inline_tlb_cross_page_load_fastpath_hits_prefilled_tlb_entries() {
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

    let mut ram = vec![0u8; 0x2000];
    ram[addr as usize..addr as usize + 8].copy_from_slice(&0x1122_3344_5566_7788u64.to_le_bytes());

    // Pre-fill both pages into the inline TLB so the split-access fast-path doesn't need to call
    // `mmu_translate`.
    let flags = TLB_FLAG_READ | TLB_FLAG_WRITE | TLB_FLAG_EXEC | TLB_FLAG_IS_RAM;
    let page0_data = (addr & PAGE_BASE_MASK) | flags;
    let page1_vaddr = 0x1000u64;
    let page1_data = (page1_vaddr & PAGE_BASE_MASK) | flags;

    let (next_rip, got_cpu, _got_ram, host_state) = run_wasm_inner_with_prefilled_tlbs(
        &block,
        cpu,
        ram,
        0x2000,
        &[(addr, page0_data), (page1_vaddr, page1_data)],
        Tier1WasmOptions {
            inline_tlb: true,
            inline_tlb_cross_page_fastpath: true,
            ..Default::default()
        },
    );

    assert_eq!(next_rip, 0x3000);
    assert_eq!(got_cpu.rip, 0x3000);
    assert_eq!(
        got_cpu.gpr[Gpr::Rax.as_u8() as usize],
        0x1122_3344_5566_7788
    );

    assert_eq!(host_state.mmu_translate_calls, 0);
    assert_eq!(host_state.mmio_exit_calls, 0);
    assert_eq!(host_state.slow_mem_reads, 0);
    assert_eq!(host_state.slow_mem_writes, 0);
}

#[test]
fn tier1_inline_tlb_cross_page_store_fastpath_hits_prefilled_tlb_entries() {
    let addr = 0xFF9u64;

    let mut b = IrBuilder::new(0x1000);
    let a0 = b.const_int(Width::W64, addr);
    let v0 = b.const_int(Width::W64, 0x1122_3344_5566_7788);
    b.store(Width::W64, a0, v0);
    let block = b.finish(IrTerminator::Jump { target: 0x3000 });
    block.validate().unwrap();

    let cpu = CpuState {
        rip: 0x1000,
        ..Default::default()
    };

    let ram = vec![0u8; 0x2000];

    let flags = TLB_FLAG_READ | TLB_FLAG_WRITE | TLB_FLAG_EXEC | TLB_FLAG_IS_RAM;
    let page0_data = (addr & PAGE_BASE_MASK) | flags;
    let page1_vaddr = 0x1000u64;
    let page1_data = (page1_vaddr & PAGE_BASE_MASK) | flags;

    let (next_rip, got_cpu, got_ram, host_state) = run_wasm_inner_with_prefilled_tlbs(
        &block,
        cpu,
        ram,
        0x2000,
        &[(addr, page0_data), (page1_vaddr, page1_data)],
        Tier1WasmOptions {
            inline_tlb: true,
            inline_tlb_cross_page_fastpath: true,
            ..Default::default()
        },
    );

    assert_eq!(next_rip, 0x3000);
    assert_eq!(got_cpu.rip, 0x3000);
    assert_eq!(
        &got_ram[addr as usize..addr as usize + 8],
        &0x1122_3344_5566_7788u64.to_le_bytes(),
    );

    assert_eq!(host_state.mmu_translate_calls, 0);
    assert_eq!(host_state.mmio_exit_calls, 0);
    assert_eq!(host_state.slow_mem_reads, 0);
    assert_eq!(host_state.slow_mem_writes, 0);
}

#[test]
fn tier1_inline_tlb_cross_page_load_fastpath_hits_prefilled_tlb_entries_with_tlb_index_wrap() {
    // Ensure the split-access fast-path handles TLB index wrap (idx 255 -> 0) correctly.
    let addr = (JIT_TLB_INDEX_MASK << PAGE_SHIFT) + 0xFF9;

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

    let mut ram = vec![0u8; ((JIT_TLB_INDEX_MASK + 2) << PAGE_SHIFT) as usize];
    ram[addr as usize..addr as usize + 8].copy_from_slice(&0x1122_3344_5566_7788u64.to_le_bytes());

    // Pre-fill both pages (vpn 255 and 256) so the cross-page fast-path doesn't call
    // `mmu_translate`.
    let flags = TLB_FLAG_READ | TLB_FLAG_WRITE | TLB_FLAG_EXEC | TLB_FLAG_IS_RAM;
    let page0_data = (addr & PAGE_BASE_MASK) | flags;
    let page1_vaddr = (JIT_TLB_ENTRIES as u64) << PAGE_SHIFT; // vpn 256
    let page1_data = (page1_vaddr & PAGE_BASE_MASK) | flags;

    let (next_rip, got_cpu, _got_ram, host_state) = run_wasm_inner_with_prefilled_tlbs(
        &block,
        cpu,
        ram,
        (JIT_TLB_INDEX_MASK + 2) << PAGE_SHIFT,
        &[(addr, page0_data), (page1_vaddr, page1_data)],
        Tier1WasmOptions {
            inline_tlb: true,
            inline_tlb_cross_page_fastpath: true,
            ..Default::default()
        },
    );

    assert_eq!(next_rip, 0x3000);
    assert_eq!(got_cpu.rip, 0x3000);
    assert_eq!(
        got_cpu.gpr[Gpr::Rax.as_u8() as usize],
        0x1122_3344_5566_7788
    );

    assert_eq!(host_state.mmu_translate_calls, 0);
    assert_eq!(host_state.mmio_exit_calls, 0);
    assert_eq!(host_state.slow_mem_reads, 0);
    assert_eq!(host_state.slow_mem_writes, 0);
}

#[test]
fn tier1_inline_tlb_cross_page_store_fastpath_hits_prefilled_tlb_entries_with_tlb_index_wrap() {
    // Ensure the split-store fast-path handles TLB index wrap (idx 255 -> 0) correctly.
    let addr = (JIT_TLB_INDEX_MASK << PAGE_SHIFT) + 0xFF9;

    let mut b = IrBuilder::new(0x1000);
    let a0 = b.const_int(Width::W64, addr);
    let v0 = b.const_int(Width::W64, 0x1122_3344_5566_7788);
    b.store(Width::W64, a0, v0);
    let block = b.finish(IrTerminator::Jump { target: 0x3000 });
    block.validate().unwrap();

    let cpu = CpuState {
        rip: 0x1000,
        ..Default::default()
    };

    let ram = vec![0u8; ((JIT_TLB_INDEX_MASK + 2) << PAGE_SHIFT) as usize];

    let flags = TLB_FLAG_READ | TLB_FLAG_WRITE | TLB_FLAG_EXEC | TLB_FLAG_IS_RAM;
    let page0_data = (addr & PAGE_BASE_MASK) | flags;
    let page1_vaddr = (JIT_TLB_ENTRIES as u64) << PAGE_SHIFT; // vpn 256
    let page1_data = (page1_vaddr & PAGE_BASE_MASK) | flags;

    let (next_rip, got_cpu, got_ram, host_state) = run_wasm_inner_with_prefilled_tlbs(
        &block,
        cpu,
        ram,
        (JIT_TLB_INDEX_MASK + 2) << PAGE_SHIFT,
        &[(addr, page0_data), (page1_vaddr, page1_data)],
        Tier1WasmOptions {
            inline_tlb: true,
            inline_tlb_cross_page_fastpath: true,
            ..Default::default()
        },
    );

    assert_eq!(next_rip, 0x3000);
    assert_eq!(got_cpu.rip, 0x3000);
    assert_eq!(
        &got_ram[addr as usize..addr as usize + 8],
        &0x1122_3344_5566_7788u64.to_le_bytes(),
    );

    assert_eq!(host_state.mmu_translate_calls, 0);
    assert_eq!(host_state.mmio_exit_calls, 0);
    assert_eq!(host_state.slow_mem_reads, 0);
    assert_eq!(host_state.slow_mem_writes, 0);
}

#[test]
fn tier1_inline_tlb_cross_page_load_fastpath_permission_miss_on_second_page_calls_translate() {
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

    let mut ram = vec![0u8; 0x2000];
    ram[addr as usize..addr as usize + 8].copy_from_slice(&0x1122_3344_5566_7788u64.to_le_bytes());

    // Pre-fill both pages, but omit READ permission on the second page to force a translate due to
    // the permission check.
    let flags_page0 = TLB_FLAG_READ | TLB_FLAG_WRITE | TLB_FLAG_EXEC | TLB_FLAG_IS_RAM;
    let page0_data = (addr & PAGE_BASE_MASK) | flags_page0;
    let page1_vaddr = 0x1000u64;
    let flags_page1 = TLB_FLAG_WRITE | TLB_FLAG_EXEC | TLB_FLAG_IS_RAM; // missing READ
    let page1_data = (page1_vaddr & PAGE_BASE_MASK) | flags_page1;

    let (next_rip, got_cpu, _got_ram, host_state) = run_wasm_inner_with_prefilled_tlbs(
        &block,
        cpu,
        ram,
        0x2000,
        &[(addr, page0_data), (page1_vaddr, page1_data)],
        Tier1WasmOptions {
            inline_tlb: true,
            inline_tlb_cross_page_fastpath: true,
            ..Default::default()
        },
    );

    assert_eq!(next_rip, 0x3000);
    assert_eq!(got_cpu.rip, 0x3000);
    assert_eq!(
        got_cpu.gpr[Gpr::Rax.as_u8() as usize],
        0x1122_3344_5566_7788
    );

    assert_eq!(host_state.mmu_translate_calls, 1);
    assert_eq!(host_state.mmio_exit_calls, 0);
    assert_eq!(host_state.slow_mem_reads, 0);
    assert_eq!(host_state.slow_mem_writes, 0);
}

#[test]
fn tier1_inline_tlb_cross_page_store_fastpath_permission_miss_on_second_page_calls_translate() {
    let addr = 0xFF9u64;

    let mut b = IrBuilder::new(0x1000);
    let a0 = b.const_int(Width::W64, addr);
    let v0 = b.const_int(Width::W64, 0x1122_3344_5566_7788);
    b.store(Width::W64, a0, v0);
    let block = b.finish(IrTerminator::Jump { target: 0x3000 });
    block.validate().unwrap();

    let cpu = CpuState {
        rip: 0x1000,
        ..Default::default()
    };

    let ram = vec![0u8; 0x2000];

    // Pre-fill both pages, but omit WRITE permission on the second page to force a translate due to
    // the permission check.
    let flags_page0 = TLB_FLAG_READ | TLB_FLAG_WRITE | TLB_FLAG_EXEC | TLB_FLAG_IS_RAM;
    let page0_data = (addr & PAGE_BASE_MASK) | flags_page0;
    let page1_vaddr = 0x1000u64;
    let flags_page1 = TLB_FLAG_READ | TLB_FLAG_EXEC | TLB_FLAG_IS_RAM; // missing WRITE
    let page1_data = (page1_vaddr & PAGE_BASE_MASK) | flags_page1;

    let (next_rip, got_cpu, got_ram, host_state) = run_wasm_inner_with_prefilled_tlbs(
        &block,
        cpu,
        ram,
        0x2000,
        &[(addr, page0_data), (page1_vaddr, page1_data)],
        Tier1WasmOptions {
            inline_tlb: true,
            inline_tlb_cross_page_fastpath: true,
            ..Default::default()
        },
    );

    assert_eq!(next_rip, 0x3000);
    assert_eq!(got_cpu.rip, 0x3000);
    assert_eq!(
        &got_ram[addr as usize..addr as usize + 8],
        &0x1122_3344_5566_7788u64.to_le_bytes(),
    );

    assert_eq!(host_state.mmu_translate_calls, 1);
    assert_eq!(host_state.mmio_exit_calls, 0);
    assert_eq!(host_state.slow_mem_reads, 0);
    assert_eq!(host_state.slow_mem_writes, 0);
}

#[test]
fn tier1_inline_tlb_cross_page_load_fastpath_handles_noncontiguous_physical_pages() {
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

    let wasm = Tier1WasmCodegen::new().compile_block_with_options(
        &block,
        Tier1WasmOptions {
            inline_tlb: true,
            inline_tlb_cross_page_fastpath: true,
            ..Default::default()
        },
    );
    validate_wasm(&wasm);

    // Match `run_wasm_inner` layout.
    let ram_base = (JIT_CTX_PTR as u64)
        + (JitContext::TOTAL_BYTE_SIZE as u64)
        + u64::from(jit_ctx::TIER2_CTX_SIZE);

    // 3 pages of RAM: page 0 contains the first 7 bytes of the load, while virtual page 1 is
    // mapped to physical page 2 (0x2000).
    let mut ram = vec![0u8; 0x3000];
    ram[0xFF9..0x1000].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7]);
    ram[0x2000] = 8;

    let total_len = ram_base as usize + ram.len();
    let mut mem = vec![0u8; total_len];

    let mut cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    write_cpu_to_wasm_bytes(&cpu, &mut cpu_bytes);
    mem[CPU_PTR as usize..CPU_PTR as usize + cpu_bytes.len()].copy_from_slice(&cpu_bytes);

    let ctx = JitContext {
        ram_base,
        tlb_salt: TLB_SALT,
    };
    ctx.write_header_to_mem(&mut mem, JIT_CTX_PTR as usize);

    mem[ram_base as usize..ram_base as usize + ram.len()].copy_from_slice(&ram);

    let pages = total_len.div_ceil(65_536) as u32;
    let (mut store, memory, func) = instantiate(&wasm, pages, 0x3000);
    memory.write(&mut store, 0, &mem).unwrap();

    // Map VPN 1 (vaddr 0x1000..0x1FFF) to physical page 2 (paddr 0x2000..0x2FFF).
    store.data_mut().override_vpn = Some(1);
    store.data_mut().override_phys_base = 0x2000;

    let ret = func.call(&mut store, (CPU_PTR, JIT_CTX_PTR)).unwrap();
    assert_eq!(ret, 0x3000);

    let mut got_mem = vec![0u8; total_len];
    memory.read(&store, 0, &mut got_mem).unwrap();
    let snap = CpuSnapshot::from_wasm_bytes(&got_mem[0..abi::CPU_STATE_SIZE as usize]);

    assert_eq!(snap.rip, 0x3000);
    assert_eq!(snap.gpr[Gpr::Rax.as_u8() as usize], 0x0807_0605_0403_0201);

    let host_state = *store.data();
    assert_eq!(host_state.mmu_translate_calls, 2);
    assert_eq!(host_state.mmio_exit_calls, 0);
    assert_eq!(host_state.slow_mem_reads, 0);
    assert_eq!(host_state.slow_mem_writes, 0);
}

#[test]
fn tier1_inline_tlb_cross_page_store_fastpath_handles_noncontiguous_physical_pages() {
    let addr = 0xFF9u64;

    let mut b = IrBuilder::new(0x1000);
    let a0 = b.const_int(Width::W64, addr);
    let v0 = b.const_int(Width::W64, 0x0807_0605_0403_0201);
    b.store(Width::W64, a0, v0);
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
            inline_tlb_cross_page_fastpath: true,
            ..Default::default()
        },
    );
    validate_wasm(&wasm);

    // Configure a code-version table so the inline store fast-path will bump page versions.
    let code_version_table_len = 4u32;
    let table_ptr = u64::from(jit_ctx::TIER2_CTX_OFFSET + jit_ctx::TIER2_CTX_SIZE);
    let table_bytes = usize::try_from(code_version_table_len)
        .unwrap()
        .checked_mul(4)
        .unwrap();
    let ram_base = table_ptr + (table_bytes as u64);

    let ram_len = 0x3000usize;
    let total_len = ram_base as usize + ram_len;
    let mut mem = vec![0u8; total_len];

    let mut cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    write_cpu_to_wasm_bytes(&cpu, &mut cpu_bytes);
    mem[CPU_PTR as usize..CPU_PTR as usize + cpu_bytes.len()].copy_from_slice(&cpu_bytes);

    let ctx = JitContext {
        ram_base,
        tlb_salt: TLB_SALT,
    };
    ctx.write_header_to_mem(&mut mem, JIT_CTX_PTR as usize);

    mem[jit_ctx::CODE_VERSION_TABLE_PTR_OFFSET as usize
        ..jit_ctx::CODE_VERSION_TABLE_PTR_OFFSET as usize + 4]
        .copy_from_slice(&(table_ptr as u32).to_le_bytes());
    mem[jit_ctx::CODE_VERSION_TABLE_LEN_OFFSET as usize
        ..jit_ctx::CODE_VERSION_TABLE_LEN_OFFSET as usize + 4]
        .copy_from_slice(&code_version_table_len.to_le_bytes());

    let pages = total_len.div_ceil(65_536) as u32;
    let (mut store, memory, func) = instantiate(&wasm, pages, 0x3000);
    memory.write(&mut store, 0, &mem).unwrap();

    store.data_mut().override_vpn = Some(1);
    store.data_mut().override_phys_base = 0x2000;

    let ret = func.call(&mut store, (CPU_PTR, JIT_CTX_PTR)).unwrap();
    assert_eq!(ret, 0x3000);

    let mut got_mem = vec![0u8; total_len];
    memory.read(&store, 0, &mut got_mem).unwrap();

    let got_ram = got_mem[ram_base as usize..ram_base as usize + ram_len].to_vec();
    assert_eq!(&got_ram[0xFF9..0x1000], &[1, 2, 3, 4, 5, 6, 7]);
    assert_eq!(got_ram[0x2000], 8);
    // Ensure we really used the remapped physical page for the final byte.
    assert_eq!(got_ram[0x1000], 0);

    // Code-version table bumps should target physical pages, not virtual pages.
    let mut table = Vec::new();
    for i in 0..code_version_table_len {
        let off = table_ptr as usize + (i as usize) * 4;
        table.push(u32::from_le_bytes(
            got_mem[off..off + 4].try_into().unwrap(),
        ));
    }
    assert_eq!(table, vec![1, 0, 1, 0]);

    let host_state = *store.data();
    assert_eq!(host_state.mmio_exit_calls, 0);
    assert_eq!(host_state.slow_mem_reads, 0);
    assert_eq!(host_state.slow_mem_writes, 0);
    assert_eq!(host_state.mmu_translate_calls, 2);
}

#[test]
fn tier1_inline_tlb_cross_page_load_fastpath_wraps_u64_address_space() {
    // x86 effective addresses wrap modulo 2^64. A wide unaligned load can therefore cross the
    // u64 boundary and wrap to vaddr=0.
    //
    // Choose an address such that `addr + 7 == 0` (for a W64 cross-page load where shift_bytes=1).
    let addr = u64::MAX - 6;
    let hi_vpn = addr >> PAGE_SHIFT;

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

    let wasm = Tier1WasmCodegen::new().compile_block_with_options(
        &block,
        Tier1WasmOptions {
            inline_tlb: true,
            inline_tlb_cross_page_fastpath: true,
            ..Default::default()
        },
    );
    validate_wasm(&wasm);

    // Match `run_wasm_inner` layout.
    let ram_base = (JIT_CTX_PTR as u64)
        + (JitContext::TOTAL_BYTE_SIZE as u64)
        + u64::from(jit_ctx::TIER2_CTX_SIZE);

    // Provide RAM for two physical pages:
    // - the wrapped-to-zero page (phys page 0) supplies the final high byte
    // - the high virtual page is remapped to phys page 1 (0x1000) to keep the access in-bounds
    let mut ram = vec![0u8; 0x2000];
    ram[0] = 8;
    ram[0x1FF9..0x2000].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7]);

    let total_len = ram_base as usize + ram.len();
    let mut mem = vec![0u8; total_len];

    let mut cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    write_cpu_to_wasm_bytes(&cpu, &mut cpu_bytes);
    mem[CPU_PTR as usize..CPU_PTR as usize + cpu_bytes.len()].copy_from_slice(&cpu_bytes);

    let ctx = JitContext {
        ram_base,
        tlb_salt: TLB_SALT,
    };
    ctx.write_header_to_mem(&mut mem, JIT_CTX_PTR as usize);

    mem[ram_base as usize..ram_base as usize + ram.len()].copy_from_slice(&ram);

    let pages = total_len.div_ceil(65_536) as u32;
    let (mut store, memory, func) = instantiate(&wasm, pages, u64::MAX);
    memory.write(&mut store, 0, &mem).unwrap();

    // Remap the high VPN into a small physical page so the test can allocate backing RAM.
    store.data_mut().override_vpn = Some(hi_vpn);
    store.data_mut().override_phys_base = 0x1000;

    let ret = func.call(&mut store, (CPU_PTR, JIT_CTX_PTR)).unwrap();
    assert_eq!(ret, 0x3000);

    let mut got_mem = vec![0u8; total_len];
    memory.read(&store, 0, &mut got_mem).unwrap();
    let snap = CpuSnapshot::from_wasm_bytes(&got_mem[0..abi::CPU_STATE_SIZE as usize]);

    assert_eq!(snap.rip, 0x3000);
    assert_eq!(
        snap.gpr[Gpr::Rax.as_u8() as usize],
        0x0807_0605_0403_0201
    );

    let host_state = *store.data();
    assert_eq!(host_state.mmio_exit_calls, 0);
    assert_eq!(host_state.slow_mem_reads, 0);
    assert_eq!(host_state.slow_mem_writes, 0);
    assert_eq!(host_state.mmu_translate_calls, 2);
}

#[test]
fn tier1_inline_tlb_cross_page_store_fastpath_wraps_u64_address_space() {
    // x86 effective addresses wrap modulo 2^64. A wide unaligned store can therefore cross the
    // u64 boundary and wrap to vaddr=0.
    //
    // Choose an address such that `addr + 7 == 0` (for a W64 cross-page store where shift_bytes=1).
    let addr = u64::MAX - 6;
    let hi_vpn = addr >> PAGE_SHIFT;

    let mut b = IrBuilder::new(0x1000);
    let a0 = b.const_int(Width::W64, addr);
    let v0 = b.const_int(Width::W64, 0x0807_0605_0403_0201);
    b.store(Width::W64, a0, v0);
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
            inline_tlb_cross_page_fastpath: true,
            ..Default::default()
        },
    );
    validate_wasm(&wasm);

    // Match `run_wasm_inner` layout.
    let ram_base = (JIT_CTX_PTR as u64)
        + (JitContext::TOTAL_BYTE_SIZE as u64)
        + u64::from(jit_ctx::TIER2_CTX_SIZE);

    // Provide RAM for two physical pages:
    // - the wrapped-to-zero page (phys page 0) receives the final high byte
    // - the high virtual page is remapped to phys page 1 (0x1000) to keep the access in-bounds
    let ram = vec![0u8; 0x2000];

    let total_len = ram_base as usize + ram.len();
    let mut mem = vec![0u8; total_len];

    let mut cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    write_cpu_to_wasm_bytes(&cpu, &mut cpu_bytes);
    mem[CPU_PTR as usize..CPU_PTR as usize + cpu_bytes.len()].copy_from_slice(&cpu_bytes);

    let ctx = JitContext {
        ram_base,
        tlb_salt: TLB_SALT,
    };
    ctx.write_header_to_mem(&mut mem, JIT_CTX_PTR as usize);

    mem[ram_base as usize..ram_base as usize + ram.len()].copy_from_slice(&ram);

    let pages = total_len.div_ceil(65_536) as u32;
    let (mut store, memory, func) = instantiate(&wasm, pages, u64::MAX);
    memory.write(&mut store, 0, &mem).unwrap();

    // Remap the high VPN into a small physical page so the test can allocate backing RAM.
    store.data_mut().override_vpn = Some(hi_vpn);
    store.data_mut().override_phys_base = 0x1000;

    let ret = func.call(&mut store, (CPU_PTR, JIT_CTX_PTR)).unwrap();
    assert_eq!(ret, 0x3000);

    let mut got_mem = vec![0u8; total_len];
    memory.read(&store, 0, &mut got_mem).unwrap();
    let snap = CpuSnapshot::from_wasm_bytes(&got_mem[0..abi::CPU_STATE_SIZE as usize]);

    assert_eq!(snap.rip, 0x3000);

    let got_ram = &got_mem[ram_base as usize..ram_base as usize + ram.len()];
    assert_eq!(got_ram[0], 8);
    assert_eq!(&got_ram[0x1FF9..0x2000], &[1, 2, 3, 4, 5, 6, 7]);

    let host_state = *store.data();
    assert_eq!(host_state.mmio_exit_calls, 0);
    assert_eq!(host_state.slow_mem_reads, 0);
    assert_eq!(host_state.slow_mem_writes, 0);
    assert_eq!(host_state.mmu_translate_calls, 2);
}

#[test]
fn tier1_inline_tlb_cross_page_load_mmio_exit_on_first_page_skips_second_page_translate() {
    // Use u64 wrap-around so the access crosses from a high, non-RAM page into vaddr=0, which is
    // RAM. The MMIO exit should trigger on the first page and should not translate the wrapped
    // second page at all.
    let addr = u64::MAX - 6;

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

    let sentinel = 0xDEAD_BEEF_DEAD_BEEF;
    let mut cpu = CpuState {
        rip: 0x1000,
        ..Default::default()
    };
    cpu.gpr[Gpr::Rax.as_u8() as usize] = sentinel;

    let ram = vec![0u8; 0x1000];

    let (next_rip, got_cpu, _got_ram, host_state) = run_wasm_inner(
        &block,
        cpu,
        ram,
        0x1000,
        None,
        Tier1WasmOptions {
            inline_tlb: true,
            inline_tlb_cross_page_fastpath: true,
            ..Default::default()
        },
    );

    assert_eq!(next_rip, 0x1000);
    assert_eq!(got_cpu.rip, 0x1000);
    assert_eq!(got_cpu.gpr[Gpr::Rax.as_u8() as usize], sentinel);

    assert_eq!(host_state.mmio_exit_calls, 1);
    assert_eq!(host_state.mmu_translate_calls, 1);
    assert_eq!(host_state.slow_mem_reads, 0);
    assert_eq!(host_state.slow_mem_writes, 0);

    let mmio = host_state
        .last_mmio
        .expect("MMIO exit payload should be recorded");
    assert_eq!(mmio.vaddr, addr);
    assert_eq!(mmio.size, 8);
    assert!(!mmio.is_write);
    assert_eq!(mmio.value, 0);
    assert_eq!(mmio.rip, 0x1000);
}

#[test]
fn tier1_inline_tlb_cross_page_store_mmio_exit_on_first_page_skips_second_page_translate() {
    // Like the load case above, but for stores. Ensure the block exits before writing any bytes
    // into the wrapped-to-zero RAM page.
    let addr = u64::MAX - 6;

    let mut b = IrBuilder::new(0x1000);
    let a0 = b.const_int(Width::W64, addr);
    let v0 = b.const_int(Width::W64, 0x0807_0605_0403_0201);
    b.store(Width::W64, a0, v0);
    let block = b.finish(IrTerminator::Jump { target: 0x3000 });
    block.validate().unwrap();

    let cpu = CpuState {
        rip: 0x1000,
        ..Default::default()
    };

    let mut ram = vec![0u8; 0x1000];
    ram[0] = 0xaa;

    let (next_rip, got_cpu, got_ram, host_state) = run_wasm_inner(
        &block,
        cpu,
        ram.clone(),
        0x1000,
        None,
        Tier1WasmOptions {
            inline_tlb: true,
            inline_tlb_cross_page_fastpath: true,
            ..Default::default()
        },
    );

    assert_eq!(next_rip, 0x1000);
    assert_eq!(got_cpu.rip, 0x1000);
    assert_eq!(got_ram, ram);

    assert_eq!(host_state.mmio_exit_calls, 1);
    assert_eq!(host_state.mmu_translate_calls, 1);
    assert_eq!(host_state.slow_mem_reads, 0);
    assert_eq!(host_state.slow_mem_writes, 0);

    let mmio = host_state
        .last_mmio
        .expect("MMIO exit payload should be recorded");
    assert_eq!(mmio.vaddr, addr);
    assert_eq!(mmio.size, 8);
    assert!(mmio.is_write);
    assert_eq!(mmio.value, 0x0807_0605_0403_0201);
    assert_eq!(mmio.rip, 0x1000);
}

#[test]
fn tier1_inline_tlb_cross_page_load_fastpath_handles_all_offsets() {
    // For a W64 load, any address in the last 7 bytes of a 4KiB page crosses into the next page.
    // Exercise all offsets to ensure the split load + recombine logic is correct.
    for addr in 0xFF9u64..=0xFFFu64 {
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

        let mut ram = vec![0u8; 0x2000];
        for (i, b) in ram.iter_mut().enumerate() {
            *b = i as u8;
        }
        let expected =
            u64::from_le_bytes(ram[addr as usize..addr as usize + 8].try_into().unwrap());

        let (next_rip, got_cpu, _got_ram, host_state) = run_wasm_inner(
            &block,
            cpu,
            ram,
            0x2000,
            None,
            Tier1WasmOptions {
                inline_tlb: true,
                inline_tlb_cross_page_fastpath: true,
                ..Default::default()
            },
        );

        assert_eq!(next_rip, 0x3000, "addr={addr:#x}");
        assert_eq!(got_cpu.rip, 0x3000, "addr={addr:#x}");
        assert_eq!(
            got_cpu.gpr[Gpr::Rax.as_u8() as usize],
            expected,
            "addr={addr:#x}"
        );
        assert!(host_state.mmu_translate_calls <= 2, "addr={addr:#x}");
        assert_eq!(host_state.slow_mem_reads, 0, "addr={addr:#x}");
        assert_eq!(host_state.mmio_exit_calls, 0, "addr={addr:#x}");
    }
}

#[test]
fn tier1_inline_tlb_cross_page_store_fastpath_handles_all_offsets() {
    for addr in 0xFF9u64..=0xFFFu64 {
        let mut b = IrBuilder::new(0x1000);
        let a0 = b.const_int(Width::W64, addr);
        let v0 = b.const_int(Width::W64, 0x1122_3344_5566_7788);
        b.store(Width::W64, a0, v0);
        let block = b.finish(IrTerminator::Jump { target: 0x3000 });
        block.validate().unwrap();

        let cpu = CpuState {
            rip: 0x1000,
            ..Default::default()
        };

        let ram = vec![0xccu8; 0x2000];
        let mut expected_ram = ram.clone();
        expected_ram[addr as usize..addr as usize + 8]
            .copy_from_slice(&0x1122_3344_5566_7788u64.to_le_bytes());

        let (next_rip, got_cpu, got_ram, host_state) = run_wasm_inner(
            &block,
            cpu,
            ram,
            0x2000,
            None,
            Tier1WasmOptions {
                inline_tlb: true,
                inline_tlb_cross_page_fastpath: true,
                ..Default::default()
            },
        );

        assert_eq!(next_rip, 0x3000, "addr={addr:#x}");
        assert_eq!(got_cpu.rip, 0x3000, "addr={addr:#x}");
        assert_eq!(got_ram, expected_ram, "addr={addr:#x}");
        assert!(host_state.mmu_translate_calls <= 2, "addr={addr:#x}");
        assert_eq!(host_state.slow_mem_writes, 0, "addr={addr:#x}");
        assert_eq!(host_state.mmio_exit_calls, 0, "addr={addr:#x}");
    }
}

#[test]
fn tier1_inline_tlb_cross_page_load_fastpath_handles_all_offsets_w32() {
    // For a W32 load, any address in the last 3 bytes of a 4KiB page crosses into the next page.
    for addr in 0xFFDu64..=0xFFFu64 {
        let mut b = IrBuilder::new(0x1000);
        let a0 = b.const_int(Width::W64, addr);
        let v0 = b.load(Width::W32, a0);
        b.write_reg(
            GuestReg::Gpr {
                reg: Gpr::Rax,
                width: Width::W32,
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

        let mut ram = vec![0u8; 0x2000];
        for (i, b) in ram.iter_mut().enumerate() {
            *b = i as u8;
        }
        let expected =
            u32::from_le_bytes(ram[addr as usize..addr as usize + 4].try_into().unwrap()) as u64;

        let (next_rip, got_cpu, _got_ram, host_state) = run_wasm_inner(
            &block,
            cpu,
            ram,
            0x2000,
            None,
            Tier1WasmOptions {
                inline_tlb: true,
                inline_tlb_cross_page_fastpath: true,
                ..Default::default()
            },
        );

        assert_eq!(next_rip, 0x3000, "addr={addr:#x}");
        assert_eq!(got_cpu.rip, 0x3000, "addr={addr:#x}");
        assert_eq!(
            got_cpu.gpr[Gpr::Rax.as_u8() as usize],
            expected,
            "addr={addr:#x}"
        );
        assert!(host_state.mmu_translate_calls <= 2, "addr={addr:#x}");
        assert_eq!(host_state.slow_mem_reads, 0, "addr={addr:#x}");
        assert_eq!(host_state.mmio_exit_calls, 0, "addr={addr:#x}");
    }
}

#[test]
fn tier1_inline_tlb_cross_page_store_fastpath_handles_all_offsets_w32() {
    const VALUE: u64 = 0x1122_3344_5566_7788;

    // For a W32 store, any address in the last 3 bytes of a 4KiB page crosses into the next page.
    for addr in 0xFFDu64..=0xFFFu64 {
        let mut b = IrBuilder::new(0x1000);
        let a0 = b.const_int(Width::W64, addr);
        let v0 = b.const_int(Width::W32, VALUE);
        b.store(Width::W32, a0, v0);
        let block = b.finish(IrTerminator::Jump { target: 0x3000 });
        block.validate().unwrap();

        let cpu = CpuState {
            rip: 0x1000,
            ..Default::default()
        };

        let ram = vec![0xccu8; 0x2000];
        let mut expected_ram = ram.clone();
        let bytes = Width::W32.truncate(VALUE).to_le_bytes();
        expected_ram[addr as usize..addr as usize + 4].copy_from_slice(&bytes[..4]);

        let (next_rip, got_cpu, got_ram, host_state) = run_wasm_inner(
            &block,
            cpu,
            ram,
            0x2000,
            None,
            Tier1WasmOptions {
                inline_tlb: true,
                inline_tlb_cross_page_fastpath: true,
                ..Default::default()
            },
        );

        assert_eq!(next_rip, 0x3000, "addr={addr:#x}");
        assert_eq!(got_cpu.rip, 0x3000, "addr={addr:#x}");
        assert_eq!(got_ram, expected_ram, "addr={addr:#x}");
        assert!(host_state.mmu_translate_calls <= 2, "addr={addr:#x}");
        assert_eq!(host_state.slow_mem_writes, 0, "addr={addr:#x}");
        assert_eq!(host_state.mmio_exit_calls, 0, "addr={addr:#x}");
    }
}

#[test]
fn tier1_inline_tlb_cross_page_load_fastpath_handles_all_offsets_w16() {
    // For a W16 load, only the very last byte of a 4KiB page crosses into the next page.
    let addr = 0xFFFu64;

    let mut b = IrBuilder::new(0x1000);
    let a0 = b.const_int(Width::W64, addr);
    let v0 = b.load(Width::W16, a0);
    b.write_reg(
        GuestReg::Gpr {
            reg: Gpr::Rax,
            width: Width::W16,
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

    let mut ram = vec![0u8; 0x2000];
    for (i, b) in ram.iter_mut().enumerate() {
        *b = i as u8;
    }
    let expected =
        u16::from_le_bytes(ram[addr as usize..addr as usize + 2].try_into().unwrap()) as u64;

    let (next_rip, got_cpu, _got_ram, host_state) = run_wasm_inner(
        &block,
        cpu,
        ram,
        0x2000,
        None,
        Tier1WasmOptions {
            inline_tlb: true,
            inline_tlb_cross_page_fastpath: true,
            ..Default::default()
        },
    );

    assert_eq!(next_rip, 0x3000);
    assert_eq!(got_cpu.rip, 0x3000);
    assert_eq!(got_cpu.gpr[Gpr::Rax.as_u8() as usize], expected);
    assert!(host_state.mmu_translate_calls <= 2);
    assert_eq!(host_state.slow_mem_reads, 0);
    assert_eq!(host_state.mmio_exit_calls, 0);
}

#[test]
fn tier1_inline_tlb_cross_page_store_fastpath_handles_all_offsets_w16() {
    const VALUE: u64 = 0x1122_3344_5566_7788;

    // For a W16 store, only the very last byte of a 4KiB page crosses into the next page.
    let addr = 0xFFFu64;

    let mut b = IrBuilder::new(0x1000);
    let a0 = b.const_int(Width::W64, addr);
    let v0 = b.const_int(Width::W16, VALUE);
    b.store(Width::W16, a0, v0);
    let block = b.finish(IrTerminator::Jump { target: 0x3000 });
    block.validate().unwrap();

    let cpu = CpuState {
        rip: 0x1000,
        ..Default::default()
    };

    let ram = vec![0xccu8; 0x2000];
    let mut expected_ram = ram.clone();
    let bytes = Width::W16.truncate(VALUE).to_le_bytes();
    expected_ram[addr as usize..addr as usize + 2].copy_from_slice(&bytes[..2]);

    let (next_rip, got_cpu, got_ram, host_state) = run_wasm_inner(
        &block,
        cpu,
        ram,
        0x2000,
        None,
        Tier1WasmOptions {
            inline_tlb: true,
            inline_tlb_cross_page_fastpath: true,
            ..Default::default()
        },
    );

    assert_eq!(next_rip, 0x3000);
    assert_eq!(got_cpu.rip, 0x3000);
    assert_eq!(got_ram, expected_ram);
    assert!(host_state.mmu_translate_calls <= 2);
    assert_eq!(host_state.slow_mem_writes, 0);
    assert_eq!(host_state.mmio_exit_calls, 0);
}

#[test]
fn tier1_inline_tlb_store_bumps_code_page_version() {
    let addr = 0x1000u64;

    let mut b = IrBuilder::new(0x1000);
    let a0 = b.const_int(Width::W64, addr);
    let v0 = b.const_int(Width::W64, 0x1122_3344_5566_7788);
    b.store(Width::W64, a0, v0);
    let block = b.finish(IrTerminator::Jump { target: 0x3000 });
    block.validate().unwrap();

    let cpu = CpuState {
        rip: 0x1000,
        ..Default::default()
    };

    let ram = vec![0u8; 0x2000];

    let (next_rip, got_cpu, got_ram, host_state, table) = run_wasm_inner_with_code_version_table(
        &block,
        cpu,
        ram,
        0x2000,
        Tier1WasmOptions {
            inline_tlb: true,
            ..Default::default()
        },
        2,
    );

    assert_eq!(next_rip, 0x3000);
    assert_eq!(got_cpu.rip, 0x3000);
    assert_eq!(
        &got_ram[addr as usize..addr as usize + 8],
        &0x1122_3344_5566_7788u64.to_le_bytes(),
    );
    assert_eq!(host_state.mmu_translate_calls, 1);
    assert_eq!(host_state.slow_mem_writes, 0);
    assert_eq!(host_state.mmio_exit_calls, 0);
    assert_eq!(table, vec![0, 1]);
}

#[test]
fn tier1_inline_tlb_store_code_version_bump_skips_out_of_bounds_page() {
    let addr = 0x1000u64;

    let mut b = IrBuilder::new(0x1000);
    let a0 = b.const_int(Width::W64, addr);
    let v0 = b.const_int(Width::W64, 0x1122_3344_5566_7788);
    b.store(Width::W64, a0, v0);
    let block = b.finish(IrTerminator::Jump { target: 0x3000 });
    block.validate().unwrap();

    let cpu = CpuState {
        rip: 0x1000,
        ..Default::default()
    };

    let mut ram = vec![0u8; 0x2000];
    let sentinel = 0xdead_beefu32.to_le_bytes();
    ram[0..4].copy_from_slice(&sentinel);

    let (next_rip, got_cpu, got_ram, host_state, table) = run_wasm_inner_with_code_version_table(
        &block,
        cpu,
        ram,
        0x2000,
        Tier1WasmOptions {
            inline_tlb: true,
            ..Default::default()
        },
        // Only one entry (page 0) in the version table; the store targets page 1.
        1,
    );

    assert_eq!(next_rip, 0x3000);
    assert_eq!(got_cpu.rip, 0x3000);
    assert_eq!(
        &got_ram[addr as usize..addr as usize + 8],
        &0x1122_3344_5566_7788u64.to_le_bytes(),
    );

    // Bounds check should skip the bump instead of writing past the end of the table into RAM.
    assert_eq!(&got_ram[0..4], &sentinel);
    assert_eq!(table, vec![0]);

    assert_eq!(host_state.mmu_translate_calls, 1);
    assert_eq!(host_state.slow_mem_writes, 0);
    assert_eq!(host_state.mmio_exit_calls, 0);
}

#[test]
fn tier1_inline_tlb_cross_page_store_code_version_bump_skips_out_of_bounds_second_page() {
    let addr = 0xFF9u64;

    let mut b = IrBuilder::new(0x1000);
    let a0 = b.const_int(Width::W64, addr);
    let v0 = b.const_int(Width::W64, 0x1122_3344_5566_7788);
    b.store(Width::W64, a0, v0);
    let block = b.finish(IrTerminator::Jump { target: 0x3000 });
    block.validate().unwrap();

    let cpu = CpuState {
        rip: 0x1000,
        ..Default::default()
    };

    let mut ram = vec![0u8; 0x2000];
    let sentinel = 0xdead_beefu32.to_le_bytes();
    ram[0..4].copy_from_slice(&sentinel);

    let (next_rip, got_cpu, got_ram, host_state, table) = run_wasm_inner_with_code_version_table(
        &block,
        cpu,
        ram,
        0x2000,
        Tier1WasmOptions {
            inline_tlb: true,
            inline_tlb_cross_page_fastpath: true,
            ..Default::default()
        },
        // Only one entry (page 0) in the version table; the second page bump would be out of
        // bounds.
        1,
    );

    assert_eq!(next_rip, 0x3000);
    assert_eq!(got_cpu.rip, 0x3000);
    assert_eq!(
        &got_ram[addr as usize..addr as usize + 8],
        &0x1122_3344_5566_7788u64.to_le_bytes(),
    );

    // Bounds check should skip the out-of-bounds bump instead of writing past the end of the table
    // into RAM.
    assert_eq!(&got_ram[0..4], &sentinel);

    // Only page 0 should be bumped.
    assert_eq!(table, vec![1]);

    assert_eq!(host_state.mmu_translate_calls, 2);
    assert_eq!(host_state.slow_mem_writes, 0);
    assert_eq!(host_state.mmio_exit_calls, 0);
}

#[test]
fn tier1_inline_tlb_cross_page_store_bumps_both_code_pages() {
    let addr = 0xFF9u64;

    let mut b = IrBuilder::new(0x1000);
    let a0 = b.const_int(Width::W64, addr);
    let v0 = b.const_int(Width::W64, 0x1122_3344_5566_7788);
    b.store(Width::W64, a0, v0);
    let block = b.finish(IrTerminator::Jump { target: 0x3000 });
    block.validate().unwrap();

    let cpu = CpuState {
        rip: 0x1000,
        ..Default::default()
    };

    // Two RAM pages so the cross-page store spans RAM→RAM.
    let ram = vec![0u8; 0x2000];

    let (next_rip, got_cpu, got_ram, host_state, table) = run_wasm_inner_with_code_version_table(
        &block,
        cpu,
        ram,
        0x2000,
        Tier1WasmOptions {
            inline_tlb: true,
            inline_tlb_cross_page_fastpath: true,
            ..Default::default()
        },
        2,
    );

    assert_eq!(next_rip, 0x3000);
    assert_eq!(got_cpu.rip, 0x3000);
    assert_eq!(
        &got_ram[addr as usize..addr as usize + 8],
        &0x1122_3344_5566_7788u64.to_le_bytes(),
    );
    assert_eq!(host_state.slow_mem_writes, 0);
    assert_eq!(host_state.mmio_exit_calls, 0);
    assert_eq!(table, vec![1, 1]);
}

#[test]
fn tier1_inline_tlb_cross_page_store_w32_bumps_both_code_pages() {
    const VALUE: u64 = 0x1122_3344_5566_7788;

    // For a W32 store, the last 3 bytes of a 4KiB page cross into the next page.
    for addr in 0xFFDu64..=0xFFFu64 {
        let mut b = IrBuilder::new(0x1000);
        let a0 = b.const_int(Width::W64, addr);
        let v0 = b.const_int(Width::W32, VALUE);
        b.store(Width::W32, a0, v0);
        let block = b.finish(IrTerminator::Jump { target: 0x3000 });
        block.validate().unwrap();

        let cpu = CpuState {
            rip: 0x1000,
            ..Default::default()
        };

        // Two RAM pages so the cross-page store spans RAM→RAM.
        let ram = vec![0u8; 0x2000];

        let (next_rip, got_cpu, got_ram, host_state, table) =
            run_wasm_inner_with_code_version_table(
                &block,
                cpu,
                ram,
                0x2000,
                Tier1WasmOptions {
                    inline_tlb: true,
                    inline_tlb_cross_page_fastpath: true,
                    ..Default::default()
                },
                2,
            );

        assert_eq!(next_rip, 0x3000, "addr={addr:#x}");
        assert_eq!(got_cpu.rip, 0x3000, "addr={addr:#x}");
        assert_eq!(
            &got_ram[addr as usize..addr as usize + 4],
            &Width::W32.truncate(VALUE).to_le_bytes()[..4],
            "addr={addr:#x}"
        );
        assert_eq!(host_state.slow_mem_writes, 0, "addr={addr:#x}");
        assert_eq!(host_state.mmio_exit_calls, 0, "addr={addr:#x}");
        assert_eq!(table, vec![1, 1], "addr={addr:#x}");
    }
}

#[test]
fn tier1_inline_tlb_cross_page_store_w16_bumps_both_code_pages() {
    const VALUE: u64 = 0x1122_3344_5566_7788;

    // For a W16 store, only the last byte of a 4KiB page crosses into the next page.
    let addr = 0xFFFu64;

    let mut b = IrBuilder::new(0x1000);
    let a0 = b.const_int(Width::W64, addr);
    let v0 = b.const_int(Width::W16, VALUE);
    b.store(Width::W16, a0, v0);
    let block = b.finish(IrTerminator::Jump { target: 0x3000 });
    block.validate().unwrap();

    let cpu = CpuState {
        rip: 0x1000,
        ..Default::default()
    };

    // Two RAM pages so the cross-page store spans RAM→RAM.
    let ram = vec![0u8; 0x2000];

    let (next_rip, got_cpu, got_ram, host_state, table) = run_wasm_inner_with_code_version_table(
        &block,
        cpu,
        ram,
        0x2000,
        Tier1WasmOptions {
            inline_tlb: true,
            inline_tlb_cross_page_fastpath: true,
            ..Default::default()
        },
        2,
    );

    assert_eq!(next_rip, 0x3000);
    assert_eq!(got_cpu.rip, 0x3000);
    assert_eq!(
        &got_ram[addr as usize..addr as usize + 2],
        &Width::W16.truncate(VALUE).to_le_bytes()[..2],
    );
    assert_eq!(host_state.slow_mem_writes, 0);
    assert_eq!(host_state.mmio_exit_calls, 0);
    assert_eq!(table, vec![1, 1]);
}

#[test]
fn tier1_inline_tlb_cross_page_load_mmio_exits_to_runtime() {
    let addr = 0xFF9u64;

    let mut b = IrBuilder::new(0x1000);
    let a0 = b.const_int(Width::W64, addr);
    let _ = b.load(Width::W64, a0);
    let block = b.finish(IrTerminator::Jump { target: 0x3000 });
    block.validate().unwrap();

    let cpu = CpuState {
        rip: 0x1000,
        ..Default::default()
    };

    // Only the first page is RAM; the second page translation should be treated as MMIO and
    // cause a runtime exit before any direct memory load occurs.
    let ram = vec![0u8; 0x1000];

    let (next_rip, got_cpu, _got_ram, host_state) = run_wasm_inner(
        &block,
        cpu,
        ram,
        0x1000,
        None,
        Tier1WasmOptions {
            inline_tlb: true,
            inline_tlb_cross_page_fastpath: true,
            ..Default::default()
        },
    );

    assert_eq!(next_rip, 0x1000);
    assert_eq!(got_cpu.rip, 0x1000);
    assert_eq!(host_state.mmio_exit_calls, 1);
    assert_eq!(host_state.mmu_translate_calls, 2);
    assert_eq!(host_state.slow_mem_reads, 0);
    assert_eq!(host_state.slow_mem_writes, 0);

    let mmio = host_state
        .last_mmio
        .expect("MMIO exit payload should be recorded");
    assert_eq!(mmio.vaddr, addr);
    assert_eq!(mmio.size, 8);
    assert!(!mmio.is_write);
    assert_eq!(mmio.value, 0);
    assert_eq!(mmio.rip, 0x1000);
}

#[test]
fn tier1_inline_tlb_cross_page_load_mmio_exit_does_not_clobber_unreached_written_gpr() {
    let addr = 0xFF9u64;

    let mut b = IrBuilder::new(0x1000);
    let a0 = b.const_int(Width::W64, addr);
    let _ = b.load(Width::W64, a0);
    // Regression: even though RBX is written later in the block, a cross-page MMIO exit inside the
    // load must not spill an uninitialized RBX local back to the CpuState.
    let v0 = b.const_int(Width::W64, 0x1234_5678_9abc_def0);
    b.write_reg(
        GuestReg::Gpr {
            reg: Gpr::Rbx,
            width: Width::W64,
            high8: false,
        },
        v0,
    );
    let block = b.finish(IrTerminator::Jump { target: 0x3000 });
    block.validate().unwrap();

    let sentinel = 0xDEAD_BEEF_DEAD_BEEF;
    let mut cpu = CpuState {
        rip: 0x1000,
        ..Default::default()
    };
    cpu.gpr[Gpr::Rbx.as_u8() as usize] = sentinel;

    // Only the first page is RAM; the second page translation should be treated as MMIO and
    // cause a runtime exit before any direct memory load occurs.
    let ram = vec![0u8; 0x1000];

    let (next_rip, got_cpu, _got_ram, host_state) = run_wasm_inner(
        &block,
        cpu,
        ram,
        0x1000,
        None,
        Tier1WasmOptions {
            inline_tlb: true,
            inline_tlb_cross_page_fastpath: true,
            ..Default::default()
        },
    );

    assert_eq!(next_rip, 0x1000);
    assert_eq!(got_cpu.rip, 0x1000);
    assert_eq!(got_cpu.gpr[Gpr::Rbx.as_u8() as usize], sentinel);

    assert_eq!(host_state.mmio_exit_calls, 1);
    assert_eq!(host_state.mmu_translate_calls, 2);
    assert_eq!(host_state.slow_mem_reads, 0);
    assert_eq!(host_state.slow_mem_writes, 0);
}

#[test]
fn tier1_inline_tlb_cross_page_store_mmio_exits_to_runtime() {
    let addr = 0xFF9u64;

    let mut b = IrBuilder::new(0x1000);
    let a0 = b.const_int(Width::W64, addr);
    let v0 = b.const_int(Width::W64, 0x1122_3344_5566_7788);
    b.store(Width::W64, a0, v0);
    let block = b.finish(IrTerminator::Jump { target: 0x3000 });
    block.validate().unwrap();

    let cpu = CpuState {
        rip: 0x1000,
        ..Default::default()
    };

    // Only the first page is RAM; the second page translation should be treated as MMIO and
    // cause a runtime exit before any direct memory store occurs.
    let ram = vec![0u8; 0x1000];

    let (next_rip, got_cpu, got_ram, host_state) = run_wasm_inner(
        &block,
        cpu,
        ram,
        0x1000,
        None,
        Tier1WasmOptions {
            inline_tlb: true,
            inline_tlb_cross_page_fastpath: true,
            ..Default::default()
        },
    );

    assert_eq!(next_rip, 0x1000);
    assert_eq!(got_cpu.rip, 0x1000);
    assert_eq!(got_ram, vec![0u8; 0x1000]);
    assert_eq!(host_state.mmio_exit_calls, 1);
    assert_eq!(host_state.mmu_translate_calls, 2);
    assert_eq!(host_state.slow_mem_reads, 0);
    assert_eq!(host_state.slow_mem_writes, 0);

    let mmio = host_state
        .last_mmio
        .expect("MMIO exit payload should be recorded");
    assert_eq!(mmio.vaddr, addr);
    assert_eq!(mmio.size, 8);
    assert!(mmio.is_write);
    assert_eq!(mmio.value, 0x1122_3344_5566_7788);
    assert_eq!(mmio.rip, 0x1000);
}

#[test]
fn tier1_inline_tlb_cross_page_store_mmio_exit_does_not_clobber_unreached_written_gpr() {
    let addr = 0xFF9u64;

    let mut b = IrBuilder::new(0x1000);
    let a0 = b.const_int(Width::W64, addr);
    let v0 = b.const_int(Width::W64, 0x1122_3344_5566_7788);
    b.store(Width::W64, a0, v0);
    let v1 = b.const_int(Width::W64, 0x1234_5678_9abc_def0);
    b.write_reg(
        GuestReg::Gpr {
            reg: Gpr::Rbx,
            width: Width::W64,
            high8: false,
        },
        v1,
    );
    let block = b.finish(IrTerminator::Jump { target: 0x3000 });
    block.validate().unwrap();

    let sentinel = 0xDEAD_BEEF_DEAD_BEEF;
    let mut cpu = CpuState {
        rip: 0x1000,
        ..Default::default()
    };
    cpu.gpr[Gpr::Rbx.as_u8() as usize] = sentinel;

    // Only the first page is RAM; the second page translation should be treated as MMIO and
    // cause a runtime exit before any direct memory store occurs.
    let ram = vec![0u8; 0x1000];

    let (next_rip, got_cpu, got_ram, host_state) = run_wasm_inner(
        &block,
        cpu,
        ram,
        0x1000,
        None,
        Tier1WasmOptions {
            inline_tlb: true,
            inline_tlb_cross_page_fastpath: true,
            ..Default::default()
        },
    );

    assert_eq!(next_rip, 0x1000);
    assert_eq!(got_cpu.rip, 0x1000);
    assert_eq!(got_cpu.gpr[Gpr::Rbx.as_u8() as usize], sentinel);
    assert_eq!(got_ram, vec![0u8; 0x1000]);

    assert_eq!(host_state.mmio_exit_calls, 1);
    assert_eq!(host_state.mmu_translate_calls, 2);
    assert_eq!(host_state.slow_mem_reads, 0);
    assert_eq!(host_state.slow_mem_writes, 0);
}

#[test]
fn tier1_inline_tlb_cross_page_load_mmio_exits_to_runtime_w16() {
    let addr = 0xFFFu64;

    let mut b = IrBuilder::new(0x1000);
    let a0 = b.const_int(Width::W64, addr);
    let _ = b.load(Width::W16, a0);
    let block = b.finish(IrTerminator::Jump { target: 0x3000 });
    block.validate().unwrap();

    let cpu = CpuState {
        rip: 0x1000,
        ..Default::default()
    };

    let ram = vec![0u8; 0x1000];
    let (next_rip, got_cpu, _got_ram, host_state) = run_wasm_inner(
        &block,
        cpu,
        ram,
        0x1000,
        None,
        Tier1WasmOptions {
            inline_tlb: true,
            inline_tlb_cross_page_fastpath: true,
            ..Default::default()
        },
    );

    assert_eq!(next_rip, 0x1000);
    assert_eq!(got_cpu.rip, 0x1000);
    assert_eq!(host_state.mmio_exit_calls, 1);
    assert_eq!(host_state.mmu_translate_calls, 2);
    assert_eq!(host_state.slow_mem_reads, 0);
    assert_eq!(host_state.slow_mem_writes, 0);

    let mmio = host_state
        .last_mmio
        .expect("MMIO exit payload should be recorded");
    assert_eq!(mmio.vaddr, addr);
    assert_eq!(mmio.size, 2);
    assert!(!mmio.is_write);
    assert_eq!(mmio.value, 0);
    assert_eq!(mmio.rip, 0x1000);
}

#[test]
fn tier1_inline_tlb_cross_page_store_mmio_exits_to_runtime_w16() {
    let addr = 0xFFFu64;

    let mut b = IrBuilder::new(0x1000);
    let a0 = b.const_int(Width::W64, addr);
    let v0 = b.const_int(Width::W16, 0xBEEFu64);
    b.store(Width::W16, a0, v0);
    let block = b.finish(IrTerminator::Jump { target: 0x3000 });
    block.validate().unwrap();

    let cpu = CpuState {
        rip: 0x1000,
        ..Default::default()
    };

    let ram = vec![0u8; 0x1000];
    let (next_rip, got_cpu, got_ram, host_state) = run_wasm_inner(
        &block,
        cpu,
        ram,
        0x1000,
        None,
        Tier1WasmOptions {
            inline_tlb: true,
            inline_tlb_cross_page_fastpath: true,
            ..Default::default()
        },
    );

    assert_eq!(next_rip, 0x1000);
    assert_eq!(got_cpu.rip, 0x1000);
    assert_eq!(got_ram, vec![0u8; 0x1000]);
    assert_eq!(host_state.mmio_exit_calls, 1);
    assert_eq!(host_state.mmu_translate_calls, 2);
    assert_eq!(host_state.slow_mem_reads, 0);
    assert_eq!(host_state.slow_mem_writes, 0);

    let mmio = host_state
        .last_mmio
        .expect("MMIO exit payload should be recorded");
    assert_eq!(mmio.vaddr, addr);
    assert_eq!(mmio.size, 2);
    assert!(mmio.is_write);
    assert_eq!(mmio.value, 0xBEEF);
    assert_eq!(mmio.rip, 0x1000);
}

#[test]
fn tier1_inline_tlb_cross_page_load_mmio_exits_to_runtime_w32() {
    let addr = 0xFFFu64;

    let mut b = IrBuilder::new(0x1000);
    let a0 = b.const_int(Width::W64, addr);
    let _ = b.load(Width::W32, a0);
    let block = b.finish(IrTerminator::Jump { target: 0x3000 });
    block.validate().unwrap();

    let cpu = CpuState {
        rip: 0x1000,
        ..Default::default()
    };

    let ram = vec![0u8; 0x1000];
    let (next_rip, got_cpu, _got_ram, host_state) = run_wasm_inner(
        &block,
        cpu,
        ram,
        0x1000,
        None,
        Tier1WasmOptions {
            inline_tlb: true,
            inline_tlb_cross_page_fastpath: true,
            ..Default::default()
        },
    );

    assert_eq!(next_rip, 0x1000);
    assert_eq!(got_cpu.rip, 0x1000);
    assert_eq!(host_state.mmio_exit_calls, 1);
    assert_eq!(host_state.mmu_translate_calls, 2);
    assert_eq!(host_state.slow_mem_reads, 0);
    assert_eq!(host_state.slow_mem_writes, 0);

    let mmio = host_state
        .last_mmio
        .expect("MMIO exit payload should be recorded");
    assert_eq!(mmio.vaddr, addr);
    assert_eq!(mmio.size, 4);
    assert!(!mmio.is_write);
    assert_eq!(mmio.value, 0);
    assert_eq!(mmio.rip, 0x1000);
}

#[test]
fn tier1_inline_tlb_cross_page_store_mmio_exits_to_runtime_w32() {
    let addr = 0xFFFu64;

    let mut b = IrBuilder::new(0x1000);
    let a0 = b.const_int(Width::W64, addr);
    let v0 = b.const_int(Width::W32, 0xDEAD_BEEFu64);
    b.store(Width::W32, a0, v0);
    let block = b.finish(IrTerminator::Jump { target: 0x3000 });
    block.validate().unwrap();

    let cpu = CpuState {
        rip: 0x1000,
        ..Default::default()
    };

    let ram = vec![0u8; 0x1000];
    let (next_rip, got_cpu, got_ram, host_state) = run_wasm_inner(
        &block,
        cpu,
        ram,
        0x1000,
        None,
        Tier1WasmOptions {
            inline_tlb: true,
            inline_tlb_cross_page_fastpath: true,
            ..Default::default()
        },
    );

    assert_eq!(next_rip, 0x1000);
    assert_eq!(got_cpu.rip, 0x1000);
    assert_eq!(got_ram, vec![0u8; 0x1000]);
    assert_eq!(host_state.mmio_exit_calls, 1);
    assert_eq!(host_state.mmu_translate_calls, 2);
    assert_eq!(host_state.slow_mem_reads, 0);
    assert_eq!(host_state.slow_mem_writes, 0);

    let mmio = host_state
        .last_mmio
        .expect("MMIO exit payload should be recorded");
    assert_eq!(mmio.vaddr, addr);
    assert_eq!(mmio.size, 4);
    assert!(mmio.is_write);
    assert_eq!(mmio.value, 0xDEAD_BEEF);
    assert_eq!(mmio.rip, 0x1000);
}

#[test]
fn tier1_inline_tlb_cross_page_load_mmio_uses_slow_helper_when_configured() {
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

    // Allocate enough bytes so the slow helper can read both pages, but have `mmu_translate` mark
    // only the first page as RAM to force the slow path when `inline_tlb_mmio_exit` is disabled.
    let mut ram = vec![0u8; 0x2000];
    ram[addr as usize..addr as usize + 8].copy_from_slice(&0x1122_3344_5566_7788u64.to_le_bytes());

    let (next_rip, got_cpu, _got_ram, host_state) = run_wasm_inner(
        &block,
        cpu,
        ram,
        0x1000,
        None,
        Tier1WasmOptions {
            inline_tlb: true,
            inline_tlb_cross_page_fastpath: true,
            inline_tlb_mmio_exit: false,
            ..Default::default()
        },
    );

    assert_eq!(next_rip, 0x3000);
    assert_eq!(got_cpu.rip, 0x3000);
    assert_eq!(
        got_cpu.gpr[Gpr::Rax.as_u8() as usize],
        0x1122_3344_5566_7788
    );

    assert_eq!(host_state.mmio_exit_calls, 0);
    assert_eq!(host_state.mmu_translate_calls, 2);
    assert_eq!(host_state.slow_mem_reads, 1);
}

#[test]
fn tier1_inline_tlb_cross_page_store_mmio_uses_slow_helper_when_configured() {
    let addr = 0xFF9u64;

    let mut b = IrBuilder::new(0x1000);
    let a0 = b.const_int(Width::W64, addr);
    let v0 = b.const_int(Width::W64, 0x1122_3344_5566_7788);
    b.store(Width::W64, a0, v0);
    let block = b.finish(IrTerminator::Jump { target: 0x3000 });
    block.validate().unwrap();

    let cpu = CpuState {
        rip: 0x1000,
        ..Default::default()
    };

    let ram = vec![0u8; 0x2000];

    let (next_rip, got_cpu, got_ram, host_state) = run_wasm_inner(
        &block,
        cpu,
        ram,
        0x1000,
        None,
        Tier1WasmOptions {
            inline_tlb: true,
            inline_tlb_cross_page_fastpath: true,
            inline_tlb_mmio_exit: false,
            ..Default::default()
        },
    );

    assert_eq!(next_rip, 0x3000);
    assert_eq!(got_cpu.rip, 0x3000);
    assert_eq!(
        &got_ram[addr as usize..addr as usize + 8],
        &0x1122_3344_5566_7788u64.to_le_bytes(),
    );

    assert_eq!(host_state.mmio_exit_calls, 0);
    assert_eq!(host_state.mmu_translate_calls, 2);
    assert_eq!(host_state.slow_mem_writes, 1);
}

#[test]
fn tier1_inline_tlb_cross_page_load_mmio_uses_slow_helper_when_configured_w16() {
    let addr = 0xFFFu64;

    let mut b = IrBuilder::new(0x1000);
    let a0 = b.const_int(Width::W64, addr);
    let v0 = b.load(Width::W16, a0);
    b.write_reg(
        GuestReg::Gpr {
            reg: Gpr::Rax,
            width: Width::W16,
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

    let mut ram = vec![0u8; 0x2000];
    ram[addr as usize..addr as usize + 2].copy_from_slice(&0xBEEFu16.to_le_bytes());

    let (next_rip, got_cpu, _got_ram, host_state) = run_wasm_inner(
        &block,
        cpu,
        ram,
        0x1000,
        None,
        Tier1WasmOptions {
            inline_tlb: true,
            inline_tlb_cross_page_fastpath: true,
            inline_tlb_mmio_exit: false,
            ..Default::default()
        },
    );

    assert_eq!(next_rip, 0x3000);
    assert_eq!(got_cpu.rip, 0x3000);
    assert_eq!(got_cpu.gpr[Gpr::Rax.as_u8() as usize] & 0xffff, 0xBEEF);

    assert_eq!(host_state.mmio_exit_calls, 0);
    assert_eq!(host_state.mmu_translate_calls, 2);
    assert_eq!(host_state.slow_mem_reads, 1);
    assert_eq!(host_state.slow_mem_writes, 0);
}

#[test]
fn tier1_inline_tlb_cross_page_store_mmio_uses_slow_helper_when_configured_w16() {
    let addr = 0xFFFu64;

    let mut b = IrBuilder::new(0x1000);
    let a0 = b.const_int(Width::W64, addr);
    let v0 = b.const_int(Width::W16, 0xBEEFu64);
    b.store(Width::W16, a0, v0);
    let block = b.finish(IrTerminator::Jump { target: 0x3000 });
    block.validate().unwrap();

    let cpu = CpuState {
        rip: 0x1000,
        ..Default::default()
    };

    let ram = vec![0u8; 0x2000];

    let (next_rip, got_cpu, got_ram, host_state) = run_wasm_inner(
        &block,
        cpu,
        ram,
        0x1000,
        None,
        Tier1WasmOptions {
            inline_tlb: true,
            inline_tlb_cross_page_fastpath: true,
            inline_tlb_mmio_exit: false,
            ..Default::default()
        },
    );

    assert_eq!(next_rip, 0x3000);
    assert_eq!(got_cpu.rip, 0x3000);
    assert_eq!(
        &got_ram[addr as usize..addr as usize + 2],
        &0xBEEFu16.to_le_bytes(),
    );

    assert_eq!(host_state.mmio_exit_calls, 0);
    assert_eq!(host_state.mmu_translate_calls, 2);
    assert_eq!(host_state.slow_mem_reads, 0);
    assert_eq!(host_state.slow_mem_writes, 1);
}

#[test]
fn tier1_inline_tlb_cross_page_load_mmio_uses_slow_helper_when_configured_w32() {
    let addr = 0xFFFu64;

    let mut b = IrBuilder::new(0x1000);
    let a0 = b.const_int(Width::W64, addr);
    let v0 = b.load(Width::W32, a0);
    b.write_reg(
        GuestReg::Gpr {
            reg: Gpr::Rax,
            width: Width::W32,
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

    let mut ram = vec![0u8; 0x2000];
    ram[addr as usize..addr as usize + 4].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());

    let (next_rip, got_cpu, _got_ram, host_state) = run_wasm_inner(
        &block,
        cpu,
        ram,
        0x1000,
        None,
        Tier1WasmOptions {
            inline_tlb: true,
            inline_tlb_cross_page_fastpath: true,
            inline_tlb_mmio_exit: false,
            ..Default::default()
        },
    );

    assert_eq!(next_rip, 0x3000);
    assert_eq!(got_cpu.rip, 0x3000);
    assert_eq!(got_cpu.gpr[Gpr::Rax.as_u8() as usize], 0xDEAD_BEEF);

    assert_eq!(host_state.mmio_exit_calls, 0);
    assert_eq!(host_state.mmu_translate_calls, 2);
    assert_eq!(host_state.slow_mem_reads, 1);
    assert_eq!(host_state.slow_mem_writes, 0);
}

#[test]
fn tier1_inline_tlb_cross_page_store_mmio_uses_slow_helper_when_configured_w32() {
    let addr = 0xFFFu64;

    let mut b = IrBuilder::new(0x1000);
    let a0 = b.const_int(Width::W64, addr);
    let v0 = b.const_int(Width::W32, 0xDEAD_BEEFu64);
    b.store(Width::W32, a0, v0);
    let block = b.finish(IrTerminator::Jump { target: 0x3000 });
    block.validate().unwrap();

    let cpu = CpuState {
        rip: 0x1000,
        ..Default::default()
    };

    let ram = vec![0u8; 0x2000];

    let (next_rip, got_cpu, got_ram, host_state) = run_wasm_inner(
        &block,
        cpu,
        ram,
        0x1000,
        None,
        Tier1WasmOptions {
            inline_tlb: true,
            inline_tlb_cross_page_fastpath: true,
            inline_tlb_mmio_exit: false,
            ..Default::default()
        },
    );

    assert_eq!(next_rip, 0x3000);
    assert_eq!(got_cpu.rip, 0x3000);
    assert_eq!(
        &got_ram[addr as usize..addr as usize + 4],
        &0xDEAD_BEEFu32.to_le_bytes(),
    );

    assert_eq!(host_state.mmio_exit_calls, 0);
    assert_eq!(host_state.mmu_translate_calls, 2);
    assert_eq!(host_state.slow_mem_reads, 0);
    assert_eq!(host_state.slow_mem_writes, 1);
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

    let mmio = host_state
        .last_mmio
        .expect("MMIO exit payload should be recorded");
    assert_eq!(mmio.vaddr, 0xF000);
    assert_eq!(mmio.size, 4);
    assert!(!mmio.is_write);
    assert_eq!(mmio.value, 0);
    assert_eq!(mmio.rip, 0x1000);
}

#[test]
fn tier1_inline_tlb_mmio_exit_reports_faulting_rip() {
    // x86_64:
    //   mov eax, 0xF000
    //   mov eax, [rax]   ; MMIO (ram_size is only 0x8000)
    let entry_rip = 0x1000u64;
    let code: [u8; 7] = [
        0xB8, 0x00, 0xF0, 0x00, 0x00, // mov eax, 0xF000
        0x8B, 0x00, // mov eax, [rax]
    ];

    let mut bus = SimpleBus::new(0x2000);
    bus.load(entry_rip, &code);

    let x86_block = discover_block_mode(
        &bus,
        entry_rip,
        BlockLimits {
            max_insts: 2,
            max_bytes: 64,
        },
        64,
    );
    assert_eq!(x86_block.insts.len(), 2);

    let second_rip = x86_block.insts[1].rip;
    assert_eq!(second_rip, x86_block.insts[0].next_rip());

    let block = translate_block(&x86_block);
    block.validate().unwrap();

    let cpu = CpuState {
        rip: entry_rip,
        ..Default::default()
    };

    let ram = vec![0u8; 0x10000];
    let (next_rip, got_cpu, _got_ram, host_state) = run_wasm(&block, cpu, ram, 0x8000);

    // MMIO should report the RIP of the faulting *second* instruction, not block entry.
    assert_eq!(next_rip, second_rip);
    assert_eq!(got_cpu.rip, second_rip);
    assert_eq!(host_state.mmio_exit_calls, 1);
    assert_eq!(host_state.mmu_translate_calls, 1);
    assert_eq!(host_state.slow_mem_reads, 0);
    assert_eq!(host_state.slow_mem_writes, 0);

    let mmio = host_state
        .last_mmio
        .expect("MMIO exit payload should be recorded");
    assert_eq!(mmio.vaddr, 0xF000);
    assert_eq!(mmio.size, 4);
    assert!(!mmio.is_write);
    assert_eq!(mmio.value, 0);
    assert_eq!(mmio.rip, second_rip);
}

#[test]
fn tier1_inline_tlb_cross_page_mmio_exit_reports_faulting_rip() {
    // x86_64:
    //   mov eax, 0xFFF
    //   mov eax, [rax]   ; crosses a page boundary into MMIO (ram_size is only 0x1000)
    let entry_rip = 0x1000u64;
    let code: [u8; 7] = [
        0xB8, 0xFF, 0x0F, 0x00, 0x00, // mov eax, 0xFFF
        0x8B, 0x00, // mov eax, [rax]
    ];

    let mut bus = SimpleBus::new(0x2000);
    bus.load(entry_rip, &code);

    let x86_block = discover_block_mode(
        &bus,
        entry_rip,
        BlockLimits {
            max_insts: 2,
            max_bytes: 64,
        },
        64,
    );
    assert_eq!(x86_block.insts.len(), 2);

    let second_rip = x86_block.insts[1].rip;
    assert_eq!(second_rip, x86_block.insts[0].next_rip());

    let block = translate_block(&x86_block);
    block.validate().unwrap();

    let cpu = CpuState {
        rip: entry_rip,
        ..Default::default()
    };

    let ram = vec![0u8; 0x1000];
    let (next_rip, got_cpu, _got_ram, host_state) = run_wasm_inner(
        &block,
        cpu,
        ram,
        0x1000,
        None,
        Tier1WasmOptions {
            inline_tlb: true,
            inline_tlb_cross_page_fastpath: true,
            ..Default::default()
        },
    );

    assert_eq!(next_rip, second_rip);
    assert_eq!(got_cpu.rip, second_rip);
    // The first instruction should still have taken effect.
    assert_eq!(got_cpu.gpr[Gpr::Rax.as_u8() as usize], 0xFFF);

    assert_eq!(host_state.mmio_exit_calls, 1);
    assert_eq!(host_state.mmu_translate_calls, 2);
    assert_eq!(host_state.slow_mem_reads, 0);
    assert_eq!(host_state.slow_mem_writes, 0);

    let mmio = host_state
        .last_mmio
        .expect("MMIO exit payload should be recorded");
    assert_eq!(mmio.vaddr, 0xFFF);
    assert_eq!(mmio.size, 4);
    assert!(!mmio.is_write);
    assert_eq!(mmio.value, 0);
    assert_eq!(mmio.rip, second_rip);
}

#[test]
fn tier1_inline_tlb_mmio_exit_preserves_unrelated_gprs() {
    let mut b = IrBuilder::new(0x1000);
    let addr = b.const_int(Width::W64, 0xF000);
    let _ = b.load(Width::W32, addr);
    let block = b.finish(IrTerminator::Jump { target: 0x3000 });
    block.validate().unwrap();

    let sentinel = 0x1122_3344_5566_7788u64;
    let mut cpu = CpuState {
        rip: 0x1000,
        ..Default::default()
    };
    cpu.gpr[Gpr::Rbx.as_u8() as usize] = sentinel;

    let ram = vec![0u8; 0x10000];
    let (next_rip, got_cpu, _got_ram, host_state) = run_wasm(&block, cpu, ram, 0x8000);

    assert_eq!(next_rip, 0x1000);
    assert_eq!(got_cpu.rip, 0x1000);
    assert_eq!(got_cpu.gpr[Gpr::Rbx.as_u8() as usize], sentinel);

    assert_eq!(host_state.mmio_exit_calls, 1);
    assert_eq!(host_state.mmu_translate_calls, 1);
    assert_eq!(host_state.slow_mem_reads, 0);
    assert_eq!(host_state.slow_mem_writes, 0);
}

#[test]
fn tier1_inline_tlb_call_helper_exit_preserves_unrelated_gprs() {
    let block = IrBlock {
        entry_rip: 0x1000,
        insts: vec![IrInst::CallHelper {
            helper: "test_helper",
            args: vec![],
            ret: None,
        }],
        terminator: IrTerminator::Jump { target: 0x3000 },
        value_types: vec![],
    };
    block.validate().unwrap();

    let sentinel = 0x1122_3344_5566_7788u64;
    let mut cpu = CpuState {
        rip: 0x1000,
        ..Default::default()
    };
    cpu.gpr[Gpr::Rbx.as_u8() as usize] = sentinel;

    let ram = vec![0u8; 0x10000];
    // CallHelper blocks don't use inline-TLB, but `run_wasm` is still fine (the code generator will
    // auto-disable inline-TLB for this block).
    let (next_rip, got_cpu, _got_ram, host_state) = run_wasm(&block, cpu, ram, 0x10000);

    assert_eq!(next_rip, 0x1000);
    assert_eq!(got_cpu.rip, 0x1000);
    assert_eq!(got_cpu.gpr[Gpr::Rbx.as_u8() as usize], sentinel);

    assert_eq!(host_state.mmio_exit_calls, 0);
    assert_eq!(host_state.mmu_translate_calls, 0);
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

    let mmio = host_state
        .last_mmio
        .expect("MMIO exit payload should be recorded");
    assert_eq!(mmio.vaddr, 0xF000);
    assert_eq!(mmio.size, 4);
    assert!(mmio.is_write);
    assert_eq!(mmio.value, 0xDEAD_BEEF);
    assert_eq!(mmio.rip, 0x1000);
}

#[test]
fn tier1_inline_tlb_mmio_load_exit_does_not_clobber_unreached_written_gpr() {
    let mut b = IrBuilder::new(0x1000);
    let addr = b.const_int(Width::W64, 0xF000);
    let _ = b.load(Width::W32, addr);

    // Regression test: with selective GPR load/spill enabled, Tier-1 must not clobber a GPR that
    // is only written *after* a potential MMIO exit point.
    let v0 = b.const_int(Width::W64, 0x1234_5678_9abc_def0);
    b.write_reg(
        GuestReg::Gpr {
            reg: Gpr::Rbx,
            width: Width::W64,
            high8: false,
        },
        v0,
    );
    let block = b.finish(IrTerminator::Jump { target: 0x3000 });
    block.validate().unwrap();

    let mut cpu = CpuState {
        rip: 0x1000,
        ..Default::default()
    };
    cpu.gpr[Gpr::Rbx.as_u8() as usize] = 0xDEAD_BEEF_DEAD_BEEF;

    let ram = vec![0u8; 0x10000];
    let (next_rip, got_cpu, _got_ram, host_state) = run_wasm(&block, cpu, ram, 0x8000);

    assert_eq!(next_rip, 0x1000);
    assert_eq!(got_cpu.rip, 0x1000);
    assert_eq!(host_state.mmio_exit_calls, 1);
    assert_eq!(host_state.mmu_translate_calls, 1);
    assert_eq!(host_state.slow_mem_reads, 0);
    assert_eq!(host_state.slow_mem_writes, 0);

    assert_eq!(
        got_cpu.gpr[Gpr::Rbx.as_u8() as usize],
        0xDEAD_BEEF_DEAD_BEEF
    );
}

#[test]
fn tier1_inline_tlb_mmio_store_exit_does_not_clobber_unreached_written_gpr() {
    let mut b = IrBuilder::new(0x1000);
    let addr = b.const_int(Width::W64, 0xF000);
    let value = b.const_int(Width::W32, 0xDEAD_BEEF);
    b.store(Width::W32, addr, value);

    // Same scenario as `tier1_inline_tlb_mmio_load_exit_does_not_clobber_unreached_written_gpr`,
    // but for inline-TLB stores.
    let v0 = b.const_int(Width::W64, 0x1234_5678_9abc_def0);
    b.write_reg(
        GuestReg::Gpr {
            reg: Gpr::Rbx,
            width: Width::W64,
            high8: false,
        },
        v0,
    );
    let block = b.finish(IrTerminator::Jump { target: 0x3000 });
    block.validate().unwrap();

    let mut cpu = CpuState {
        rip: 0x1000,
        ..Default::default()
    };
    cpu.gpr[Gpr::Rbx.as_u8() as usize] = 0xDEAD_BEEF_DEAD_BEEF;

    let ram = vec![0u8; 0x10000];
    let (next_rip, got_cpu, _got_ram, host_state) = run_wasm(&block, cpu, ram, 0x8000);

    assert_eq!(next_rip, 0x1000);
    assert_eq!(got_cpu.rip, 0x1000);
    assert_eq!(host_state.mmio_exit_calls, 1);
    assert_eq!(host_state.mmu_translate_calls, 1);
    assert_eq!(host_state.slow_mem_reads, 0);
    assert_eq!(host_state.slow_mem_writes, 0);

    assert_eq!(
        got_cpu.gpr[Gpr::Rbx.as_u8() as usize],
        0xDEAD_BEEF_DEAD_BEEF
    );
}

#[test]
fn tier1_inline_tlb_cross_page_mmio_load_exit_does_not_clobber_unreached_written_gpr() {
    let mut b = IrBuilder::new(0x1000);
    let addr = b.const_int(Width::W64, 0xFF9);
    let _ = b.load(Width::W64, addr);

    // Same scenario as `tier1_inline_tlb_mmio_load_exit_does_not_clobber_unreached_written_gpr`,
    // but for a cross-page MMIO exit (page0 is RAM, page1 is non-RAM).
    let v0 = b.const_int(Width::W64, 0x1234_5678_9abc_def0);
    b.write_reg(
        GuestReg::Gpr {
            reg: Gpr::Rbx,
            width: Width::W64,
            high8: false,
        },
        v0,
    );
    let block = b.finish(IrTerminator::Jump { target: 0x3000 });
    block.validate().unwrap();

    let mut cpu = CpuState {
        rip: 0x1000,
        ..Default::default()
    };
    cpu.gpr[Gpr::Rbx.as_u8() as usize] = 0xDEAD_BEEF_DEAD_BEEF;

    let ram = vec![0u8; 0x1000];
    let (next_rip, got_cpu, _got_ram, host_state) = run_wasm_inner(
        &block,
        cpu,
        ram,
        0x1000,
        None,
        Tier1WasmOptions {
            inline_tlb: true,
            inline_tlb_cross_page_fastpath: true,
            ..Default::default()
        },
    );

    assert_eq!(next_rip, 0x1000);
    assert_eq!(got_cpu.rip, 0x1000);
    assert_eq!(host_state.mmio_exit_calls, 1);
    assert_eq!(host_state.mmu_translate_calls, 2);
    assert_eq!(host_state.slow_mem_reads, 0);
    assert_eq!(host_state.slow_mem_writes, 0);

    assert_eq!(
        got_cpu.gpr[Gpr::Rbx.as_u8() as usize],
        0xDEAD_BEEF_DEAD_BEEF
    );
}

#[test]
fn tier1_inline_tlb_cross_page_mmio_store_exit_does_not_clobber_unreached_written_gpr() {
    let mut b = IrBuilder::new(0x1000);
    let addr = b.const_int(Width::W64, 0xFF9);
    let value = b.const_int(Width::W64, 0x1122_3344_5566_7788);
    b.store(Width::W64, addr, value);

    // Same scenario as `tier1_inline_tlb_mmio_store_exit_does_not_clobber_unreached_written_gpr`,
    // but for a cross-page MMIO exit (page0 is RAM, page1 is non-RAM).
    let v0 = b.const_int(Width::W64, 0x1234_5678_9abc_def0);
    b.write_reg(
        GuestReg::Gpr {
            reg: Gpr::Rbx,
            width: Width::W64,
            high8: false,
        },
        v0,
    );
    let block = b.finish(IrTerminator::Jump { target: 0x3000 });
    block.validate().unwrap();

    let mut cpu = CpuState {
        rip: 0x1000,
        ..Default::default()
    };
    cpu.gpr[Gpr::Rbx.as_u8() as usize] = 0xDEAD_BEEF_DEAD_BEEF;

    let ram = vec![0u8; 0x1000];
    let (next_rip, got_cpu, got_ram, host_state) = run_wasm_inner(
        &block,
        cpu,
        ram,
        0x1000,
        None,
        Tier1WasmOptions {
            inline_tlb: true,
            inline_tlb_cross_page_fastpath: true,
            ..Default::default()
        },
    );

    assert_eq!(next_rip, 0x1000);
    assert_eq!(got_cpu.rip, 0x1000);
    assert_eq!(got_ram, vec![0u8; 0x1000]);

    assert_eq!(host_state.mmio_exit_calls, 1);
    assert_eq!(host_state.mmu_translate_calls, 2);
    assert_eq!(host_state.slow_mem_reads, 0);
    assert_eq!(host_state.slow_mem_writes, 0);

    assert_eq!(
        got_cpu.gpr[Gpr::Rbx.as_u8() as usize],
        0xDEAD_BEEF_DEAD_BEEF
    );
}

#[test]
fn tier1_inline_tlb_mmio_exit_reports_precise_rip_mid_block() {
    // x86:
    //   0x1000: mov ecx, 0x12345678
    //   0x1005: mov eax, dword ptr [rax]   (MMIO -> runtime exit)
    //   0x1007: int3                      (unreached)
    let entry = 0x1000u64;
    let code = [
        0xb9, 0x78, 0x56, 0x34, 0x12, // mov ecx, 0x12345678
        0x8b, 0x00, // mov eax, dword ptr [rax]
        0xcc, // int3
    ];

    let mut bus = SimpleBus::new(0x2000);
    bus.load(entry, &code);

    let bb = discover_block_mode(&bus, entry, BlockLimits::default(), 64);
    let block = translate_block(&bb);

    let mut cpu = CpuState {
        rip: entry,
        ..Default::default()
    };
    // Make the second instruction load from MMIO.
    cpu.gpr[Gpr::Rax.as_u8() as usize] = 0xF000;

    let ram = vec![0u8; 0x10000];
    let (next_rip, got_cpu, _got_ram, host_state) = run_wasm(&block, cpu, ram, 0x8000);

    // The MMIO exit should report the RIP of the instruction that faulted, not the block entry.
    assert_eq!(next_rip, entry + 5);
    assert_eq!(got_cpu.rip, entry + 5);

    // The first instruction's effects should be committed (no backend rollback in this harness).
    assert_eq!(
        got_cpu.gpr[Gpr::Rcx.as_u8() as usize],
        0x1234_5678,
        "expected first instruction to commit before MMIO exit"
    );

    assert_eq!(host_state.mmio_exit_calls, 1);
}

#[test]
fn tier1_inline_tlb_mmio_load_uses_slow_helper_when_configured() {
    let mut b = IrBuilder::new(0x1000);
    let addr = b.const_int(Width::W64, 0xF000);
    let loaded = b.load(Width::W32, addr);
    b.write_reg(
        GuestReg::Gpr {
            reg: Gpr::Rax,
            width: Width::W32,
            high8: false,
        },
        loaded,
    );
    let block = b.finish(IrTerminator::Jump { target: 0x3000 });
    block.validate().unwrap();

    let cpu = CpuState {
        rip: 0x1000,
        ..Default::default()
    };

    let mut ram = vec![0u8; 0x10000];
    ram[0xF000..0xF004].copy_from_slice(&0x1234_5678u32.to_le_bytes());

    let (next_rip, got_cpu, _got_ram, host_state) = run_wasm_inner(
        &block,
        cpu,
        ram,
        0x8000,
        None,
        Tier1WasmOptions {
            inline_tlb: true,
            inline_tlb_mmio_exit: false,
            ..Default::default()
        },
    );

    assert_eq!(next_rip, 0x3000);
    assert_eq!(got_cpu.rip, 0x3000);
    assert_eq!(got_cpu.gpr[Gpr::Rax.as_u8() as usize], 0x1234_5678);

    assert_eq!(host_state.mmio_exit_calls, 0);
    assert_eq!(host_state.mmu_translate_calls, 1);
    assert_eq!(host_state.slow_mem_reads, 1);
    assert_eq!(host_state.slow_mem_writes, 0);
}

#[test]
fn tier1_inline_tlb_mmio_store_uses_slow_helper_when_configured() {
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

    let (next_rip, got_cpu, got_ram, host_state) = run_wasm_inner(
        &block,
        cpu,
        ram,
        0x8000,
        None,
        Tier1WasmOptions {
            inline_tlb: true,
            inline_tlb_mmio_exit: false,
            ..Default::default()
        },
    );

    assert_eq!(next_rip, 0x3000);
    assert_eq!(got_cpu.rip, 0x3000);
    assert_eq!(&got_ram[0xF000..0xF004], &0xDEAD_BEEFu32.to_le_bytes());

    assert_eq!(host_state.mmio_exit_calls, 0);
    assert_eq!(host_state.mmu_translate_calls, 1);
    assert_eq!(host_state.slow_mem_reads, 0);
    assert_eq!(host_state.slow_mem_writes, 1);
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

#[test]
fn tier1_inline_tlb_high_ram_remap_cross_page_load_uses_contiguous_ram_offset() {
    const HIGH_RAM_BASE: u64 = 0x1_0000_0000;
    let addr_u64 = HIGH_RAM_BASE + 0xFFF;

    let desired_offset: usize = 0x10000;
    let ram_base: u64 = 0x5000_0000 + desired_offset as u64;

    let mut b = IrBuilder::new(0x1000);
    let addr = b.const_int(Width::W64, addr_u64);
    let v0 = b.load(Width::W16, addr);
    b.write_reg(
        GuestReg::Gpr {
            reg: Gpr::Rax,
            width: Width::W16,
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
            inline_tlb_cross_page_fastpath: true,
            ..Default::default()
        },
    );
    validate_wasm(&wasm);

    // Large enough to cover the computed offsets for the two bytes crossing the page boundary.
    let mut mem = vec![0u8; desired_offset + 0x2000];

    let mut cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    write_cpu_to_wasm_bytes(&cpu, &mut cpu_bytes);
    mem[CPU_PTR as usize..CPU_PTR as usize + cpu_bytes.len()].copy_from_slice(&cpu_bytes);

    let ctx = JitContext {
        ram_base,
        tlb_salt: TLB_SALT,
    };
    ctx.write_header_to_mem(&mut mem, JIT_CTX_PTR as usize);

    // We expect the cross-page W16 load at (4GiB + 0xFFF) to read:
    //   lo = byte at 0xFFF, hi = byte at 0x1000
    // after the Q35 high-RAM remap.
    mem[desired_offset + 0xFFF] = 0x34;
    mem[desired_offset + 0x1000] = 0x12;

    let pages = mem.len().div_ceil(65_536) as u32;
    // Ensure both pages are classified as RAM by `mmu_translate`.
    let ram_size = HIGH_RAM_BASE + 0x2000;
    let (mut store, memory, func) = instantiate(&wasm, pages, ram_size);
    memory.write(&mut store, 0, &mem).unwrap();

    let ret = func.call(&mut store, (CPU_PTR, JIT_CTX_PTR)).unwrap();
    assert_eq!(ret, 0x3000);

    let mut got_mem = vec![0u8; mem.len()];
    memory.read(&store, 0, &mut got_mem).unwrap();

    let snap = CpuSnapshot::from_wasm_bytes(&got_mem[0..abi::CPU_STATE_SIZE as usize]);
    assert_eq!(snap.rip, 0x3000);
    assert_eq!(snap.gpr[Gpr::Rax.as_u8() as usize] & 0xffff, 0x1234);

    let host_state = *store.data();
    assert!(host_state.mmu_translate_calls <= 2);
    assert_eq!(host_state.mmio_exit_calls, 0);
    assert_eq!(host_state.slow_mem_reads, 0);
    assert_eq!(host_state.slow_mem_writes, 0);
}

#[test]
fn tier1_inline_tlb_high_ram_remap_cross_page_store_uses_contiguous_ram_offset() {
    const HIGH_RAM_BASE: u64 = 0x1_0000_0000;
    let addr_u64 = HIGH_RAM_BASE + 0xFFF;

    let desired_offset: usize = 0x10000;
    let ram_base: u64 = 0x5000_0000 + desired_offset as u64;

    let mut b = IrBuilder::new(0x1000);
    let addr = b.const_int(Width::W64, addr_u64);
    let v0 = b.const_int(Width::W16, 0xBEEFu64);
    b.store(Width::W16, addr, v0);
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
            inline_tlb_cross_page_fastpath: true,
            ..Default::default()
        },
    );
    validate_wasm(&wasm);

    let mut mem = vec![0u8; desired_offset + 0x2000];

    let mut cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    write_cpu_to_wasm_bytes(&cpu, &mut cpu_bytes);
    mem[CPU_PTR as usize..CPU_PTR as usize + cpu_bytes.len()].copy_from_slice(&cpu_bytes);

    let ctx = JitContext {
        ram_base,
        tlb_salt: TLB_SALT,
    };
    ctx.write_header_to_mem(&mut mem, JIT_CTX_PTR as usize);

    let pages = mem.len().div_ceil(65_536) as u32;
    let ram_size = HIGH_RAM_BASE + 0x2000;
    let (mut store, memory, func) = instantiate(&wasm, pages, ram_size);
    memory.write(&mut store, 0, &mem).unwrap();

    let ret = func.call(&mut store, (CPU_PTR, JIT_CTX_PTR)).unwrap();
    assert_eq!(ret, 0x3000);

    let mut got_mem = vec![0u8; mem.len()];
    memory.read(&store, 0, &mut got_mem).unwrap();

    assert_eq!(got_mem[desired_offset + 0xFFF], 0xef);
    assert_eq!(got_mem[desired_offset + 0x1000], 0xbe);

    let host_state = *store.data();
    assert!(host_state.mmu_translate_calls <= 2);
    assert_eq!(host_state.mmio_exit_calls, 0);
    assert_eq!(host_state.slow_mem_reads, 0);
    assert_eq!(host_state.slow_mem_writes, 0);
}
