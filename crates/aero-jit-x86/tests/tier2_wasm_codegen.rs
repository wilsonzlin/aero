use aero_cpu_core::jit::runtime::PageVersionTracker;
use aero_cpu_core::state::RFLAGS_DF;
use aero_types::{Flag, FlagSet, Gpr, Width};
mod tier1_common;

use tier1_common::SimpleBus;

use aero_jit_x86::abi;
use aero_jit_x86::jit_ctx;
use aero_jit_x86::tier2::interp::{run_trace_with_cached_regs, RunExit, RuntimeEnv, T2State};
use aero_jit_x86::tier2::ir::{
    BinOp, Block, BlockId, Function, Instr, Operand, Terminator, TraceIr, TraceKind, ValueId,
};
use aero_jit_x86::tier2::opt::{optimize_trace, OptConfig};
use aero_jit_x86::tier2::profile::{ProfileData, TraceConfig};
use aero_jit_x86::tier2::trace::TraceBuilder;
use aero_jit_x86::tier2::wasm_codegen::{
    Tier2WasmCodegen, Tier2WasmOptions, EXPORT_TRACE_FN, IMPORT_CODE_PAGE_VERSION,
};
use aero_jit_x86::wasm::{
    IMPORT_MEMORY, IMPORT_MEM_READ_U16, IMPORT_MEM_READ_U32, IMPORT_MEM_READ_U64,
    IMPORT_MEM_READ_U8, IMPORT_MEM_WRITE_U16, IMPORT_MEM_WRITE_U32, IMPORT_MEM_WRITE_U64,
    IMPORT_MEM_WRITE_U8, IMPORT_MODULE,
};

use wasmi::{Caller, Engine, Func, Linker, Memory, MemoryType, Module, Store, TypedFunc};
use wasmparser::Validator;

const CPU_PTR: i32 = 0x1_0000;
const JIT_CTX_PTR: i32 = CPU_PTR + (abi::CPU_STATE_SIZE as i32);
const GUEST_MEM_SIZE: usize = 0x1_0000; // 1 page

fn validate_wasm(bytes: &[u8]) {
    let mut validator = Validator::new();
    validator.validate_all(bytes).unwrap();
}

#[derive(Clone, Debug, Default)]
struct HostEnv {
    code_version_calls: u64,
    bump_on_call: Option<(u64, u64)>,
}

fn instantiate_trace(
    bytes: &[u8],
    host_env: HostEnv,
) -> (Store<HostEnv>, Memory, TypedFunc<(i32, i32), i64>) {
    let engine = Engine::default();
    let module = Module::new(&engine, bytes).unwrap();

    let mut store = Store::new(&engine, host_env);
    let mut linker = Linker::new(&engine);

    // Two pages: guest memory in page 0, CpuState at CPU_PTR in page 1.
    let memory = Memory::new(&mut store, MemoryType::new(2, None)).unwrap();
    linker.define(IMPORT_MODULE, IMPORT_MEMORY, memory).unwrap();

    define_mem_helpers(&mut store, &mut linker, memory);

    let mem = memory;
    linker
        .define(
            IMPORT_MODULE,
            IMPORT_CODE_PAGE_VERSION,
            Func::wrap(
                &mut store,
                move |mut caller: Caller<'_, HostEnv>, cpu_ptr: i32, page: i64| -> i64 {
                    let page = page as u64;
                    let cpu_ptr = cpu_ptr as usize;

                    let mut buf = [0u8; 4];
                    mem.read(
                        &caller,
                        cpu_ptr + jit_ctx::CODE_VERSION_TABLE_PTR_OFFSET as usize,
                        &mut buf,
                    )
                    .unwrap();
                    let table_ptr = u32::from_le_bytes(buf) as u64;

                    mem.read(
                        &caller,
                        cpu_ptr + jit_ctx::CODE_VERSION_TABLE_LEN_OFFSET as usize,
                        &mut buf,
                    )
                    .unwrap();
                    let table_len = u32::from_le_bytes(buf) as u64;

                    let call_idx = {
                        let data = caller.data_mut();
                        data.code_version_calls += 1;
                        data.code_version_calls
                    };

                    // Optional one-shot bump used by tests to simulate mid-trace invalidation.
                    if let Some((at, bump_page)) = caller.data().bump_on_call {
                        if call_idx == at {
                            caller.data_mut().bump_on_call = None;
                            if bump_page < table_len {
                                let addr = table_ptr as usize + bump_page as usize * 4;
                                mem.read(&caller, addr, &mut buf).unwrap();
                                let cur = u32::from_le_bytes(buf);
                                let next = cur.wrapping_add(1);
                                mem.write(&mut caller, addr, &next.to_le_bytes()).unwrap();
                            }
                        }
                    }

                    if page >= table_len {
                        return 0;
                    }
                    let addr = table_ptr as usize + page as usize * 4;
                    mem.read(&caller, addr, &mut buf).unwrap();
                    u32::from_le_bytes(buf) as i64
                },
            ),
        )
        .unwrap();

    let instance = linker.instantiate_and_start(&mut store, &module).unwrap();
    let trace = instance
        .get_typed_func::<(i32, i32), i64>(&store, EXPORT_TRACE_FN)
        .unwrap();
    (store, memory, trace)
}

