use aero_cpu_core::exec::{ExecDispatcher, ExecutedTier, Interpreter, InterpreterBlockExit, Vcpu};
use aero_cpu_core::jit::cache::{CompiledBlockHandle, CompiledBlockMeta};
use aero_cpu_core::jit::runtime::{
    CompileRequestSink, JitBackend, JitBlockExit, JitConfig, JitRuntime,
};
use aero_cpu_core::mem::FlatTestBus;
use aero_cpu_core::state::CpuMode;

#[derive(Default)]
struct NoCompileSink;

impl CompileRequestSink for NoCompileSink {
    fn request_compile(&mut self, _entry_rip: u64) {}
}

#[derive(Default)]
struct PanicInterpreter;

impl Interpreter<Vcpu<FlatTestBus>> for PanicInterpreter {
    fn exec_block(&mut self, _cpu: &mut Vcpu<FlatTestBus>) -> InterpreterBlockExit {
        panic!("interpreter should not be invoked in commit-flag tests");
    }
}

#[derive(Default)]
struct RollbackBackend;

impl JitBackend for RollbackBackend {
    type Cpu = Vcpu<FlatTestBus>;

    fn execute(&mut self, _table_index: u32, _cpu: &mut Self::Cpu) -> JitBlockExit {
        JitBlockExit {
            next_rip: 0,
            exit_to_interpreter: true,
            committed: false,
        }
    }
}

#[derive(Default)]
struct CommitBackend;

impl JitBackend for CommitBackend {
    type Cpu = Vcpu<FlatTestBus>;

    fn execute(&mut self, _table_index: u32, _cpu: &mut Self::Cpu) -> JitBlockExit {
        JitBlockExit {
            next_rip: 0,
            exit_to_interpreter: false,
            committed: true,
        }
    }
}

fn install_block<B: JitBackend<Cpu = Vcpu<FlatTestBus>>, C: CompileRequestSink>(
    jit: &mut JitRuntime<B, C>,
    instruction_count: u32,
    inhibit_interrupts_after_block: bool,
) {
    jit.install_handle(CompiledBlockHandle {
        entry_rip: 0,
        table_index: 0,
        meta: CompiledBlockMeta {
            code_paddr: 0,
            byte_len: 0,
            page_versions_generation: 0,
            page_versions: Vec::new(),
            instruction_count,
            inhibit_interrupts_after_block,
        },
    });
}

#[test]
fn jit_rollback_does_not_advance_tsc_or_age_interrupt_shadow() {
    let config = JitConfig {
        enabled: true,
        hot_threshold: 1,
        cache_max_blocks: 4,
        cache_max_bytes: 0,
        code_version_max_pages: 64,
    };

    let mut jit = JitRuntime::new(config, RollbackBackend, NoCompileSink);
    install_block(&mut jit, 5, false);
    let mut dispatcher = ExecDispatcher::new(PanicInterpreter, jit);

    let mut cpu = Vcpu::new_with_mode(CpuMode::Bit16, FlatTestBus::new(0x1000));
    cpu.cpu.state.segments.cs.base = 0;
    cpu.cpu.state.set_rip(0);

    cpu.cpu.pending.inhibit_interrupts_for_one_instruction();
    cpu.cpu.time.set_tsc(100);
    cpu.cpu.state.msr.tsc = 100;

    let outcome = dispatcher.step(&mut cpu);
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

    assert_eq!(
        cpu.cpu.pending.interrupt_inhibit(),
        1,
        "rollback must not age interrupt shadow state"
    );
    assert_eq!(
        cpu.cpu.state.msr.tsc, 100,
        "rollback must not advance architectural TSC"
    );
    assert_eq!(
        cpu.cpu.time.read_tsc(),
        100,
        "rollback must not advance internal time source"
    );
}

