#![cfg(not(target_arch = "wasm32"))]

use std::cell::RefCell;
use std::rc::Rc;

use aero_cpu_core::exec::{
    ExecCpu, ExecDispatcher, ExecutedTier, Interpreter, InterpreterBlockExit, StepOutcome,
};
use aero_cpu_core::jit::runtime::{JitBackend, JitBlockExit, JitConfig, JitRuntime};
use aero_cpu_core::state::CpuState;
use aero_jit_x86::abi;
use aero_jit_x86::backend::{Tier1Cpu, WasmtimeBackend};
use aero_jit_x86::tier1::ir::interp as tier1_interp;
use aero_jit_x86::tier1_pipeline::{Tier1CompileQueue, Tier1Compiler, Tier1WasmRegistry};
use aero_jit_x86::{discover_block, translate_block, BlockLimits, Tier1Bus};
use aero_types::Gpr;

#[derive(Clone)]
struct SharedWasmtimeBackend(Rc<RefCell<WasmtimeBackend<TestCpu>>>);

impl SharedWasmtimeBackend {
    fn new() -> Self {
        Self(Rc::new(RefCell::new(WasmtimeBackend::<TestCpu>::new())))
    }
}

impl Tier1Bus for SharedWasmtimeBackend {
    fn read_u8(&self, addr: u64) -> u8 {
        self.0.borrow().read_u8(addr)
    }

    fn write_u8(&mut self, addr: u64, value: u8) {
        self.0.borrow_mut().write_u8(addr, value);
    }
}

impl Tier1WasmRegistry for SharedWasmtimeBackend {
    fn register_tier1_block(&mut self, wasm: Vec<u8>, exit_to_interpreter: bool) -> u32 {
        self.0
            .borrow_mut()
            .register_tier1_block(wasm, exit_to_interpreter)
    }
}

impl JitBackend for SharedWasmtimeBackend {
    type Cpu = TestCpu;

    fn execute(&mut self, table_index: u32, cpu: &mut TestCpu) -> JitBlockExit {
        self.0.borrow_mut().execute(table_index, cpu)
    }
}

#[derive(Debug, Default, Clone)]
struct TestCpu {
    state: CpuState,
}

impl Tier1Cpu for TestCpu {
    fn tier1_state(&self) -> &CpuState {
        &self.state
    }

    fn tier1_state_mut(&mut self) -> &mut CpuState {
        &mut self.state
    }
}

impl ExecCpu for TestCpu {
    fn rip(&self) -> u64 {
        self.state.rip
    }

    fn set_rip(&mut self, rip: u64) {
        self.state.rip = rip;
    }

    fn maybe_deliver_interrupt(&mut self) -> bool {
        false
    }
}

struct Tier1Interpreter {
    bus: SharedWasmtimeBackend,
}

impl Interpreter<TestCpu> for Tier1Interpreter {
    fn exec_block(&mut self, cpu: &mut TestCpu) -> InterpreterBlockExit {
        let entry = cpu.rip();
        let block = discover_block(&self.bus, entry, BlockLimits::default());
        let instructions_retired = block.insts.len() as u64;
        let ir = translate_block(&block);
        let mut cpu_mem = vec![0u8; abi::CPU_STATE_SIZE as usize];
        tier1_interp::TestCpu::from_cpu_state(&cpu.state).write_to_abi_mem(&mut cpu_mem);
        let _ = tier1_interp::execute_block(&ir, &mut cpu_mem, &mut self.bus);
        tier1_interp::TestCpu::from_abi_mem(&cpu_mem).write_to_cpu_state(&mut cpu.state);
        InterpreterBlockExit {
            next_rip: cpu.state.rip,
            instructions_retired,
        }
    }
}

type Tier1Dispatcher = ExecDispatcher<Tier1Interpreter, SharedWasmtimeBackend, Tier1CompileQueue>;
type Tier1TestCompiler = Tier1Compiler<SharedWasmtimeBackend, SharedWasmtimeBackend>;
struct RuntimeHarness {
    backend: SharedWasmtimeBackend,
    compile_queue: Tier1CompileQueue,
    dispatcher: Tier1Dispatcher,
    compiler: Tier1TestCompiler,
    cpu: TestCpu,
}

