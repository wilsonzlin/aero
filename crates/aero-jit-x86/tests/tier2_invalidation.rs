use std::cell::RefCell;
use std::rc::Rc;

use aero_cpu_core::jit::cache::CompiledBlockHandle;
use aero_cpu_core::jit::runtime::{
    CompileRequestSink, JitBackend, JitBlockExit, JitConfig, JitRuntime,
};
use aero_cpu_core::state::RFLAGS_DF;
use aero_types::{Flag, FlagSet, Gpr};
mod tier1_common;

use tier1_common::{write_cpu_to_wasm_bytes, CpuSnapshot, SimpleBus};

use aero_jit_x86::abi;
use aero_jit_x86::profile::{ProfileData, TraceConfig};
use aero_jit_x86::tier2::exec::{run_trace_with_cached_regs, RunExit, RuntimeEnv, T2State};
use aero_jit_x86::tier2::ir::{
    BinOp, Block, BlockId, Function, Instr, Operand, Terminator, TraceKind, ValueId,
};
use aero_jit_x86::tier2::opt::{optimize_trace, OptConfig};
use aero_jit_x86::tier2::trace::TraceBuilder;
use aero_jit_x86::tier2::wasm::{Tier2WasmCodegen, EXPORT_TRACE_FN};
use aero_jit_x86::wasm::{
    IMPORT_MEM_READ_U16, IMPORT_MEM_READ_U32, IMPORT_MEM_READ_U64, IMPORT_MEM_READ_U8,
    IMPORT_MEM_WRITE_U16, IMPORT_MEM_WRITE_U32, IMPORT_MEM_WRITE_U64, IMPORT_MEM_WRITE_U8,
    IMPORT_MEMORY, IMPORT_MODULE,
};

use wasmi::{Caller, Engine, Func, Linker, Memory, MemoryType, Module, Store, TypedFunc};

const CPU_PTR: i32 = 0x1_0000;
const JIT_CTX_PTR: i32 = CPU_PTR + (abi::CPU_STATE_SIZE as i32);
const GUEST_MEM_SIZE: usize = 0x1_0000; // one wasm page

#[derive(Clone, Default)]
struct RecordingCompileSink(Rc<RefCell<Vec<u64>>>);

impl RecordingCompileSink {
    fn snapshot(&self) -> Vec<u64> {
        self.0.borrow().clone()
    }
}

impl CompileRequestSink for RecordingCompileSink {
    fn request_compile(&mut self, entry_rip: u64) {
        self.0.borrow_mut().push(entry_rip);
    }
}

fn v(idx: u32) -> ValueId {
    ValueId(idx)
}

fn define_mem_helpers(store: &mut Store<()>, linker: &mut Linker<()>, memory: Memory) {
    fn read<const N: usize>(caller: &mut Caller<'_, ()>, memory: &Memory, addr: usize) -> u64 {
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

    fn write<const N: usize>(caller: &mut Caller<'_, ()>, memory: &Memory, addr: usize, value: u64) {
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
                move |mut caller: Caller<'_, ()>, _cpu_ptr: i32, addr: i64| -> i32 {
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
                move |mut caller: Caller<'_, ()>, _cpu_ptr: i32, addr: i64| -> i32 {
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
                move |mut caller: Caller<'_, ()>, _cpu_ptr: i32, addr: i64| -> i32 {
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
                move |mut caller: Caller<'_, ()>, _cpu_ptr: i32, addr: i64| -> i64 {
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
                move |mut caller: Caller<'_, ()>, _cpu_ptr: i32, addr: i64, value: i32| {
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
                move |mut caller: Caller<'_, ()>, _cpu_ptr: i32, addr: i64, value: i32| {
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
                move |mut caller: Caller<'_, ()>, _cpu_ptr: i32, addr: i64, value: i32| {
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
                move |mut caller: Caller<'_, ()>, _cpu_ptr: i32, addr: i64, value: i64| {
                    write::<8>(&mut caller, &mem, addr as usize, value as u64);
                },
            ),
        )
        .unwrap();
}

struct WasmiTraceBackend {
    store: Store<()>,
    memory: Memory,
    trace: TypedFunc<(i32, i32), i64>,
}

impl WasmiTraceBackend {
    fn new(wasm: &[u8]) -> Self {
        let engine = Engine::default();
        let module = Module::new(&engine, wasm).expect("compile wasm");

        let mut store = Store::new(&engine, ());
        let mut linker = Linker::new(&engine);

        let memory = Memory::new(&mut store, MemoryType::new(2, None)).expect("alloc memory");
        linker
            .define(IMPORT_MODULE, IMPORT_MEMORY, memory.clone())
            .unwrap();
        define_mem_helpers(&mut store, &mut linker, memory.clone());

        let instance = linker
            .instantiate_and_start(&mut store, &module)
            .expect("instantiate");
        let trace = instance
            .get_typed_func::<(i32, i32), i64>(&store, EXPORT_TRACE_FN)
            .expect("trace export");

        Self { store, memory, trace }
    }
}

