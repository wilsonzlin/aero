use std::collections::HashMap;

use aero_cpu_core::state::RFLAGS_DF;
use aero_types::{Flag, FlagSet, Gpr, Width};
mod tier1_common;

use tier1_common::SimpleBus;

use aero_jit::abi;
use aero_jit::profile::{ProfileData, TraceConfig};
use aero_jit::tier2::exec::{run_trace_with_cached_regs, RunExit, RuntimeEnv, T2State};
use aero_jit::tier2::ir::{
    BinOp, Block, BlockId, Function, Instr, Operand, Terminator, TraceIr, TraceKind, ValueId,
};
use aero_jit::tier2::opt::{optimize_trace, OptConfig};
use aero_jit::tier2::trace::TraceBuilder;
use aero_jit::tier2::wasm::{Tier2WasmCodegen, EXPORT_TRACE_FN, IMPORT_CODE_PAGE_VERSION};
use aero_jit::wasm::{
    IMPORT_MEMORY, IMPORT_MEM_READ_U16, IMPORT_MEM_READ_U32, IMPORT_MEM_READ_U64,
    IMPORT_MEM_READ_U8, IMPORT_MEM_WRITE_U16, IMPORT_MEM_WRITE_U32, IMPORT_MEM_WRITE_U64,
    IMPORT_MEM_WRITE_U8, IMPORT_MODULE,
};

use wasmi::{Caller, Engine, Func, Linker, Memory, MemoryType, Module, Store, TypedFunc};
use wasmparser::Validator;

const CPU_PTR: i32 = 0x1_0000;
const GUEST_MEM_SIZE: usize = 0x1_0000; // 1 page

fn validate_wasm(bytes: &[u8]) {
    let mut validator = Validator::new();
    validator.validate_all(bytes).unwrap();
}

#[derive(Clone, Debug, Default)]
struct HostEnv {
    code_versions: HashMap<u64, u64>,
}

fn instantiate_trace(
    bytes: &[u8],
    code_versions: HashMap<u64, u64>,
) -> (Store<HostEnv>, Memory, TypedFunc<i32, i64>) {
    let engine = Engine::default();
    let module = Module::new(&engine, bytes).unwrap();

    let mut store = Store::new(&engine, HostEnv { code_versions });
    let mut linker = Linker::new(&engine);

    // Two pages: guest memory in page 0, CpuState at CPU_PTR in page 1.
    let memory = Memory::new(&mut store, MemoryType::new(2, None)).unwrap();
    linker
        .define(IMPORT_MODULE, IMPORT_MEMORY, memory.clone())
        .unwrap();

    define_mem_helpers(&mut store, &mut linker, memory.clone());

    linker
        .define(
            IMPORT_MODULE,
            IMPORT_CODE_PAGE_VERSION,
            Func::wrap(
                &mut store,
                |caller: Caller<'_, HostEnv>, page: i64| -> i64 {
                    let page = page as u64;
                    caller.data().code_versions.get(&page).copied().unwrap_or(0) as i64
                },
            ),
        )
        .unwrap();

    let instance = linker.instantiate_and_start(&mut store, &module).unwrap();
    let trace = instance
        .get_typed_func::<i32, i64>(&store, EXPORT_TRACE_FN)
        .unwrap();
    (store, memory, trace)
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

    let mem = memory.clone();
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

    let mem = memory.clone();
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

    let mem = memory.clone();
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

    let mem = memory.clone();
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

    let mem = memory.clone();
    linker
        .define(
            IMPORT_MODULE,
            IMPORT_MEM_WRITE_U8,
            Func::wrap(
                &mut *store,
                move |mut caller: Caller<'_, HostEnv>, _cpu_ptr: i32, addr: i64, value: i32| {
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
                move |mut caller: Caller<'_, HostEnv>, _cpu_ptr: i32, addr: i64, value: i32| {
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
                move |mut caller: Caller<'_, HostEnv>, _cpu_ptr: i32, addr: i64, value: i32| {
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
                move |mut caller: Caller<'_, HostEnv>, _cpu_ptr: i32, addr: i64, value: i64| {
                    write::<8>(&mut caller, &mem, addr as usize, value as u64);
                },
            ),
        )
        .unwrap();
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
    for i in 0..16 {
        let off = abi::CPU_GPR_OFF[i] as usize;
        bytes[off..off + 8].copy_from_slice(&cpu.gpr[i].to_le_bytes());
    }
    bytes[abi::CPU_RIP_OFF as usize..abi::CPU_RIP_OFF as usize + 8]
        .copy_from_slice(&cpu.rip.to_le_bytes());
    bytes[abi::CPU_RFLAGS_OFF as usize..abi::CPU_RFLAGS_OFF as usize + 8]
        .copy_from_slice(&cpu.rflags.to_le_bytes());
}

fn read_cpu_state(bytes: &[u8]) -> ([u64; 16], u64, u64) {
    let mut gpr = [0u64; 16];
    for i in 0..16 {
        gpr[i] = read_u64_le(bytes, abi::CPU_GPR_OFF[i] as usize);
    }
    let rip = read_u64_le(bytes, abi::CPU_RIP_OFF as usize);
    let rflags = read_u64_le(bytes, abi::CPU_RFLAGS_OFF as usize);
    (gpr, rip, rflags)
}

fn v(idx: u32) -> ValueId {
    ValueId(idx)
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
    profile.code_page_versions.insert(0, 7);

    let builder = TraceBuilder::new(
        &func,
        &profile,
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
    env.code_page_versions.insert(0, 7);

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

    let mut code_versions = HashMap::new();
    code_versions.insert(0, 7);
    let (mut store, memory, func) = instantiate_trace(&wasm, code_versions);
    let guest_mem_init = vec![0u8; GUEST_MEM_SIZE];
    memory.write(&mut store, 0, &guest_mem_init).unwrap();

    let mut cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    write_cpu_state(&mut cpu_bytes, &init_state.cpu);
    memory
        .write(&mut store, CPU_PTR as usize, &cpu_bytes)
        .unwrap();

    let got_rip = func.call(&mut store, CPU_PTR).unwrap() as u64;
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

    let (mut store, memory, func) = instantiate_trace(&wasm, HashMap::new());

    let guest_mem_init = vec![0u8; GUEST_MEM_SIZE];
    memory.write(&mut store, 0, &guest_mem_init).unwrap();

    let mut cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
    write_cpu_state(&mut cpu_bytes, &init_state.cpu);
    memory
        .write(&mut store, CPU_PTR as usize, &cpu_bytes)
        .unwrap();

    let got_rip = func.call(&mut store, CPU_PTR).unwrap() as u64;
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