fn instantiate_trace_without_code_page_version(
    bytes: &[u8],
    host_env: HostEnv,
) -> (Store<HostEnv>, Memory, TypedFunc<(i32, i32), i64>) {
    let engine = Engine::default();
    let module = Module::new(&engine, bytes).unwrap();

    let mut store = Store::new(&engine, host_env);
    let mut linker = Linker::new(&engine);

    // Two pages: guest memory in page 0, CpuState at CPU_PTR in page 1.
    let memory = Memory::new(&mut store, MemoryType::new(2, None)).unwrap();
    linker.define(IMPORT_MODULE, IMPORT_MEMORY, memory).unwrap();

    define_mem_helpers(&mut store, &mut linker, memory);

    // Crucially: do NOT define `env.code_page_version`. This should still instantiate when the
    // trace is compiled with `Tier2WasmOptions { code_version_guard_import: false, .. }`.
    let instance = linker.instantiate_and_start(&mut store, &module).unwrap();
    let trace = instance
        .get_typed_func::<(i32, i32), i64>(&store, EXPORT_TRACE_FN)
        .unwrap();
    (store, memory, trace)
}

fn bump_code_versions(
    caller: &mut Caller<'_, HostEnv>,
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

    let start_page = paddr >> aero_jit_x86::PAGE_SHIFT;
    let end = paddr.saturating_add(len as u64 - 1);
    let end_page = end >> aero_jit_x86::PAGE_SHIFT;

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

fn define_mem_helpers(store: &mut Store<HostEnv>, linker: &mut Linker<HostEnv>, memory: Memory) {
    fn read<const N: usize>(caller: &mut Caller<'_, HostEnv>, memory: &Memory, addr: usize) -> u64 {
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
        caller: &mut Caller<'_, HostEnv>,
        memory: &Memory,
        addr: usize,
        value: u64,
    ) {
        let mut buf = [0u8; N];
        for (i, b) in buf.iter_mut().enumerate() {
            *b = (value >> (i * 8)) as u8;
        }
        memory
            .write(caller, addr, &buf)
            .expect("memory write in bounds");
    }

    // Reads.
    {
        let mem = memory;
        linker
            .define(
                IMPORT_MODULE,
                IMPORT_MEM_READ_U8,
                Func::wrap(
                    &mut *store,
                    move |mut caller: Caller<'_, HostEnv>, _cpu_ptr: i32, addr: i64| -> i32 {
                        read::<1>(&mut caller, &mem, addr as usize) as i32
                    },
                ),
            )
            .unwrap();
    }
    {
        let mem = memory;
        linker
            .define(
                IMPORT_MODULE,
                IMPORT_MEM_READ_U16,
                Func::wrap(
                    &mut *store,
                    move |mut caller: Caller<'_, HostEnv>, _cpu_ptr: i32, addr: i64| -> i32 {
                        read::<2>(&mut caller, &mem, addr as usize) as i32
                    },
                ),
            )
            .unwrap();
    }
    {
        let mem = memory;
        linker
            .define(
                IMPORT_MODULE,
                IMPORT_MEM_READ_U32,
                Func::wrap(
                    &mut *store,
                    move |mut caller: Caller<'_, HostEnv>, _cpu_ptr: i32, addr: i64| -> i32 {
                        read::<4>(&mut caller, &mem, addr as usize) as i32
                    },
                ),
            )
            .unwrap();
    }
    {
        let mem = memory;
        linker
            .define(
                IMPORT_MODULE,
                IMPORT_MEM_READ_U64,
                Func::wrap(
                    &mut *store,
                    move |mut caller: Caller<'_, HostEnv>, _cpu_ptr: i32, addr: i64| -> i64 {
                        read::<8>(&mut caller, &mem, addr as usize) as i64
                    },
                ),
            )
            .unwrap();
    }

    // Writes.
    {
        let mem = memory;
        linker
            .define(
                IMPORT_MODULE,
                IMPORT_MEM_WRITE_U8,
                Func::wrap(
                    &mut *store,
                    move |mut caller: Caller<'_, HostEnv>, cpu_ptr: i32, addr: i64, value: i32| {
                        write::<1>(&mut caller, &mem, addr as usize, value as u64);
                        bump_code_versions(&mut caller, &mem, cpu_ptr, addr as u64, 1);
                    },
                ),
            )
            .unwrap();
    }
    {
        let mem = memory;
        linker
            .define(
                IMPORT_MODULE,
                IMPORT_MEM_WRITE_U16,
                Func::wrap(
                    &mut *store,
                    move |mut caller: Caller<'_, HostEnv>, cpu_ptr: i32, addr: i64, value: i32| {
                        write::<2>(&mut caller, &mem, addr as usize, value as u64);
                        bump_code_versions(&mut caller, &mem, cpu_ptr, addr as u64, 2);
                    },
                ),
            )
            .unwrap();
    }
    {
        let mem = memory;
        linker
            .define(
                IMPORT_MODULE,
                IMPORT_MEM_WRITE_U32,
                Func::wrap(
                    &mut *store,
                    move |mut caller: Caller<'_, HostEnv>, cpu_ptr: i32, addr: i64, value: i32| {
                        write::<4>(&mut caller, &mem, addr as usize, value as u64);
                        bump_code_versions(&mut caller, &mem, cpu_ptr, addr as u64, 4);
                    },
                ),
            )
            .unwrap();
    }
    {
        let mem = memory;
        linker
            .define(
                IMPORT_MODULE,
                IMPORT_MEM_WRITE_U64,
                Func::wrap(
                    &mut *store,
                    move |mut caller: Caller<'_, HostEnv>, cpu_ptr: i32, addr: i64, value: i64| {
                        write::<8>(&mut caller, &mem, addr as usize, value as u64);
                        bump_code_versions(&mut caller, &mem, cpu_ptr, addr as u64, 8);
                    },
                ),
            )
            .unwrap();
    }
}