impl JitBackend for WasmiTraceBackend {
    type Cpu = T2State;

    fn execute(&mut self, table_index: u32, cpu: &mut Self::Cpu) -> JitBlockExit {
        assert_eq!(table_index, 0, "unexpected table index");

        // Ensure guest memory starts zeroed and CPU state lives at `CPU_PTR` in the second page.
        let guest_mem = vec![0u8; GUEST_MEM_SIZE];
        self.memory.write(&mut self.store, 0, &guest_mem).unwrap();

        let mut cpu_bytes = vec![0u8; abi::CPU_STATE_SIZE as usize];
        write_cpu_to_wasm_bytes(&cpu.cpu, &mut cpu_bytes);
        self.memory
            .write(&mut self.store, CPU_PTR as usize, &cpu_bytes)
            .unwrap();

        let next_rip = self
            .trace
            .call(&mut self.store, (CPU_PTR, JIT_CTX_PTR))
            .unwrap() as u64;

        self.memory
            .read(&self.store, CPU_PTR as usize, &mut cpu_bytes)
            .unwrap();
        let snapshot = CpuSnapshot::from_wasm_bytes(&cpu_bytes);
        cpu.cpu.gpr = snapshot.gpr;
        cpu.cpu.rip = snapshot.rip;
        cpu.cpu.set_rflags(snapshot.rflags);

        JitBlockExit {
            next_rip,
            exit_to_interpreter: false,
        }
    }
}

#[test]
fn tier2_trace_is_invalidated_via_jit_runtime_page_versions() {
    // A tiny loop trace that increments RAX until it reaches 10 then side-exits.
    let entry_rip = 0x1000;
    let exit_rip = 0x2000;

    let func = Function {
        entry: BlockId(0),
        blocks: vec![
            Block {
                id: BlockId(0),
                start_rip: entry_rip,
                instrs: vec![
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
                        flags: FlagSet::ALU,
                    },
                    Instr::StoreReg {
                        reg: Gpr::Rax,
                        src: Operand::Value(v(2)),
                    },
                    Instr::Const { dst: v(3), value: 10 },
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
                start_rip: exit_rip,
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
    let backend = WasmiTraceBackend::new(&wasm);

    let env = RuntimeEnv::default();
    let mut expected_state = T2State::default();
    expected_state.cpu.rip = entry_rip;
    expected_state.cpu.gpr[Gpr::Rax.as_u8() as usize] = 0;
    expected_state.cpu.rflags = abi::RFLAGS_RESERVED1 | RFLAGS_DF;
    for flag in [Flag::Cf, Flag::Pf, Flag::Af, Flag::Zf, Flag::Sf, Flag::Of] {
        expected_state.cpu.rflags |= 1u64 << flag.rflags_bit();
    }

    let mut interp_state = expected_state.clone();
    let mut bus = SimpleBus::new(GUEST_MEM_SIZE);
    let expected = run_trace_with_cached_regs(
        &trace.ir,
        &env,
        &mut bus,
        &mut interp_state,
        1_000_000,
        &opt.regalloc.cached,
    );
    assert_eq!(expected.exit, RunExit::SideExit { next_rip: exit_rip });

    let config = JitConfig {
        enabled: true,
        hot_threshold: 1_000_000,
        cache_max_blocks: 16,
        cache_max_bytes: 0,
    };
    let compile = RecordingCompileSink::default();
    let mut jit = JitRuntime::new(config, backend, compile.clone());

    let meta = jit.snapshot_meta(entry_rip, 64);
    jit.install_handle(CompiledBlockHandle {
        entry_rip,
        table_index: 0,
        meta,
    });
    assert!(jit.is_compiled(entry_rip));

    let handle = jit.prepare_block(entry_rip).expect("handle");
    let mut cpu = expected_state.clone();
    let exit = jit.execute_block(&mut cpu, &handle);
    assert_eq!(exit.next_rip, exit_rip);
    assert_eq!(cpu, interp_state);
    assert!(compile.snapshot().is_empty());

    // Self-modifying code bumps the page version and invalidates the trace.
    jit.on_guest_write(entry_rip + 4, 1);
    assert!(jit.prepare_block(entry_rip).is_none());
    assert!(!jit.is_compiled(entry_rip));

    assert_eq!(compile.snapshot(), vec![entry_rip]);
}
