#![cfg(not(target_arch = "wasm32"))]

use aero_cpu_core::jit::runtime::PageVersionTracker;
use aero_cpu_core::state::RFLAGS_DF;
use aero_types::{Flag, FlagSet, Gpr, Width};
mod tier1_common;

use tier1_common::{pick_invalid_opcode, SimpleBus};

use aero_jit_x86::abi;
use aero_jit_x86::jit_ctx;
use aero_jit_x86::tier2::interp::{run_trace_with_cached_regs, RunExit, RuntimeEnv, T2State};
#[cfg(not(target_arch = "wasm32"))]
use aero_jit_x86::tier2::ir::flag_to_set;
use aero_jit_x86::tier2::ir::FlagValues;
use aero_jit_x86::tier2::ir::{
    BinOp, Block, BlockId, Function, Instr, Operand, Terminator, TraceIr, TraceKind, ValueId,
};
use aero_jit_x86::tier2::opt::{optimize_trace, OptConfig};
use aero_jit_x86::tier2::profile::{ProfileData, TraceConfig};
use aero_jit_x86::tier2::trace::TraceBuilder;
use aero_jit_x86::tier2::wasm_codegen::{
    Tier2WasmCodegen, Tier2WasmOptions, EXPORT_TRACE_FN, IMPORT_CODE_PAGE_VERSION,
};
use aero_jit_x86::tier2::{build_function_from_x86, CfgBuildConfig};
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
        if let Payload::CodeSectionEntry(body) = payload.expect("parse wasm") {
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
            Instr::Const {
                dst: v(1),
                value: 1,
            },
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
fn tier2_trace_wasm_updates_rflags_even_when_flags_are_not_observed_in_trace() {
    // The Tier-2 optimizer treats trace boundaries/side exits as observing full flags, so flag
    // writes must not be dropped even if the trace never reads flags internally.
    let mut trace = TraceIr {
        prologue: vec![],
        body: vec![
            Instr::Const {
                dst: v(0),
                value: 0xffff_ffff_ffff_ffff,
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
        ],
        kind: TraceKind::Linear,
    };

    let opt = optimize_trace(&mut trace, &OptConfig::default());
    let wasm = Tier2WasmCodegen::new().compile_trace(&trace, &opt.regalloc);
    validate_wasm(&wasm);

    let (has_load, has_store) = wasm_accesses_cpu_rflags(&wasm);
    assert!(
        has_load,
        "trace that writes flags should i64.load CpuState.rflags"
    );
    assert!(
        has_store,
        "trace that writes flags should i64.store CpuState.rflags"
    );

    let env = RuntimeEnv::default();

    let mut init_state = T2State::default();
    init_state.cpu.rip = 0x1234;
    init_state.cpu.rflags = abi::RFLAGS_RESERVED1 | RFLAGS_DF;
    init_state.cpu.gpr[Gpr::Rax.as_u8() as usize] = 0;

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
    assert_eq!(interp_state.cpu.gpr[Gpr::Rax.as_u8() as usize], 0);

    let alu_mask = (1u64 << Flag::Cf.rflags_bit())
        | (1u64 << Flag::Pf.rflags_bit())
        | (1u64 << Flag::Af.rflags_bit())
        | (1u64 << Flag::Zf.rflags_bit())
        | (1u64 << Flag::Sf.rflags_bit())
        | (1u64 << Flag::Of.rflags_bit());
    let expected_flags = (1u64 << Flag::Cf.rflags_bit())
        | (1u64 << Flag::Pf.rflags_bit())
        | (1u64 << Flag::Af.rflags_bit())
        | (1u64 << Flag::Zf.rflags_bit());
    assert_eq!(
        interp_state.cpu.rflags & alu_mask,
        expected_flags,
        "interpreter should compute CF/PF/AF/ZF for 0xffff.. + 1"
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

    let exit_reason = read_u32_from_memory(
        &memory,
        &store,
        CPU_PTR as usize + jit_ctx::TRACE_EXIT_REASON_OFFSET as usize,
    );
    assert_eq!(
        exit_reason,
        jit_ctx::TRACE_EXIT_REASON_NONE,
        "guard side exits should not set TRACE_EXIT_REASON_CODE_INVALIDATION"
    );

    let mut got_cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    memory
        .read(&store, CPU_PTR as usize, &mut got_cpu_bytes)
        .unwrap();
    let (got_gpr, got_rip_in_cpu, got_rflags) = read_cpu_state(&got_cpu_bytes);
    assert_eq!(got_gpr, interp_state.cpu.gpr);
    assert_eq!(got_rip_in_cpu, interp_state.cpu.rip);
    assert_eq!(got_rflags, interp_state.cpu.rflags);
}

#[test]
fn tier2_trace_wasm_spills_rflags_on_side_exit_when_flags_are_not_observed_in_trace() {
    // Same as the above test, but ensure we also spill updated flags on a side exit.
    let mut trace = TraceIr {
        prologue: vec![],
        body: vec![
            Instr::Const {
                dst: v(0),
                value: 0xffff_ffff_ffff_ffff,
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
            Instr::SideExit { exit_rip: 0x2000 },
        ],
        kind: TraceKind::Linear,
    };

    let opt = optimize_trace(&mut trace, &OptConfig::default());
    let wasm = Tier2WasmCodegen::new().compile_trace(&trace, &opt.regalloc);
    validate_wasm(&wasm);

    let (has_load, has_store) = wasm_accesses_cpu_rflags(&wasm);
    assert!(
        has_load,
        "trace that writes flags should i64.load CpuState.rflags"
    );
    assert!(
        has_store,
        "trace that writes flags should i64.store CpuState.rflags"
    );

    let env = RuntimeEnv::default();

    let mut init_state = T2State::default();
    init_state.cpu.rip = 0x1234;
    init_state.cpu.rflags = abi::RFLAGS_RESERVED1 | RFLAGS_DF;
    init_state.cpu.gpr[Gpr::Rax.as_u8() as usize] = 0;

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
    assert_eq!(res.exit, RunExit::SideExit { next_rip: 0x2000 });
    assert_eq!(interp_state.cpu.rip, 0x2000);

    let alu_mask = (1u64 << Flag::Cf.rflags_bit())
        | (1u64 << Flag::Pf.rflags_bit())
        | (1u64 << Flag::Af.rflags_bit())
        | (1u64 << Flag::Zf.rflags_bit())
        | (1u64 << Flag::Sf.rflags_bit())
        | (1u64 << Flag::Of.rflags_bit());
    let expected_flags = (1u64 << Flag::Cf.rflags_bit())
        | (1u64 << Flag::Pf.rflags_bit())
        | (1u64 << Flag::Af.rflags_bit())
        | (1u64 << Flag::Zf.rflags_bit());
    assert_eq!(interp_state.cpu.rflags & alu_mask, expected_flags);

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

    let exit_reason = read_u32_from_memory(
        &memory,
        &store,
        CPU_PTR as usize + jit_ctx::TRACE_EXIT_REASON_OFFSET as usize,
    );
    assert_eq!(
        exit_reason,
        jit_ctx::TRACE_EXIT_REASON_NONE,
        "guard side exits should not set TRACE_EXIT_REASON_CODE_INVALIDATION"
    );

    let mut got_cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    memory
        .read(&store, CPU_PTR as usize, &mut got_cpu_bytes)
        .unwrap();
    let (got_gpr, got_rip_in_cpu, got_rflags) = read_cpu_state(&got_cpu_bytes);
    assert_eq!(got_gpr, interp_state.cpu.gpr);
    assert_eq!(got_rip_in_cpu, interp_state.cpu.rip);
    assert_eq!(got_rflags, interp_state.cpu.rflags);
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
fn tier2_trace_exit_reason_is_reset_to_none_on_each_entry() {
    // The Tier-2 trace ABI uses `jit_ctx::TRACE_EXIT_REASON_OFFSET` to communicate why a trace
    // exited. This value must be reset to `TRACE_EXIT_REASON_NONE` at the start of *each* trace
    // invocation so callers don't observe stale invalidation reasons from a previous run.
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

    // Mismatch the guard so the first invocation invalidates.
    let table_ptr = install_code_version_table(&memory, &mut store, &[0]);

    let got_rip = func.call(&mut store, (CPU_PTR, JIT_CTX_PTR)).unwrap() as u64;
    assert_eq!(got_rip, 0x9999);
    let exit_reason = read_u32_from_memory(
        &memory,
        &store,
        CPU_PTR as usize + jit_ctx::TRACE_EXIT_REASON_OFFSET as usize,
    );
    assert_eq!(exit_reason, jit_ctx::TRACE_EXIT_REASON_CODE_INVALIDATION);

    // Update the table so the guard passes. The next invocation must reset exit reason to NONE.
    write_u32_to_memory(&memory, &mut store, table_ptr as usize, 1);
    memory
        .write(&mut store, CPU_PTR as usize, &cpu_bytes)
        .unwrap();

    let got_rip = func.call(&mut store, (CPU_PTR, JIT_CTX_PTR)).unwrap() as u64;
    assert_eq!(got_rip, 0x1111);
    let exit_reason = read_u32_from_memory(
        &memory,
        &store,
        CPU_PTR as usize + jit_ctx::TRACE_EXIT_REASON_OFFSET as usize,
    );
    assert_eq!(exit_reason, jit_ctx::TRACE_EXIT_REASON_NONE);
}

#[test]
fn tier2_inline_code_version_guard_treats_empty_table_as_zero() {
    // When the runtime has not configured a code-version table (`len == 0`), inline guards should
    // treat all pages as version 0 and must not dereference the table pointer (which is expected
    // to be null).
    let mut trace = TraceIr {
        prologue: vec![Instr::GuardCodeVersion {
            page: 0,
            expected: 0,
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

    install_code_version_table(&memory, &mut store, &[]);

    let got_rip = func.call(&mut store, (CPU_PTR, JIT_CTX_PTR)).unwrap() as u64;
    assert_eq!(got_rip, init_state.cpu.rip);
    let exit_reason = read_u32_from_memory(
        &memory,
        &store,
        CPU_PTR as usize + jit_ctx::TRACE_EXIT_REASON_OFFSET as usize,
    );
    assert_eq!(exit_reason, jit_ctx::TRACE_EXIT_REASON_NONE);
    assert_eq!(
        store.data().code_version_calls,
        0,
        "inline guard should not call env.code_page_version"
    );
}

#[test]
fn tier2_inline_code_version_guard_invalidates_against_empty_table_when_expected_nonzero() {
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

    install_code_version_table(&memory, &mut store, &[]);

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
fn tier2_code_version_guard_out_of_range_pages_default_to_zero_with_host_import() {
    // When `page >= table_len`, the runtime treats the code version as 0. The legacy import path
    // should reflect this behavior.
    let mut trace = TraceIr {
        prologue: vec![Instr::GuardCodeVersion {
            page: 5,
            expected: 0,
            exit_rip: 0x9999,
        }],
        body: vec![],
        kind: TraceKind::Linear,
    };

    let opt = optimize_trace(&mut trace, &OptConfig::default());
    let wasm = Tier2WasmCodegen::new().compile_trace(&trace, &opt.regalloc);
    validate_wasm(&wasm);

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
    install_code_version_table(&memory, &mut store, &[1]);

    let got_rip = func.call(&mut store, (CPU_PTR, JIT_CTX_PTR)).unwrap() as u64;
    assert_eq!(got_rip, 0x1111);
    assert_eq!(
        store.data().code_version_calls,
        1,
        "host-import guards should call env.code_page_version even for out-of-range pages"
    );
}

#[test]
fn tier2_code_version_guard_out_of_range_pages_default_to_zero_with_inline_table_reads() {
    // Same behavior as the host import variant, but using inline table loads.
    let mut trace = TraceIr {
        prologue: vec![Instr::GuardCodeVersion {
            page: 5,
            expected: 0,
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
    install_code_version_table(&memory, &mut store, &[1]);

    let got_rip = func.call(&mut store, (CPU_PTR, JIT_CTX_PTR)).unwrap() as u64;
    assert_eq!(got_rip, 0x1111);
    assert_eq!(
        store.data().code_version_calls,
        0,
        "inline guards should not call env.code_page_version"
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
fn tier2_trace_wasm_matches_interpreter_on_shift_flags_used_by_guard() {
    // mov al, 0x81
    // shl al, 1
    // jc taken
    // mov al, 0
    // <invalid>
    // taken:
    // mov al, 1
    // <invalid>
    //
    // For the given input, SHL sets CF=1, so JC must be taken.
    let invalid = pick_invalid_opcode(64);
    let code = [
        0xB0, 0x81, // mov al, 0x81
        0xC0, 0xE0, 0x01, // shl al, 1
        0x72, 0x03, // jc +3 (to mov al, 1)
        0xB0, 0x00,    // mov al, 0
        invalid, // <invalid>
        0xB0, 0x01,    // mov al, 1
        invalid, // <invalid>
    ];

    let mut bus = SimpleBus::new(64);
    bus.load(0, &code);
    let func = build_function_from_x86(&bus, 0, 64, CfgBuildConfig::default());

    let entry = func.entry;
    let taken = func.find_block_by_rip(10).expect("taken block");
    let not_taken = func.find_block_by_rip(7).expect("not taken block");

    let mut profile = ProfileData::default();
    profile.block_counts.insert(entry, 10_000);
    profile.block_counts.insert(taken, 10_000);
    profile.block_counts.insert(not_taken, 1);
    profile.edge_counts.insert((entry, taken), 9_000);
    profile.edge_counts.insert((entry, not_taken), 1_000);

    let page_versions = PageVersionTracker::default();
    page_versions.set_version(0, 1);

    let cfg = TraceConfig {
        hot_block_threshold: 1000,
        max_blocks: 8,
        max_instrs: 256,
    };

    let builder = TraceBuilder::new(&func, &profile, &page_versions, cfg);
    let trace = builder.build_from(entry).expect("trace");
    assert_eq!(trace.ir.kind, TraceKind::Linear);

    let mut trace_ir = trace.ir.clone();
    let opt = optimize_trace(&mut trace_ir, &OptConfig::default());
    let wasm = Tier2WasmCodegen::new().compile_trace(&trace_ir, &opt.regalloc);
    validate_wasm(&wasm);

    let mut init_state = T2State::default();
    init_state.cpu.rip = 0;
    init_state.cpu.rflags = abi::RFLAGS_RESERVED1;
    init_state.cpu.gpr[Gpr::Rax.as_u8() as usize] = 0;

    let env = RuntimeEnv::default();
    env.page_versions.set_version(0, 1);

    let mut interp_state = init_state.clone();
    let expected = run_trace_with_cached_regs(
        &trace_ir,
        &env,
        &mut bus,
        &mut interp_state,
        1,
        &opt.regalloc.cached,
    );
    assert_eq!(expected.exit, RunExit::SideExit { next_rip: 12 });
    assert_eq!(interp_state.cpu.gpr[Gpr::Rax.as_u8() as usize] & 0xff, 1);

    let (mut store, memory, trace_fn) = instantiate_trace(&wasm, HostEnv::default());
    let guest_mem_init = vec![0u8; GUEST_MEM_SIZE];
    memory.write(&mut store, 0, &guest_mem_init).unwrap();

    let mut cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    write_cpu_state(&mut cpu_bytes, &init_state.cpu);
    memory
        .write(&mut store, CPU_PTR as usize, &cpu_bytes)
        .unwrap();
    install_code_version_table(&memory, &mut store, &[1]);

    let got_rip = trace_fn.call(&mut store, (CPU_PTR, JIT_CTX_PTR)).unwrap() as u64;
    assert_eq!(got_rip, interp_state.cpu.rip);

    let mut got_cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    memory
        .read(&store, CPU_PTR as usize, &mut got_cpu_bytes)
        .unwrap();
    let (got_gpr, got_rip_in_cpu, got_rflags) = read_cpu_state(&got_cpu_bytes);
    assert_eq!(got_gpr, interp_state.cpu.gpr);
    assert_eq!(got_rip_in_cpu, interp_state.cpu.rip);
    assert_eq!(got_rflags, interp_state.cpu.rflags);
}

#[test]
fn tier2_trace_wasm_matches_interpreter_on_parity_flag_guard() {
    // Exercise `BinOp` flag computation + `LoadFlag` + `Guard`.
    //
    // Compute `RBX & 3` with flags enabled; for `RBX = 3` the result is 3 and PF must be set (even
    // parity). The trace guards that PF is set and stores the loaded flag into RAX.
    let mut trace = TraceIr {
        prologue: vec![],
        body: vec![
            Instr::LoadReg {
                dst: v(0),
                reg: Gpr::Rbx,
            },
            Instr::Const {
                dst: v(1),
                value: 3,
            },
            Instr::BinOp {
                dst: v(2),
                op: BinOp::And,
                lhs: Operand::Value(v(0)),
                rhs: Operand::Value(v(1)),
                flags: FlagSet::ALU,
            },
            Instr::LoadFlag {
                dst: v(3),
                flag: Flag::Pf,
            },
            Instr::Guard {
                cond: Operand::Value(v(3)),
                expected: true,
                exit_rip: 0x2000,
            },
            Instr::StoreReg {
                reg: Gpr::Rax,
                src: Operand::Value(v(3)),
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
    init_state.cpu.gpr[Gpr::Rbx.as_u8() as usize] = 3;
    init_state.cpu.gpr[Gpr::Rax.as_u8() as usize] = 0;

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
    assert_eq!(interp_state.cpu.gpr[Gpr::Rax.as_u8() as usize] & 0xff, 1);

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

    let mut got_cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    memory
        .read(&store, CPU_PTR as usize, &mut got_cpu_bytes)
        .unwrap();
    let (got_gpr, got_rip_in_cpu, got_rflags) = read_cpu_state(&got_cpu_bytes);
    assert_eq!(got_gpr, interp_state.cpu.gpr);
    assert_eq!(got_rip_in_cpu, interp_state.cpu.rip);
    assert_eq!(got_rflags, interp_state.cpu.rflags);
}

#[test]
fn tier2_trace_wasm_matches_interpreter_on_set_flags_preserves_unmasked_bits() {
    // Exercise `Instr::SetFlags` and ensure it:
    // - updates only the masked flags,
    // - preserves unmasked bits like DF,
    // - always keeps the reserved bit 1 set.
    let mut trace = TraceIr {
        prologue: vec![],
        body: vec![
            Instr::SetFlags {
                mask: FlagSet::CF.union(FlagSet::PF),
                values: FlagValues {
                    cf: true,
                    pf: false,
                    ..Default::default()
                },
            },
            Instr::LoadFlag {
                dst: v(0),
                flag: Flag::Cf,
            },
            Instr::LoadFlag {
                dst: v(1),
                flag: Flag::Pf,
            },
            Instr::StoreReg {
                reg: Gpr::Rax,
                src: Operand::Value(v(0)),
            },
            Instr::StoreReg {
                reg: Gpr::Rbx,
                src: Operand::Value(v(1)),
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
    init_state.cpu.rflags = abi::RFLAGS_RESERVED1
        | RFLAGS_DF
        | (1u64 << Flag::Pf.rflags_bit())
        | (1u64 << Flag::Zf.rflags_bit());
    init_state.cpu.gpr[Gpr::Rax.as_u8() as usize] = 0;
    init_state.cpu.gpr[Gpr::Rbx.as_u8() as usize] = 0;

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
    assert_eq!(interp_state.cpu.gpr[Gpr::Rax.as_u8() as usize] & 0xff, 1);
    assert_eq!(interp_state.cpu.gpr[Gpr::Rbx.as_u8() as usize] & 0xff, 0);
    assert_ne!(
        interp_state.cpu.rflags & RFLAGS_DF,
        0,
        "DF should be preserved"
    );
    assert_ne!(
        interp_state.cpu.rflags & abi::RFLAGS_RESERVED1,
        0,
        "reserved bit 1 should stay set"
    );
    assert_ne!(
        interp_state.cpu.rflags & (1u64 << Flag::Zf.rflags_bit()),
        0,
        "ZF should be preserved (not in SetFlags mask)"
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

    let mut got_cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    memory
        .read(&store, CPU_PTR as usize, &mut got_cpu_bytes)
        .unwrap();
    let (got_gpr, got_rip_in_cpu, got_rflags) = read_cpu_state(&got_cpu_bytes);
    assert_eq!(got_gpr, interp_state.cpu.gpr);
    assert_eq!(got_rip_in_cpu, interp_state.cpu.rip);
    assert_eq!(got_rflags, interp_state.cpu.rflags);
}

#[test]
fn tier2_trace_wasm_matches_interpreter_on_carry_flag_guard() {
    // Exercise CF (carry) computation on addition.
    //
    // 0xffff.. + 1 = 0x0, which sets CF=1.
    let mut trace = TraceIr {
        prologue: vec![],
        body: vec![
            Instr::Const {
                dst: v(0),
                value: 0xffff_ffff_ffff_ffff,
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
            Instr::LoadFlag {
                dst: v(3),
                flag: Flag::Cf,
            },
            Instr::Guard {
                cond: Operand::Value(v(3)),
                expected: true,
                exit_rip: 0x2000,
            },
            Instr::StoreReg {
                reg: Gpr::Rax,
                src: Operand::Value(v(2)),
            },
            Instr::StoreReg {
                reg: Gpr::Rbx,
                src: Operand::Value(v(3)),
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
    init_state.cpu.gpr[Gpr::Rax.as_u8() as usize] = 0;
    init_state.cpu.gpr[Gpr::Rbx.as_u8() as usize] = 0;

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
    assert_eq!(interp_state.cpu.gpr[Gpr::Rax.as_u8() as usize], 0);
    assert_eq!(interp_state.cpu.gpr[Gpr::Rbx.as_u8() as usize] & 0xff, 1);

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

    let mut got_cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    memory
        .read(&store, CPU_PTR as usize, &mut got_cpu_bytes)
        .unwrap();
    let (got_gpr, got_rip_in_cpu, got_rflags) = read_cpu_state(&got_cpu_bytes);
    assert_eq!(got_gpr, interp_state.cpu.gpr);
    assert_eq!(got_rip_in_cpu, interp_state.cpu.rip);
    assert_eq!(got_rflags, interp_state.cpu.rflags);
}

#[test]
fn tier2_trace_wasm_matches_interpreter_on_zero_flag_guard() {
    // Exercise ZF computation on a logical operation.
    //
    // x ^ x = 0, which sets ZF=1.
    let mut trace = TraceIr {
        prologue: vec![],
        body: vec![
            Instr::Const {
                dst: v(0),
                value: 0x1234_5678_9abc_def0,
            },
            Instr::BinOp {
                dst: v(1),
                op: BinOp::Xor,
                lhs: Operand::Value(v(0)),
                rhs: Operand::Value(v(0)),
                flags: FlagSet::ALU,
            },
            Instr::LoadFlag {
                dst: v(2),
                flag: Flag::Zf,
            },
            Instr::Guard {
                cond: Operand::Value(v(2)),
                expected: true,
                exit_rip: 0x2000,
            },
            Instr::StoreReg {
                reg: Gpr::Rax,
                src: Operand::Value(v(1)),
            },
            Instr::StoreReg {
                reg: Gpr::Rbx,
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
    init_state.cpu.gpr[Gpr::Rax.as_u8() as usize] = 0;
    init_state.cpu.gpr[Gpr::Rbx.as_u8() as usize] = 0;

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
    assert_eq!(interp_state.cpu.gpr[Gpr::Rax.as_u8() as usize], 0);
    assert_eq!(interp_state.cpu.gpr[Gpr::Rbx.as_u8() as usize] & 0xff, 1);

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

    let mut got_cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    memory
        .read(&store, CPU_PTR as usize, &mut got_cpu_bytes)
        .unwrap();
    let (got_gpr, got_rip_in_cpu, got_rflags) = read_cpu_state(&got_cpu_bytes);
    assert_eq!(got_gpr, interp_state.cpu.gpr);
    assert_eq!(got_rip_in_cpu, interp_state.cpu.rip);
    assert_eq!(got_rflags, interp_state.cpu.rflags);
}

#[test]
fn tier2_trace_wasm_matches_interpreter_on_sign_flag_guard() {
    // Exercise SF computation on a logical operation.
    //
    // x ^ 0 = x; for x with the sign bit set, SF must be 1.
    let mut trace = TraceIr {
        prologue: vec![],
        body: vec![
            Instr::Const {
                dst: v(0),
                value: 0x8000_0000_0000_0000,
            },
            Instr::Const {
                dst: v(1),
                value: 0,
            },
            Instr::BinOp {
                dst: v(2),
                op: BinOp::Xor,
                lhs: Operand::Value(v(0)),
                rhs: Operand::Value(v(1)),
                flags: FlagSet::ALU,
            },
            Instr::LoadFlag {
                dst: v(3),
                flag: Flag::Sf,
            },
            Instr::Guard {
                cond: Operand::Value(v(3)),
                expected: true,
                exit_rip: 0x2000,
            },
            Instr::StoreReg {
                reg: Gpr::Rax,
                src: Operand::Value(v(2)),
            },
            Instr::StoreReg {
                reg: Gpr::Rbx,
                src: Operand::Value(v(3)),
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
    init_state.cpu.gpr[Gpr::Rax.as_u8() as usize] = 0;
    init_state.cpu.gpr[Gpr::Rbx.as_u8() as usize] = 0;

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
        0x8000_0000_0000_0000u64
    );
    assert_eq!(interp_state.cpu.gpr[Gpr::Rbx.as_u8() as usize] & 0xff, 1);

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

    let mut got_cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    memory
        .read(&store, CPU_PTR as usize, &mut got_cpu_bytes)
        .unwrap();
    let (got_gpr, got_rip_in_cpu, got_rflags) = read_cpu_state(&got_cpu_bytes);
    assert_eq!(got_gpr, interp_state.cpu.gpr);
    assert_eq!(got_rip_in_cpu, interp_state.cpu.rip);
    assert_eq!(got_rflags, interp_state.cpu.rflags);
}

#[test]
fn tier2_trace_wasm_matches_interpreter_on_sub_borrow_flag_guard() {
    // Exercise CF (borrow) computation on subtraction.
    //
    // 0 - 1 = 0xffff.., which sets CF=1 (unsigned borrow).
    let mut trace = TraceIr {
        prologue: vec![],
        body: vec![
            Instr::Const {
                dst: v(0),
                value: 0,
            },
            Instr::Const {
                dst: v(1),
                value: 1,
            },
            Instr::BinOp {
                dst: v(2),
                op: BinOp::Sub,
                lhs: Operand::Value(v(0)),
                rhs: Operand::Value(v(1)),
                flags: FlagSet::ALU,
            },
            Instr::LoadFlag {
                dst: v(3),
                flag: Flag::Cf,
            },
            Instr::Guard {
                cond: Operand::Value(v(3)),
                expected: true,
                exit_rip: 0x2000,
            },
            Instr::StoreReg {
                reg: Gpr::Rax,
                src: Operand::Value(v(2)),
            },
            Instr::StoreReg {
                reg: Gpr::Rbx,
                src: Operand::Value(v(3)),
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
    init_state.cpu.gpr[Gpr::Rax.as_u8() as usize] = 0;
    init_state.cpu.gpr[Gpr::Rbx.as_u8() as usize] = 0;

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
        0xffff_ffff_ffff_ffff
    );
    assert_eq!(interp_state.cpu.gpr[Gpr::Rbx.as_u8() as usize] & 0xff, 1);

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

    let mut got_cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    memory
        .read(&store, CPU_PTR as usize, &mut got_cpu_bytes)
        .unwrap();
    let (got_gpr, got_rip_in_cpu, got_rflags) = read_cpu_state(&got_cpu_bytes);
    assert_eq!(got_gpr, interp_state.cpu.gpr);
    assert_eq!(got_rip_in_cpu, interp_state.cpu.rip);
    assert_eq!(got_rflags, interp_state.cpu.rflags);
}

#[test]
fn tier2_trace_wasm_matches_interpreter_on_sub_overflow_flag_guard() {
    // Exercise OF (signed overflow) computation on subtraction.
    //
    // 0x8000.. - 1 = 0x7fff.., which sets OF=1.
    let mut trace = TraceIr {
        prologue: vec![],
        body: vec![
            Instr::Const {
                dst: v(0),
                value: 0x8000_0000_0000_0000,
            },
            Instr::Const {
                dst: v(1),
                value: 1,
            },
            Instr::BinOp {
                dst: v(2),
                op: BinOp::Sub,
                lhs: Operand::Value(v(0)),
                rhs: Operand::Value(v(1)),
                flags: FlagSet::ALU,
            },
            Instr::LoadFlag {
                dst: v(3),
                flag: Flag::Of,
            },
            Instr::Guard {
                cond: Operand::Value(v(3)),
                expected: true,
                exit_rip: 0x2000,
            },
            Instr::StoreReg {
                reg: Gpr::Rax,
                src: Operand::Value(v(2)),
            },
            Instr::StoreReg {
                reg: Gpr::Rbx,
                src: Operand::Value(v(3)),
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
    init_state.cpu.gpr[Gpr::Rax.as_u8() as usize] = 0;
    init_state.cpu.gpr[Gpr::Rbx.as_u8() as usize] = 0;

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
        0x7fff_ffff_ffff_ffff
    );
    assert_eq!(interp_state.cpu.gpr[Gpr::Rbx.as_u8() as usize] & 0xff, 1);

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

    let mut got_cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    memory
        .read(&store, CPU_PTR as usize, &mut got_cpu_bytes)
        .unwrap();
    let (got_gpr, got_rip_in_cpu, got_rflags) = read_cpu_state(&got_cpu_bytes);
    assert_eq!(got_gpr, interp_state.cpu.gpr);
    assert_eq!(got_rip_in_cpu, interp_state.cpu.rip);
    assert_eq!(got_rflags, interp_state.cpu.rflags);
}

#[test]
fn tier2_trace_wasm_matches_interpreter_on_partial_binop_flag_updates() {
    // Ensure `Instr::BinOp` obeys the provided `FlagSet` mask and preserves unmasked bits.
    //
    // Use `0xffff.. + 1` which would set CF/ZF/PF/AF in a full ALU update, but request only CF.
    // Initialize other flags to opposite values so incorrect writes are observable.
    let mut trace = TraceIr {
        prologue: vec![],
        body: vec![
            Instr::Const {
                dst: v(0),
                value: 0xffff_ffff_ffff_ffff,
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
                flags: FlagSet::CF,
            },
            Instr::LoadFlag {
                dst: v(3),
                flag: Flag::Cf,
            },
            Instr::Guard {
                cond: Operand::Value(v(3)),
                expected: true,
                exit_rip: 0x2000,
            },
            Instr::StoreReg {
                reg: Gpr::Rax,
                src: Operand::Value(v(2)),
            },
            Instr::StoreReg {
                reg: Gpr::Rbx,
                src: Operand::Value(v(3)),
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
    init_state.cpu.rflags = abi::RFLAGS_RESERVED1
        | RFLAGS_DF
        | (1u64 << Flag::Sf.rflags_bit())
        | (1u64 << Flag::Of.rflags_bit());
    init_state.cpu.gpr[Gpr::Rax.as_u8() as usize] = 0;
    init_state.cpu.gpr[Gpr::Rbx.as_u8() as usize] = 0;

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
    assert_eq!(interp_state.cpu.gpr[Gpr::Rax.as_u8() as usize], 0);
    assert_eq!(interp_state.cpu.gpr[Gpr::Rbx.as_u8() as usize] & 0xff, 1);
    assert_eq!(
        interp_state.cpu.rflags,
        init_state.cpu.rflags | (1u64 << Flag::Cf.rflags_bit())
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

    let mut got_cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    memory
        .read(&store, CPU_PTR as usize, &mut got_cpu_bytes)
        .unwrap();
    let (got_gpr, got_rip_in_cpu, got_rflags) = read_cpu_state(&got_cpu_bytes);
    assert_eq!(got_gpr, interp_state.cpu.gpr);
    assert_eq!(got_rip_in_cpu, interp_state.cpu.rip);
    assert_eq!(got_rflags, interp_state.cpu.rflags);
}

#[test]
fn tier2_trace_wasm_forces_reserved1_rflags_bit_on_flag_updates() {
    // Both the interpreter and Tier-2 WASM codegen are expected to force RFLAGS bit 1 to 1 when
    // flags are updated (x86 invariant). Ensure this happens even if the input CpuState has
    // RFLAGS_RESERVED1 cleared.
    let mut trace = TraceIr {
        prologue: vec![],
        body: vec![
            Instr::Const {
                dst: v(0),
                value: 1,
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
        ],
        kind: TraceKind::Linear,
    };

    let opt = optimize_trace(&mut trace, &OptConfig::default());
    let wasm = Tier2WasmCodegen::new().compile_trace(&trace, &opt.regalloc);
    validate_wasm(&wasm);

    let env = RuntimeEnv::default();

    let mut init_state = T2State::default();
    init_state.cpu.rip = 0x1234;
    init_state.cpu.rflags = RFLAGS_DF; // intentionally omit RFLAGS_RESERVED1
    init_state.cpu.gpr[Gpr::Rax.as_u8() as usize] = 0;

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
    assert_eq!(interp_state.cpu.gpr[Gpr::Rax.as_u8() as usize], 2);
    assert_ne!(
        interp_state.cpu.rflags & abi::RFLAGS_RESERVED1,
        0,
        "interpreter should force reserved bit 1 to 1"
    );
    assert_ne!(
        interp_state.cpu.rflags & RFLAGS_DF,
        0,
        "DF should be preserved"
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

    let mut got_cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    memory
        .read(&store, CPU_PTR as usize, &mut got_cpu_bytes)
        .unwrap();
    let (got_gpr, got_rip_in_cpu, got_rflags) = read_cpu_state(&got_cpu_bytes);
    assert_eq!(got_gpr, interp_state.cpu.gpr);
    assert_eq!(got_rip_in_cpu, interp_state.cpu.rip);
    assert_eq!(got_rflags, interp_state.cpu.rflags);
}

#[test]
fn tier2_trace_wasm_matches_interpreter_on_guard_expected_false_side_exit() {
    // Exercise `Instr::Guard` with `expected: false` and ensure it can side-exit when the
    // condition is true.
    //
    // Also ensure that a preceding `expected: false` guard does *not* side-exit when the
    // condition is false.
    let mut trace = TraceIr {
        prologue: vec![],
        body: vec![
            Instr::Const {
                dst: v(0),
                value: 0,
            },
            Instr::Guard {
                cond: Operand::Value(v(0)),
                expected: false,
                exit_rip: 0x1000,
            },
            Instr::Const {
                dst: v(1),
                value: 0x1111,
            },
            Instr::StoreReg {
                reg: Gpr::Rax,
                src: Operand::Value(v(1)),
            },
            Instr::Const {
                dst: v(2),
                value: 1,
            },
            Instr::Guard {
                cond: Operand::Value(v(2)),
                expected: false,
                exit_rip: 0x2000,
            },
            Instr::Const {
                dst: v(3),
                value: 0x2222,
            },
            Instr::StoreReg {
                reg: Gpr::Rax,
                src: Operand::Value(v(3)),
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
    init_state.cpu.gpr[Gpr::Rax.as_u8() as usize] = 0;

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
    assert_eq!(res.exit, RunExit::SideExit { next_rip: 0x2000 });
    assert_eq!(interp_state.cpu.rip, 0x2000);
    assert_eq!(interp_state.cpu.gpr[Gpr::Rax.as_u8() as usize], 0x1111);

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

    let mut got_cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    memory
        .read(&store, CPU_PTR as usize, &mut got_cpu_bytes)
        .unwrap();
    let (got_gpr, got_rip_in_cpu, got_rflags) = read_cpu_state(&got_cpu_bytes);
    assert_eq!(got_gpr, interp_state.cpu.gpr);
    assert_eq!(got_rip_in_cpu, interp_state.cpu.rip);
    assert_eq!(got_rflags, interp_state.cpu.rflags);
}

#[test]
fn tier2_trace_wasm_matches_interpreter_on_guard_expected_true_side_exit() {
    // Exercise `Instr::Guard` with `expected: true` and ensure it can side-exit when the
    // condition is false.
    let mut trace = TraceIr {
        prologue: vec![],
        body: vec![
            Instr::Const {
                dst: v(0),
                value: 1,
            },
            Instr::Guard {
                cond: Operand::Value(v(0)),
                expected: true,
                exit_rip: 0x1000,
            },
            Instr::Const {
                dst: v(1),
                value: 0x1111,
            },
            Instr::StoreReg {
                reg: Gpr::Rax,
                src: Operand::Value(v(1)),
            },
            Instr::Const {
                dst: v(2),
                value: 0,
            },
            Instr::Guard {
                cond: Operand::Value(v(2)),
                expected: true,
                exit_rip: 0x2000,
            },
            Instr::Const {
                dst: v(3),
                value: 0x2222,
            },
            Instr::StoreReg {
                reg: Gpr::Rax,
                src: Operand::Value(v(3)),
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
    init_state.cpu.gpr[Gpr::Rax.as_u8() as usize] = 0;

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
    assert_eq!(res.exit, RunExit::SideExit { next_rip: 0x2000 });
    assert_eq!(interp_state.cpu.rip, 0x2000);
    assert_eq!(interp_state.cpu.gpr[Gpr::Rax.as_u8() as usize], 0x1111);

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

    let mut got_cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    memory
        .read(&store, CPU_PTR as usize, &mut got_cpu_bytes)
        .unwrap();
    let (got_gpr, got_rip_in_cpu, got_rflags) = read_cpu_state(&got_cpu_bytes);
    assert_eq!(got_gpr, interp_state.cpu.gpr);
    assert_eq!(got_rip_in_cpu, interp_state.cpu.rip);
    assert_eq!(got_rflags, interp_state.cpu.rflags);
}

#[test]
fn tier2_trace_wasm_keeps_exit_reason_none_on_explicit_side_exit() {
    // `Instr::SideExit` should not be treated as a code invalidation exit and therefore must not
    // set `TRACE_EXIT_REASON_CODE_INVALIDATION`.
    let mut trace = TraceIr {
        prologue: vec![],
        body: vec![
            Instr::Const {
                dst: v(0),
                value: 0x1111,
            },
            Instr::StoreReg {
                reg: Gpr::Rax,
                src: Operand::Value(v(0)),
            },
            Instr::SideExit { exit_rip: 0x2000 },
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
    init_state.cpu.gpr[Gpr::Rax.as_u8() as usize] = 0;

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
    assert_eq!(res.exit, RunExit::SideExit { next_rip: 0x2000 });
    assert_eq!(interp_state.cpu.rip, 0x2000);
    assert_eq!(interp_state.cpu.gpr[Gpr::Rax.as_u8() as usize], 0x1111);

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

    let exit_reason = read_u32_from_memory(
        &memory,
        &store,
        CPU_PTR as usize + jit_ctx::TRACE_EXIT_REASON_OFFSET as usize,
    );
    assert_eq!(
        exit_reason,
        jit_ctx::TRACE_EXIT_REASON_NONE,
        "explicit side exits should not set TRACE_EXIT_REASON_CODE_INVALIDATION"
    );

    let mut got_cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    memory
        .read(&store, CPU_PTR as usize, &mut got_cpu_bytes)
        .unwrap();
    let (got_gpr, got_rip_in_cpu, got_rflags) = read_cpu_state(&got_cpu_bytes);
    assert_eq!(got_gpr, interp_state.cpu.gpr);
    assert_eq!(got_rip_in_cpu, interp_state.cpu.rip);
    assert_eq!(got_rflags, interp_state.cpu.rflags);
}

#[test]
fn tier2_trace_wasm_matches_interpreter_on_aux_carry_flag_guard() {
    // Exercise AF (aux carry) computation on addition.
    //
    // 0x0f + 1 = 0x10, which sets AF=1.
    let mut trace = TraceIr {
        prologue: vec![],
        body: vec![
            Instr::Const {
                dst: v(0),
                value: 0x0f,
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
            Instr::LoadFlag {
                dst: v(3),
                flag: Flag::Af,
            },
            Instr::Guard {
                cond: Operand::Value(v(3)),
                expected: true,
                exit_rip: 0x2000,
            },
            Instr::StoreReg {
                reg: Gpr::Rax,
                src: Operand::Value(v(2)),
            },
            Instr::StoreReg {
                reg: Gpr::Rbx,
                src: Operand::Value(v(3)),
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
    init_state.cpu.gpr[Gpr::Rax.as_u8() as usize] = 0;
    init_state.cpu.gpr[Gpr::Rbx.as_u8() as usize] = 0;

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
    assert_eq!(interp_state.cpu.gpr[Gpr::Rax.as_u8() as usize], 0x10);
    assert_eq!(interp_state.cpu.gpr[Gpr::Rbx.as_u8() as usize] & 0xff, 1);

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

    let mut got_cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    memory
        .read(&store, CPU_PTR as usize, &mut got_cpu_bytes)
        .unwrap();
    let (got_gpr, got_rip_in_cpu, got_rflags) = read_cpu_state(&got_cpu_bytes);
    assert_eq!(got_gpr, interp_state.cpu.gpr);
    assert_eq!(got_rip_in_cpu, interp_state.cpu.rip);
    assert_eq!(got_rflags, interp_state.cpu.rflags);
}

#[test]
fn tier2_trace_wasm_matches_interpreter_on_overflow_flag_guard() {
    // Exercise OF (signed overflow) computation on addition.
    //
    // 0x7fff.. + 1 = 0x8000.., which sets OF=1.
    let mut trace = TraceIr {
        prologue: vec![],
        body: vec![
            Instr::Const {
                dst: v(0),
                value: 0x7fff_ffff_ffff_ffff,
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
            Instr::LoadFlag {
                dst: v(3),
                flag: Flag::Of,
            },
            Instr::Guard {
                cond: Operand::Value(v(3)),
                expected: true,
                exit_rip: 0x2000,
            },
            Instr::StoreReg {
                reg: Gpr::Rax,
                src: Operand::Value(v(2)),
            },
            Instr::StoreReg {
                reg: Gpr::Rbx,
                src: Operand::Value(v(3)),
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
    init_state.cpu.gpr[Gpr::Rax.as_u8() as usize] = 0;
    init_state.cpu.gpr[Gpr::Rbx.as_u8() as usize] = 0;

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
        0x8000_0000_0000_0000u64
    );
    assert_eq!(interp_state.cpu.gpr[Gpr::Rbx.as_u8() as usize] & 0xff, 1);

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

    let mut got_cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    memory
        .read(&store, CPU_PTR as usize, &mut got_cpu_bytes)
        .unwrap();
    let (got_gpr, got_rip_in_cpu, got_rflags) = read_cpu_state(&got_cpu_bytes);
    assert_eq!(got_gpr, interp_state.cpu.gpr);
    assert_eq!(got_rip_in_cpu, interp_state.cpu.rip);
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
        ip_mask: u64::MAX,
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
fn tier2_trace_wasm_matches_interpreter_on_loop_side_exit_with_inline_code_version_guards() {
    // Same loop trace as `tier2_trace_wasm_matches_interpreter_on_loop_side_exit`, but compile with
    // `code_version_guard_import = false` so `GuardCodeVersion` reads the version table directly
    // from linear memory (instead of calling a host import).
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
        ip_mask: u64::MAX,
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
    let wasm = Tier2WasmCodegen::new().compile_trace_with_options(
        &trace.ir,
        &opt.regalloc,
        Tier2WasmOptions {
            code_version_guard_import: false,
            ..Default::default()
        },
    );
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

    let (mut store, memory, func) =
        instantiate_trace_without_code_page_version(&wasm, HostEnv::default());
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
    assert_eq!(
        store.data().code_version_calls,
        0,
        "inline code-version guards should not call env.code_page_version"
    );

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
fn tier2_trace_wasm_matches_interpreter_on_addr_mem_ops() {
    // Exercise `Instr::Addr` (including a non-power-of-two scale and negative displacement) feeding
    // into memory operations.
    let mut trace = TraceIr {
        prologue: vec![],
        body: vec![
            Instr::Const {
                dst: v(0),
                value: 0x200,
            },
            Instr::Const {
                dst: v(1),
                value: 0x10,
            },
            Instr::Addr {
                dst: v(2),
                base: Operand::Value(v(0)),
                index: Operand::Value(v(1)),
                scale: 3,
                disp: -5,
            },
            Instr::Const {
                dst: v(3),
                value: 0x1122_3344_5566_7788,
            },
            Instr::StoreMem {
                addr: Operand::Value(v(2)),
                src: Operand::Value(v(3)),
                width: Width::W64,
            },
            Instr::LoadMem {
                dst: v(4),
                addr: Operand::Value(v(2)),
                width: Width::W64,
            },
            Instr::StoreReg {
                reg: Gpr::Rax,
                src: Operand::Value(v(4)),
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
        ip_mask: u64::MAX,
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

#[test]
fn tier2_loop_trace_inline_code_version_guard_invalidates_after_store_bumps_table() {
    // Compile a small Tier-2 loop trace that:
    // - checks page 0's code version each iteration (inline table read; no host import),
    // - performs a guest memory store that bumps page 0's entry in the code-version table, then
    // - repeats.
    //
    // On the 2nd iteration, the code-version guard should detect the bumped entry and trigger a
    // code invalidation exit.
    //
    // Include an iteration-limit guard so the test terminates even if invalidation is broken.
    let mut trace = TraceIr {
        prologue: vec![],
        body: vec![
            // Guard page 0 on each iteration.
            Instr::GuardCodeVersion {
                page: 0,
                expected: 1,
                exit_rip: 0x9999,
            },
            // Write to guest memory page 0. The WASM host helpers bump the code-version table for
            // the affected pages to simulate self-modifying code invalidation.
            Instr::StoreMem {
                addr: Operand::Const(0),
                src: Operand::Const(0xAB),
                width: Width::W8,
            },
            // Increment RAX as a loop counter.
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
                flags: FlagSet::EMPTY,
            },
            Instr::StoreReg {
                reg: Gpr::Rax,
                src: Operand::Value(v(2)),
            },
            // Exit after 3 iterations if invalidation doesn't happen (avoid hanging forever).
            Instr::Const {
                dst: v(3),
                value: 3,
            },
            Instr::BinOp {
                dst: v(4),
                op: BinOp::LtU,
                lhs: Operand::Value(v(2)),
                rhs: Operand::Value(v(3)),
                flags: FlagSet::EMPTY,
            },
            Instr::Guard {
                cond: Operand::Value(v(4)),
                expected: true,
                exit_rip: 0x7777,
            },
        ],
        kind: TraceKind::Loop,
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

    let guest_mem_init = vec![0u8; GUEST_MEM_SIZE];

    let mut init_state = T2State::default();
    init_state.cpu.rip = 0x1111;
    init_state.cpu.rflags = abi::RFLAGS_RESERVED1;
    init_state.cpu.gpr[Gpr::Rax.as_u8() as usize] = 0;

    let env = RuntimeEnv::default();
    env.page_versions.set_version(0, 1);

    let mut interp_state = init_state.clone();
    let mut bus = SimpleBus::new(GUEST_MEM_SIZE);
    bus.load(0, &guest_mem_init);
    let expected = run_trace_with_cached_regs(
        &trace,
        &env,
        &mut bus,
        &mut interp_state,
        10,
        &opt.regalloc.cached,
    );
    assert_eq!(expected.exit, RunExit::Invalidate { next_rip: 0x9999 });
    assert_eq!(env.page_versions.version(0), 2);

    let (mut store, memory, func) =
        instantiate_trace_without_code_page_version(&wasm, HostEnv::default());
    memory.write(&mut store, 0, &guest_mem_init).unwrap();

    let mut cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    write_cpu_state(&mut cpu_bytes, &init_state.cpu);
    memory
        .write(&mut store, CPU_PTR as usize, &cpu_bytes)
        .unwrap();

    let table_ptr = install_code_version_table(&memory, &mut store, &[1]);

    let got_rip = func.call(&mut store, (CPU_PTR, JIT_CTX_PTR)).unwrap() as u64;
    let exit_reason = read_u32_from_memory(
        &memory,
        &store,
        CPU_PTR as usize + jit_ctx::TRACE_EXIT_REASON_OFFSET as usize,
    );
    assert_eq!(
        exit_reason,
        jit_ctx::TRACE_EXIT_REASON_CODE_INVALIDATION,
        "trace should set TRACE_EXIT_REASON_CODE_INVALIDATION"
    );
    assert_eq!(got_rip, 0x9999, "trace should invalidate via guard exit");
    assert_eq!(
        store.data().code_version_calls,
        0,
        "inline code-version guards should not call env.code_page_version"
    );

    // Guest memory.
    let mut got_guest_mem = vec![0u8; GUEST_MEM_SIZE];
    memory.read(&store, 0, &mut got_guest_mem).unwrap();
    assert_eq!(got_guest_mem.as_slice(), bus.mem());

    // CPU state.
    let mut got_cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    memory
        .read(&store, CPU_PTR as usize, &mut got_cpu_bytes)
        .unwrap();
    let (got_gpr, got_rip, got_rflags) = read_cpu_state(&got_cpu_bytes);
    assert_eq!(got_gpr, interp_state.cpu.gpr);
    assert_eq!(got_rip, interp_state.cpu.rip);
    assert_eq!(got_rflags, interp_state.cpu.rflags);

    // The store should bump page 0 once (1 -> 2) before the guard invalidates on the next
    // iteration.
    let bumped_page0 = read_u32_from_memory(&memory, &store, table_ptr as usize);
    assert_eq!(bumped_page0, 2);

    // The store itself should have executed exactly once.
    let mut byte = [0u8; 1];
    memory.read(&store, 0, &mut byte).unwrap();
    assert_eq!(byte[0], 0xAB);
}

#[test]
fn tier2_loop_trace_invalidates_when_storemem_bumps_code_version_interpreter_matches_wasm() {
    // Similar to `tier2_loop_trace_inline_code_version_guard_invalidates_after_store_bumps_table`,
    // but run the Tier-2 interpreter as a reference and use the legacy `env.code_page_version`
    // import path for the WASM codegen.
    //
    // This ensures Tier-2 interpreter semantics for `Instr::StoreMem` (page-version bumping) stay
    // aligned with the WASM harness behaviour.
    let entry_rip = 0x1000u64;
    let side_exit_rip = 0x2000u64;
    let store_addr = 0x100u64;
    let page = store_addr >> aero_jit_x86::PAGE_SHIFT;
    let initial_version: u32 = 5;

    let trace = TraceIr {
        prologue: vec![
            Instr::Const {
                dst: v(0),
                value: store_addr,
            },
            Instr::Const {
                dst: v(1),
                value: 0xAA,
            },
            // Loop termination threshold for the regression "no invalidation" path.
            Instr::Const {
                dst: v(2),
                value: 3,
            },
        ],
        body: vec![
            Instr::GuardCodeVersion {
                page,
                expected: initial_version,
                exit_rip: entry_rip,
            },
            // Increment a counter in RAX.
            Instr::LoadReg {
                dst: v(3),
                reg: Gpr::Rax,
            },
            Instr::Const {
                dst: v(4),
                value: 1,
            },
            Instr::BinOp {
                dst: v(5),
                op: BinOp::Add,
                lhs: Operand::Value(v(3)),
                rhs: Operand::Value(v(4)),
                flags: FlagSet::EMPTY,
            },
            Instr::StoreReg {
                reg: Gpr::Rax,
                src: Operand::Value(v(5)),
            },
            // Bump the guarded code page.
            Instr::StoreMem {
                addr: Operand::Value(v(0)),
                src: Operand::Value(v(1)),
                width: Width::W8,
            },
            // If counter < 3, keep looping. Otherwise, side-exit so we don't spin forever in
            // regressions where invalidation doesn't happen.
            Instr::BinOp {
                dst: v(6),
                op: BinOp::LtU,
                lhs: Operand::Value(v(5)),
                rhs: Operand::Value(v(2)),
                flags: FlagSet::EMPTY,
            },
            Instr::Guard {
                cond: Operand::Value(v(6)),
                expected: true,
                exit_rip: side_exit_rip,
            },
        ],
        kind: TraceKind::Loop,
    };

    // ---- Tier-2 interpreter (reference) ---------------------------------------------------------
    let env = RuntimeEnv::default();
    env.page_versions.set_version(page, initial_version);

    let mut init_state = T2State::default();
    init_state.cpu.gpr[Gpr::Rax.as_u8() as usize] = 0;
    init_state.cpu.rip = entry_rip;
    init_state.cpu.rflags = abi::RFLAGS_RESERVED1;

    let mut interp_state = init_state.clone();
    let mut bus = SimpleBus::new(GUEST_MEM_SIZE);
    let expected =
        aero_jit_x86::tier2::interp::run_trace(&trace, &env, &mut bus, &mut interp_state, 5);
    assert_eq!(
        expected.exit,
        RunExit::Invalidate { next_rip: entry_rip },
        "loop should invalidate on the second iteration after StoreMem bumps the guarded page version"
    );
    assert_eq!(
        env.page_versions.version(page),
        initial_version.wrapping_add(1),
        "StoreMem should bump the guarded page version in the interpreter"
    );

    // ---- Optimize + compile to WASM ------------------------------------------------------------
    let mut optimized = trace.clone();
    let opt = optimize_trace(&mut optimized, &OptConfig::default());
    let wasm = Tier2WasmCodegen::new().compile_trace(&optimized, &opt.regalloc);
    validate_wasm(&wasm);

    // ---- Execute the WASM trace via wasmi ------------------------------------------------------
    let (mut store, memory, func) = instantiate_trace(&wasm, HostEnv::default());
    let guest_mem_init = vec![0u8; GUEST_MEM_SIZE];
    memory.write(&mut store, 0, &guest_mem_init).unwrap();

    let mut cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    write_cpu_state(&mut cpu_bytes, &init_state.cpu);
    memory
        .write(&mut store, CPU_PTR as usize, &cpu_bytes)
        .unwrap();

    let table_ptr = install_code_version_table(&memory, &mut store, &[initial_version]);

    let got_next_rip = func.call(&mut store, (CPU_PTR, JIT_CTX_PTR)).unwrap() as u64;
    assert_eq!(got_next_rip, entry_rip);

    let exit_reason = read_u32_from_memory(
        &memory,
        &store,
        CPU_PTR as usize + jit_ctx::TRACE_EXIT_REASON_OFFSET as usize,
    );
    assert_eq!(exit_reason, jit_ctx::TRACE_EXIT_REASON_CODE_INVALIDATION);

    let bumped = read_u32_from_memory(&memory, &store, table_ptr as usize);
    assert_eq!(
        bumped,
        initial_version.wrapping_add(1),
        "StoreMem should bump the guarded page version in the WASM harness code-version table"
    );

    // Guest memory.
    let mut got_guest_mem = vec![0u8; GUEST_MEM_SIZE];
    memory.read(&store, 0, &mut got_guest_mem).unwrap();
    assert_eq!(got_guest_mem.as_slice(), bus.mem());

    // CPU state.
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
fn tier2_loop_trace_code_version_wraparound_on_storemem_interpreter_matches_wasm() {
    // Same as `tier2_loop_trace_invalidates_when_storemem_bumps_code_version_interpreter_matches_wasm`,
    // but start from `u32::MAX` so we test wraparound semantics (`0xffff_ffff + 1 == 0`).
    let entry_rip = 0x1000u64;
    let side_exit_rip = 0x2000u64;
    let store_addr = 0x100u64;
    let page = store_addr >> aero_jit_x86::PAGE_SHIFT;
    let initial_version: u32 = u32::MAX;

    let trace = TraceIr {
        prologue: vec![
            Instr::Const {
                dst: v(0),
                value: store_addr,
            },
            Instr::Const {
                dst: v(1),
                value: 0xAA,
            },
            // Loop termination threshold for the regression "no invalidation" path.
            Instr::Const {
                dst: v(2),
                value: 3,
            },
        ],
        body: vec![
            Instr::GuardCodeVersion {
                page,
                expected: initial_version,
                exit_rip: entry_rip,
            },
            // Increment a counter in RAX.
            Instr::LoadReg {
                dst: v(3),
                reg: Gpr::Rax,
            },
            Instr::Const {
                dst: v(4),
                value: 1,
            },
            Instr::BinOp {
                dst: v(5),
                op: BinOp::Add,
                lhs: Operand::Value(v(3)),
                rhs: Operand::Value(v(4)),
                flags: FlagSet::EMPTY,
            },
            Instr::StoreReg {
                reg: Gpr::Rax,
                src: Operand::Value(v(5)),
            },
            // Bump the guarded code page (wraparound to 0).
            Instr::StoreMem {
                addr: Operand::Value(v(0)),
                src: Operand::Value(v(1)),
                width: Width::W8,
            },
            // If counter < 3, keep looping. Otherwise, side-exit so we don't spin forever in
            // regressions where invalidation doesn't happen.
            Instr::BinOp {
                dst: v(6),
                op: BinOp::LtU,
                lhs: Operand::Value(v(5)),
                rhs: Operand::Value(v(2)),
                flags: FlagSet::EMPTY,
            },
            Instr::Guard {
                cond: Operand::Value(v(6)),
                expected: true,
                exit_rip: side_exit_rip,
            },
        ],
        kind: TraceKind::Loop,
    };

    // ---- Tier-2 interpreter (reference) ---------------------------------------------------------
    let env = RuntimeEnv::default();
    env.page_versions.set_version(page, initial_version);

    let mut init_state = T2State::default();
    init_state.cpu.gpr[Gpr::Rax.as_u8() as usize] = 0;
    init_state.cpu.rip = entry_rip;
    init_state.cpu.rflags = abi::RFLAGS_RESERVED1;

    let mut interp_state = init_state.clone();
    let mut bus = SimpleBus::new(GUEST_MEM_SIZE);
    let expected =
        aero_jit_x86::tier2::interp::run_trace(&trace, &env, &mut bus, &mut interp_state, 5);
    assert_eq!(
        expected.exit,
        RunExit::Invalidate {
            next_rip: entry_rip
        },
        "loop should invalidate after StoreMem wraps the guarded page version"
    );
    assert_eq!(
        env.page_versions.version(page),
        0,
        "StoreMem should bump the guarded page version with wrapping arithmetic"
    );

    // ---- Optimize + compile to WASM ------------------------------------------------------------
    let mut optimized = trace.clone();
    let opt = optimize_trace(&mut optimized, &OptConfig::default());
    let wasm = Tier2WasmCodegen::new().compile_trace(&optimized, &opt.regalloc);
    validate_wasm(&wasm);

    // ---- Execute the WASM trace via wasmi ------------------------------------------------------
    let (mut store, memory, func) = instantiate_trace(&wasm, HostEnv::default());
    let guest_mem_init = vec![0u8; GUEST_MEM_SIZE];
    memory.write(&mut store, 0, &guest_mem_init).unwrap();

    let mut cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    write_cpu_state(&mut cpu_bytes, &init_state.cpu);
    memory
        .write(&mut store, CPU_PTR as usize, &cpu_bytes)
        .unwrap();

    let table_ptr = install_code_version_table(&memory, &mut store, &[initial_version]);

    let got_next_rip = func.call(&mut store, (CPU_PTR, JIT_CTX_PTR)).unwrap() as u64;
    assert_eq!(got_next_rip, entry_rip);

    let exit_reason = read_u32_from_memory(
        &memory,
        &store,
        CPU_PTR as usize + jit_ctx::TRACE_EXIT_REASON_OFFSET as usize,
    );
    assert_eq!(exit_reason, jit_ctx::TRACE_EXIT_REASON_CODE_INVALIDATION);

    let bumped = read_u32_from_memory(&memory, &store, table_ptr as usize);
    assert_eq!(
        bumped, 0,
        "StoreMem should bump the guarded page version with wrapping arithmetic in the WASM harness code-version table"
    );

    // Guest memory.
    let mut got_guest_mem = vec![0u8; GUEST_MEM_SIZE];
    memory.read(&store, 0, &mut got_guest_mem).unwrap();
    assert_eq!(got_guest_mem.as_slice(), bus.mem());

    // CPU state.
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
fn tier2_loop_trace_cross_page_store_bumps_both_pages_interpreter_matches_wasm() {
    // Cross-page StoreMem should bump *all* spanned 4KiB pages.
    //
    // Write an 8-byte value starting at the last byte of page 0 so the store spans pages 0 and 1.
    // Guard both pages each iteration and ensure the trace invalidates on the next iteration due
    // to the bumped versions.
    let entry_rip = 0x1000u64;
    let side_exit_rip = 0x2000u64;
    let store_addr = aero_jit_x86::PAGE_SIZE - 1; // spans into page 1 for an 8-byte store
    let page0 = store_addr >> aero_jit_x86::PAGE_SHIFT;
    let page1 = (store_addr + (Width::W64.bytes() as u64) - 1) >> aero_jit_x86::PAGE_SHIFT;
    assert_eq!(page0, 0);
    assert_eq!(page1, 1);

    let initial_page0: u32 = 5;
    let initial_page1: u32 = 7;

    let trace = TraceIr {
        prologue: vec![
            Instr::Const {
                dst: v(0),
                value: store_addr,
            },
            Instr::Const {
                dst: v(1),
                value: 0x1122_3344_5566_7788,
            },
            // Loop termination threshold for the regression "no invalidation" path.
            Instr::Const {
                dst: v(2),
                value: 3,
            },
        ],
        body: vec![
            Instr::GuardCodeVersion {
                page: page0,
                expected: initial_page0,
                exit_rip: entry_rip,
            },
            Instr::GuardCodeVersion {
                page: page1,
                expected: initial_page1,
                exit_rip: entry_rip,
            },
            // Increment a counter in RAX.
            Instr::LoadReg {
                dst: v(3),
                reg: Gpr::Rax,
            },
            Instr::Const {
                dst: v(4),
                value: 1,
            },
            Instr::BinOp {
                dst: v(5),
                op: BinOp::Add,
                lhs: Operand::Value(v(3)),
                rhs: Operand::Value(v(4)),
                flags: FlagSet::EMPTY,
            },
            Instr::StoreReg {
                reg: Gpr::Rax,
                src: Operand::Value(v(5)),
            },
            // Cross-page store (pages 0 and 1).
            Instr::StoreMem {
                addr: Operand::Value(v(0)),
                src: Operand::Value(v(1)),
                width: Width::W64,
            },
            // If counter < 3, keep looping. Otherwise, side-exit so we don't spin forever in
            // regressions where invalidation doesn't happen.
            Instr::BinOp {
                dst: v(6),
                op: BinOp::LtU,
                lhs: Operand::Value(v(5)),
                rhs: Operand::Value(v(2)),
                flags: FlagSet::EMPTY,
            },
            Instr::Guard {
                cond: Operand::Value(v(6)),
                expected: true,
                exit_rip: side_exit_rip,
            },
        ],
        kind: TraceKind::Loop,
    };

    // ---- Tier-2 interpreter (reference) ---------------------------------------------------------
    let env = RuntimeEnv::default();
    env.page_versions.set_version(page0, initial_page0);
    env.page_versions.set_version(page1, initial_page1);

    let mut init_state = T2State::default();
    init_state.cpu.gpr[Gpr::Rax.as_u8() as usize] = 0;
    init_state.cpu.rip = entry_rip;
    init_state.cpu.rflags = abi::RFLAGS_RESERVED1;

    let mut interp_state = init_state.clone();
    let mut bus = SimpleBus::new(GUEST_MEM_SIZE);
    let expected =
        aero_jit_x86::tier2::interp::run_trace(&trace, &env, &mut bus, &mut interp_state, 5);
    assert_eq!(
        expected.exit,
        RunExit::Invalidate {
            next_rip: entry_rip
        }
    );
    assert_eq!(
        env.page_versions.version(page0),
        initial_page0.wrapping_add(1)
    );
    assert_eq!(
        env.page_versions.version(page1),
        initial_page1.wrapping_add(1)
    );

    // ---- Optimize + compile to WASM ------------------------------------------------------------
    let mut optimized = trace.clone();
    let opt = optimize_trace(&mut optimized, &OptConfig::default());
    let wasm = Tier2WasmCodegen::new().compile_trace(&optimized, &opt.regalloc);
    validate_wasm(&wasm);

    // ---- Execute the WASM trace via wasmi ------------------------------------------------------
    let (mut store, memory, func) = instantiate_trace(&wasm, HostEnv::default());
    let guest_mem_init = vec![0u8; GUEST_MEM_SIZE];
    memory.write(&mut store, 0, &guest_mem_init).unwrap();

    let mut cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    write_cpu_state(&mut cpu_bytes, &init_state.cpu);
    memory
        .write(&mut store, CPU_PTR as usize, &cpu_bytes)
        .unwrap();

    let table_ptr =
        install_code_version_table(&memory, &mut store, &[initial_page0, initial_page1]);

    let got_next_rip = func.call(&mut store, (CPU_PTR, JIT_CTX_PTR)).unwrap() as u64;
    assert_eq!(got_next_rip, entry_rip);

    let exit_reason = read_u32_from_memory(
        &memory,
        &store,
        CPU_PTR as usize + jit_ctx::TRACE_EXIT_REASON_OFFSET as usize,
    );
    assert_eq!(exit_reason, jit_ctx::TRACE_EXIT_REASON_CODE_INVALIDATION);

    let bumped0 = read_u32_from_memory(&memory, &store, table_ptr as usize);
    let bumped1 = read_u32_from_memory(&memory, &store, table_ptr as usize + 4);
    assert_eq!(bumped0, initial_page0.wrapping_add(1));
    assert_eq!(bumped1, initial_page1.wrapping_add(1));

    // Guest memory.
    let mut got_guest_mem = vec![0u8; GUEST_MEM_SIZE];
    memory.read(&store, 0, &mut got_guest_mem).unwrap();
    assert_eq!(got_guest_mem.as_slice(), bus.mem());

    // CPU state.
    let mut got_cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    memory
        .read(&store, CPU_PTR as usize, &mut got_cpu_bytes)
        .unwrap();
    let (got_gpr, got_rip, got_rflags) = read_cpu_state(&got_cpu_bytes);
    assert_eq!(got_gpr, interp_state.cpu.gpr);
    assert_eq!(got_rip, interp_state.cpu.rip);
    assert_eq!(got_rflags, interp_state.cpu.rflags);
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
                    body.push(Instr::Const { dst, value });
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
                    let bytes = width.bytes();
                    let addr = if !safe_addrs.is_empty() && rng.gen_bool(0.6) {
                        Operand::Value(*safe_addrs.choose(rng).unwrap())
                    } else {
                        Operand::Const(rng.gen_range(0..(GUEST_MEM_SIZE - bytes)) as u64)
                    };
                    body.push(Instr::LoadMem { dst, addr, width });
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
                    let bytes = width.bytes();
                    let addr = if !safe_addrs.is_empty() && rng.gen_bool(0.6) {
                        Operand::Value(*safe_addrs.choose(rng).unwrap())
                    } else {
                        Operand::Const(rng.gen_range(0..(GUEST_MEM_SIZE - bytes)) as u64)
                    };
                    let src = gen_operand(rng, &values);
                    body.push(Instr::StoreMem { addr, src, width });
                }
                85..=89 => {
                    let dst = v(next_value);
                    next_value += 1;
                    let flag = *[Flag::Cf, Flag::Pf, Flag::Af, Flag::Zf, Flag::Sf, Flag::Of]
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

    #[test]
    fn tier2_trace_wasm_matches_interpreter_on_random_traces_without_code_version_guard_import() {
        // Use a different seed from the legacy-import random test so we cover a wider variety of
        // traces overall while still remaining deterministic.
        let mut rng = ChaCha8Rng::seed_from_u64(0x5EED_C0DE);
        for i in 0..50 {
            let env = RuntimeEnv::default();
            let mut code_versions: Vec<u32> = (0..8).map(|_| rng.gen()).collect();
            if code_versions.iter().all(|v| *v == 0) {
                code_versions[0] = 1;
            }
            for (page, version) in code_versions.iter().copied().enumerate() {
                env.page_versions.set_version(page as u64, version);
            }

            let instr_count = rng.gen_range(20..=50);
            let mut trace = gen_random_trace(&mut rng, instr_count, &code_versions);

            // Ensure at least one code-version guard so we actually exercise the inline table-read
            // path (otherwise the option would be a no-op for this trace).
            if !trace
                .prologue
                .iter()
                .any(|i| matches!(i, Instr::GuardCodeVersion { .. }))
            {
                let table_len = code_versions.len() as u64;
                let page = rng.gen_range(0..table_len);
                let expected = if rng.gen_bool(0.8) {
                    code_versions[page as usize]
                } else {
                    code_versions[page as usize].wrapping_add(1)
                };
                trace.prologue.insert(
                    0,
                    Instr::GuardCodeVersion {
                        page,
                        expected,
                        exit_rip: 0x3000u64 + (rng.gen::<u16>() as u64),
                    },
                );
            }

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
            let wasm = Tier2WasmCodegen::new().compile_trace_with_options(
                &optimized,
                &opt.regalloc,
                Tier2WasmOptions {
                    code_version_guard_import: false,
                    ..Default::default()
                },
            );
            validate_wasm(&wasm);

            // ---- Execute the WASM trace via wasmi ----------------------------------------------
            let (mut store, memory, func) =
                instantiate_trace_without_code_page_version(&wasm, HostEnv::default());
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

            let mut got_guest_mem = vec![0u8; GUEST_MEM_SIZE];
            memory.read(&store, 0, &mut got_guest_mem).unwrap();
            assert_eq!(
                got_guest_mem.as_slice(),
                bus.mem(),
                "guest memory mismatch on iteration {i}\ntrace: {trace:?}\noptimized: {optimized:?}"
            );

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