fn write_u32_to_memory(memory: &Memory, store: &mut Store<HostEnv>, addr: usize, value: u32) {
    memory
        .write(store, addr, &value.to_le_bytes())
        .expect("memory write in bounds");
}

fn read_u32_from_memory(memory: &Memory, store: &Store<HostEnv>, addr: usize) -> u32 {
    let mut buf = [0u8; 4];
    memory
        .read(store, addr, &mut buf)
        .expect("memory read in bounds");
    u32::from_le_bytes(buf)
}

fn install_code_version_table(memory: &Memory, store: &mut Store<HostEnv>, table: &[u32]) -> u32 {
    if table.is_empty() {
        write_u32_to_memory(
            memory,
            store,
            CPU_PTR as usize + jit_ctx::CODE_VERSION_TABLE_PTR_OFFSET as usize,
            0,
        );
        write_u32_to_memory(
            memory,
            store,
            CPU_PTR as usize + jit_ctx::CODE_VERSION_TABLE_LEN_OFFSET as usize,
            0,
        );
        return 0;
    }

    let table_ptr = (CPU_PTR as u32) + jit_ctx::TIER2_CTX_OFFSET + jit_ctx::JIT_CTX_SIZE;
    let table_len = u32::try_from(table.len()).expect("table too large");

    write_u32_to_memory(
        memory,
        store,
        CPU_PTR as usize + jit_ctx::CODE_VERSION_TABLE_PTR_OFFSET as usize,
        table_ptr,
    );
    write_u32_to_memory(
        memory,
        store,
        CPU_PTR as usize + jit_ctx::CODE_VERSION_TABLE_LEN_OFFSET as usize,
        table_len,
    );

    for (idx, version) in table.iter().copied().enumerate() {
        write_u32_to_memory(memory, store, table_ptr as usize + idx * 4, version);
    }

    table_ptr
}

fn read_u64_le(bytes: &[u8], off: usize) -> u64 {
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&bytes[off..off + 8]);
    u64::from_le_bytes(buf)
}

fn write_cpu_state(bytes: &mut [u8], cpu: &aero_cpu_core::state::CpuState) {
    assert!(
        bytes.len() >= abi::CPU_STATE_SIZE as usize,
        "cpu state buffer too small"
    );
    for (&off, reg) in abi::CPU_GPR_OFF.iter().zip(cpu.gpr.iter()) {
        let off = off as usize;
        bytes[off..off + 8].copy_from_slice(&reg.to_le_bytes());
    }
    bytes[abi::CPU_RIP_OFF as usize..abi::CPU_RIP_OFF as usize + 8]
        .copy_from_slice(&cpu.rip.to_le_bytes());
    bytes[abi::CPU_RFLAGS_OFF as usize..abi::CPU_RFLAGS_OFF as usize + 8]
        .copy_from_slice(&cpu.rflags.to_le_bytes());
}