#[test]
fn jit_commit_advances_tsc_and_ages_interrupt_shadow() {
    let config = JitConfig {
        enabled: true,
        hot_threshold: 1,
        cache_max_blocks: 4,
        cache_max_bytes: 0,
        code_version_max_pages: 64,
    };

    let mut jit = JitRuntime::new(config, CommitBackend, NoCompileSink);
    install_block(&mut jit, 5, false);
    let mut dispatcher = ExecDispatcher::new(PanicInterpreter, jit);

    let mut cpu = Vcpu::new_with_mode(CpuMode::Bit16, FlatTestBus::new(0x1000));
    cpu.cpu.state.segments.cs.base = 0;
    cpu.cpu.state.set_rip(0);

    cpu.cpu.pending.inhibit_interrupts_for_one_instruction();
    cpu.cpu.time.set_tsc(100);
    cpu.cpu.state.msr.tsc = 100;

    let outcome = dispatcher.step(&mut cpu);
    match outcome {
        aero_cpu_core::exec::StepOutcome::Block {
            tier,
            instructions_retired,
            ..
        } => {
            assert_eq!(tier, ExecutedTier::Jit);
            assert_eq!(instructions_retired, 5);
        }
        other => panic!("expected JIT block, got {other:?}"),
    }

    assert_eq!(
        cpu.cpu.pending.interrupt_inhibit(),
        0,
        "committed blocks must age interrupt shadow state"
    );
    assert_eq!(
        cpu.cpu.state.msr.tsc, 105,
        "committed blocks must advance architectural TSC by instruction count"
    );
    assert_eq!(
        cpu.cpu.time.read_tsc(),
        105,
        "committed blocks must advance internal time source by instruction count"
    );
}

#[test]
fn jit_rollback_does_not_apply_inhibit_interrupts_after_block() {
    let config = JitConfig {
        enabled: true,
        hot_threshold: 1,
        cache_max_blocks: 4,
        cache_max_bytes: 0,
        code_version_max_pages: 64,
    };

    let mut jit = JitRuntime::new(config, RollbackBackend, NoCompileSink);
    install_block(&mut jit, 5, true);
    let mut dispatcher = ExecDispatcher::new(PanicInterpreter, jit);

    let mut cpu = Vcpu::new_with_mode(CpuMode::Bit16, FlatTestBus::new(0x1000));
    cpu.cpu.state.segments.cs.base = 0;
    cpu.cpu.state.set_rip(0);

    cpu.cpu.time.set_tsc(100);
    cpu.cpu.state.msr.tsc = 100;

    let outcome = dispatcher.step(&mut cpu);
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

    assert_eq!(
        cpu.cpu.pending.interrupt_inhibit(),
        0,
        "rollback must not apply inhibit_interrupts_after_block"
    );
    assert_eq!(
        cpu.cpu.state.msr.tsc, 100,
        "rollback must not advance architectural TSC"
    );
}

#[test]
fn jit_commit_applies_inhibit_interrupts_after_block() {
    let config = JitConfig {
        enabled: true,
        hot_threshold: 1,
        cache_max_blocks: 4,
        cache_max_bytes: 0,
        code_version_max_pages: 64,
    };

    let mut jit = JitRuntime::new(config, CommitBackend, NoCompileSink);
    install_block(&mut jit, 5, true);
    let mut dispatcher = ExecDispatcher::new(PanicInterpreter, jit);

    let mut cpu = Vcpu::new_with_mode(CpuMode::Bit16, FlatTestBus::new(0x1000));
    cpu.cpu.state.segments.cs.base = 0;
    cpu.cpu.state.set_rip(0);

    cpu.cpu.time.set_tsc(100);
    cpu.cpu.state.msr.tsc = 100;

    let outcome = dispatcher.step(&mut cpu);
    match outcome {
        aero_cpu_core::exec::StepOutcome::Block {
            tier,
            instructions_retired,
            ..
        } => {
            assert_eq!(tier, ExecutedTier::Jit);
            assert_eq!(instructions_retired, 5);
        }
        other => panic!("expected JIT block, got {other:?}"),
    }

    assert_eq!(
        cpu.cpu.pending.interrupt_inhibit(),
        1,
        "committed blocks must apply inhibit_interrupts_after_block"
    );
    assert_eq!(
        cpu.cpu.state.msr.tsc, 105,
        "committed blocks must advance architectural TSC by instruction count"
    );
}
