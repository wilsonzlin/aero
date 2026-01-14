use aero_cpu_core::jit::runtime::PageVersionTracker;
use aero_cpu_core::state::RFLAGS_DF;
use aero_types::{Flag, FlagSet, Gpr, Width};
mod tier1_common;

use tier1_common::SimpleBus;

use aero_jit_x86::abi;
use aero_jit_x86::jit_ctx;
use aero_jit_x86::tier2::interp::{run_trace_with_cached_regs, RunExit, RuntimeEnv, T2State};
use aero_jit_x86::tier2::ir::{
    flag_to_set, BinOp, Block, BlockId, FlagValues, Function, Instr, Operand, Terminator, TraceIr,
    TraceKind, ValueId,
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
use wasmparser::{Operator, Parser, Payload, Validator};

const CPU_PTR: i32 = 0x1_0000;
const JIT_CTX_PTR: i32 = CPU_PTR + (abi::CPU_STATE_SIZE as i32);
const GUEST_MEM_SIZE: usize = 0x1_0000; // 1 page

fn validate_wasm(bytes: &[u8]) {
    let mut validator = Validator::new();
    validator.validate_all(bytes).unwrap();
}

fn wasm_accesses_cpu_rflags(wasm: &[u8]) -> (bool, bool) {
    let mut has_load = false;
    let mut has_store = false;
    for payload in Parser::new(0).parse_all(wasm) {
        match payload.expect("parse wasm") {
            Payload::CodeSectionEntry(body) => {
                let mut reader = body.get_operators_reader().expect("operators reader");
                while !reader.eof() {
                    match reader.read().expect("read operator") {
                        Operator::I64Load { memarg } => {
                            if memarg.offset == u64::from(abi::CPU_RFLAGS_OFF) {
                                has_load = true;
                            }
                        }
                        Operator::I64Store { memarg } => {
                            if memarg.offset == u64::from(abi::CPU_RFLAGS_OFF) {
                                has_store = true;
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    (has_load, has_store)
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
fn tier2_trace_wasm_without_flag_usage_does_not_touch_rflags() {
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
                op: BinOp::Add,
                lhs: Operand::Value(v(0)),
                rhs: Operand::Value(v(1)),
                flags: FlagSet::EMPTY,
            },
            Instr::StoreReg {
                reg: Gpr::Rax,
                src: Operand::Value(v(2)),
            },
            Instr::SideExit { exit_rip: 0x9999 },
        ],
        kind: TraceKind::Linear,
    };
    let opt = optimize_trace(&mut trace, &OptConfig::default());
    let wasm = Tier2WasmCodegen::new().compile_trace(&trace, &opt.regalloc);
    validate_wasm(&wasm);

    let (has_load, has_store) = wasm_accesses_cpu_rflags(&wasm);
    assert!(
        !has_load,
        "trace without flag usage should not i64.load CpuState.rflags"
    );
    assert!(
        !has_store,
        "trace without flag usage should not i64.store CpuState.rflags"
    );
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
            Instr::Const {
                dst: v(1),
                value: 1,
            },
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

    let page_versions = PageVersionTracker::default();
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

    let env = RuntimeEnv::default();
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

    let page_versions = PageVersionTracker::default();
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

    // The loop trace will guard pages [0, 1] at the start of each iteration (2 calls per
    // iteration). Bump page 1 on the 2nd call (page 1 of the first iteration) to simulate
    // mid-trace self-modifying code.
    let host_env = HostEnv {
        code_version_calls: 0,
        bump_on_call: Some((2, 1)),
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

#[cfg(not(target_arch = "wasm32"))]
mod random_traces {
    use super::*;

    use rand::{seq::SliceRandom, Rng, RngCore, SeedableRng};
    use rand_chacha::ChaCha8Rng;

    use aero_jit_x86::tier2::interp::run_trace;
    use aero_jit_x86::tier2::ir::ALL_GPRS;

    fn v(idx: u32) -> ValueId {
        ValueId(idx)
    }

    fn make_random_state(rng: &mut ChaCha8Rng) -> T2State {
        let mut state = T2State::default();

        for reg in ALL_GPRS {
            state.cpu.gpr[reg.as_u8() as usize] = rng.gen();
        }

        state.cpu.rip = 0x1000;
        state.cpu.rflags = abi::RFLAGS_RESERVED1;
        for flag in [Flag::Cf, Flag::Pf, Flag::Af, Flag::Zf, Flag::Sf, Flag::Of] {
            if rng.gen() {
                state.cpu.rflags |= 1u64 << flag.rflags_bit();
            }
        }

        state
    }

    fn gen_operand(rng: &mut ChaCha8Rng, values: &[ValueId]) -> Operand {
        if !values.is_empty() && rng.gen_bool(0.7) {
            Operand::Value(values[rng.gen_range(0..values.len())])
        } else {
            Operand::Const(rng.gen())
        }
    }

    fn gen_random_trace(
        rng: &mut ChaCha8Rng,
        instr_count: usize,
        code_versions: &[u32],
    ) -> TraceIr {
        let mut next_value: u32 = 0;
        let mut values: Vec<ValueId> = Vec::new();
        let mut safe_addrs: Vec<ValueId> = Vec::new();
        let kind = if rng.gen_bool(0.25) {
            TraceKind::Loop
        } else {
            TraceKind::Linear
        };
        // Keep `GuardCodeVersion` in the prologue. The verifier treats code-version guards in the
        // body of a linear trace as a loop artifact.
        let mut prologue: Vec<Instr> = Vec::new();
        let mut body: Vec<Instr> = Vec::new();

        // Add a small number of code-version guards in the prologue (matching the real trace
        // builder and satisfying Tier-2 IR verifier invariants for linear traces).
        if !code_versions.is_empty() && rng.gen_bool(0.35) {
            let table_len = code_versions.len() as u64;
            let guard_count = if rng.gen_bool(0.15) { 2 } else { 1 };
            for _ in 0..guard_count {
                let page = rng.gen_range(0..table_len);
                let expected = if rng.gen_bool(0.8) {
                    code_versions[page as usize]
                } else {
                    code_versions[page as usize].wrapping_add(1)
                };
                let exit_rip = 0x3000u64 + (rng.gen::<u16>() as u64);
                prologue.push(Instr::GuardCodeVersion {
                    page,
                    expected,
                    exit_rip,
                });
            }
        }

        // Seed at least one safe, in-bounds address value sometimes so memory ops can use
        // `Operand::Value` addresses (exercise value-local address plumbing).
        if rng.gen_bool(0.5) {
            let dst = v(next_value);
            next_value += 1;
            let value = rng.gen_range(0..=(GUEST_MEM_SIZE - 8)) as u64;
            prologue.push(Instr::Const { dst, value });
            values.push(dst);
            safe_addrs.push(dst);
        }

        while body.len() < instr_count {
            match rng.gen_range(0..100u32) {
                0..=15 => {
                    let dst = v(next_value);
                    next_value += 1;
                    let value = if rng.gen_bool(0.25) {
                        // Bias towards generating some in-bounds addresses we can safely use for
                        // any load/store width.
                        rng.gen_range(0..=(GUEST_MEM_SIZE - 8)) as u64
                    } else {
                        rng.gen()
                    };
                    body.push(Instr::Const {
                        dst,
                        value,
                    });
                    values.push(dst);
                    if value <= (GUEST_MEM_SIZE - 8) as u64 {
                        safe_addrs.push(dst);
                    }
                }
                16..=33 => {
                    let dst = v(next_value);
                    next_value += 1;
                    let reg = *ALL_GPRS.choose(rng).unwrap();
                    body.push(Instr::LoadReg { dst, reg });
                    values.push(dst);
                }
                34..=64 => {
                    if values.is_empty() {
                        continue;
                    }
                    let dst = v(next_value);
                    next_value += 1;
                    let op = match rng.gen_range(0..11u32) {
                        0 => BinOp::Add,
                        1 => BinOp::Sub,
                        2 => BinOp::Mul,
                        3 => BinOp::And,
                        4 => BinOp::Or,
                        5 => BinOp::Xor,
                        6 => BinOp::Shl,
                        7 => BinOp::Shr,
                        8 => BinOp::Sar,
                        9 => BinOp::Eq,
                        _ => BinOp::LtU,
                    };
                    let lhs = gen_operand(rng, &values);
                    let rhs = gen_operand(rng, &values);
                    let flags = if rng.gen_bool(0.3) {
                        FlagSet::ALU
                    } else {
                        FlagSet::EMPTY
                    };
                    body.push(Instr::BinOp {
                        dst,
                        op,
                        lhs,
                        rhs,
                        flags,
                    });
                    values.push(dst);
                }
                65..=74 => {
                    let dst = v(next_value);
                    next_value += 1;
                    let base = gen_operand(rng, &values);
                    let index = gen_operand(rng, &values);
                    let scale = *[1u8, 2, 4, 8].choose(rng).unwrap();
                    let disp = rng.gen::<i32>() as i64;
                    body.push(Instr::Addr {
                        dst,
                        base,
                        index,
                        scale,
                        disp,
                    });
                    values.push(dst);
                }
                75..=79 => {
                    // Memory loads use constant, in-bounds addresses to keep the harness simple.
                    let dst = v(next_value);
                    next_value += 1;
                    let width = *[Width::W8, Width::W16, Width::W32, Width::W64]
                        .choose(rng)
                        .unwrap();
                    let bytes = width.bytes() as usize;
                    let addr = if !safe_addrs.is_empty() && rng.gen_bool(0.6) {
                        Operand::Value(*safe_addrs.choose(rng).unwrap())
                    } else {
                        Operand::Const(rng.gen_range(0..(GUEST_MEM_SIZE - bytes)) as u64)
                    };
                    body.push(Instr::LoadMem {
                        dst,
                        addr,
                        width,
                    });
                    values.push(dst);
                }
                80..=84 => {
                    // Memory stores use constant, in-bounds addresses to keep the harness simple.
                    //
                    // Note: the WASM test harness bumps the code-version table on memory writes.
                    // This random trace generator keeps all `GuardCodeVersion` ops in the prologue,
                    // so these bumps do not affect guard checks within the same trace execution.
                    let width = *[Width::W8, Width::W16, Width::W32, Width::W64]
                        .choose(rng)
                        .unwrap();
                    let bytes = width.bytes() as usize;
                    let addr = if !safe_addrs.is_empty() && rng.gen_bool(0.6) {
                        Operand::Value(*safe_addrs.choose(rng).unwrap())
                    } else {
                        Operand::Const(rng.gen_range(0..(GUEST_MEM_SIZE - bytes)) as u64)
                    };
                    let src = gen_operand(rng, &values);
                    body.push(Instr::StoreMem {
                        addr,
                        src,
                        width,
                    });
                }
                85..=89 => {
                    let dst = v(next_value);
                    next_value += 1;
                    let flag = *[
                        Flag::Cf,
                        Flag::Pf,
                        Flag::Af,
                        Flag::Zf,
                        Flag::Sf,
                        Flag::Of,
                    ]
                    .choose(rng)
                    .unwrap();
                    body.push(Instr::LoadFlag { dst, flag });
                    values.push(dst);
                }
                90..=92 => {
                    // Directly set a random subset of flags.
                    let mut mask = FlagSet::EMPTY;
                    for flag in [Flag::Cf, Flag::Pf, Flag::Af, Flag::Zf, Flag::Sf, Flag::Of] {
                        if rng.gen_bool(0.5) {
                            mask = mask.union(flag_to_set(flag));
                        }
                    }
                    if mask.is_empty() {
                        mask = FlagSet::CF;
                    }
                    body.push(Instr::SetFlags {
                        mask,
                        values: FlagValues {
                            cf: rng.gen(),
                            pf: rng.gen(),
                            af: rng.gen(),
                            zf: rng.gen(),
                            sf: rng.gen(),
                            of: rng.gen(),
                        },
                    });
                }
                93..=95 => {
                    // Conditional guard (side exit).
                    let cond = gen_operand(rng, &values);
                    let expected = rng.gen_bool(0.5);
                    let exit_rip = 0x2000u64 + (rng.gen::<u16>() as u64);
                    body.push(Instr::Guard {
                        cond,
                        expected,
                        exit_rip,
                    });
                }
                _ => {
                    if values.is_empty() {
                        continue;
                    }
                    let reg = *ALL_GPRS.choose(rng).unwrap();
                    let src = gen_operand(rng, &values);
                    body.push(Instr::StoreReg { reg, src });
                }
            }
        }

        // Add an extra code-version guard sometimes, so we cover both:
        // - success path (fallthrough to return/side-exit)
        // - invalidation path (guard mismatch).
        if rng.gen_bool(0.2) {
            let table_len = code_versions.len() as u64;
            let page = if table_len != 0 {
                rng.gen_range(0..table_len)
            } else {
                0
            };
            let expected = if table_len != 0 && rng.gen_bool(0.7) {
                code_versions[page as usize]
            } else if table_len != 0 {
                code_versions[page as usize].wrapping_add(1)
            } else if rng.gen_bool(0.7) {
                0
            } else {
                1
            };
            let exit_rip = 0x3000u64 + (rng.gen::<u16>() as u64);
            prologue.push(Instr::GuardCodeVersion {
                page,
                expected,
                exit_rip,
            });
        }

        // Add occasional side exits; keep the exit RIP distinct from the trace's entry RIP so we
        // can disambiguate `Returned` from `SideExit` on the WASM side.
        //
        // Loop traces must terminate (the WASM trace executes an actual `loop {}`), so always end
        // them with a side exit.
        if kind == TraceKind::Loop || rng.gen_bool(0.25) {
            let exit_rip = 0x2000u64 + (rng.gen::<u16>() as u64);
            body.push(Instr::SideExit { exit_rip });
        }

        TraceIr {
            prologue,
            body,
            kind,
        }
    }

    fn wasm_exit_to_run_exit(exit_reason: u32, init_rip: u64, next_rip: u64) -> RunExit {
        match exit_reason {
            jit_ctx::TRACE_EXIT_REASON_NONE => {
                if next_rip == init_rip {
                    RunExit::Returned
                } else {
                    RunExit::SideExit { next_rip }
                }
            }
            jit_ctx::TRACE_EXIT_REASON_CODE_INVALIDATION => RunExit::Invalidate { next_rip },
            other => panic!("unexpected Tier2 trace exit reason: {other}"),
        }
    }

    #[test]
    fn tier2_trace_wasm_matches_interpreter_on_random_traces() {
        let mut rng = ChaCha8Rng::seed_from_u64(0x5EED);

        // Tier-2 WASM compilation + instantiation is more expensive than pure interpreter checks, so
        // keep the iteration count modest while still providing broad coverage.
        for i in 0..75 {
            let env = RuntimeEnv::default();
            // Install a small code-version table so random traces can exercise
            // `Instr::GuardCodeVersion` both in the interpreter and in WASM.
            let mut code_versions: Vec<u32> = (0..8).map(|_| rng.gen()).collect();
            // Ensure at least one entry is non-zero so "guard success on non-zero" is possible.
            if code_versions.iter().all(|v| *v == 0) {
                code_versions[0] = 1;
            }
            for (page, version) in code_versions.iter().copied().enumerate() {
                env.page_versions.set_version(page as u64, version);
            }

            let instr_count = rng.gen_range(20..=50);
            let trace = gen_random_trace(&mut rng, instr_count, &code_versions);

            let mut guest_mem_init = vec![0u8; GUEST_MEM_SIZE];
            rng.fill_bytes(&mut guest_mem_init);

            let init_state = make_random_state(&mut rng);
            let init_rip = init_state.cpu.rip;

            // ---- Tier-2 interpreter (reference) -------------------------------------------------
            let mut interp_state = init_state.clone();
            let mut bus = SimpleBus::new(GUEST_MEM_SIZE);
            bus.load(0, &guest_mem_init);
            let expected = run_trace(&trace, &env, &mut bus, &mut interp_state, 1);

            // ---- Optimize + compile to WASM ----------------------------------------------------
            let mut optimized = trace.clone();
            let opt = optimize_trace(&mut optimized, &OptConfig::default());
            let wasm = Tier2WasmCodegen::new().compile_trace(&optimized, &opt.regalloc);
            validate_wasm(&wasm);

            // ---- Execute the WASM trace via wasmi ----------------------------------------------
            let (mut store, memory, func) = instantiate_trace(&wasm, HostEnv::default());
            memory.write(&mut store, 0, &guest_mem_init).unwrap();

            let mut cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
            write_cpu_state(&mut cpu_bytes, &init_state.cpu);
            memory
                .write(&mut store, CPU_PTR as usize, &cpu_bytes)
                .unwrap();
            install_code_version_table(&memory, &mut store, &code_versions);

            let next_rip = func.call(&mut store, (CPU_PTR, JIT_CTX_PTR)).unwrap() as u64;
            let exit_reason = read_u32_from_memory(
                &memory,
                &store,
                CPU_PTR as usize + jit_ctx::TRACE_EXIT_REASON_OFFSET as usize,
            );
            let got_exit = wasm_exit_to_run_exit(exit_reason, init_rip, next_rip);

            assert_eq!(
                expected.exit, got_exit,
                "exit mismatch on iteration {i}\ntrace: {trace:?}\noptimized: {optimized:?}"
            );

            // Guest memory.
            let mut got_guest_mem = vec![0u8; GUEST_MEM_SIZE];
            memory.read(&store, 0, &mut got_guest_mem).unwrap();
            assert_eq!(
                got_guest_mem.as_slice(),
                bus.mem(),
                "guest memory mismatch on iteration {i}\ntrace: {trace:?}\noptimized: {optimized:?}"
            );

            // CPU state.
            let mut got_cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
            memory
                .read(&store, CPU_PTR as usize, &mut got_cpu_bytes)
                .unwrap();
            let (got_gpr, got_rip, got_rflags) = read_cpu_state(&got_cpu_bytes);
            assert_eq!(
                got_gpr, interp_state.cpu.gpr,
                "gpr mismatch on iteration {i}\ntrace: {trace:?}\noptimized: {optimized:?}"
            );
            assert_eq!(
                got_rip, interp_state.cpu.rip,
                "rip mismatch on iteration {i}\ntrace: {trace:?}\noptimized: {optimized:?}"
            );
            assert_eq!(
                got_rflags, interp_state.cpu.rflags,
                "rflags mismatch on iteration {i}\ntrace: {trace:?}\noptimized: {optimized:?}"
            );
        }
    }
}
