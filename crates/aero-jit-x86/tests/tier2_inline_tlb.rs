#![cfg(debug_assertions)]

use aero_jit_x86::abi;
use aero_jit_x86::jit_ctx::{self, JitContext};
use aero_jit_x86::tier2::ir::{Instr, Operand, TraceIr, TraceKind, ValueId};
use aero_jit_x86::tier2::opt::RegAllocPlan;
use aero_jit_x86::tier2::wasm_codegen::{
    Tier2WasmCodegen, Tier2WasmOptions, EXPORT_TRACE_FN, IMPORT_CODE_PAGE_VERSION,
};
use aero_jit_x86::wasm::{
    IMPORT_MEMORY, IMPORT_MEM_READ_U16, IMPORT_MEM_READ_U32, IMPORT_MEM_READ_U64,
    IMPORT_MEM_READ_U8, IMPORT_MEM_WRITE_U16, IMPORT_MEM_WRITE_U32, IMPORT_MEM_WRITE_U64,
    IMPORT_MEM_WRITE_U8, IMPORT_MMU_TRANSLATE, IMPORT_MODULE,
};
use aero_jit_x86::{
    JIT_TLB_ENTRIES, JIT_TLB_ENTRY_SIZE, JIT_TLB_INDEX_MASK, PAGE_BASE_MASK, PAGE_SHIFT, PAGE_SIZE,
    TLB_FLAG_EXEC, TLB_FLAG_IS_RAM, TLB_FLAG_READ, TLB_FLAG_WRITE,
};
use aero_types::{Gpr, Width};
use wasmi::{Caller, Engine, Func, Linker, Memory, MemoryType, Module, Store, TypedFunc};
use wasmparser::Validator;

#[derive(Debug, Default, Clone, Copy)]
struct HostState {
    mmu_translate_calls: u64,
    slow_mem_reads: u64,
    slow_mem_writes: u64,
    ram_size: u64,
}

fn validate_wasm(bytes: &[u8]) {
    let mut validator = Validator::new();
    validator.validate_all(bytes).unwrap();
}

fn read_u64_le(bytes: &[u8], off: usize) -> u64 {
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&bytes[off..off + 8]);
    u64::from_le_bytes(buf)
}

fn write_u64_le(bytes: &mut [u8], off: usize, val: u64) {
    bytes[off..off + 8].copy_from_slice(&val.to_le_bytes());
}

fn write_cpu_rip(bytes: &mut [u8], cpu_ptr: usize, rip: u64) {
    write_u64_le(bytes, cpu_ptr + abi::CPU_RIP_OFF as usize, rip);
}

fn write_cpu_rflags(bytes: &mut [u8], cpu_ptr: usize, rflags: u64) {
    write_u64_le(bytes, cpu_ptr + abi::CPU_RFLAGS_OFF as usize, rflags);
}

fn read_u32_le(bytes: &[u8], off: usize) -> u32 {
    let mut buf = [0u8; 4];
    buf.copy_from_slice(&bytes[off..off + 4]);
    u32::from_le_bytes(buf)
}

fn instantiate(
    wasm: &[u8],
    memory_pages: u32,
    ram_size: u64,
) -> (Store<HostState>, Memory, TypedFunc<(i32, i32), i64>) {
    let engine = Engine::default();
    let module = Module::new(&engine, wasm).unwrap();

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
    linker
        .define(
            IMPORT_MODULE,
            IMPORT_CODE_PAGE_VERSION,
            Func::wrap(
                &mut store,
                |_caller: Caller<'_, HostState>, _cpu_ptr: i32, _page: i64| -> i64 { 0 },
            ),
        )
        .unwrap();

    let instance = linker.instantiate_and_start(&mut store, &module).unwrap();
    let func = instance
        .get_typed_func::<(i32, i32), i64>(&store, EXPORT_TRACE_FN)
        .unwrap();
    (store, memory, func)
}