fn read_cpu_state(bytes: &[u8]) -> ([u64; 16], u64, u64) {
    let mut gpr = [0u64; 16];
    for (dst, off) in gpr.iter_mut().zip(abi::CPU_GPR_OFF.iter()) {
        *dst = read_u64_le(bytes, *off as usize);
    }
    let rip = read_u64_le(bytes, abi::CPU_RIP_OFF as usize);
    let rflags = read_u64_le(bytes, abi::CPU_RFLAGS_OFF as usize);
    (gpr, rip, rflags)
}

fn v(idx: u32) -> ValueId {
    ValueId(idx)
}

#[test]
fn tier2_code_version_guard_can_inline_version_table_reads() {
    let mut trace = TraceIr {
        prologue: vec![Instr::GuardCodeVersion {
            page: 0,
            expected: 1,
            exit_rip: 0x9999,
        }],
        body: vec![],
        kind: TraceKind::Linear,
    };

    let opt = optimize_trace(&mut trace, &OptConfig::default());
    let wasm = Tier2WasmCodegen::new().compile_trace_with_options(
        &trace,
        &opt.regalloc,
        Tier2WasmOptions {
            code_version_guard_import: false,
            ..Default::default()
        },
    );
    validate_wasm(&wasm);

    let (mut store, memory, func) =
        instantiate_trace_without_code_page_version(&wasm, HostEnv::default());
    let guest_mem_init = vec![0u8; GUEST_MEM_SIZE];
    memory.write(&mut store, 0, &guest_mem_init).unwrap();

    let mut cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    let mut init_state = T2State::default();
    init_state.cpu.rip = 0x1111;
    init_state.cpu.rflags = abi::RFLAGS_RESERVED1;
    write_cpu_state(&mut cpu_bytes, &init_state.cpu);
    memory
        .write(&mut store, CPU_PTR as usize, &cpu_bytes)
        .unwrap();

    let table_ptr = install_code_version_table(&memory, &mut store, &[1]);

    let got_rip = func.call(&mut store, (CPU_PTR, JIT_CTX_PTR)).unwrap() as u64;
    assert_eq!(got_rip, 0x1111);
    let exit_reason = read_u32_from_memory(
        &memory,
        &store,
        CPU_PTR as usize + jit_ctx::TRACE_EXIT_REASON_OFFSET as usize,
    );
    assert_eq!(exit_reason, jit_ctx::TRACE_EXIT_REASON_NONE);

    // Update the table entry directly and re-run: the guard should trigger invalidation without
    // calling the host import.
    write_u32_to_memory(&memory, &mut store, table_ptr as usize, 2);
    memory
        .write(&mut store, CPU_PTR as usize, &cpu_bytes)
        .unwrap();

    let got_rip = func.call(&mut store, (CPU_PTR, JIT_CTX_PTR)).unwrap() as u64;
    assert_eq!(got_rip, 0x9999);
    let exit_reason = read_u32_from_memory(
        &memory,
        &store,
        CPU_PTR as usize + jit_ctx::TRACE_EXIT_REASON_OFFSET as usize,
    );
    assert_eq!(exit_reason, jit_ctx::TRACE_EXIT_REASON_CODE_INVALIDATION);
    assert_eq!(
        store.data().code_version_calls,
        0,
        "inline guard should not call env.code_page_version"
    );
}

#[test]
fn tier2_trace_without_code_version_guards_does_not_require_code_page_version_import() {
    let mut trace = TraceIr {
        prologue: vec![],
        body: vec![
            Instr::Const {
                dst: v(0),
                value: 0xdead_beef,
            },
            Instr::StoreReg {
                reg: Gpr::Rax,
                src: Operand::Value(v(0)),
            },
        ],
        kind: TraceKind::Linear,
    };

    let opt = optimize_trace(&mut trace, &OptConfig::default());
    let wasm = Tier2WasmCodegen::new().compile_trace(&trace, &opt.regalloc);
    validate_wasm(&wasm);

    // The default Tier-2 options set `code_version_guard_import = true`, but traces without
    // `GuardCodeVersion` shouldn't need (or import) `env.code_page_version`.
    let (mut store, memory, func) =
        instantiate_trace_without_code_page_version(&wasm, HostEnv::default());

    let guest_mem_init = vec![0u8; GUEST_MEM_SIZE];
    memory.write(&mut store, 0, &guest_mem_init).unwrap();

    let mut init_state = T2State::default();
    init_state.cpu.rip = 0x1234;
    init_state.cpu.rflags = abi::RFLAGS_RESERVED1;
    let mut cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    write_cpu_state(&mut cpu_bytes, &init_state.cpu);
    memory
        .write(&mut store, CPU_PTR as usize, &cpu_bytes)
        .unwrap();
    install_code_version_table(&memory, &mut store, &[]);

    let got_rip = func.call(&mut store, (CPU_PTR, JIT_CTX_PTR)).unwrap() as u64;
    assert_eq!(got_rip, init_state.cpu.rip);

    memory
        .read(&store, CPU_PTR as usize, &mut cpu_bytes)
        .unwrap();
    let (got_gpr, got_rip, _got_rflags) = read_cpu_state(&cpu_bytes);
    assert_eq!(got_rip, init_state.cpu.rip);
    assert_eq!(got_gpr[Gpr::Rax.as_u8() as usize], 0xdead_beef);
}

