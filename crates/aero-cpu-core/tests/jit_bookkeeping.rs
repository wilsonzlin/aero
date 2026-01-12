use aero_cpu_core::exec::{
    ExecCpu, ExecDispatcher, ExecutedTier, Interpreter, InterpreterBlockExit, StepOutcome, Vcpu,
};
use aero_cpu_core::interrupts::CpuCore;
use aero_cpu_core::jit::cache::CompiledBlockHandle;
use aero_cpu_core::jit::runtime::{CompileRequestSink, JitBackend, JitBlockExit, JitConfig, JitRuntime};
use aero_cpu_core::mem::{CpuBus, FlatTestBus};
use aero_cpu_core::state::{gpr, CpuMode, RFLAGS_IF};
use aero_cpu_core::time::{TimeSource, DEFAULT_TSC_HZ};

#[derive(Default)]
struct NullCompileSink;

impl CompileRequestSink for NullCompileSink {
    fn request_compile(&mut self, _entry_rip: u64) {}
}

#[derive(Debug, Clone, Copy)]
struct FixedExitBackend {
    exit: JitBlockExit,
}

impl JitBackend for FixedExitBackend {
    type Cpu = Vcpu<FlatTestBus>;

    fn execute(&mut self, _table_index: u32, _cpu: &mut Self::Cpu) -> JitBlockExit {
        self.exit
    }
}

#[derive(Default)]
struct NoopInterpreter;

impl Interpreter<Vcpu<FlatTestBus>> for NoopInterpreter {
    fn exec_block(&mut self, cpu: &mut Vcpu<FlatTestBus>) -> InterpreterBlockExit {
        InterpreterBlockExit {
            next_rip: cpu.rip(),
            instructions_retired: 0,
        }
    }
}

fn install_ivt_entry(bus: &mut FlatTestBus, vector: u8, segment: u16, offset: u16) {
    let base = u64::from(vector) * 4;
    bus.write_u16(base, offset).unwrap();
    bus.write_u16(base + 2, segment).unwrap();
}

#[test]
fn tsc_advances_on_committed_jit_blocks() {
    let entry_rip = 0x1000u64;
    let insts = 5u32;
    let initial_tsc = 1000u64;

    let backend = FixedExitBackend {
        exit: JitBlockExit {
            next_rip: entry_rip,
            exit_to_interpreter: false,
            committed: true,
        },
    };
    let config = JitConfig {
        enabled: true,
        hot_threshold: 1,
        cache_max_blocks: 16,
        cache_max_bytes: 0,
    };
    let jit = JitRuntime::new(config, backend, NullCompileSink);
    let mut dispatcher = ExecDispatcher::new(NoopInterpreter::default(), jit);

    // Install a compiled handle that retires `insts` guest instructions.
    {
        let jit = dispatcher.jit_mut();
        let mut meta = jit.make_meta(0, 0);
        meta.instruction_count = insts;
        meta.inhibit_interrupts_after_block = false;
        jit.install_handle(CompiledBlockHandle {
            entry_rip,
            table_index: 0,
            meta,
        });
    }

    let mut core = CpuCore::new(CpuMode::Real);
    core.time = TimeSource::new_deterministic(DEFAULT_TSC_HZ);
    core.time.set_tsc(initial_tsc);
    core.state.msr.tsc = initial_tsc;
    core.state.set_rip(entry_rip);

    let bus = FlatTestBus::new(0x20000);
    let mut cpu = Vcpu::new(core, bus);

    match dispatcher.step(&mut cpu) {
        StepOutcome::Block {
            tier,
            instructions_retired,
            ..
        } => {
            assert_eq!(tier, ExecutedTier::Jit);
            assert_eq!(instructions_retired, u64::from(insts));
        }
        other => panic!("unexpected outcome: {other:?}"),
    }

    assert_eq!(cpu.cpu.state.msr.tsc, initial_tsc + u64::from(insts));
}

