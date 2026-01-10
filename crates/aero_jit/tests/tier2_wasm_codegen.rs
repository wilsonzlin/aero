use std::collections::HashMap;

use aero_jit::opt::{optimize_trace, OptConfig};
use aero_jit::profile::{ProfileData, TraceConfig};
use aero_jit::t2_exec::{run_trace_with_cached_regs, Flags, RunExit, RuntimeEnv, T2State};
use aero_jit::t2_ir::{
    BinOp, Block, BlockId, FlagMask, Function, Instr, Operand, Terminator, TraceKind, ValueId,
};
use aero_jit::trace::TraceBuilder;
use aero_jit::wasm::tier2::{
    Tier2WasmCodegen, EXPORT_TRACE_FN, FLAGS_MASK_OFFSET, IMPORT_CODE_PAGE_VERSION,
};
use aero_jit::wasm::{IMPORT_MEMORY, IMPORT_MODULE};
use aero_jit::{CpuState, Reg};

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

fn flags_from_mask(mask: u32) -> Flags {
    Flags {
        zf: (mask & (1 << 0)) != 0,
        sf: (mask & (1 << 1)) != 0,
        cf: (mask & (1 << 2)) != 0,
        of: (mask & (1 << 3)) != 0,
    }
}

fn flags_to_mask(flags: Flags) -> u32 {
    (flags.zf as u32) << 0
        | (flags.sf as u32) << 1
        | (flags.cf as u32) << 2
        | (flags.of as u32) << 3
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
                        reg: Reg::Rax,
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
                        reg: Reg::Rax,
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
    init_state.cpu.set_reg(Reg::Rax, 0);
    init_state.cpu.rip = 0;
    init_state.flags = Flags {
        zf: true,
        sf: true,
        cf: true,
        of: true,
    };

    let mut env = RuntimeEnv::default();
    env.code_page_versions.insert(0, 7);

    let mut interp_state = init_state.clone();
    let expected = run_trace_with_cached_regs(
        &trace.ir,
        &env,
        &mut interp_state,
        1_000_000,
        &opt.regalloc.cached,
    );
    assert_eq!(expected.exit, RunExit::SideExit { next_rip: 100 });

    let mut mem = vec![0u8; 65536];
    init_state.cpu.write_to_mem(&mut mem, 0);
    let mask = flags_to_mask(init_state.flags);
    mem[FLAGS_MASK_OFFSET as usize..FLAGS_MASK_OFFSET as usize + 4]
        .copy_from_slice(&mask.to_le_bytes());

    let mut code_versions = HashMap::new();
    code_versions.insert(0, 7);
    let (mut store, memory, func) = instantiate_trace(&wasm, code_versions);
    memory.write(&mut store, 0, &mem).unwrap();

    let got_rip = func.call(&mut store, 0).unwrap() as u64;
    assert_eq!(got_rip, 100);

    let mut got_mem = vec![0u8; mem.len()];
    memory.read(&store, 0, &mut got_mem).unwrap();
    let got_cpu = CpuState::read_from_mem(&got_mem, 0);
    let got_mask = u32::from_le_bytes(
        got_mem[FLAGS_MASK_OFFSET as usize..FLAGS_MASK_OFFSET as usize + 4]
            .try_into()
            .unwrap(),
    );
    let got_flags = flags_from_mask(got_mask);

    assert_eq!(got_cpu, interp_state.cpu);
    assert_eq!(got_flags, interp_state.flags);
}