#[test]
fn tier2_inline_tlb_option_is_ignored_for_memory_free_traces() {
    // Inline-TLB only affects memory loads/stores. A trace with no memory ops should not require
    // any inline-TLB-specific imports (e.g. `env.mmu_translate`).
    let mut trace = TraceIr {
        prologue: vec![],
        body: vec![Instr::StoreReg {
            reg: Gpr::Rax,
            src: Operand::Const(1),
        }],
        kind: TraceKind::Linear,
    };

    let opt = optimize_trace(&mut trace, &OptConfig::default());
    let wasm = Tier2WasmCodegen::new().compile_trace_with_options(
        &trace,
        &opt.regalloc,
        Tier2WasmOptions {
            inline_tlb: true,
            code_version_guard_import: true,
            ..Default::default()
        },
    );
    validate_wasm(&wasm);

    // `instantiate_trace` intentionally does not define `env.mmu_translate`; this should still work
    // because the code generator should treat `inline_tlb` as disabled when the trace does no
    // memory operations.
    let (mut store, memory, func) = instantiate_trace(&wasm, HostEnv::default());
    let guest_mem_init = vec![0u8; GUEST_MEM_SIZE];
    memory.write(&mut store, 0, &guest_mem_init).unwrap();

    let mut cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    let mut init_state = T2State::default();
    init_state.cpu.rip = 0x1111;
    init_state.cpu.rflags = abi::RFLAGS_RESERVED1;
    write_cpu_state(&mut cpu_bytes, &init_state.cpu);
    memory
        .write(&mut store, CPU_PTR as usize, &cpu_bytes)
        .unwrap();
    install_code_version_table(&memory, &mut store, &[]);

    let got_rip = func.call(&mut store, (CPU_PTR, JIT_CTX_PTR)).unwrap() as u64;
    assert_eq!(got_rip, init_state.cpu.rip);
}

#[test]
fn tier2_trace_wasm_matches_interpreter_on_sar() {
    let mut trace = TraceIr {
        prologue: vec![],
        body: vec![
            Instr::LoadReg {
                dst: v(0),
                reg: Gpr::Rax,
            },
            Instr::Const { dst: v(1), value: 1 },
            Instr::BinOp {
                dst: v(2),
                op: BinOp::Sar,
                lhs: Operand::Value(v(0)),
                rhs: Operand::Value(v(1)),
                flags: FlagSet::EMPTY,
            },
            Instr::StoreReg {
                reg: Gpr::Rax,
                src: Operand::Value(v(2)),
            },
        ],
        kind: TraceKind::Linear,
    };

    let opt = optimize_trace(&mut trace, &OptConfig::default());
    assert!(
        trace
            .iter_instrs()
            .any(|i| matches!(i, Instr::BinOp { op: BinOp::Sar, .. })),
        "optimized trace should still contain a SAR BinOp"
    );

    let wasm = Tier2WasmCodegen::new().compile_trace(&trace, &opt.regalloc);
    validate_wasm(&wasm);

    let env = RuntimeEnv::default();

    let mut init_state = T2State::default();
    init_state.cpu.rip = 0x1234;
    init_state.cpu.rflags = abi::RFLAGS_RESERVED1 | RFLAGS_DF;
    init_state.cpu.gpr[Gpr::Rax.as_u8() as usize] = 0x8000_0000_0000_0000u64;

    let mut interp_state = init_state.clone();
    let mut bus = SimpleBus::new(GUEST_MEM_SIZE);
    let res = run_trace_with_cached_regs(
        &trace,
        &env,
        &mut bus,
        &mut interp_state,
        1,
        &opt.regalloc.cached,
    );
    assert_eq!(res.exit, RunExit::Returned);
    assert_eq!(
        interp_state.cpu.gpr[Gpr::Rax.as_u8() as usize],
        0xC000_0000_0000_0000u64
    );

    let (mut store, memory, func) =
        instantiate_trace_without_code_page_version(&wasm, HostEnv::default());
    let guest_mem_init = vec![0u8; GUEST_MEM_SIZE];
    memory.write(&mut store, 0, &guest_mem_init).unwrap();

    let mut cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    write_cpu_state(&mut cpu_bytes, &init_state.cpu);
    memory
        .write(&mut store, CPU_PTR as usize, &cpu_bytes)
        .unwrap();
    install_code_version_table(&memory, &mut store, &[]);

    let got_rip = func.call(&mut store, (CPU_PTR, JIT_CTX_PTR)).unwrap() as u64;
    assert_eq!(got_rip, interp_state.cpu.rip);

    let mut got_guest_mem = vec![0u8; GUEST_MEM_SIZE];
    memory.read(&store, 0, &mut got_guest_mem).unwrap();
    assert_eq!(got_guest_mem.as_slice(), bus.mem());

    let mut got_cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    memory
        .read(&store, CPU_PTR as usize, &mut got_cpu_bytes)
        .unwrap();
    let (got_gpr, got_rip, got_rflags) = read_cpu_state(&got_cpu_bytes);
    assert_eq!(got_gpr, interp_state.cpu.gpr);
    assert_eq!(got_rip, interp_state.cpu.rip);
    assert_eq!(got_rflags, interp_state.cpu.rflags);
}