#[test]
fn interrupt_shadow_ages_across_committed_jit_blocks() {
    let entry_rip = 0x1000u64;
    let vector = 0x20u8;

    let backend = FixedExitBackend {
        exit: JitBlockExit {
            next_rip: entry_rip,
            exit_to_interpreter: false,
            committed: true,
        },
    };
    let config = JitConfig {
        enabled: true,
        hot_threshold: 1,
        cache_max_blocks: 16,
        cache_max_bytes: 0,
    };
    let jit = JitRuntime::new(config, backend, NullCompileSink);
    let mut dispatcher = ExecDispatcher::new(NoopInterpreter::default(), jit);

    // One instruction is enough to age an active interrupt shadow (interrupt_inhibit = 1).
    {
        let jit = dispatcher.jit_mut();
        let mut meta = jit.make_meta(0, 0);
        meta.instruction_count = 1;
        jit.install_handle(CompiledBlockHandle {
            entry_rip,
            table_index: 0,
            meta,
        });
    }

    let mut core = CpuCore::new(CpuMode::Real);
    core.state.set_rip(entry_rip);
    core.state.set_flag(RFLAGS_IF, true);
    core.state.write_gpr16(gpr::RSP, 0x8000);
    core.pending.inhibit_interrupts_for_one_instruction();
    core.pending.inject_external_interrupt(vector);

    let mut bus = FlatTestBus::new(0x20000);
    // Point vector 0x20 at 0x0000:0x1234 (arbitrary in-bounds handler).
    install_ivt_entry(&mut bus, vector, 0x0000, 0x1234);

    let mut cpu = Vcpu::new(core, bus);

    // Shadow active: interrupt should not be delivered yet. JIT block still commits and should age
    // the shadow.
    match dispatcher.step(&mut cpu) {
        StepOutcome::Block {
            tier,
            instructions_retired,
            ..
        } => {
            assert_eq!(tier, ExecutedTier::Jit);
            assert_eq!(instructions_retired, 1);
        }
        other => panic!("unexpected outcome: {other:?}"),
    }
    assert_eq!(cpu.cpu.pending.external_interrupts.len(), 1);

    // Shadow should now be expired, so the queued external interrupt is delivered at the next
    // instruction boundary (i.e. next dispatcher step).
    assert_eq!(dispatcher.step(&mut cpu), StepOutcome::InterruptDelivered);
    assert!(cpu.cpu.pending.external_interrupts.is_empty());
}

#[test]
fn rollback_jit_exits_do_not_advance_time_or_age_interrupt_shadow() {
    let entry_rip = 0x1000u64;
    let vector = 0x20u8;
    let initial_tsc = 1234u64;

    let backend = FixedExitBackend {
        exit: JitBlockExit {
            next_rip: entry_rip,
            exit_to_interpreter: true,
            committed: false,
        },
    };
    let config = JitConfig {
        enabled: true,
        hot_threshold: 1,
        cache_max_blocks: 16,
        cache_max_bytes: 0,
    };
    let jit = JitRuntime::new(config, backend, NullCompileSink);
    let mut dispatcher = ExecDispatcher::new(NoopInterpreter::default(), jit);

    // Install a compiled handle that *would* retire instructions if the block committed.
    {
        let jit = dispatcher.jit_mut();
        let mut meta = jit.make_meta(0, 0);
        meta.instruction_count = 5;
        jit.install_handle(CompiledBlockHandle {
            entry_rip,
            table_index: 0,
            meta,
        });
    }

    let mut core = CpuCore::new(CpuMode::Real);
    core.time = TimeSource::new_deterministic(DEFAULT_TSC_HZ);
    core.time.set_tsc(initial_tsc);
    core.state.msr.tsc = initial_tsc;
    core.state.set_rip(entry_rip);
    core.state.set_flag(RFLAGS_IF, true);
    core.state.write_gpr16(gpr::RSP, 0x8000);
    core.pending.inhibit_interrupts_for_one_instruction();
    core.pending.inject_external_interrupt(vector);

    let mut bus = FlatTestBus::new(0x20000);
    install_ivt_entry(&mut bus, vector, 0x0000, 0x1234);

    let mut cpu = Vcpu::new(core, bus);

    // Run the JIT block. It "exits" but rolls back, so no retirement/time advancement should
    // occur.
    match dispatcher.step(&mut cpu) {
        StepOutcome::Block {
            tier,
            instructions_retired,
            ..
        } => {
            assert_eq!(tier, ExecutedTier::Jit);
            assert_eq!(instructions_retired, 0);
        }
        other => panic!("unexpected outcome: {other:?}"),
    }
    assert_eq!(cpu.cpu.state.msr.tsc, initial_tsc);

    // Interrupt shadow should still be active (so delivery is still inhibited).
    assert!(!cpu.maybe_deliver_interrupt());
    assert_eq!(cpu.cpu.pending.external_interrupts.len(), 1);
}
