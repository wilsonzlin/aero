use std::collections::HashMap;

use aero_cpu::SimpleBus;
use aero_cpu_core::state as core_state;
use aero_types::{Flag, Gpr, Width};

use aero_jit::abi::{CPU_GPR_OFF, CPU_RFLAGS_OFF, CPU_RIP_OFF};
use aero_jit::opt::{optimize_trace, OptConfig};
use aero_jit::profile::{ProfileData, TraceConfig};
use aero_jit::t2_exec::{run_trace_with_cached_regs, RunExit, RuntimeEnv, T2State};
use aero_jit::t2_ir::{
    BinOp, Block, BlockId, FlagMask, Function, Instr, Operand, Terminator, TraceIr, TraceKind,
    ValueId,
};
use aero_jit::trace::TraceBuilder;
use aero_jit::wasm::tier2::{Tier2WasmCodegen, EXPORT_TRACE_FN, IMPORT_CODE_PAGE_VERSION};
use aero_jit::wasm::{
    IMPORT_MEM_READ_U16, IMPORT_MEM_READ_U32, IMPORT_MEM_READ_U64, IMPORT_MEM_READ_U8,
    IMPORT_MEM_WRITE_U16, IMPORT_MEM_WRITE_U32, IMPORT_MEM_WRITE_U64, IMPORT_MEM_WRITE_U8,
    IMPORT_MEMORY, IMPORT_MODULE,
};

use wasmi::{Caller, Engine, Func, Linker, Memory, MemoryType, Module, Store, TypedFunc};
use wasmparser::Validator;

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

    let memory = Memory::new(&mut store, MemoryType::new(1, None)).unwrap();
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

fn v(idx: u32) -> ValueId {
    ValueId(idx)
}

fn all_gprs() -> [Gpr; 16] {
    [
        Gpr::Rax,
        Gpr::Rcx,
        Gpr::Rdx,
        Gpr::Rbx,
        Gpr::Rsp,
        Gpr::Rbp,
        Gpr::Rsi,
        Gpr::Rdi,
        Gpr::R8,
        Gpr::R9,
        Gpr::R10,
        Gpr::R11,
        Gpr::R12,
        Gpr::R13,
        Gpr::R14,
        Gpr::R15,
    ]
}

fn write_u64(mem: &mut [u8], off: u32, value: u64) {
    let off = off as usize;
    mem[off..off + 8].copy_from_slice(&value.to_le_bytes());
}

fn read_u64(mem: &[u8], off: u32) -> u64 {
    let off = off as usize;
    u64::from_le_bytes(mem[off..off + 8].try_into().unwrap())
}

fn write_cpu_state(mem: &mut [u8], cpu: &aero_jit::CpuState, rflags: u64) {
    for reg in all_gprs() {
        write_u64(mem, CPU_GPR_OFF[reg.as_u8() as usize], cpu.get_gpr(reg));
    }
    write_u64(mem, CPU_RIP_OFF, cpu.rip);
    write_u64(mem, CPU_RFLAGS_OFF, rflags);
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
                        flags: FlagMask::ALL,
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
                        flags: FlagMask::EMPTY,
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
    init_state.cpu.set_gpr(Gpr::Rax, 0);
    init_state.cpu.rip = 0;
    let mut initial_rflags = core_state::RFLAGS_RESERVED1 | core_state::RFLAGS_DF;
    for flag in [Flag::Cf, Flag::Pf, Flag::Af, Flag::Zf, Flag::Sf, Flag::Of] {
        initial_rflags |= 1u64 << flag.rflags_bit();
    }
    init_state.rflags = initial_rflags;

    let mut env = RuntimeEnv::default();
    env.code_page_versions.insert(0, 7);

    let mut interp_state = init_state.clone();
    let mut bus = SimpleBus::new(65536);
    let expected = run_trace_with_cached_regs(
        &trace.ir,
        &env,
        &mut bus,
        &mut interp_state,
        1_000_000,
        &opt.regalloc.cached,
    );
    assert_eq!(expected.exit, RunExit::SideExit { next_rip: 100 });

    let mut mem = vec![0u8; 65536];
    write_cpu_state(&mut mem, &init_state.cpu, init_state.rflags);

    let mut code_versions = HashMap::new();
    code_versions.insert(0, 7);
    let (mut store, memory, func) = instantiate_trace(&wasm, code_versions);
    memory.write(&mut store, 0, &mem).unwrap();

    let got_rip = func.call(&mut store, 0).unwrap() as u64;
    assert_eq!(got_rip, 100);

    let mut got_mem = vec![0u8; mem.len()];
    memory.read(&store, 0, &mut got_mem).unwrap();

    let got_rflags = read_u64(&got_mem, CPU_RFLAGS_OFF);
    assert_ne!(got_rflags & core_state::RFLAGS_RESERVED1, 0);
    assert_eq!(
        got_rflags & core_state::RFLAGS_DF,
        initial_rflags & core_state::RFLAGS_DF,
        "unrelated RFLAGS bits should be preserved"
    );
    assert_eq!(got_rflags, interp_state.rflags);

    for reg in all_gprs() {
        assert_eq!(
            read_u64(&got_mem, CPU_GPR_OFF[reg.as_u8() as usize]),
            interp_state.cpu.get_gpr(reg),
            "{reg:?} mismatch"
        );
    }
    assert_eq!(read_u64(&got_mem, CPU_RIP_OFF), interp_state.cpu.rip);
}