#[test]
fn tier2_trace_wasm_matches_interpreter_on_loop_side_exit() {
    // A tiny loop in Tier-2 IR form (built from a CFG) that increments RAX until it reaches 10,
    // then side-exits to RIP=100.
    let func = Function {
        entry: BlockId(0),
        blocks: vec![
            Block {
                id: BlockId(0),
                start_rip: 0,
                code_len: 64,
                instrs: vec![
                    Instr::LoadReg {
                        dst: v(0),
                        reg: Gpr::Rax,
                    },
                    Instr::Const {
                        dst: v(1),
                        value: 1,
                    },
                    Instr::BinOp {
                        dst: v(2),
                        op: BinOp::Add,
                        lhs: Operand::Value(v(0)),
                        rhs: Operand::Value(v(1)),
                        flags: FlagSet::ALU,
                    },
                    Instr::StoreReg {
                        reg: Gpr::Rax,
                        src: Operand::Value(v(2)),
                    },
                    Instr::Const {
                        dst: v(3),
                        value: 10,
                    },
                    Instr::BinOp {
                        dst: v(4),
                        op: BinOp::LtU,
                        lhs: Operand::Value(v(2)),
                        rhs: Operand::Value(v(3)),
                        flags: FlagSet::EMPTY,
                    },
                ],
                term: Terminator::Branch {
                    cond: Operand::Value(v(4)),
                    then_bb: BlockId(0),
                    else_bb: BlockId(1),
                },
            },
            Block {
                id: BlockId(1),
                start_rip: 100,
                code_len: 1,
                instrs: vec![],
                term: Terminator::Return,
            },
        ],
    };

    let mut profile = ProfileData::default();
    profile.block_counts.insert(BlockId(0), 10_000);
    profile.edge_counts.insert((BlockId(0), BlockId(0)), 9_000);
    profile.edge_counts.insert((BlockId(0), BlockId(1)), 1_000);
    profile.hot_backedges.insert((BlockId(0), BlockId(0)));

    let mut page_versions = PageVersionTracker::default();
    page_versions.set_version(0, 7);

    let builder = TraceBuilder::new(
        &func,
        &profile,
        &page_versions,
        TraceConfig {
            hot_block_threshold: 1000,
            max_blocks: 8,
            max_instrs: 256,
        },
    );
    let mut trace = builder.build_from(BlockId(0)).expect("trace");
    assert_eq!(trace.ir.kind, TraceKind::Loop);

    let opt = optimize_trace(&mut trace.ir, &OptConfig::default());
    let wasm = Tier2WasmCodegen::new().compile_trace(&trace.ir, &opt.regalloc);
    validate_wasm(&wasm);

    let mut init_state = T2State::default();
    init_state.cpu.rip = 0;
    init_state.cpu.gpr[Gpr::Rax.as_u8() as usize] = 0;
    init_state.cpu.rflags = abi::RFLAGS_RESERVED1 | RFLAGS_DF;
    for flag in [Flag::Cf, Flag::Pf, Flag::Af, Flag::Zf, Flag::Sf, Flag::Of] {
        init_state.cpu.rflags |= 1u64 << flag.rflags_bit();
    }

    let mut env = RuntimeEnv::default();
    env.page_versions.set_version(0, 7);

    let mut interp_state = init_state.clone();
    let mut bus = SimpleBus::new(GUEST_MEM_SIZE);
    let expected = run_trace_with_cached_regs(
        &trace.ir,
        &env,
        &mut bus,
        &mut interp_state,
        1_000_000,
        &opt.regalloc.cached,
    );
    assert_eq!(expected.exit, RunExit::SideExit { next_rip: 100 });

    let (mut store, memory, func) = instantiate_trace(&wasm, HostEnv::default());
    let guest_mem_init = vec![0u8; GUEST_MEM_SIZE];
    memory.write(&mut store, 0, &guest_mem_init).unwrap();

    let mut cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    write_cpu_state(&mut cpu_bytes, &init_state.cpu);
    memory
        .write(&mut store, CPU_PTR as usize, &cpu_bytes)
        .unwrap();
    install_code_version_table(&memory, &mut store, &[7]);

    let got_rip = func.call(&mut store, (CPU_PTR, JIT_CTX_PTR)).unwrap() as u64;
    assert_eq!(got_rip, 100);

    let mut got_guest_mem = vec![0u8; GUEST_MEM_SIZE];
    memory.read(&store, 0, &mut got_guest_mem).unwrap();
    assert_eq!(got_guest_mem.as_slice(), bus.mem());

    let mut got_cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    memory
        .read(&store, CPU_PTR as usize, &mut got_cpu_bytes)
        .unwrap();
    let (got_gpr, got_rip, got_rflags) = read_cpu_state(&got_cpu_bytes);
    assert_eq!(got_gpr, interp_state.cpu.gpr);
    assert_eq!(got_rip, interp_state.cpu.rip);
    assert_eq!(got_rflags, interp_state.cpu.rflags);
}