fn read_u64_from_memory(caller: &mut Caller<'_, HostState>, memory: &Memory, addr: usize) -> u64 {
    let mut buf = [0u8; 8];
    memory
        .read(caller, addr, &mut buf)
        .expect("memory read in bounds");
    u64::from_le_bytes(buf)
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
        v: u64,
    ) {
        let mut buf = [0u8; N];
        for (i, b) in buf.iter_mut().enumerate() {
            *b = (v >> (i * 8)) as u8;
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
                        cpu_ptr as usize
                            + (abi::CPU_STATE_SIZE as usize)
                            + (JitContext::RAM_BASE_OFFSET as usize),
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
                        cpu_ptr as usize
                            + (abi::CPU_STATE_SIZE as usize)
                            + (JitContext::RAM_BASE_OFFSET as usize),
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
                        cpu_ptr as usize
                            + (abi::CPU_STATE_SIZE as usize)
                            + (JitContext::RAM_BASE_OFFSET as usize),
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
                        cpu_ptr as usize
                            + (abi::CPU_STATE_SIZE as usize)
                            + (JitContext::RAM_BASE_OFFSET as usize),
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
                        cpu_ptr as usize
                            + (abi::CPU_STATE_SIZE as usize)
                            + (JitContext::RAM_BASE_OFFSET as usize),
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
                        cpu_ptr as usize
                            + (abi::CPU_STATE_SIZE as usize)
                            + (JitContext::RAM_BASE_OFFSET as usize),
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
                        cpu_ptr as usize
                            + (abi::CPU_STATE_SIZE as usize)
                            + (JitContext::RAM_BASE_OFFSET as usize),
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
                        cpu_ptr as usize
                            + (abi::CPU_STATE_SIZE as usize)
                            + (JitContext::RAM_BASE_OFFSET as usize),
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
                      _cpu_ptr: i32,
                      jit_ctx_ptr: i32,
                      vaddr: i64,
                      _access: i32|
                      -> i64 {
                    caller.data_mut().mmu_translate_calls += 1;

                    let vaddr_u = vaddr as u64;
                    let vpn = vaddr_u >> PAGE_SHIFT;
                    let idx = vpn & JIT_TLB_INDEX_MASK;

                    let tlb_salt = read_u64_from_memory(
                        &mut caller,
                        &mem,
                        jit_ctx_ptr as usize + (JitContext::TLB_SALT_OFFSET as usize),
                    );

                    let tag = (vpn ^ tlb_salt) | 1;
                    let is_ram = vaddr_u < caller.data().ram_size;

                    let phys_base = vaddr_u & PAGE_BASE_MASK;
                    let flags = TLB_FLAG_READ
                        | TLB_FLAG_WRITE
                        | TLB_FLAG_EXEC
                        | if is_ram { TLB_FLAG_IS_RAM } else { 0 };
                    let data = phys_base | flags;

                    let entry_addr = jit_ctx_ptr as usize
                        + (JitContext::TLB_OFFSET as usize)
                        + (idx as usize) * (JIT_TLB_ENTRY_SIZE as usize);
                    let mem_mut = mem.data_mut(&mut caller);
                    mem_mut[entry_addr..entry_addr + 8].copy_from_slice(&tag.to_le_bytes());
                    mem_mut[entry_addr + 8..entry_addr + 16].copy_from_slice(&data.to_le_bytes());

                    data as i64
                },
            ),
        )
        .unwrap();
}

fn run_trace(
    trace: &TraceIr,
    ram: Vec<u8>,
    cpu_ptr: u64,
    ram_size: u64,
) -> (u64, Vec<u8>, [u64; 16], HostState) {
    let plan = RegAllocPlan::default();
    let wasm = Tier2WasmCodegen::new().compile_trace_with_options(
        trace,
        &plan,
        Tier2WasmOptions {
            inline_tlb: true,
            code_version_guard_import: true,
            ..Default::default()
        },
    );
    validate_wasm(&wasm);

    let cpu_ptr_usize = cpu_ptr as usize;
    let jit_ctx_ptr_usize = cpu_ptr_usize + (abi::CPU_STATE_SIZE as usize);
    let total_len =
        jit_ctx_ptr_usize + JitContext::TOTAL_BYTE_SIZE + (jit_ctx::TIER2_CTX_SIZE as usize);
    let mut mem = vec![0u8; total_len];

    // RAM at `ram_base = 0`.
    assert!(ram.len() <= cpu_ptr as usize, "ram must fit before cpu_ptr");
    mem[..ram.len()].copy_from_slice(&ram);

    // CPU state at `cpu_ptr`, JIT context immediately following.
    write_cpu_rip(&mut mem, cpu_ptr_usize, 0x1000);
    write_cpu_rflags(&mut mem, cpu_ptr_usize, 0x2);

    let ctx = JitContext {
        ram_base: 0,
        tlb_salt: 0x1234_5678_9abc_def0,
    };
    ctx.write_header_to_mem(&mut mem, jit_ctx_ptr_usize);

    let pages = (total_len.div_ceil(65_536)) as u32;
    let (mut store, memory, func) = instantiate(&wasm, pages, ram_size);
    memory.write(&mut store, 0, &mem).unwrap();

    let ret = func
        .call(&mut store, (cpu_ptr as i32, jit_ctx_ptr_usize as i32))
        .unwrap() as u64;

    let mut got_mem = vec![0u8; total_len];
    memory.read(&store, 0, &mut got_mem).unwrap();

    let mut gpr = [0u64; 16];
    for (dst, off) in gpr.iter_mut().zip(abi::CPU_GPR_OFF.iter()) {
        *dst = read_u64_le(&got_mem, cpu_ptr_usize + (*off as usize));
    }

    (ret, got_mem[..ram.len()].to_vec(), gpr, *store.data())
}