#[test]
fn tier2_trace_wasm_memory_ops_match_interpreter() {
    let addr = 0x2000u64;
    let initial = 0x1122_3344_5566_7788u64;

    let mut trace = TraceIr {
        prologue: vec![],
        body: vec![
            Instr::Const { dst: v(0), value: addr },
            Instr::LoadMem {
                dst: v(1),
                addr: Operand::Value(v(0)),
                width: Width::W64,
            },
            Instr::StoreReg {
                reg: Gpr::Rax,
                src: Operand::Value(v(1)),
            },
            Instr::Const { dst: v(2), value: 1 },
            Instr::BinOp {
                dst: v(3),
                op: BinOp::Add,
                lhs: Operand::Value(v(1)),
                rhs: Operand::Value(v(2)),
                flags: FlagMask::ALL,
            },
            Instr::StoreMem {
                addr: Operand::Value(v(0)),
                src: Operand::Value(v(3)),
                width: Width::W64,
            },
            Instr::SideExit { exit_rip: 0x5000 },
        ],
        kind: TraceKind::Linear,
    };

    let opt = optimize_trace(&mut trace, &OptConfig::default());
    let wasm = Tier2WasmCodegen::new().compile_trace(&trace, &opt.regalloc);
    validate_wasm(&wasm);

    let mut init_state = T2State::default();
    init_state.cpu.rip = 0x1234;
    init_state.rflags = core_state::RFLAGS_RESERVED1;

    let env = RuntimeEnv::default();
    let mut bus = SimpleBus::new(65536);
    bus.load(addr, &initial.to_le_bytes());

    let mut interp_state = init_state.clone();
    let expected = run_trace_with_cached_regs(
        &trace,
        &env,
        &mut bus,
        &mut interp_state,
        1,
        &opt.regalloc.cached,
    );
    assert_eq!(expected.exit, RunExit::SideExit { next_rip: 0x5000 });

    let expected_val = u64::from_le_bytes(
        bus.mem()[addr as usize..addr as usize + 8]
            .try_into()
            .unwrap(),
    );

    let mut mem = vec![0u8; 65536];
    mem[addr as usize..addr as usize + 8].copy_from_slice(&initial.to_le_bytes());
    write_cpu_state(&mut mem, &init_state.cpu, init_state.rflags);

    let (mut store, memory, func) = instantiate_trace(&wasm, HashMap::new());
    memory.write(&mut store, 0, &mem).unwrap();

    let got_rip = func.call(&mut store, 0).unwrap() as u64;
    assert_eq!(got_rip, 0x5000);

    let mut got_mem = vec![0u8; mem.len()];
    memory.read(&store, 0, &mut got_mem).unwrap();

    assert_eq!(
        read_u64(&got_mem, CPU_GPR_OFF[Gpr::Rax.as_u8() as usize]),
        interp_state.cpu.get_gpr(Gpr::Rax)
    );
    assert_eq!(read_u64(&got_mem, CPU_RIP_OFF), interp_state.cpu.rip);
    assert_eq!(read_u64(&got_mem, CPU_RFLAGS_OFF), interp_state.rflags);

    let got_val = read_u64(&got_mem, addr as u32);
    assert_eq!(got_val, expected_val);
}
