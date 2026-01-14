#![cfg(debug_assertions)]

use aero_jit_x86::abi;
use aero_jit_x86::jit_ctx::{self, JitContext};
use aero_jit_x86::tier2::ir::{BinOp, Instr, Operand, TraceIr, TraceKind, ValueId};
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
use aero_types::{FlagSet, Gpr, Width};
use wasmi::{Caller, Engine, Func, Linker, Memory, MemoryType, Module, Store, TypedFunc};
use wasmparser::Validator;

#[derive(Debug, Default, Clone, Copy)]
struct HostState {
    mmu_translate_calls: u64,
    slow_mem_reads: u64,
    slow_mem_writes: u64,
    ram_size: u64,
    /// Optional test-only safety valve: panic if `mmu_translate` is called more than this many times
    /// (useful to prevent accidental infinite loops in `TraceKind::Loop` tests).
    max_mmu_translate_calls: Option<u64>,
    /// When set, the first `mmu_translate` call will return a translation without `TLB_FLAG_WRITE`
    /// set. This forces the inline-TLB permission check path to re-translate.
    drop_write_flag_on_first_call: bool,
    /// When set, the first `mmu_translate` call will return a translation without `TLB_FLAG_READ`
    /// set. This forces the inline-TLB permission check path to re-translate.
    drop_read_flag_on_first_call: bool,
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

fn write_u32_le(bytes: &mut [u8], off: usize, val: u32) {
    bytes[off..off + 4].copy_from_slice(&val.to_le_bytes());
}

fn write_cpu_rip(bytes: &mut [u8], cpu_ptr: usize, rip: u64) {
    write_u64_le(bytes, cpu_ptr + abi::CPU_RIP_OFF as usize, rip);
}

fn write_cpu_rflags(bytes: &mut [u8], cpu_ptr: usize, rflags: u64) {
    write_u64_le(bytes, cpu_ptr + abi::CPU_RFLAGS_OFF as usize, rflags);
}

fn write_cpu_gpr(bytes: &mut [u8], cpu_ptr: usize, reg: Gpr, value: u64) {
    write_u64_le(
        bytes,
        cpu_ptr + (abi::CPU_GPR_OFF[reg.as_u8() as usize] as usize),
        value,
    );
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
    instantiate_with_host_state(
        wasm,
        memory_pages,
        HostState {
            ram_size,
            ..Default::default()
        },
    )
}

fn instantiate_with_host_state(
    wasm: &[u8],
    memory_pages: u32,
    host_state: HostState,
) -> (Store<HostState>, Memory, TypedFunc<(i32, i32), i64>) {
    let engine = Engine::default();
    let module = Module::new(&engine, wasm).unwrap();

    let mut store = Store::new(&engine, host_state);
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

fn bump_code_versions(
    caller: &mut Caller<'_, HostState>,
    memory: &Memory,
    cpu_ptr: i32,
    paddr: u64,
    len: usize,
) {
    if len == 0 {
        return;
    }
    let cpu_ptr = cpu_ptr as usize;

    let mut buf = [0u8; 4];
    memory
        .read(
            &mut *caller,
            cpu_ptr + jit_ctx::CODE_VERSION_TABLE_LEN_OFFSET as usize,
            &mut buf,
        )
        .unwrap();
    let table_len = u32::from_le_bytes(buf) as u64;
    if table_len == 0 {
        return;
    }

    memory
        .read(
            &mut *caller,
            cpu_ptr + jit_ctx::CODE_VERSION_TABLE_PTR_OFFSET as usize,
            &mut buf,
        )
        .unwrap();
    let table_ptr = u32::from_le_bytes(buf) as u64;

    let start_page = paddr >> PAGE_SHIFT;
    let end = paddr.saturating_add(len as u64 - 1);
    let end_page = end >> PAGE_SHIFT;

    for page in start_page..=end_page {
        if page >= table_len {
            continue;
        }
        let addr = table_ptr as usize + page as usize * 4;
        memory.read(&mut *caller, addr, &mut buf).unwrap();
        let cur = u32::from_le_bytes(buf);
        memory
            .write(&mut *caller, addr, &cur.wrapping_add(1).to_le_bytes())
            .unwrap();
    }
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
                    if (addr as u64) < caller.data().ram_size {
                        bump_code_versions(&mut caller, &mem, cpu_ptr, addr as u64, 1);
                    }
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
                    if (addr as u64) < caller.data().ram_size {
                        bump_code_versions(&mut caller, &mem, cpu_ptr, addr as u64, 2);
                    }
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
                    if (addr as u64) < caller.data().ram_size {
                        bump_code_versions(&mut caller, &mem, cpu_ptr, addr as u64, 4);
                    }
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
                    if (addr as u64) < caller.data().ram_size {
                        bump_code_versions(&mut caller, &mem, cpu_ptr, addr as u64, 8);
                    }
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
                    let call_idx = {
                        let data = caller.data_mut();
                        data.mmu_translate_calls += 1;
                        data.mmu_translate_calls
                    };
                    if let Some(max) = caller.data().max_mmu_translate_calls {
                        assert!(
                            call_idx <= max,
                            "mmu_translate called too many times: {call_idx} > {max}"
                        );
                    }

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
                    let mut flags = TLB_FLAG_READ
                        | TLB_FLAG_WRITE
                        | TLB_FLAG_EXEC
                        | if is_ram { TLB_FLAG_IS_RAM } else { 0 };
                    if caller.data().drop_write_flag_on_first_call && call_idx == 1 {
                        flags &= !TLB_FLAG_WRITE;
                    }
                    if caller.data().drop_read_flag_on_first_call && call_idx == 1 {
                        flags &= !TLB_FLAG_READ;
                    }
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

fn run_trace_with_init_gprs(
    trace: &TraceIr,
    ram: Vec<u8>,
    cpu_ptr: u64,
    ram_size: u64,
    init_gprs: &[(Gpr, u64)],
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
    for &(reg, value) in init_gprs {
        write_cpu_gpr(&mut mem, cpu_ptr_usize, reg, value);
    }

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

fn run_trace_with_prefilled_tlbs(
    trace: &TraceIr,
    ram: Vec<u8>,
    cpu_ptr: u64,
    ram_size: u64,
    prefill_tlbs: &[(u64, u64)],
) -> (u64, Vec<u8>, [u64; 16], HostState) {
    let tlb_salt = 0x1234_5678_9abc_def0u64;
    let mut raw = Vec::with_capacity(prefill_tlbs.len());
    for &(vaddr, tlb_data) in prefill_tlbs {
        let vpn = vaddr >> PAGE_SHIFT;
        let tag = (vpn ^ tlb_salt) | 1;
        raw.push((vaddr, tag, tlb_data));
    }
    run_trace_with_custom_tlb_salt_and_raw_prefilled_tlbs(
        trace, ram, cpu_ptr, ram_size, tlb_salt, &raw,
    )
}

fn run_trace_with_custom_tlb_salt_and_raw_prefilled_tlbs(
    trace: &TraceIr,
    ram: Vec<u8>,
    cpu_ptr: u64,
    ram_size: u64,
    ctx_tlb_salt: u64,
    prefill_tlbs: &[(u64, u64, u64)],
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
        tlb_salt: ctx_tlb_salt,
    };
    ctx.write_header_to_mem(&mut mem, jit_ctx_ptr_usize);

    for &(vaddr, tag, tlb_data) in prefill_tlbs {
        let vpn = vaddr >> PAGE_SHIFT;
        let idx = (vpn & JIT_TLB_INDEX_MASK) as usize;
        let entry_addr = jit_ctx_ptr_usize
            + (JitContext::TLB_OFFSET as usize)
            + idx * (JIT_TLB_ENTRY_SIZE as usize);
        mem[entry_addr..entry_addr + 8].copy_from_slice(&tag.to_le_bytes());
        mem[entry_addr + 8..entry_addr + 16].copy_from_slice(&tlb_data.to_le_bytes());
    }

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

fn run_trace_with_code_version_table(
    trace: &TraceIr,
    ram: Vec<u8>,
    cpu_ptr: u64,
    ram_size: u64,
    table_ptr: u32,
    table: &[u32],
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
    let table_ptr_usize = table_ptr as usize;
    assert!(
        table_ptr_usize
            .checked_add(table.len().saturating_mul(4))
            .is_some_and(|end| end <= cpu_ptr_usize),
        "code version table must live within guest RAM for this test"
    );
    assert!(
        table_ptr_usize.is_multiple_of(4),
        "code version table must be 4-byte aligned"
    );

    let jit_ctx_ptr_usize = cpu_ptr_usize + (abi::CPU_STATE_SIZE as usize);
    let total_len =
        jit_ctx_ptr_usize + JitContext::TOTAL_BYTE_SIZE + (jit_ctx::TIER2_CTX_SIZE as usize);
    let mut mem = vec![0u8; total_len];

    // RAM at `ram_base = 0`.
    assert!(ram.len() <= cpu_ptr as usize, "ram must fit before cpu_ptr");
    mem[..ram.len()].copy_from_slice(&ram);

    // Install the page-version table in guest RAM (at `table_ptr`) and point the Tier-2 ctx fields
    // at it.
    write_u32_le(
        &mut mem,
        cpu_ptr_usize + jit_ctx::CODE_VERSION_TABLE_PTR_OFFSET as usize,
        table_ptr,
    );
    write_u32_le(
        &mut mem,
        cpu_ptr_usize + jit_ctx::CODE_VERSION_TABLE_LEN_OFFSET as usize,
        table.len() as u32,
    );
    for (idx, v) in table.iter().copied().enumerate() {
        write_u32_le(&mut mem, table_ptr_usize + idx * 4, v);
    }

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

fn run_trace_with_host_state(
    trace: &TraceIr,
    ram: Vec<u8>,
    cpu_ptr: u64,
    host_state: HostState,
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
    let ram_size = host_state.ram_size;

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
    let (mut store, memory, func) = instantiate_with_host_state(&wasm, pages, host_state);
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

    // Ensure `ram_size` survives in the copied-out host state for debugging.
    let mut out = *store.data();
    out.ram_size = ram_size;
    (ret, got_mem[..ram.len()].to_vec(), gpr, out)
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
fn tier2_inline_tlb_loop_trace_guard_exits_and_preserves_inline_fast_path() {
    // Execute an inline-TLB store trace compiled as a `TraceKind::Loop`, and use a guard to exit
    // after a few iterations. This exercises the depth accounting for `br` targets in loop traces.
    //
    // The trace uses RAX as an in-trace loop counter/address, incrementing it by one page each
    // iteration so `mmu_translate` is called repeatedly. A test-only `max_mmu_translate_calls`
    // safety valve ensures we fail fast (without hanging) if the guard doesn't exit the loop.
    const ITERATIONS: u64 = 3;
    let exit_rip = 0x2222u64;
    let exit_at: u64 = ITERATIONS * PAGE_SIZE;

    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![
            // v0 = rax
            Instr::LoadReg {
                dst: ValueId(0),
                reg: Gpr::Rax,
            },
            // [v0] = 0xab
            Instr::StoreMem {
                addr: Operand::Value(ValueId(0)),
                src: Operand::Const(0xAB),
                width: Width::W8,
            },
            // v1 = v0 + PAGE_SIZE
            Instr::BinOp {
                dst: ValueId(1),
                op: BinOp::Add,
                lhs: Operand::Value(ValueId(0)),
                rhs: Operand::Const(PAGE_SIZE),
                flags: FlagSet::EMPTY,
            },
            // rax = v1
            Instr::StoreReg {
                reg: Gpr::Rax,
                src: Operand::Value(ValueId(1)),
            },
            // v2 = (v1 == exit_at)
            Instr::BinOp {
                dst: ValueId(2),
                op: BinOp::Eq,
                lhs: Operand::Value(ValueId(1)),
                rhs: Operand::Const(exit_at),
                flags: FlagSet::EMPTY,
            },
            // Exit when v2 becomes true.
            Instr::Guard {
                cond: Operand::Value(ValueId(2)),
                expected: false,
                exit_rip,
            },
        ],
        kind: TraceKind::Loop,
    };

    let ram = vec![0u8; 0x20_000];
    let cpu_ptr = ram.len() as u64;
    let (ret, got_ram, gpr, host) = run_trace_with_host_state(
        &trace,
        ram,
        cpu_ptr,
        HostState {
            ram_size: 0x20_000,
            max_mmu_translate_calls: Some(10),
            ..Default::default()
        },
    );

    assert_eq!(ret, exit_rip);
    assert_eq!(gpr[Gpr::Rax.as_u8() as usize], exit_at);

    assert_eq!(got_ram[0], 0xAB);
    assert_eq!(got_ram[PAGE_SIZE as usize], 0xAB);
    assert_eq!(got_ram[(2 * PAGE_SIZE) as usize], 0xAB);

    assert_eq!(host.mmu_translate_calls, ITERATIONS);
    assert_eq!(host.slow_mem_reads, 0);
    assert_eq!(host.slow_mem_writes, 0);
}

#[test]
fn tier2_inline_tlb_load_on_prefilled_non_ram_tlb_entry_uses_slow_helper() {
    // Ensure a cached non-RAM entry (missing `TLB_FLAG_IS_RAM`) falls back to the slow helper
    // without calling `mmu_translate`.
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![
            Instr::LoadMem {
                dst: ValueId(0),
                addr: Operand::Const(0x1234),
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
    ram[0x1234..0x1234 + 4].copy_from_slice(&0xDDCC_BBAAu32.to_le_bytes());
    let cpu_ptr = ram.len() as u64;
    let tlb_data = (0x1234u64 & PAGE_BASE_MASK) | (TLB_FLAG_READ | TLB_FLAG_WRITE | TLB_FLAG_EXEC);

    let (_ret, _got_ram, gpr, host) =
        run_trace_with_prefilled_tlbs(&trace, ram, cpu_ptr, 0x20_000, &[(0x1234, tlb_data)]);

    assert_eq!(gpr[Gpr::Rax.as_u8() as usize] as u32, 0xDDCC_BBAA);
    assert_eq!(host.mmu_translate_calls, 0);
    assert_eq!(host.slow_mem_reads, 1);
    assert_eq!(host.slow_mem_writes, 0);
}

#[test]
fn tier2_inline_tlb_store_on_prefilled_non_ram_tlb_entry_uses_slow_helper() {
    // Ensure a cached non-RAM entry (missing `TLB_FLAG_IS_RAM`) falls back to the slow helper
    // without calling `mmu_translate`.
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![Instr::StoreMem {
            addr: Operand::Const(0x1234),
            src: Operand::Const(0xDDCC_BBAA),
            width: Width::W32,
        }],
        kind: TraceKind::Linear,
    };

    let ram = vec![0u8; 0x20_000];
    let cpu_ptr = ram.len() as u64;
    let tlb_data = (0x1234u64 & PAGE_BASE_MASK) | (TLB_FLAG_READ | TLB_FLAG_WRITE | TLB_FLAG_EXEC);

    let (_ret, got_ram, _gpr, host) =
        run_trace_with_prefilled_tlbs(&trace, ram, cpu_ptr, 0x20_000, &[(0x1234, tlb_data)]);

    assert_eq!(read_u32_le(&got_ram, 0x1234), 0xDDCC_BBAA);
    assert_eq!(host.mmu_translate_calls, 0);
    assert_eq!(host.slow_mem_reads, 0);
    assert_eq!(host.slow_mem_writes, 1);
}

#[test]
fn tier2_inline_tlb_prefilled_ram_entry_uses_physical_base_for_load() {
    // Ensure the inline-TLB RAM fast path uses the physical page base from the cached TLB entry
    // when computing the RAM address (i.e. supports non-identity mappings).
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![
            Instr::LoadMem {
                dst: ValueId(0),
                addr: Operand::Const(0x1010),
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
    ram[0x1010..0x1010 + 4].copy_from_slice(&0x1111_2222u32.to_le_bytes());
    ram[0x2010..0x2010 + 4].copy_from_slice(&0x3333_4444u32.to_le_bytes());
    let cpu_ptr = ram.len() as u64;

    // Prefill a TLB entry that maps vaddr page 1 (0x1000..0x1FFF) to phys_base 0x2000.
    let flags = TLB_FLAG_READ | TLB_FLAG_WRITE | TLB_FLAG_EXEC | TLB_FLAG_IS_RAM;
    let tlb_data = (0x2000u64 & PAGE_BASE_MASK) | flags;

    let (_ret, _got_ram, gpr, host) =
        run_trace_with_prefilled_tlbs(&trace, ram, cpu_ptr, 0, &[(0x1010, tlb_data)]);

    assert_eq!(gpr[Gpr::Rax.as_u8() as usize] as u32, 0x3333_4444);
    assert_eq!(host.mmu_translate_calls, 0);
    assert_eq!(host.slow_mem_reads, 0);
    assert_eq!(host.slow_mem_writes, 0);
}

#[test]
fn tier2_inline_tlb_prefilled_ram_entry_uses_physical_base_for_store() {
    // Like `tier2_inline_tlb_prefilled_ram_entry_uses_physical_base_for_load`, but for stores.
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![Instr::StoreMem {
            addr: Operand::Const(0x1010),
            src: Operand::Const(0xDDCC_BBAA),
            width: Width::W32,
        }],
        kind: TraceKind::Linear,
    };

    let mut ram = vec![0u8; 0x20_000];
    ram[0x1010..0x1010 + 4].copy_from_slice(&0x1111_2222u32.to_le_bytes());
    ram[0x2010..0x2010 + 4].copy_from_slice(&0x3333_4444u32.to_le_bytes());
    let cpu_ptr = ram.len() as u64;

    let flags = TLB_FLAG_READ | TLB_FLAG_WRITE | TLB_FLAG_EXEC | TLB_FLAG_IS_RAM;
    let tlb_data = (0x2000u64 & PAGE_BASE_MASK) | flags;

    let (_ret, got_ram, _gpr, host) =
        run_trace_with_prefilled_tlbs(&trace, ram, cpu_ptr, 0, &[(0x1010, tlb_data)]);

    // Store should target the physical backing page (0x2000), not the virtual address (0x1000).
    assert_eq!(read_u32_le(&got_ram, 0x1010), 0x1111_2222);
    assert_eq!(read_u32_le(&got_ram, 0x2010), 0xDDCC_BBAA);

    assert_eq!(host.mmu_translate_calls, 0);
    assert_eq!(host.slow_mem_reads, 0);
    assert_eq!(host.slow_mem_writes, 0);
}

#[test]
fn tier2_inline_tlb_prefilled_ram_entry_bumps_physical_code_page_version() {
    // Ensure code-version bumping uses the physical page number from the cached TLB entry (not the
    // virtual page number).
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![Instr::StoreMem {
            addr: Operand::Const(0x1010),
            src: Operand::Const(0xAB),
            width: Width::W8,
        }],
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

    let table_ptr: usize = 0x8000;
    let table_len: u32 = 4;
    let table_bytes = (table_len as usize) * 4;

    let ram_len = table_ptr + table_bytes + 0x3000;
    let mut ram = vec![0u8; ram_len];
    // Sentinel bytes to detect whether the store targets vaddr or paddr.
    ram[0x1010] = 0x11;
    ram[0x2010] = 0x22;

    // Initialize the code-version table with distinct values so we can see which entry changes.
    write_u32_le(&mut ram, table_ptr + 4, 10); // page 1
    write_u32_le(&mut ram, table_ptr + 8, 20); // page 2 (expected bump target)

    let cpu_ptr = ram.len() as u64;
    let cpu_ptr_usize = cpu_ptr as usize;
    let jit_ctx_ptr_usize = cpu_ptr_usize + (abi::CPU_STATE_SIZE as usize);
    let total_len =
        jit_ctx_ptr_usize + JitContext::TOTAL_BYTE_SIZE + (jit_ctx::TIER2_CTX_SIZE as usize);
    let mut mem = vec![0u8; total_len];

    // RAM at `ram_base = 0`.
    mem[..ram.len()].copy_from_slice(&ram);

    // Configure the code-version table (stored in the Tier-2 ctx region relative to `cpu_ptr`).
    write_u32_le(
        &mut mem,
        cpu_ptr_usize + jit_ctx::CODE_VERSION_TABLE_PTR_OFFSET as usize,
        table_ptr as u32,
    );
    write_u32_le(
        &mut mem,
        cpu_ptr_usize + jit_ctx::CODE_VERSION_TABLE_LEN_OFFSET as usize,
        table_len,
    );

    // CPU state at `cpu_ptr`, JIT context immediately following.
    write_cpu_rip(&mut mem, cpu_ptr_usize, 0x1000);
    write_cpu_rflags(&mut mem, cpu_ptr_usize, 0x2);

    let tlb_salt = 0x1234_5678_9abc_def0u64;
    let ctx = JitContext {
        ram_base: 0,
        tlb_salt,
    };
    ctx.write_header_to_mem(&mut mem, jit_ctx_ptr_usize);

    // Prefill a TLB entry that maps vaddr page 1 (0x1000) to phys_base page 2 (0x2000).
    let vaddr = 0x1010u64;
    let vpn = vaddr >> PAGE_SHIFT;
    let idx = (vpn & JIT_TLB_INDEX_MASK) as usize;
    let entry_addr =
        jit_ctx_ptr_usize + (JitContext::TLB_OFFSET as usize) + idx * (JIT_TLB_ENTRY_SIZE as usize);
    let tag = (vpn ^ tlb_salt) | 1;
    let tlb_data = (0x2000u64 & PAGE_BASE_MASK)
        | (TLB_FLAG_READ | TLB_FLAG_WRITE | TLB_FLAG_EXEC | TLB_FLAG_IS_RAM);
    mem[entry_addr..entry_addr + 8].copy_from_slice(&tag.to_le_bytes());
    mem[entry_addr + 8..entry_addr + 16].copy_from_slice(&tlb_data.to_le_bytes());

    let pages = (total_len.div_ceil(65_536)) as u32;
    // Mark all pages as non-RAM for `mmu_translate`; the test relies on the prefilled TLB entry and
    // should not call `mmu_translate` at all.
    let (mut store, memory, func) = instantiate(&wasm, pages, 0);
    memory.write(&mut store, 0, &mem).unwrap();

    let ret = func
        .call(&mut store, (cpu_ptr as i32, jit_ctx_ptr_usize as i32))
        .unwrap() as u64;
    assert_eq!(ret, 0x1000);

    let mut got_mem = vec![0u8; total_len];
    memory.read(&store, 0, &mut got_mem).unwrap();
    let got_ram = &got_mem[..ram.len()];

    // Store should use paddr=0x2010, not vaddr=0x1010.
    assert_eq!(got_ram[0x1010], 0x11);
    assert_eq!(got_ram[0x2010], 0xAB);

    // Bump should use physical page index (2), not virtual page index (1).
    assert_eq!(read_u32_le(got_ram, table_ptr + 4), 10);
    assert_eq!(read_u32_le(got_ram, table_ptr + 8), 21);

    let host = *store.data();
    assert_eq!(host.mmu_translate_calls, 0);
    assert_eq!(host.slow_mem_reads, 0);
    assert_eq!(host.slow_mem_writes, 0);
}

#[test]
fn tier2_inline_tlb_permission_retranslate_updates_is_ram_flag_for_load() {
    // If a cached entry is missing the required permission flag, the inline-TLB path calls
    // `mmu_translate` and must use the updated `tlb_data` for subsequent checks (including the
    // `TLB_FLAG_IS_RAM` fast-path check).
    //
    // To catch bugs where the post-translate `tlb_data` isn't used, prefill an entry that:
    // - matches the tag
    // - is missing READ permission
    // - is missing `TLB_FLAG_IS_RAM`
    //
    // Correct behavior: call `mmu_translate` once, then take the RAM fast path (no slow helper).
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![
            Instr::LoadMem {
                dst: ValueId(0),
                addr: Operand::Const(0x1000),
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
    ram[0x1000..0x1000 + 4].copy_from_slice(&0x1122_3344u32.to_le_bytes());
    let cpu_ptr = ram.len() as u64;

    let tlb_data = (0x1000u64 & PAGE_BASE_MASK) | (TLB_FLAG_WRITE | TLB_FLAG_EXEC);
    let (_ret, _got_ram, gpr, host) =
        run_trace_with_prefilled_tlbs(&trace, ram, cpu_ptr, 0x20_000, &[(0x1000, tlb_data)]);

    assert_eq!(gpr[Gpr::Rax.as_u8() as usize] as u32, 0x1122_3344);
    assert_eq!(host.mmu_translate_calls, 1);
    assert_eq!(host.slow_mem_reads, 0);
    assert_eq!(host.slow_mem_writes, 0);
}

#[test]
fn tier2_inline_tlb_permission_retranslate_updates_is_ram_flag_for_store() {
    // Like `tier2_inline_tlb_permission_retranslate_updates_is_ram_flag_for_load`, but for stores.
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![Instr::StoreMem {
            addr: Operand::Const(0x1000),
            src: Operand::Const(0xAB),
            width: Width::W8,
        }],
        kind: TraceKind::Linear,
    };

    let ram = vec![0u8; 0x20_000];
    let cpu_ptr = ram.len() as u64;

    let tlb_data = (0x1000u64 & PAGE_BASE_MASK) | (TLB_FLAG_READ | TLB_FLAG_EXEC);
    let (_ret, got_ram, _gpr, host) =
        run_trace_with_prefilled_tlbs(&trace, ram, cpu_ptr, 0x20_000, &[(0x1000, tlb_data)]);

    assert_eq!(got_ram[0x1000], 0xAB);
    assert_eq!(host.mmu_translate_calls, 1);
    assert_eq!(host.slow_mem_reads, 0);
    assert_eq!(host.slow_mem_writes, 0);
}

#[test]
fn tier2_inline_tlb_permission_retranslate_can_clear_is_ram_flag_for_load() {
    // Complement `tier2_inline_tlb_permission_retranslate_updates_is_ram_flag_for_load`: ensure a
    // permission-miss re-translate can *remove* `TLB_FLAG_IS_RAM` and that the updated value is
    // used to select the slow helper path.
    //
    // We prefill a matching entry that incorrectly claims RAM, but set `ram_size` small enough so
    // `mmu_translate` returns a non-RAM translation.
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![
            Instr::LoadMem {
                dst: ValueId(0),
                addr: Operand::Const(0x1234),
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
    ram[0x1234..0x1234 + 4].copy_from_slice(&0xDDCC_BBAAu32.to_le_bytes());
    let cpu_ptr = ram.len() as u64;

    // Prefill a matching entry, but omit READ permission to force a re-translate. Intentionally
    // include `TLB_FLAG_IS_RAM` even though `mmu_translate` will classify this as non-RAM.
    let tlb_data =
        (0x1234u64 & PAGE_BASE_MASK) | (TLB_FLAG_WRITE | TLB_FLAG_EXEC | TLB_FLAG_IS_RAM);

    let (_ret, _got_ram, gpr, host) =
        run_trace_with_prefilled_tlbs(&trace, ram, cpu_ptr, 0x1000, &[(0x1234, tlb_data)]);

    assert_eq!(gpr[Gpr::Rax.as_u8() as usize] as u32, 0xDDCC_BBAA);
    assert_eq!(host.mmu_translate_calls, 1);
    assert_eq!(host.slow_mem_reads, 1);
    assert_eq!(host.slow_mem_writes, 0);
}

#[test]
fn tier2_inline_tlb_permission_retranslate_can_clear_is_ram_flag_for_store() {
    // Like `tier2_inline_tlb_permission_retranslate_can_clear_is_ram_flag_for_load`, but for
    // stores.
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![Instr::StoreMem {
            addr: Operand::Const(0x1234),
            src: Operand::Const(0xAB),
            width: Width::W8,
        }],
        kind: TraceKind::Linear,
    };

    let ram = vec![0u8; 0x20_000];
    let cpu_ptr = ram.len() as u64;

    // Prefill a matching entry, but omit WRITE permission to force a re-translate. Intentionally
    // include `TLB_FLAG_IS_RAM` even though `mmu_translate` will classify this as non-RAM.
    let tlb_data = (0x1234u64 & PAGE_BASE_MASK) | (TLB_FLAG_READ | TLB_FLAG_EXEC | TLB_FLAG_IS_RAM);

    let (_ret, got_ram, _gpr, host) =
        run_trace_with_prefilled_tlbs(&trace, ram, cpu_ptr, 0x1000, &[(0x1234, tlb_data)]);

    assert_eq!(got_ram[0x1234], 0xAB);
    assert_eq!(host.mmu_translate_calls, 1);
    assert_eq!(host.slow_mem_reads, 0);
    assert_eq!(host.slow_mem_writes, 1);
}

#[test]
fn tier2_inline_tlb_load_permission_miss_on_prefilled_entry_calls_translate() {
    // If a cached TLB entry lacks the required permission flag, the inline-TLB permission check
    // should call `mmu_translate` and retry using the updated entry.
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![
            Instr::LoadMem {
                dst: ValueId(0),
                addr: Operand::Const(0x1000),
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
    ram[0x1000..0x1000 + 4].copy_from_slice(&0x1122_3344u32.to_le_bytes());
    let cpu_ptr = ram.len() as u64;

    // Prefill a matching entry, but omit READ permission.
    let tlb_data =
        (0x1000u64 & PAGE_BASE_MASK) | (TLB_FLAG_WRITE | TLB_FLAG_EXEC | TLB_FLAG_IS_RAM);

    let (_ret, _got_ram, gpr, host) =
        run_trace_with_prefilled_tlbs(&trace, ram, cpu_ptr, 0x20_000, &[(0x1000, tlb_data)]);

    assert_eq!(gpr[Gpr::Rax.as_u8() as usize] as u32, 0x1122_3344);
    assert_eq!(host.mmu_translate_calls, 1);
    assert_eq!(host.slow_mem_reads, 0);
    assert_eq!(host.slow_mem_writes, 0);
}

#[test]
fn tier2_inline_tlb_store_permission_miss_on_prefilled_entry_calls_translate() {
    // If a cached TLB entry lacks the required permission flag, the inline-TLB permission check
    // should call `mmu_translate` and retry using the updated entry.
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![Instr::StoreMem {
            addr: Operand::Const(0x1000),
            src: Operand::Const(0xAB),
            width: Width::W8,
        }],
        kind: TraceKind::Linear,
    };

    let ram = vec![0u8; 0x20_000];
    let cpu_ptr = ram.len() as u64;

    // Prefill a matching entry, but omit WRITE permission.
    let tlb_data = (0x1000u64 & PAGE_BASE_MASK) | (TLB_FLAG_READ | TLB_FLAG_EXEC | TLB_FLAG_IS_RAM);

    let (_ret, got_ram, _gpr, host) =
        run_trace_with_prefilled_tlbs(&trace, ram, cpu_ptr, 0x20_000, &[(0x1000, tlb_data)]);

    assert_eq!(got_ram[0x1000], 0xAB);
    assert_eq!(host.mmu_translate_calls, 1);
    assert_eq!(host.slow_mem_reads, 0);
    assert_eq!(host.slow_mem_writes, 0);
}

#[test]
fn tier2_inline_tlb_tlb_salt_mismatch_forces_retranslate() {
    // The runtime can invalidate all cached entries by changing the TLB salt (rather than zeroing
    // tags). Ensure Tier-2 uses the salt from `JitContext` when checking tags.
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![
            Instr::LoadMem {
                dst: ValueId(0),
                addr: Operand::Const(0x1000),
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
    ram[0x1000..0x1000 + 4].copy_from_slice(&0x1122_3344u32.to_le_bytes());
    let cpu_ptr = ram.len() as u64;

    let vaddr = 0x1000u64;
    let vpn = vaddr >> PAGE_SHIFT;
    let old_salt = 0x1234_5678_9abc_def0u64;
    let new_salt = old_salt ^ 0x1111_1111_1111_1111;
    let stale_tag = (vpn ^ old_salt) | 1;
    let data = (vaddr & PAGE_BASE_MASK)
        | (TLB_FLAG_READ | TLB_FLAG_WRITE | TLB_FLAG_EXEC | TLB_FLAG_IS_RAM);

    let (_ret, _got_ram, gpr, host) = run_trace_with_custom_tlb_salt_and_raw_prefilled_tlbs(
        &trace,
        ram,
        cpu_ptr,
        0x20_000,
        new_salt,
        &[(vaddr, stale_tag, data)],
    );

    assert_eq!(gpr[Gpr::Rax.as_u8() as usize] as u32, 0x1122_3344);
    assert_eq!(host.mmu_translate_calls, 1);
    assert_eq!(host.slow_mem_reads, 0);
    assert_eq!(host.slow_mem_writes, 0);
}

#[test]
fn tier2_inline_tlb_tlb_tag_uses_or1_to_reserve_zero_for_invalidation() {
    // Tag=0 is reserved for invalidation. Ensure Tier-2 computes expected tags as
    // `(vpn ^ salt) | 1`, even when `vpn ^ salt == 0`.
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![
            Instr::LoadMem {
                dst: ValueId(0),
                addr: Operand::Const(0x1000),
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
    ram[0x1000..0x1000 + 4].copy_from_slice(&0x1122_3344u32.to_le_bytes());
    let cpu_ptr = ram.len() as u64;

    let vaddr = 0x1000u64;
    let vpn = vaddr >> PAGE_SHIFT;
    let salt = vpn;
    let tag = (vpn ^ salt) | 1;
    assert_eq!(tag, 1, "sanity: vpn^salt should be 0, so tag must be 1");

    let data = (vaddr & PAGE_BASE_MASK)
        | (TLB_FLAG_READ | TLB_FLAG_WRITE | TLB_FLAG_EXEC | TLB_FLAG_IS_RAM);

    let (_ret, _got_ram, gpr, host) = run_trace_with_custom_tlb_salt_and_raw_prefilled_tlbs(
        &trace,
        ram,
        cpu_ptr,
        0x20_000,
        salt,
        &[(vaddr, tag, data)],
    );

    assert_eq!(gpr[Gpr::Rax.as_u8() as usize] as u32, 0x1122_3344);
    assert_eq!(host.mmu_translate_calls, 0);
    assert_eq!(host.slow_mem_reads, 0);
    assert_eq!(host.slow_mem_writes, 0);
}

#[test]
fn tier2_inline_tlb_cross_page_store_uses_slow_helper() {
    let addr = PAGE_SIZE - 2;
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![Instr::StoreMem {
            addr: Operand::Const(addr),
            src: Operand::Const(0xDDCC_BBAA),
            width: Width::W32,
        }],
        kind: TraceKind::Linear,
    };

    let ram = vec![0u8; 0x20_000];
    let cpu_ptr = ram.len() as u64;
    let (_ret, got_ram, _gpr, host) = run_trace(&trace, ram, cpu_ptr, 0x20_000);

    assert_eq!(read_u32_le(&got_ram, addr as usize), 0xDDCC_BBAA);
    assert_eq!(host.mmu_translate_calls, 0);
    assert_eq!(host.slow_mem_reads, 0);
    assert_eq!(host.slow_mem_writes, 1);
}

#[test]
fn tier2_inline_tlb_cross_page_store_bumps_code_version_table_via_slow_helper() {
    let addr = 0x1fff; // last byte of page 1
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![Instr::StoreMem {
            addr: Operand::Const(addr),
            src: Operand::Const(0x1122_3344_5566_7788),
            width: Width::W64,
        }],
        kind: TraceKind::Linear,
    };

    let ram = vec![0u8; 0x20_000];
    let cpu_ptr = ram.len() as u64;
    let table_ptr: u32 = 0x8000;
    let (_ret, got_ram, _gpr, host) = run_trace_with_code_version_table(
        &trace,
        ram,
        cpu_ptr,
        0x20_000,
        table_ptr,
        &[123, u32::MAX, 5, 6],
    );

    // Cross-page store should still write the guest RAM and bump both touched pages (1 and 2).
    assert_eq!(read_u64_le(&got_ram, addr as usize), 0x1122_3344_5566_7788);
    assert_eq!(read_u32_le(&got_ram, table_ptr as usize), 123);
    assert_eq!(read_u32_le(&got_ram, table_ptr as usize + 4), 0);
    assert_eq!(read_u32_le(&got_ram, table_ptr as usize + 8), 6);
    assert_eq!(read_u32_le(&got_ram, table_ptr as usize + 12), 6);

    assert_eq!(host.mmu_translate_calls, 0);
    assert_eq!(host.slow_mem_reads, 0);
    assert_eq!(host.slow_mem_writes, 1);
}

#[test]
fn tier2_inline_tlb_store_bumps_code_version_table_on_unshared_memory() {
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![Instr::StoreMem {
            addr: Operand::Const(0x10),
            src: Operand::Const(0xAB),
            width: Width::W8,
        }],
        kind: TraceKind::Linear,
    };

    let ram = vec![0u8; 0x20_000];
    let cpu_ptr = ram.len() as u64;
    let table_ptr: u32 = 0x1000;
    let (_ret, got_ram, _gpr, host) =
        run_trace_with_code_version_table(&trace, ram, cpu_ptr, 0x20_000, table_ptr, &[u32::MAX]);

    assert_eq!(got_ram[0x10], 0xAB);
    assert_eq!(read_u32_le(&got_ram, table_ptr as usize), 0);
    assert_eq!(host.mmu_translate_calls, 1);
    assert_eq!(host.slow_mem_reads, 0);
    assert_eq!(host.slow_mem_writes, 0);
}

#[test]
fn tier2_inline_tlb_store_with_zero_length_code_version_table_does_not_trap() {
    // If the runtime hasn't configured a code-version table (`len == 0`), the inline bump fast-path
    // should be disabled and stores should still succeed.
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![Instr::StoreMem {
            addr: Operand::Const(0x10),
            src: Operand::Const(0xAB),
            width: Width::W8,
        }],
        kind: TraceKind::Linear,
    };

    let ram = vec![0u8; 0x20_000];
    let cpu_ptr = ram.len() as u64;
    // Point at some in-RAM location; it should not be dereferenced when len == 0.
    let table_ptr: u32 = 0x1000;
    let (_ret, got_ram, _gpr, host) =
        run_trace_with_code_version_table(&trace, ram, cpu_ptr, 0x20_000, table_ptr, &[]);

    assert_eq!(got_ram[0x10], 0xAB);
    assert_eq!(host.mmu_translate_calls, 1);
    assert_eq!(host.slow_mem_reads, 0);
    assert_eq!(host.slow_mem_writes, 0);
}

#[test]
fn tier2_inline_tlb_store_with_zero_length_code_version_table_ignores_invalid_ptr() {
    // Like `tier2_inline_tlb_store_with_zero_length_code_version_table_does_not_trap`, but ensure
    // the bump fast-path does not attempt to dereference the table pointer when `len == 0`.
    //
    // This is important because runtimes may leave the pointer uninitialized until the table is
    // configured.
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![Instr::StoreMem {
            addr: Operand::Const(0x10),
            src: Operand::Const(0xAB),
            width: Width::W8,
        }],
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

    let ram = vec![0u8; 0x20_000];
    let cpu_ptr = ram.len() as u64;

    let cpu_ptr_usize = cpu_ptr as usize;
    let jit_ctx_ptr_usize = cpu_ptr_usize + (abi::CPU_STATE_SIZE as usize);
    let total_len =
        jit_ctx_ptr_usize + JitContext::TOTAL_BYTE_SIZE + (jit_ctx::TIER2_CTX_SIZE as usize);
    let mut mem = vec![0u8; total_len];

    // RAM at `ram_base = 0`.
    mem[..ram.len()].copy_from_slice(&ram);

    // Set an intentionally invalid pointer but `len = 0`. The bump code should not dereference it.
    write_u32_le(
        &mut mem,
        cpu_ptr_usize + jit_ctx::CODE_VERSION_TABLE_PTR_OFFSET as usize,
        u32::MAX,
    );
    write_u32_le(
        &mut mem,
        cpu_ptr_usize + jit_ctx::CODE_VERSION_TABLE_LEN_OFFSET as usize,
        0,
    );

    // CPU state at `cpu_ptr`, JIT context immediately following.
    write_cpu_rip(&mut mem, cpu_ptr_usize, 0x1000);
    write_cpu_rflags(&mut mem, cpu_ptr_usize, 0x2);

    let ctx = JitContext {
        ram_base: 0,
        tlb_salt: 0x1234_5678_9abc_def0,
    };
    ctx.write_header_to_mem(&mut mem, jit_ctx_ptr_usize);

    let pages = (total_len.div_ceil(65_536)) as u32;
    let (mut store, memory, func) = instantiate(&wasm, pages, 0x20_000);
    memory.write(&mut store, 0, &mem).unwrap();

    let ret = func
        .call(&mut store, (cpu_ptr as i32, jit_ctx_ptr_usize as i32))
        .unwrap() as u64;
    assert_eq!(ret, 0x1000);

    let mut got_mem = vec![0u8; total_len];
    memory.read(&store, 0, &mut got_mem).unwrap();
    let got_ram = &got_mem[..ram.len()];
    assert_eq!(got_ram[0x10], 0xAB);

    let host = *store.data();
    assert_eq!(host.mmu_translate_calls, 1);
    assert_eq!(host.slow_mem_reads, 0);
    assert_eq!(host.slow_mem_writes, 0);
}

#[test]
fn tier2_inline_tlb_store_to_non_ram_uses_slow_helper_and_does_not_bump_code_version_table() {
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![Instr::StoreMem {
            addr: Operand::Const(0x1234),
            src: Operand::Const(0xCD),
            width: Width::W8,
        }],
        kind: TraceKind::Linear,
    };

    let ram = vec![0u8; 0x20_000];
    let cpu_ptr = ram.len() as u64;
    let table_ptr: u32 = 0x2000;
    let (_ret, got_ram, _gpr, host) = run_trace_with_code_version_table(
        &trace,
        ram,
        cpu_ptr,
        // Mark only the first 0x1000 bytes as RAM so the store address is considered non-RAM.
        0x1000,
        table_ptr,
        &[123],
    );

    assert_eq!(got_ram[0x1234], 0xCD);
    assert_eq!(read_u32_le(&got_ram, table_ptr as usize), 123);
    assert_eq!(host.mmu_translate_calls, 1);
    assert_eq!(host.slow_mem_reads, 0);
    assert_eq!(host.slow_mem_writes, 1);
}

#[test]
fn tier2_inline_tlb_store_to_out_of_range_page_does_not_bump_code_version_table() {
    // Store to page 3, but provide a code-version table with only 1 entry (page 0). The inline bump
    // fast-path should bounds-check and skip the update.
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![Instr::StoreMem {
            addr: Operand::Const(0x3000),
            src: Operand::Const(0xEE),
            width: Width::W8,
        }],
        kind: TraceKind::Linear,
    };

    let ram = vec![0u8; 0x20_000];
    let cpu_ptr = ram.len() as u64;
    let table_ptr: u32 = 0x1000;
    let (_ret, got_ram, _gpr, host) =
        run_trace_with_code_version_table(&trace, ram, cpu_ptr, 0x20_000, table_ptr, &[7]);

    assert_eq!(got_ram[0x3000], 0xEE);
    assert_eq!(read_u32_le(&got_ram, table_ptr as usize), 7);
    assert_eq!(host.mmu_translate_calls, 1);
    assert_eq!(host.slow_mem_reads, 0);
    assert_eq!(host.slow_mem_writes, 0);
}

#[test]
fn tier2_inline_tlb_store_permission_check_retranslates_on_missing_write_flag() {
    // Force `mmu_translate` to return a translation without write permission on its first call.
    // The inline-TLB permission check should detect the missing flag and re-translate, resulting
    // in two `mmu_translate` calls for a single store.
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![Instr::StoreMem {
            addr: Operand::Const(0x1000),
            src: Operand::Const(0xAB),
            width: Width::W8,
        }],
        kind: TraceKind::Linear,
    };

    let ram = vec![0u8; 0x20_000];
    let cpu_ptr = ram.len() as u64;
    let (_ret, got_ram, _gpr, host) = run_trace_with_host_state(
        &trace,
        ram,
        cpu_ptr,
        HostState {
            ram_size: 0x20_000,
            drop_write_flag_on_first_call: true,
            ..Default::default()
        },
    );

    assert_eq!(got_ram[0x1000], 0xAB);
    assert_eq!(host.mmu_translate_calls, 2);
    assert_eq!(host.slow_mem_reads, 0);
    assert_eq!(host.slow_mem_writes, 0);
}

#[test]
fn tier2_inline_tlb_load_from_non_ram_uses_slow_helper() {
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![
            Instr::LoadMem {
                dst: ValueId(0),
                addr: Operand::Const(0x1234),
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
    ram[0x1234..0x1234 + 4].copy_from_slice(&0xDDCC_BBAAu32.to_le_bytes());
    let cpu_ptr = ram.len() as u64;
    let (_ret, _got_ram, gpr, host) = run_trace_with_host_state(
        &trace,
        ram,
        cpu_ptr,
        HostState {
            // Mark only the first 0x1000 bytes as RAM so 0x1234 is classified as non-RAM.
            ram_size: 0x1000,
            ..Default::default()
        },
    );

    assert_eq!(gpr[Gpr::Rax.as_u8() as usize] as u32, 0xDDCC_BBAA);
    assert_eq!(host.mmu_translate_calls, 1);
    assert_eq!(host.slow_mem_reads, 1);
    assert_eq!(host.slow_mem_writes, 0);
}

#[test]
fn tier2_inline_tlb_load_permission_check_retranslates_on_missing_read_flag() {
    // Force `mmu_translate` to return a translation without read permission on its first call. The
    // inline-TLB permission check should detect the missing flag and re-translate.
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![
            Instr::LoadMem {
                dst: ValueId(0),
                addr: Operand::Const(0x1000),
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
    ram[0x1000..0x1000 + 4].copy_from_slice(&0x1122_3344u32.to_le_bytes());
    let cpu_ptr = ram.len() as u64;
    let (_ret, _got_ram, gpr, host) = run_trace_with_host_state(
        &trace,
        ram,
        cpu_ptr,
        HostState {
            ram_size: 0x20_000,
            drop_read_flag_on_first_call: true,
            ..Default::default()
        },
    );

    assert_eq!(gpr[Gpr::Rax.as_u8() as usize] as u32, 0x1122_3344);
    assert_eq!(host.mmu_translate_calls, 2);
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
fn tier2_inline_tlb_dynamic_w32_load_cross_page_check_boundary() {
    // Dynamic (non-constant) addresses emit a runtime cross-page check. Ensure the boundary
    // condition is correct for W32 accesses: offset 0xFFC stays in-page, 0xFFD crosses.
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![
            Instr::LoadReg {
                dst: ValueId(0),
                reg: Gpr::Rax,
            },
            Instr::LoadMem {
                dst: ValueId(1),
                addr: Operand::Value(ValueId(0)),
                width: Width::W32,
            },
            Instr::StoreReg {
                reg: Gpr::Rbx,
                src: Operand::Value(ValueId(1)),
            },
        ],
        kind: TraceKind::Linear,
    };

    let fast_addr: u64 = PAGE_SIZE - 4; // 0xFFC
    let slow_addr: u64 = PAGE_SIZE + (PAGE_SIZE - 3); // 0x1FFD

    let mut ram = vec![0u8; 0x20_000];
    ram[fast_addr as usize..fast_addr as usize + 4].copy_from_slice(&0xAABB_CCDDu32.to_le_bytes());
    ram[slow_addr as usize..slow_addr as usize + 4].copy_from_slice(&0x1122_3344u32.to_le_bytes());

    let cpu_ptr = ram.len() as u64;

    // offset == 0xFFC: should take the inline-TLB fast path.
    let (ret, _got_ram, gpr, host) = run_trace_with_init_gprs(
        &trace,
        ram.clone(),
        cpu_ptr,
        0x20_000,
        &[(Gpr::Rax, fast_addr)],
    );
    assert_eq!(ret, 0x1000);
    assert_eq!(gpr[Gpr::Rbx.as_u8() as usize] as u32, 0xAABB_CCDD);
    assert_eq!(host.mmu_translate_calls, 1);
    assert_eq!(host.slow_mem_reads, 0);
    assert_eq!(host.slow_mem_writes, 0);

    // offset == 0xFFD: should take the slow helper path.
    let (ret, _got_ram, gpr, host) =
        run_trace_with_init_gprs(&trace, ram, cpu_ptr, 0x20_000, &[(Gpr::Rax, slow_addr)]);
    assert_eq!(ret, 0x1000);
    assert_eq!(gpr[Gpr::Rbx.as_u8() as usize] as u32, 0x1122_3344);
    assert_eq!(host.mmu_translate_calls, 0);
    assert_eq!(host.slow_mem_reads, 1);
    assert_eq!(host.slow_mem_writes, 0);
}

#[test]
fn tier2_inline_tlb_dynamic_w32_store_cross_page_check_boundary() {
    // Like `tier2_inline_tlb_dynamic_w32_load_cross_page_check_boundary`, but for stores.
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![
            Instr::LoadReg {
                dst: ValueId(0),
                reg: Gpr::Rax,
            },
            Instr::StoreMem {
                addr: Operand::Value(ValueId(0)),
                src: Operand::Const(0xDDCC_BBAA),
                width: Width::W32,
            },
        ],
        kind: TraceKind::Linear,
    };

    let fast_addr: u64 = PAGE_SIZE - 4; // 0xFFC
    let slow_addr: u64 = PAGE_SIZE + (PAGE_SIZE - 3); // 0x1FFD

    let ram = vec![0u8; 0x20_000];
    let cpu_ptr = ram.len() as u64;

    // offset == 0xFFC: should take the inline-TLB fast path.
    let (ret, got_ram, _gpr, host) = run_trace_with_init_gprs(
        &trace,
        ram.clone(),
        cpu_ptr,
        0x20_000,
        &[(Gpr::Rax, fast_addr)],
    );
    assert_eq!(ret, 0x1000);
    assert_eq!(read_u32_le(&got_ram, fast_addr as usize), 0xDDCC_BBAA);
    assert_eq!(host.mmu_translate_calls, 1);
    assert_eq!(host.slow_mem_reads, 0);
    assert_eq!(host.slow_mem_writes, 0);

    // offset == 0xFFD: should take the slow helper path.
    let (ret, got_ram, _gpr, host) =
        run_trace_with_init_gprs(&trace, ram, cpu_ptr, 0x20_000, &[(Gpr::Rax, slow_addr)]);
    assert_eq!(ret, 0x1000);
    assert_eq!(read_u32_le(&got_ram, slow_addr as usize), 0xDDCC_BBAA);
    assert_eq!(host.mmu_translate_calls, 0);
    assert_eq!(host.slow_mem_reads, 0);
    assert_eq!(host.slow_mem_writes, 1);
}

#[test]
fn tier2_inline_tlb_dynamic_w16_load_cross_page_check_boundary() {
    // Dynamic (non-constant) addresses emit a runtime cross-page check. Ensure the boundary
    // condition is correct for W16 accesses: offset 0xFFE stays in-page, 0xFFF crosses.
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![
            Instr::LoadReg {
                dst: ValueId(0),
                reg: Gpr::Rax,
            },
            Instr::LoadMem {
                dst: ValueId(1),
                addr: Operand::Value(ValueId(0)),
                width: Width::W16,
            },
            Instr::StoreReg {
                reg: Gpr::Rbx,
                src: Operand::Value(ValueId(1)),
            },
        ],
        kind: TraceKind::Linear,
    };

    let fast_addr: u64 = PAGE_SIZE - 2; // 0xFFE
    let slow_addr: u64 = PAGE_SIZE + (PAGE_SIZE - 1); // 0x1FFF

    let mut ram = vec![0u8; 0x20_000];
    ram[fast_addr as usize..fast_addr as usize + 2].copy_from_slice(&0xBEEFu16.to_le_bytes());
    ram[slow_addr as usize..slow_addr as usize + 2].copy_from_slice(&0x1234u16.to_le_bytes());

    let cpu_ptr = ram.len() as u64;

    // offset == 0xFFE: should take the inline-TLB fast path.
    let (ret, _got_ram, gpr, host) = run_trace_with_init_gprs(
        &trace,
        ram.clone(),
        cpu_ptr,
        0x20_000,
        &[(Gpr::Rax, fast_addr)],
    );
    assert_eq!(ret, 0x1000);
    assert_eq!(gpr[Gpr::Rbx.as_u8() as usize] as u16, 0xBEEF);
    assert_eq!(host.mmu_translate_calls, 1);
    assert_eq!(host.slow_mem_reads, 0);
    assert_eq!(host.slow_mem_writes, 0);

    // offset == 0xFFF: should take the slow helper path.
    let (ret, _got_ram, gpr, host) =
        run_trace_with_init_gprs(&trace, ram, cpu_ptr, 0x20_000, &[(Gpr::Rax, slow_addr)]);
    assert_eq!(ret, 0x1000);
    assert_eq!(gpr[Gpr::Rbx.as_u8() as usize] as u16, 0x1234);
    assert_eq!(host.mmu_translate_calls, 0);
    assert_eq!(host.slow_mem_reads, 1);
    assert_eq!(host.slow_mem_writes, 0);
}

#[test]
fn tier2_inline_tlb_dynamic_w16_store_cross_page_check_boundary() {
    // Like `tier2_inline_tlb_dynamic_w16_load_cross_page_check_boundary`, but for stores.
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![
            Instr::LoadReg {
                dst: ValueId(0),
                reg: Gpr::Rax,
            },
            Instr::StoreMem {
                addr: Operand::Value(ValueId(0)),
                src: Operand::Const(0xBEEF),
                width: Width::W16,
            },
        ],
        kind: TraceKind::Linear,
    };

    let fast_addr: u64 = PAGE_SIZE - 2; // 0xFFE
    let slow_addr: u64 = PAGE_SIZE + (PAGE_SIZE - 1); // 0x1FFF

    let ram = vec![0u8; 0x20_000];
    let cpu_ptr = ram.len() as u64;

    // offset == 0xFFE: should take the inline-TLB fast path.
    let (ret, got_ram, _gpr, host) = run_trace_with_init_gprs(
        &trace,
        ram.clone(),
        cpu_ptr,
        0x20_000,
        &[(Gpr::Rax, fast_addr)],
    );
    assert_eq!(ret, 0x1000);
    assert_eq!(
        &got_ram[fast_addr as usize..fast_addr as usize + 2],
        &0xBEEFu16.to_le_bytes()
    );
    assert_eq!(host.mmu_translate_calls, 1);
    assert_eq!(host.slow_mem_reads, 0);
    assert_eq!(host.slow_mem_writes, 0);

    // offset == 0xFFF: should take the slow helper path.
    let (ret, got_ram, _gpr, host) =
        run_trace_with_init_gprs(&trace, ram, cpu_ptr, 0x20_000, &[(Gpr::Rax, slow_addr)]);
    assert_eq!(ret, 0x1000);
    assert_eq!(
        &got_ram[slow_addr as usize..slow_addr as usize + 2],
        &0xBEEFu16.to_le_bytes()
    );
    assert_eq!(host.mmu_translate_calls, 0);
    assert_eq!(host.slow_mem_reads, 0);
    assert_eq!(host.slow_mem_writes, 1);
}

#[test]
fn tier2_inline_tlb_dynamic_w64_load_cross_page_check_boundary() {
    // Dynamic (non-constant) addresses emit a runtime cross-page check. Ensure the boundary
    // condition is correct for W64 accesses: offset 0xFF8 stays in-page, 0xFF9 crosses.
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![
            Instr::LoadReg {
                dst: ValueId(0),
                reg: Gpr::Rax,
            },
            Instr::LoadMem {
                dst: ValueId(1),
                addr: Operand::Value(ValueId(0)),
                width: Width::W64,
            },
            Instr::StoreReg {
                reg: Gpr::Rbx,
                src: Operand::Value(ValueId(1)),
            },
        ],
        kind: TraceKind::Linear,
    };

    let fast_addr: u64 = PAGE_SIZE - 8; // 0xFF8
    let slow_addr: u64 = PAGE_SIZE + (PAGE_SIZE - 7); // 0x1FF9

    let mut ram = vec![0u8; 0x20_000];
    ram[fast_addr as usize..fast_addr as usize + 8]
        .copy_from_slice(&0x0102_0304_0506_0708u64.to_le_bytes());
    ram[slow_addr as usize..slow_addr as usize + 8]
        .copy_from_slice(&0x1122_3344_5566_7788u64.to_le_bytes());

    let cpu_ptr = ram.len() as u64;

    // offset == 0xFF8: should take the inline-TLB fast path.
    let (ret, _got_ram, gpr, host) = run_trace_with_init_gprs(
        &trace,
        ram.clone(),
        cpu_ptr,
        0x20_000,
        &[(Gpr::Rax, fast_addr)],
    );
    assert_eq!(ret, 0x1000);
    assert_eq!(gpr[Gpr::Rbx.as_u8() as usize], 0x0102_0304_0506_0708);
    assert_eq!(host.mmu_translate_calls, 1);
    assert_eq!(host.slow_mem_reads, 0);
    assert_eq!(host.slow_mem_writes, 0);

    // offset == 0xFF9: should take the slow helper path.
    let (ret, _got_ram, gpr, host) =
        run_trace_with_init_gprs(&trace, ram, cpu_ptr, 0x20_000, &[(Gpr::Rax, slow_addr)]);
    assert_eq!(ret, 0x1000);
    assert_eq!(gpr[Gpr::Rbx.as_u8() as usize], 0x1122_3344_5566_7788);
    assert_eq!(host.mmu_translate_calls, 0);
    assert_eq!(host.slow_mem_reads, 1);
    assert_eq!(host.slow_mem_writes, 0);
}

#[test]
fn tier2_inline_tlb_dynamic_w64_store_cross_page_check_boundary() {
    // Like `tier2_inline_tlb_dynamic_w64_load_cross_page_check_boundary`, but for stores.
    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![
            Instr::LoadReg {
                dst: ValueId(0),
                reg: Gpr::Rax,
            },
            Instr::StoreMem {
                addr: Operand::Value(ValueId(0)),
                src: Operand::Const(0x1122_3344_5566_7788),
                width: Width::W64,
            },
        ],
        kind: TraceKind::Linear,
    };

    let fast_addr: u64 = PAGE_SIZE - 8; // 0xFF8
    let slow_addr: u64 = PAGE_SIZE + (PAGE_SIZE - 7); // 0x1FF9

    let ram = vec![0u8; 0x20_000];
    let cpu_ptr = ram.len() as u64;

    // offset == 0xFF8: should take the inline-TLB fast path.
    let (ret, got_ram, _gpr, host) = run_trace_with_init_gprs(
        &trace,
        ram.clone(),
        cpu_ptr,
        0x20_000,
        &[(Gpr::Rax, fast_addr)],
    );
    assert_eq!(ret, 0x1000);
    assert_eq!(
        read_u64_le(&got_ram, fast_addr as usize),
        0x1122_3344_5566_7788
    );
    assert_eq!(host.mmu_translate_calls, 1);
    assert_eq!(host.slow_mem_reads, 0);
    assert_eq!(host.slow_mem_writes, 0);

    // offset == 0xFF9: should take the slow helper path.
    let (ret, got_ram, _gpr, host) =
        run_trace_with_init_gprs(&trace, ram, cpu_ptr, 0x20_000, &[(Gpr::Rax, slow_addr)]);
    assert_eq!(ret, 0x1000);
    assert_eq!(
        read_u64_le(&got_ram, slow_addr as usize),
        0x1122_3344_5566_7788
    );
    assert_eq!(host.mmu_translate_calls, 0);
    assert_eq!(host.slow_mem_reads, 0);
    assert_eq!(host.slow_mem_writes, 1);
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
fn tier2_inline_tlb_high_ram_remap_uses_physical_address_not_vaddr() {
    // Like `tier2_inline_tlb_high_ram_remap_load_uses_contiguous_ram_offset`, but ensure the Q35
    // remap logic is driven by the *physical* address (from the cached TLB entry), not the virtual
    // address. This matters when page tables map a low virtual address to high RAM.
    //
    // We map vaddr=0x10 (page 0) to phys_base=4GiB via a prefilled TLB entry, and expect the load
    // to use the Q35 remap path and wrap into a small in-bounds offset.
    const HIGH_RAM_BASE: u64 = 0x1_0000_0000;

    let desired_offset: usize = 0x10000;
    let ram_base: u64 = 0x5000_0000 + desired_offset as u64;

    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![
            Instr::LoadMem {
                dst: ValueId(0),
                addr: Operand::Const(0x10),
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
    // slice at `desired_offset + 0x10`.
    let mut ram = vec![0u8; 0x20_000];
    ram[desired_offset + 0x10] = 0x7f;

    let cpu_ptr = ram.len() as u64;
    let cpu_ptr_usize = cpu_ptr as usize;
    let jit_ctx_ptr_usize = cpu_ptr_usize + (abi::CPU_STATE_SIZE as usize);
    let total_len =
        jit_ctx_ptr_usize + JitContext::TOTAL_BYTE_SIZE + (jit_ctx::TIER2_CTX_SIZE as usize);
    let mut mem = vec![0u8; total_len];

    mem[..ram.len()].copy_from_slice(&ram);
    write_cpu_rip(&mut mem, cpu_ptr_usize, 0x1000);
    write_cpu_rflags(&mut mem, cpu_ptr_usize, 0x2);

    let tlb_salt = 0x1234_5678_9abc_def0u64;
    let ctx = JitContext { ram_base, tlb_salt };
    ctx.write_header_to_mem(&mut mem, jit_ctx_ptr_usize);

    // Prefill a TLB entry for vaddr page 0 that maps to phys_base=4GiB.
    let vaddr = 0x10u64;
    let vpn = vaddr >> PAGE_SHIFT;
    let idx = (vpn & JIT_TLB_INDEX_MASK) as usize;
    let entry_addr =
        jit_ctx_ptr_usize + (JitContext::TLB_OFFSET as usize) + idx * (JIT_TLB_ENTRY_SIZE as usize);
    let tag = (vpn ^ tlb_salt) | 1;
    let data = (HIGH_RAM_BASE & PAGE_BASE_MASK)
        | (TLB_FLAG_READ | TLB_FLAG_WRITE | TLB_FLAG_EXEC | TLB_FLAG_IS_RAM);
    mem[entry_addr..entry_addr + 8].copy_from_slice(&tag.to_le_bytes());
    mem[entry_addr + 8..entry_addr + 16].copy_from_slice(&data.to_le_bytes());

    let pages = total_len.div_ceil(65_536) as u32;
    // Mark all pages as non-RAM for `mmu_translate`; the test relies on the prefilled TLB entry and
    // should not call `mmu_translate` at all.
    let (mut store, memory, func) = instantiate(&wasm, pages, 0);
    memory.write(&mut store, 0, &mem).unwrap();

    let ret = func
        .call(&mut store, (cpu_ptr as i32, jit_ctx_ptr_usize as i32))
        .unwrap() as u64;
    assert_eq!(ret, 0x1000);

    let mut got_mem = vec![0u8; total_len];
    memory.read(&store, 0, &mut got_mem).unwrap();

    let rax = read_u64_le(
        &got_mem,
        cpu_ptr_usize + abi::gpr_offset(Gpr::Rax.as_u8() as usize) as usize,
    );
    assert_eq!(rax & 0xff, 0x7f);

    let host = *store.data();
    assert_eq!(host.mmu_translate_calls, 0);
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

#[test]
fn tier2_inline_tlb_high_ram_remap_store_bumps_physical_code_page_version() {
    // Ensure self-modifying code invalidation (code-version bump) uses the physical page number
    // (4GiB >> 12) and does not accidentally use the Q35 remapped offset page number
    // (0xB000_0000 >> 12).
    const HIGH_RAM_BASE: u64 = 0x1_0000_0000;
    let desired_offset: usize = 0x10000;
    let ram_base: u64 = 0x5000_0000 + desired_offset as u64;

    // Allocate a version table large enough to include the 4GiB physical page number.
    let phys_page: u64 = HIGH_RAM_BASE >> PAGE_SHIFT;
    let table_len: u32 = (phys_page as u32) + 1;
    let table_ptr: usize = 0x20000;
    let table_bytes = usize::try_from(table_len).unwrap().checked_mul(4).unwrap();

    let trace = TraceIr {
        prologue: Vec::new(),
        body: vec![Instr::StoreMem {
            addr: Operand::Const(HIGH_RAM_BASE),
            src: Operand::Const(0xab),
            width: Width::W8,
        }],
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

    let ram_len = table_ptr + table_bytes + 0x1000;
    let ram = vec![0u8; ram_len];

    let cpu_ptr = ram.len() as u64;
    let cpu_ptr_usize = cpu_ptr as usize;
    let jit_ctx_ptr_usize = cpu_ptr_usize + (abi::CPU_STATE_SIZE as usize);
    let total_len =
        jit_ctx_ptr_usize + JitContext::TOTAL_BYTE_SIZE + (jit_ctx::TIER2_CTX_SIZE as usize);
    let mut mem = vec![0u8; total_len];

    // RAM backing store at offset 0.
    mem[..ram.len()].copy_from_slice(&ram);

    // Configure the code-version table (stored in the Tier-2 ctx region relative to `cpu_ptr`).
    write_u32_le(
        &mut mem,
        cpu_ptr_usize + jit_ctx::CODE_VERSION_TABLE_PTR_OFFSET as usize,
        table_ptr as u32,
    );
    write_u32_le(
        &mut mem,
        cpu_ptr_usize + jit_ctx::CODE_VERSION_TABLE_LEN_OFFSET as usize,
        table_len,
    );

    // CPU state at `cpu_ptr`, JIT context immediately following.
    write_cpu_rip(&mut mem, cpu_ptr_usize, 0x1000);
    write_cpu_rflags(&mut mem, cpu_ptr_usize, 0x2);

    let ctx = JitContext {
        ram_base,
        tlb_salt: 0x1234_5678_9abc_def0,
    };
    ctx.write_header_to_mem(&mut mem, jit_ctx_ptr_usize);

    let pages = total_len.div_ceil(65_536) as u32;
    // Make `mmu_translate` classify the 4GiB address as RAM so the inline-TLB store fast-path is taken.
    let ram_size = HIGH_RAM_BASE + 0x1000;
    let (mut store, memory, func) = instantiate(&wasm, pages, ram_size);
    memory.write(&mut store, 0, &mem).unwrap();

    let _ret = func
        .call(&mut store, (cpu_ptr as i32, jit_ctx_ptr_usize as i32))
        .unwrap();

    let mut got_mem = vec![0u8; total_len];
    memory.read(&store, 0, &mut got_mem).unwrap();

    let got_ram = &got_mem[..ram.len()];

    // Store should target the remapped contiguous RAM backing store.
    assert_eq!(got_ram[desired_offset], 0xab);

    // Bump should target the physical page index (4GiB >> 12).
    let phys_off = table_ptr + (phys_page as usize) * 4;
    let phys_val = u32::from_le_bytes(got_ram[phys_off..phys_off + 4].try_into().unwrap());
    assert_eq!(phys_val, 1);

    // And should *not* use the Q35 remapped offset page index (LOW_RAM_END >> 12).
    let remap_page: u64 = aero_pc_constants::PCIE_ECAM_BASE >> PAGE_SHIFT;
    let remap_off = table_ptr + (remap_page as usize) * 4;
    let remap_val = u32::from_le_bytes(got_ram[remap_off..remap_off + 4].try_into().unwrap());
    assert_eq!(remap_val, 0);

    let host = *store.data();
    assert_eq!(host.mmu_translate_calls, 1);
    assert_eq!(host.slow_mem_reads, 0);
    assert_eq!(host.slow_mem_writes, 0);
}
