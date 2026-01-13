use aero_cpu_core::exec::{
    ExecCpu, ExecDispatcher, ExecutedTier, Interpreter, InterpreterBlockExit,
};
use aero_cpu_core::jit::cache::{CompiledBlockHandle, CompiledBlockMeta};
use aero_cpu_core::jit::runtime::{
    CompileRequestSink, JitBackend, JitBlockExit, JitConfig, JitRuntime,
};
use aero_cpu_core::mem::FlatTestBus;
use aero_cpu_core::state::CpuMode;
use aero_perf::{PerfCounters, PerfWorker};
use std::sync::Arc;

#[derive(Default)]
struct NoCompileSink;

impl CompileRequestSink for NoCompileSink {
    fn request_compile(&mut self, _entry_rip: u64) {}
}

#[derive(Default)]
struct UnusedJitBackend;

impl JitBackend for UnusedJitBackend {
    type Cpu = aero_cpu_core::exec::Vcpu<FlatTestBus>;

    fn execute(&mut self, _table_index: u32, _cpu: &mut Self::Cpu) -> JitBlockExit {
        panic!("JIT backend should not be invoked in Tier-0-only test");
    }
}

#[test]
fn tier0_only_step_updates_perf_worker() {
    let mut bus = FlatTestBus::new(0x1000);
    // 3x NOP (no control transfers) so Tier0Interpreter executes exactly `max_insts`.
    bus.load(0, &[0x90, 0x90, 0x90]);

    let mut cpu = aero_cpu_core::exec::Vcpu::new_with_mode(CpuMode::Bit16, bus);
    cpu.cpu.state.segments.cs.base = 0;
    cpu.cpu.state.set_rip(0);

    let interp = aero_cpu_core::exec::Tier0Interpreter::new(3);

    let config = JitConfig {
        enabled: false,
        hot_threshold: 1,
        cache_max_blocks: 1,
        cache_max_bytes: 0,
    };
    let jit = JitRuntime::new(
        config,
        UnusedJitBackend::default(),
        NoCompileSink::default(),
    );
    let mut dispatcher = ExecDispatcher::new(interp, jit);

    let shared = Arc::new(PerfCounters::new());
    let mut perf = PerfWorker::new(shared);

    let outcome = dispatcher.step_with_perf(&mut cpu, &mut perf);
    match outcome {
        aero_cpu_core::exec::StepOutcome::Block {
            tier,
            instructions_retired,
            next_rip,
            ..
        } => {
            assert_eq!(tier, ExecutedTier::Interpreter);
            assert_eq!(instructions_retired, 3);
            assert_eq!(next_rip, 3);
        }
        other => panic!("expected interpreter block, got {other:?}"),
    }

    assert_eq!(perf.lifetime_snapshot().instructions_executed, 3);
}

#[derive(Debug, Default, Clone)]
struct TestCpu {
    rip: u64,
}

impl ExecCpu for TestCpu {
    fn rip(&self) -> u64 {
        self.rip
    }

    fn set_rip(&mut self, rip: u64) {
        self.rip = rip;
    }

    fn maybe_deliver_interrupt(&mut self) -> bool {
        false
    }
}

#[derive(Default)]
struct RollbackJitBackend;

impl JitBackend for RollbackJitBackend {
    type Cpu = TestCpu;

    fn execute(&mut self, _table_index: u32, _cpu: &mut TestCpu) -> JitBlockExit {
        JitBlockExit {
            next_rip: 1,
            exit_to_interpreter: false,
            committed: false,
        }
    }
}

#[derive(Default)]
struct PanicInterpreter;

impl Interpreter<TestCpu> for PanicInterpreter {
    fn exec_block(&mut self, _cpu: &mut TestCpu) -> InterpreterBlockExit {
        panic!("interpreter should not be invoked in JIT rollback test");
    }
}

#[test]
fn jit_rollback_does_not_advance_perf_counters() {
    let config = JitConfig {
        enabled: true,
        hot_threshold: 1,
        cache_max_blocks: 4,
        cache_max_bytes: 0,
    };

    let mut jit = JitRuntime::new(
        config,
        RollbackJitBackend::default(),
        NoCompileSink::default(),
    );
    jit.install_handle(CompiledBlockHandle {
        entry_rip: 0,
        table_index: 0,
        meta: CompiledBlockMeta {
            code_paddr: 0x1000,
            byte_len: 4,
            page_versions: Vec::new(),
            // Non-zero so the test would catch incorrectly counting uncommitted blocks.
            instruction_count: 5,
            inhibit_interrupts_after_block: false,
        },
    });

    let mut dispatcher = ExecDispatcher::new(PanicInterpreter::default(), jit);
    let mut cpu = TestCpu { rip: 0 };

    let shared = Arc::new(PerfCounters::new());
    let mut perf = PerfWorker::new(shared);

    let outcome = dispatcher.step_with_perf(&mut cpu, &mut perf);
    match outcome {
        aero_cpu_core::exec::StepOutcome::Block {
            tier,
            instructions_retired,
            ..
        } => {
            assert_eq!(tier, ExecutedTier::Jit);
            assert_eq!(instructions_retired, 0);
        }
        other => panic!("expected JIT block, got {other:?}"),
    }

    assert_eq!(perf.lifetime_snapshot().instructions_executed, 0);
}

#[derive(Debug, Default)]
struct InterruptCpu {
    rip: u64,
    pending: bool,
}

impl ExecCpu for InterruptCpu {
    fn rip(&self) -> u64 {
        self.rip
    }

    fn set_rip(&mut self, rip: u64) {
        self.rip = rip;
    }

    fn maybe_deliver_interrupt(&mut self) -> bool {
        if self.pending {
            self.pending = false;
            true
        } else {
            false
        }
    }
}

#[derive(Default)]
struct PanicBackend;

impl JitBackend for PanicBackend {
    type Cpu = InterruptCpu;

    fn execute(&mut self, _table_index: u32, _cpu: &mut InterruptCpu) -> JitBlockExit {
        panic!("backend should not be invoked when interrupt is delivered");
    }
}

#[derive(Default)]
struct PanicInterp;

impl Interpreter<InterruptCpu> for PanicInterp {
    fn exec_block(&mut self, _cpu: &mut InterruptCpu) -> InterpreterBlockExit {
        panic!("interpreter should not be invoked when interrupt is delivered");
    }
}

#[test]
fn interrupt_delivery_does_not_advance_perf_counters() {
    let config = JitConfig {
        enabled: false,
        hot_threshold: 1,
        cache_max_blocks: 1,
        cache_max_bytes: 0,
    };
    let jit = JitRuntime::new(config, PanicBackend::default(), NoCompileSink::default());
    let mut dispatcher = ExecDispatcher::new(PanicInterp::default(), jit);

    let shared = Arc::new(PerfCounters::new());
    let mut perf = PerfWorker::new(shared);

    let mut cpu = InterruptCpu {
        rip: 0,
        pending: true,
    };
    let outcome = dispatcher.step_with_perf(&mut cpu, &mut perf);

    assert_eq!(
        outcome,
        aero_cpu_core::exec::StepOutcome::InterruptDelivered
    );
    assert_eq!(perf.lifetime_snapshot().instructions_executed, 0);
}