#[test]
fn tier2_inline_tlb_same_page_access_hits_and_caches() {
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![
            Instr::StoreMem {
                addr: Operand::Const(0x1000),
                src: Operand::Const(0xAB),
                width: Width::W8,
            },
            Instr::StoreMem {
                addr: Operand::Const(0x1004),
                src: Operand::Const(0x1234_5678),
                width: Width::W32,
            },
            Instr::LoadMem {
                dst: ValueId(0),
                addr: Operand::Const(0x1000),
                width: Width::W8,
            },
            Instr::LoadMem {
                dst: ValueId(1),
                addr: Operand::Const(0x1004),
                width: Width::W32,
            },
            Instr::StoreReg {
                reg: Gpr::Rbx,
                src: Operand::Value(ValueId(0)),
            },
            Instr::StoreReg {
                reg: Gpr::Rax,
                src: Operand::Value(ValueId(1)),
            },
        ],
        kind: TraceKind::Linear,
    };

    let ram = vec![0u8; 0x20_000];
    let cpu_ptr = ram.len() as u64;
    let (ret, got_ram, gpr, host) = run_trace(&trace, ram, cpu_ptr, 0x20_000);

    assert_eq!(ret, 0x1000);
    assert_eq!(gpr[Gpr::Rax.as_u8() as usize], 0x1234_5678);
    assert_eq!(gpr[Gpr::Rbx.as_u8() as usize] & 0xff, 0xAB);

    assert_eq!(got_ram[0x1000], 0xAB);
    assert_eq!(read_u32_le(&got_ram, 0x1004), 0x1234_5678);

    assert_eq!(host.mmu_translate_calls, 1);
    assert_eq!(host.slow_mem_reads, 0);
    assert_eq!(host.slow_mem_writes, 0);
}

#[test]
fn tier2_inline_tlb_collision_forces_retranslate() {
    let collide_addr = (JIT_TLB_ENTRIES as u64) << PAGE_SHIFT;

    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![
            Instr::LoadMem {
                dst: ValueId(0),
                addr: Operand::Const(0),
                width: Width::W32,
            },
            Instr::LoadMem {
                dst: ValueId(1),
                addr: Operand::Const(collide_addr),
                width: Width::W32,
            },
            Instr::LoadMem {
                dst: ValueId(2),
                addr: Operand::Const(4),
                width: Width::W32,
            },
            Instr::StoreReg {
                reg: Gpr::Rax,
                src: Operand::Value(ValueId(0)),
            },
            Instr::StoreReg {
                reg: Gpr::Rbx,
                src: Operand::Value(ValueId(1)),
            },
            Instr::StoreReg {
                reg: Gpr::Rcx,
                src: Operand::Value(ValueId(2)),
            },
        ],
        kind: TraceKind::Linear,
    };

    let mut ram = vec![0u8; 0x20_0000]; // 2MiB
    ram[0..4].copy_from_slice(&0x1111_2222u32.to_le_bytes());
    ram[collide_addr as usize..collide_addr as usize + 4]
        .copy_from_slice(&0x3333_4444u32.to_le_bytes());
    ram[4..8].copy_from_slice(&0x5555_6666u32.to_le_bytes());

    let cpu_ptr = ram.len() as u64;
    let (_ret, _got_ram, gpr, host) = run_trace(&trace, ram, cpu_ptr, 0x20_0000);

    assert_eq!(gpr[Gpr::Rax.as_u8() as usize], 0x1111_2222);
    assert_eq!(gpr[Gpr::Rbx.as_u8() as usize], 0x3333_4444);
    assert_eq!(gpr[Gpr::Rcx.as_u8() as usize], 0x5555_6666);

    assert_eq!(host.mmu_translate_calls, 3);
    assert_eq!(host.slow_mem_reads, 0);
    assert_eq!(host.slow_mem_writes, 0);
}

