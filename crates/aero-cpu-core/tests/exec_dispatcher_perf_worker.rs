use aero_cpu_core::exec::{
    ExecCpu, ExecDispatcher, ExecutedTier, Interpreter, InterpreterBlockExit, Tier0RepIterTracker,
};
use aero_cpu_core::interp::tier0::exec::{run_batch, BatchExit};
use aero_cpu_core::jit::cache::{CompiledBlockHandle, CompiledBlockMeta};
use aero_cpu_core::jit::runtime::{
    CompileRequestSink, JitBackend, JitBlockExit, JitConfig, JitRuntime,
};
use aero_cpu_core::mem::FlatTestBus;
use aero_cpu_core::state::{CpuMode, CpuState};
use aero_perf::{PerfCounters, PerfWorker};
use aero_x86::Register;
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
        code_version_max_pages: 64,
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
        code_version_max_pages: 64,
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
            page_versions_generation: 0,
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
        code_version_max_pages: 64,
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

#[test]
fn run_blocks_with_perf_counts_across_multiple_blocks() {
    let mut bus = FlatTestBus::new(0x1000);
    bus.load(0, &[0x90, 0x90, 0x90]); // NOP * 3

    let mut cpu = aero_cpu_core::exec::Vcpu::new_with_mode(CpuMode::Bit16, bus);
    cpu.cpu.state.segments.cs.base = 0;
    cpu.cpu.state.set_rip(0);

    // Force one instruction per interpreter block so `blocks == instructions`.
    let interp = aero_cpu_core::exec::Tier0Interpreter::new(1);
    let config = JitConfig {
        enabled: false,
        hot_threshold: 1,
        cache_max_blocks: 1,
        cache_max_bytes: 0,
        code_version_max_pages: 64,
    };
    let jit = JitRuntime::new(
        config,
        UnusedJitBackend::default(),
        NoCompileSink::default(),
    );
    let mut dispatcher = ExecDispatcher::new(interp, jit);

    let shared = Arc::new(PerfCounters::new());
    let mut perf = PerfWorker::new(shared);

    dispatcher.run_blocks_with_perf(&mut cpu, &mut perf, 3);

    assert_eq!(perf.lifetime_snapshot().instructions_executed, 3);
    assert_eq!(cpu.cpu.state.rip(), 3);
}

#[test]
fn rep_iter_tracker_is_noop_for_non_rep_string_instruction() {
    let state = CpuState::new(CpuMode::Bit16);
    let decoded = aero_x86::decode(&[0xA4], 0, state.bitness()).unwrap(); // MOVSB

    assert!(Tier0RepIterTracker::begin(&state, &decoded, false).is_none());

    let mut bytes = [0u8; 15];
    bytes[0] = 0xA4;
    assert!(Tier0RepIterTracker::begin_from_bytes(&state, &decoded, &bytes).is_none());
}

#[test]
fn rep_iter_tracker_begin_from_bytes_counts_iterations_with_addr_size_override() {
    const CODE_ADDR: u64 = 0;
    let code = [0xF3, 0x67, 0xA4]; // REP + addr-size override + MOVSB

    let mut state = CpuState::new(CpuMode::Bit32);
    state.segments.cs.base = 0;
    state.segments.ds.base = 0x1000;
    state.segments.es.base = 0x2000;
    state.set_rip(CODE_ADDR);

    // Address-size override in 32-bit mode selects SI/DI/CX.
    state.write_reg(Register::SI, 0x10);
    state.write_reg(Register::DI, 0x20);
    state.write_reg(Register::CX, 3);

    let mut bus = FlatTestBus::new(0x10_000);
    bus.load(CODE_ADDR, &code);
    // Initialize DS memory with some bytes for MOVSB to copy.
    bus.load(0x1000 + 0x10, &[0x11, 0x22, 0x33]);

    let decoded = aero_x86::decode(&code, CODE_ADDR, state.bitness()).unwrap();
    let mut fetched = [0u8; 15];
    fetched[..code.len()].copy_from_slice(&code);

    let tracker =
        Tier0RepIterTracker::begin_from_bytes(&state, &decoded, &fetched).expect("should track");

    let res = run_batch(&mut state, &mut bus, 1);
    assert_eq!(res.exit, BatchExit::Completed);

    let shared = Arc::new(PerfCounters::new());
    let mut perf = PerfWorker::new(shared);
    perf.retire_instructions(1);
    tracker.finish(&state, &mut perf);

    assert_eq!(perf.lifetime_snapshot().instructions_executed, 1);
    assert_eq!(perf.lifetime_snapshot().rep_iterations, 3);
    assert_eq!(state.read_reg(Register::CX), 0);
}