#[test]
fn tier2_trace_wasm_matches_interpreter_on_memory_ops() {
    let mut trace = TraceIr {
        prologue: vec![],
        body: vec![
            Instr::Const {
                dst: v(0),
                value: 0x100,
            },
            Instr::Const {
                dst: v(1),
                value: 0x1122_3344_5566_7788,
            },
            Instr::StoreMem {
                addr: Operand::Value(v(0)),
                src: Operand::Value(v(1)),
                width: Width::W64,
            },
            Instr::LoadMem {
                dst: v(2),
                addr: Operand::Value(v(0)),
                width: Width::W64,
            },
            Instr::StoreReg {
                reg: Gpr::Rax,
                src: Operand::Value(v(2)),
            },
        ],
        kind: TraceKind::Linear,
    };

    let opt = optimize_trace(&mut trace, &OptConfig::default());
    let wasm = Tier2WasmCodegen::new().compile_trace(&trace, &opt.regalloc);
    validate_wasm(&wasm);

    let env = RuntimeEnv::default();

    let mut init_state = T2State::default();
    init_state.cpu.rip = 0x1234;
    init_state.cpu.rflags = abi::RFLAGS_RESERVED1 | RFLAGS_DF;

    let mut interp_state = init_state.clone();
    let mut bus = SimpleBus::new(GUEST_MEM_SIZE);
    let res = run_trace_with_cached_regs(
        &trace,
        &env,
        &mut bus,
        &mut interp_state,
        1,
        &opt.regalloc.cached,
    );
    assert_eq!(res.exit, RunExit::Returned);

    assert_eq!(
        interp_state.cpu.gpr[Gpr::Rax.as_u8() as usize],
        0x1122_3344_5566_7788
    );

    let (mut store, memory, func) = instantiate_trace(&wasm, HostEnv::default());
    let guest_mem_init = vec![0u8; GUEST_MEM_SIZE];
    memory.write(&mut store, 0, &guest_mem_init).unwrap();

    let mut cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    write_cpu_state(&mut cpu_bytes, &init_state.cpu);
    memory
        .write(&mut store, CPU_PTR as usize, &cpu_bytes)
        .unwrap();
    install_code_version_table(&memory, &mut store, &[]);

    let got_rip = func.call(&mut store, (CPU_PTR, JIT_CTX_PTR)).unwrap() as u64;
    assert_eq!(got_rip, interp_state.cpu.rip);

    let mut got_guest_mem = vec![0u8; GUEST_MEM_SIZE];
    memory.read(&store, 0, &mut got_guest_mem).unwrap();
    assert_eq!(got_guest_mem.as_slice(), bus.mem());

    let mut got_cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    memory
        .read(&store, CPU_PTR as usize, &mut got_cpu_bytes)
        .unwrap();
    let (got_gpr, got_rip, got_rflags) = read_cpu_state(&got_cpu_bytes);
    assert_eq!(got_gpr, interp_state.cpu.gpr);
    assert_eq!(got_rip, interp_state.cpu.rip);
    assert_eq!(got_rflags, interp_state.cpu.rflags);
}