#[test]
fn tier2_inline_tlb_cross_page_load_uses_slow_helper() {
    let addr = PAGE_SIZE - 2;
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![
            Instr::LoadMem {
                dst: ValueId(0),
                addr: Operand::Const(addr),
                width: Width::W32,
            },
            Instr::StoreReg {
                reg: Gpr::Rax,
                src: Operand::Value(ValueId(0)),
            },
        ],
        kind: TraceKind::Linear,
    };

    let mut ram = vec![0u8; 0x20_000];
    ram[addr as usize..addr as usize + 4].copy_from_slice(&0xDDCC_BBAAu32.to_le_bytes());

    let cpu_ptr = ram.len() as u64;
    let (_ret, _got_ram, gpr, host) = run_trace(&trace, ram, cpu_ptr, 0x20_000);

    assert_eq!(gpr[Gpr::Rax.as_u8() as usize] as u32, 0xDDCC_BBAA);
    assert_eq!(host.mmu_translate_calls, 0);
    assert_eq!(host.slow_mem_reads, 1);
    assert_eq!(host.slow_mem_writes, 0);
}

#[test]
fn tier2_inline_tlb_high_ram_remap_load_uses_contiguous_ram_offset() {
    // Q35 layout:
    // - low RAM:  [0x0000_0000 .. 0xB000_0000)
    // - hole:     [0xB000_0000 .. 0x1_0000_0000)
    // - high RAM: [0x1_0000_0000 .. ...] remapped to start at 0xB000_0000 in the contiguous RAM
    //             backing store.
    const HIGH_RAM_BASE: u64 = 0x1_0000_0000;

    // Use the same i32.wrap_i64 wraparound trick as the Tier-1 inline-TLB remap test: pick a
    // `ram_base` such that the *correct* Q35 remap wraps the final wasm32 linear-memory address into
    // a small in-bounds offset, while the buggy `ram_base + paddr` mapping stays large and traps.
    let desired_offset: usize = 0x10000;
    let ram_base: u64 = 0x5000_0000 + desired_offset as u64;

    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![
            Instr::LoadMem {
                dst: ValueId(0),
                addr: Operand::Const(HIGH_RAM_BASE),
                width: Width::W8,
            },
            Instr::StoreReg {
                reg: Gpr::Rax,
                src: Operand::Value(ValueId(0)),
            },
        ],
        kind: TraceKind::Linear,
    };

    let plan = RegAllocPlan::default();
    let wasm = Tier2WasmCodegen::new().compile_trace_with_options(
        &trace,
        &plan,
        Tier2WasmOptions {
            inline_tlb: true,
            // No code-version guards in this trace, but preserve the existing default ABI.
            code_version_guard_import: true,
            ..Default::default()
        },
    );
    validate_wasm(&wasm);

    // Keep RAM small (no multi-GiB allocations). The translated wasm address should wrap into this
    // slice at `desired_offset`.
    let mut ram = vec![0u8; 0x20_000];
    ram[desired_offset] = 0x7f;

    let cpu_ptr = ram.len() as u64;
    let cpu_ptr_usize = cpu_ptr as usize;
    let jit_ctx_ptr_usize = cpu_ptr_usize + (abi::CPU_STATE_SIZE as usize);
    let total_len =
        jit_ctx_ptr_usize + JitContext::TOTAL_BYTE_SIZE + (jit_ctx::TIER2_CTX_SIZE as usize);
    let mut mem = vec![0u8; total_len];

    mem[..ram.len()].copy_from_slice(&ram);
    write_cpu_rip(&mut mem, cpu_ptr_usize, 0x1000);
    write_cpu_rflags(&mut mem, cpu_ptr_usize, 0x2);

    let ctx = JitContext {
        ram_base,
        tlb_salt: 0x1234_5678_9abc_def0,
    };
    ctx.write_header_to_mem(&mut mem, jit_ctx_ptr_usize);

    let pages = total_len.div_ceil(65_536) as u32;
    // Make `mmu_translate` classify the 4GiB address as RAM so the inline-TLB fast-path is taken.
    let ram_size = HIGH_RAM_BASE + 0x1000;
    let (mut store, memory, func) = instantiate(&wasm, pages, ram_size);
    memory.write(&mut store, 0, &mem).unwrap();

    let _ret = func
        .call(&mut store, (cpu_ptr as i32, jit_ctx_ptr_usize as i32))
        .unwrap();

    let mut got_mem = vec![0u8; total_len];
    memory.read(&store, 0, &mut got_mem).unwrap();

    let rax = read_u64_le(
        &got_mem,
        cpu_ptr_usize + abi::gpr_offset(Gpr::Rax.as_u8() as usize) as usize,
    );
    assert_eq!(rax & 0xff, 0x7f);

    let host = *store.data();
    assert_eq!(host.mmu_translate_calls, 1);
    assert_eq!(host.slow_mem_reads, 0);
    assert_eq!(host.slow_mem_writes, 0);
}