fn setup_runtime(hot_threshold: u32) -> RuntimeHarness {
    let entry = 0x1000u64;
    let code = [
        0x83, 0xc0, 0x01, // add eax, 1
        0xeb, 0xfb, // jmp -5
    ];

    let mut backend = SharedWasmtimeBackend::new();
    for (i, b) in code.iter().enumerate() {
        backend.write_u8(entry + i as u64, *b);
    }

    let compile_queue = Tier1CompileQueue::new();
    let config = JitConfig {
        enabled: true,
        hot_threshold,
        cache_max_blocks: 16,
        cache_max_bytes: 0,
    };
    let jit = JitRuntime::new(config, backend.clone(), compile_queue.clone());
    let interp = Tier1Interpreter {
        bus: backend.clone(),
    };
    let dispatcher = ExecDispatcher::new(interp, jit);

    let compiler = Tier1Compiler::new(backend.clone(), backend.clone()).with_limits(BlockLimits {
        max_insts: 16,
        max_bytes: 64,
    });

    let cpu = TestCpu {
        state: CpuState {
            rip: entry,
            ..Default::default()
        },
    };

    RuntimeHarness {
        backend,
        compile_queue,
        dispatcher,
        compiler,
        cpu,
    }
}

#[test]
fn tier1_end_to_end_compile_install_and_execute() {
    let entry = 0x1000u64;
    let RuntimeHarness {
        backend,
        compile_queue,
        mut dispatcher,
        mut compiler,
        mut cpu,
    } = setup_runtime(3);

    // Run blocks until hotness triggers compilation. Keep running to ensure the request is only
    // queued once.
    for i in 0..5 {
        match dispatcher.step(&mut cpu) {
            StepOutcome::Block {
                tier,
                entry_rip,
                next_rip,
                ..
            } => {
                assert_eq!(
                    tier,
                    ExecutedTier::Interpreter,
                    "step {i} should be interpreted"
                );
                assert_eq!(entry_rip, entry);
                assert_eq!(next_rip, entry);
            }
            StepOutcome::InterruptDelivered => panic!("unexpected interrupt"),
        }
    }

    // Hotness should trigger a single compile request for this RIP.
    assert_eq!(compile_queue.drain(), vec![entry]);

    // Compile and install Tier-1 block.
    {
        let jit = dispatcher.jit_mut();
        let handle = compiler.compile_handle(&*jit, entry, 64).unwrap();
        jit.install_handle(handle);
    }

    // Subsequent executions should use the JIT tier and continue producing correct exits.
    match dispatcher.step(&mut cpu) {
        StepOutcome::Block {
            tier,
            entry_rip,
            next_rip,
            ..
        } => {
            assert_eq!(tier, ExecutedTier::Jit);
            assert_eq!(entry_rip, entry);
            assert_eq!(next_rip, entry);
        }
        StepOutcome::InterruptDelivered => panic!("unexpected interrupt"),
    }

    // 5 interpreted blocks + 1 JIT block, each does `add eax, 1`.
    assert_eq!(cpu.state.gpr[Gpr::Rax.as_u8() as usize], 6);

    // Ensure the backend handle we used for compilation is shared with the runtime backend.
    // If it weren't, we'd install a table index that the runtime couldn't execute.
    let mut backend = backend;
    let _ = backend.execute(0, &mut cpu);
}

#[test]
fn tier1_stale_snapshot_is_rejected_and_requeued() {
    let entry = 0x1000u64;
    let RuntimeHarness {
        backend: _backend,
        compile_queue,
        mut dispatcher,
        mut compiler,
        mut cpu,
    } = setup_runtime(1);

    // First block execution should request compilation immediately.
    dispatcher.step(&mut cpu);
    assert_eq!(compile_queue.drain(), vec![entry]);

    // Compile, then invalidate the code page before installing the handle.
    {
        let jit = dispatcher.jit_mut();
        let handle = compiler.compile_handle(&*jit, entry, 64).unwrap();
        jit.on_guest_write(entry, 1);
        jit.install_handle(handle);
        assert!(!jit.is_compiled(entry));
    }

    // The runtime should request compilation again after rejecting the stale handle.
    assert_eq!(compile_queue.drain(), vec![entry]);
}