#[test]
fn tier2_loop_trace_invalidates_on_mid_execution_code_version_bump() {
    // Same basic loop shape as the side-exit test, but place the entry block at the end of a
    // 4KiB page so its code spans 2 pages. We then bump the second page's version mid-trace and
    // assert the loop trace deopts.
    let entry_rip = 0x0FF0u64;
    let func = Function {
        entry: BlockId(0),
        blocks: vec![
            Block {
                id: BlockId(0),
                start_rip: entry_rip,
                code_len: 0x40, // crosses the 0x1000 boundary (pages 0 and 1)
                instrs: vec![
                    Instr::LoadReg {
                        dst: v(0),
                        reg: Gpr::Rax,
                    },
                    Instr::Const {
                        dst: v(1),
                        value: 1,
                    },
                    Instr::BinOp {
                        dst: v(2),
                        op: BinOp::Add,
                        lhs: Operand::Value(v(0)),
                        rhs: Operand::Value(v(1)),
                        flags: FlagSet::ALU,
                    },
                    Instr::StoreReg {
                        reg: Gpr::Rax,
                        src: Operand::Value(v(2)),
                    },
                    Instr::Const {
                        dst: v(3),
                        value: 10,
                    },
                    Instr::BinOp {
                        dst: v(4),
                        op: BinOp::LtU,
                        lhs: Operand::Value(v(2)),
                        rhs: Operand::Value(v(3)),
                        flags: FlagSet::EMPTY,
                    },
                ],
                term: Terminator::Branch {
                    cond: Operand::Value(v(4)),
                    then_bb: BlockId(0),
                    else_bb: BlockId(1),
                },
            },
            Block {
                id: BlockId(1),
                start_rip: 0x2000,
                code_len: 1,
                instrs: vec![],
                term: Terminator::Return,
            },
        ],
    };

    let mut profile = ProfileData::default();
    profile.block_counts.insert(BlockId(0), 10_000);
    profile.edge_counts.insert((BlockId(0), BlockId(0)), 9_000);
    profile.edge_counts.insert((BlockId(0), BlockId(1)), 1_000);
    profile.hot_backedges.insert((BlockId(0), BlockId(0)));

    let mut page_versions = PageVersionTracker::default();
    page_versions.set_version(0, 1);
    page_versions.set_version(1, 2);

    let builder = TraceBuilder::new(
        &func,
        &profile,
        &page_versions,
        TraceConfig {
            hot_block_threshold: 1000,
            max_blocks: 8,
            max_instrs: 256,
        },
    );
    let mut trace = builder.build_from(BlockId(0)).expect("trace");
    assert_eq!(trace.ir.kind, TraceKind::Loop);

    let opt = optimize_trace(&mut trace.ir, &OptConfig::default());
    let wasm = Tier2WasmCodegen::new().compile_trace(&trace.ir, &opt.regalloc);
    validate_wasm(&wasm);

    let mut init_state = T2State::default();
    init_state.cpu.gpr[Gpr::Rax.as_u8() as usize] = 0;
    init_state.cpu.rip = entry_rip;
    init_state.cpu.rflags = abi::RFLAGS_RESERVED1;

    // The loop trace will guard pages [0, 1] at trace entry (2 calls), and again at the start of
    // the loop body. Bump page 1 on the 4th call to simulate mid-trace self-modifying code.
    let host_env = HostEnv {
        code_version_calls: 0,
        bump_on_call: Some((4, 1)),
    };
    let (mut store, memory, func) = instantiate_trace(&wasm, host_env);

    let guest_mem_init = vec![0u8; GUEST_MEM_SIZE];
    memory.write(&mut store, 0, &guest_mem_init).unwrap();

    let mut cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    write_cpu_state(&mut cpu_bytes, &init_state.cpu);
    memory
        .write(&mut store, CPU_PTR as usize, &cpu_bytes)
        .unwrap();

    let table_ptr = install_code_version_table(&memory, &mut store, &[1, 2]);

    let got_rip = func.call(&mut store, (CPU_PTR, JIT_CTX_PTR)).unwrap() as u64;
    assert_eq!(got_rip, entry_rip);

    let exit_reason = read_u32_from_memory(
        &memory,
        &store,
        CPU_PTR as usize + jit_ctx::TRACE_EXIT_REASON_OFFSET as usize,
    );
    assert_eq!(exit_reason, jit_ctx::TRACE_EXIT_REASON_CODE_INVALIDATION);

    let bumped_page1 = read_u32_from_memory(&memory, &store, table_ptr as usize + 4);
    assert_eq!(bumped_page1, 3);
}