#[test]
fn tier2_inline_tlb_high_ram_remap_store_uses_contiguous_ram_offset() {
    const HIGH_RAM_BASE: u64 = 0x1_0000_0000;

    let desired_offset: usize = 0x10000;
    let ram_base: u64 = 0x5000_0000 + desired_offset as u64;

    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![
            Instr::StoreMem {
                addr: Operand::Const(HIGH_RAM_BASE),
                src: Operand::Const(0xab),
                width: Width::W8,
            },
            Instr::LoadMem {
                dst: ValueId(0),
                addr: Operand::Const(HIGH_RAM_BASE),
                width: Width::W8,
            },
            Instr::StoreReg {
                reg: Gpr::Rax,
                src: Operand::Value(ValueId(0)),
            },
        ],
        kind: TraceKind::Linear,
    };

    let plan = RegAllocPlan::default();
    let wasm = Tier2WasmCodegen::new().compile_trace_with_options(
        &trace,
        &plan,
        Tier2WasmOptions {
            inline_tlb: true,
            code_version_guard_import: true,
            ..Default::default()
        },
    );
    validate_wasm(&wasm);

    let mut ram = vec![0u8; 0x20_000];
    ram[desired_offset] = 0;

    let cpu_ptr = ram.len() as u64;
    let cpu_ptr_usize = cpu_ptr as usize;
    let jit_ctx_ptr_usize = cpu_ptr_usize + (abi::CPU_STATE_SIZE as usize);
    let total_len =
        jit_ctx_ptr_usize + JitContext::TOTAL_BYTE_SIZE + (jit_ctx::TIER2_CTX_SIZE as usize);
    let mut mem = vec![0u8; total_len];

    mem[..ram.len()].copy_from_slice(&ram);
    write_cpu_rip(&mut mem, cpu_ptr_usize, 0x1000);
    write_cpu_rflags(&mut mem, cpu_ptr_usize, 0x2);

    let ctx = JitContext {
        ram_base,
        tlb_salt: 0x1234_5678_9abc_def0,
    };
    ctx.write_header_to_mem(&mut mem, jit_ctx_ptr_usize);

    let pages = total_len.div_ceil(65_536) as u32;
    let ram_size = HIGH_RAM_BASE + 0x1000;
    let (mut store, memory, func) = instantiate(&wasm, pages, ram_size);
    memory.write(&mut store, 0, &mem).unwrap();

    let _ret = func
        .call(&mut store, (cpu_ptr as i32, jit_ctx_ptr_usize as i32))
        .unwrap();

    let mut got_mem = vec![0u8; total_len];
    memory.read(&store, 0, &mut got_mem).unwrap();

    let got_ram = &got_mem[..ram.len()];
    assert_eq!(got_ram[desired_offset], 0xab);

    let rax = read_u64_le(
        &got_mem,
        cpu_ptr_usize + abi::gpr_offset(Gpr::Rax.as_u8() as usize) as usize,
    );
    assert_eq!(rax & 0xff, 0xab);

    let host = *store.data();
    assert_eq!(host.mmu_translate_calls, 1);
    assert_eq!(host.slow_mem_reads, 0);
    assert_eq!(host.slow_mem_writes, 0);
}
