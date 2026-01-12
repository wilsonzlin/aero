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

#[derive(Debug)]
struct OneShotBackend {
    exit: JitBlockExit,
    executed: bool,
}

impl JitBackend for OneShotBackend {
    type Cpu = Vcpu<FlatTestBus>;

    fn execute(&mut self, _table_index: u32, _cpu: &mut Self::Cpu) -> JitBlockExit {
        assert!(
            !self.executed,
            "unexpected repeated JIT execution: exit_to_interpreter should force an interpreter step"
        );
        self.executed = true;
        self.exit
    }
}

#[derive(Debug, Clone, Copy)]
struct FirstExitSecondPanicBackend {
    first_exit: JitBlockExit,
}

impl JitBackend for FirstExitSecondPanicBackend {
    type Cpu = Vcpu<FlatTestBus>;

    fn execute(&mut self, table_index: u32, _cpu: &mut Self::Cpu) -> JitBlockExit {
        match table_index {
            0 => self.first_exit,
            1 => panic!("unexpected JIT execution: exit_to_interpreter should force an interpreter step"),
            other => panic!("unexpected JIT table index {other}"),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct TwoExitBackend {
    exit0: JitBlockExit,
    exit1: JitBlockExit,
}

impl JitBackend for TwoExitBackend {
    type Cpu = Vcpu<FlatTestBus>;

    fn execute(&mut self, table_index: u32, _cpu: &mut Self::Cpu) -> JitBlockExit {
        match table_index {
            0 => self.exit0,
            1 => self.exit1,
            other => panic!("unexpected JIT table index {other}"),
        }
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
fn committed_exit_to_interpreter_advances_tsc_and_forces_one_interpreter_step() {
    let entry_rip = 0x1000u64;
    let next_rip = 0x2000u64;
    let insts = 4u32;
    let initial_tsc = 999u64;

    let backend = FirstExitSecondPanicBackend {
        first_exit: JitBlockExit {
            next_rip,
            exit_to_interpreter: true,
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
    let mut dispatcher = ExecDispatcher::new(NoopInterpreter, jit);

    // Install the exiting block and also install a block at `next_rip` so we can ensure the forced
    // interpreter step overrides a compiled handle.
    {
        let jit = dispatcher.jit_mut();

        let mut meta0 = jit.make_meta(0, 0);
        meta0.instruction_count = insts;
        meta0.inhibit_interrupts_after_block = false;
        jit.install_handle(CompiledBlockHandle {
            entry_rip,
            table_index: 0,
            meta: meta0,
        });

        let mut meta1 = jit.make_meta(0, 0);
        meta1.instruction_count = 1;
        jit.install_handle(CompiledBlockHandle {
            entry_rip: next_rip,
            table_index: 1,
            meta: meta1,
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
            entry_rip: got_entry,
            next_rip: got_next,
            instructions_retired,
        } => {
            assert_eq!(tier, ExecutedTier::Jit);
            assert_eq!(got_entry, entry_rip);
            assert_eq!(got_next, next_rip);
            assert_eq!(instructions_retired, u64::from(insts));
        }
        other => panic!("unexpected outcome: {other:?}"),
    }
    assert_eq!(cpu.cpu.state.msr.tsc, initial_tsc + u64::from(insts));

    // The next step must run the interpreter once, even though `next_rip` is compiled.
    match dispatcher.step(&mut cpu) {
        StepOutcome::Block {
            tier,
            entry_rip: got_entry,
            next_rip: got_next,
            instructions_retired,
        } => {
            assert_eq!(tier, ExecutedTier::Interpreter);
            assert_eq!(got_entry, next_rip);
            assert_eq!(got_next, next_rip);
            assert_eq!(instructions_retired, 0);
        }
        other => panic!("unexpected outcome: {other:?}"),
    }
    // No extra retirement/time advancement from the no-op interpreter.
    assert_eq!(cpu.cpu.state.msr.tsc, initial_tsc + u64::from(insts));
}

#[test]
fn rollback_exit_to_interpreter_forces_one_interpreter_step() {
    let entry_rip = 0x1000u64;

    let backend = OneShotBackend {
        exit: JitBlockExit {
            // Roll back and re-execute from the same RIP.
            next_rip: entry_rip,
            exit_to_interpreter: true,
            committed: false,
        },
        executed: false,
    };
    let config = JitConfig {
        enabled: true,
        hot_threshold: 1,
        cache_max_blocks: 16,
        cache_max_bytes: 0,
    };
    let jit = JitRuntime::new(config, backend, NullCompileSink);
    let mut dispatcher = ExecDispatcher::new(NoopInterpreter, jit);

    // Install a compiled handle for `entry_rip` so the dispatcher would normally try to run it
    // again on the next step unless the `exit_to_interpreter` sticky flag is respected.
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
            assert_eq!(instructions_retired, 0);
        }
        other => panic!("unexpected outcome: {other:?}"),
    }

    // Must run interpreter once (and *not* execute the JIT block again).
    match dispatcher.step(&mut cpu) {
        StepOutcome::Block {
            tier,
            instructions_retired,
            ..
        } => {
            assert_eq!(tier, ExecutedTier::Interpreter);
            assert_eq!(instructions_retired, 0);
        }
        other => panic!("unexpected outcome: {other:?}"),
    }
}

#[test]
fn inhibit_interrupts_after_block_creates_and_ages_shadow() {
    let entry_a = 0x1000u64;
    let entry_b = 0x2000u64;
    let vector = 0x20u8;

    let backend = TwoExitBackend {
        exit0: JitBlockExit {
            next_rip: entry_b,
            exit_to_interpreter: false,
            committed: true,
        },
        exit1: JitBlockExit {
            next_rip: entry_b,
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
    let mut dispatcher = ExecDispatcher::new(NoopInterpreter, jit);

    // Block A: creates an interrupt shadow after it retires (e.g. STI/MOV SS semantics).
    {
        let jit = dispatcher.jit_mut();
        let mut meta = jit.make_meta(0, 0);
        meta.instruction_count = 1;
        meta.inhibit_interrupts_after_block = true;
        jit.install_handle(CompiledBlockHandle {
            entry_rip: entry_a,
            table_index: 0,
            meta,
        });
    }

    // Block B: a normal instruction that does not create a new shadow but should age an existing
    // one.
    {
        let jit = dispatcher.jit_mut();
        let mut meta = jit.make_meta(0, 0);
        meta.instruction_count = 1;
        meta.inhibit_interrupts_after_block = false;
        jit.install_handle(CompiledBlockHandle {
            entry_rip: entry_b,
            table_index: 1,
            meta,
        });
    }

    let mut core = CpuCore::new(CpuMode::Real);
    core.state.set_rip(entry_a);
    core.state.set_flag(RFLAGS_IF, true);
    core.state.write_gpr16(gpr::RSP, 0x8000);

    let mut bus = FlatTestBus::new(0x20000);
    install_ivt_entry(&mut bus, vector, 0x0000, 0x1234);
    let mut cpu = Vcpu::new(core, bus);

    // Step 1: execute block A, which creates the shadow. No interrupt pending yet, so it should run.
    match dispatcher.step(&mut cpu) {
        StepOutcome::Block { tier, .. } => assert_eq!(tier, ExecutedTier::Jit),
        other => panic!("unexpected outcome: {other:?}"),
    }
    assert_eq!(cpu.cpu.pending.interrupt_inhibit(), 1);

    // Now an external interrupt becomes pending. The shadow should block immediate delivery at the
    // next instruction boundary.
    cpu.cpu.pending.inject_external_interrupt(vector);

    // Step 2: shadow active, so interrupt is *not* delivered. Instead we execute block B, which
    // ages the shadow.
    match dispatcher.step(&mut cpu) {
        StepOutcome::Block { tier, .. } => assert_eq!(tier, ExecutedTier::Jit),
        other => panic!("unexpected outcome: {other:?}"),
    }
    assert_eq!(cpu.cpu.pending.external_interrupts.len(), 1);
    assert_eq!(cpu.cpu.pending.interrupt_inhibit(), 0);

    // Step 3: shadow expired; interrupt is delivered.
    assert_eq!(dispatcher.step(&mut cpu), StepOutcome::InterruptDelivered);
    assert!(cpu.cpu.pending.external_interrupts.is_empty());
    assert_eq!(cpu.cpu.state.rip(), 0x1234);
}

#[test]
fn rollback_does_not_create_interrupt_shadow_even_if_meta_requests_it() {
    let entry_rip = 0x1000u64;
    let vector = 0x20u8;
    let initial_tsc = 123u64;

    let backend = FixedExitBackend {
        exit: JitBlockExit {
            next_rip: entry_rip,
            exit_to_interpreter: false,
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
    let mut dispatcher = ExecDispatcher::new(NoopInterpreter, jit);

    // Install a compiled handle that claims it would create an interrupt shadow if the block
    // retired. Because the backend rolls back (`committed=false`), the dispatcher must not apply
    // the shadow.
    {
        let jit = dispatcher.jit_mut();
        let mut meta = jit.make_meta(0, 0);
        meta.instruction_count = 1;
        meta.inhibit_interrupts_after_block = true;
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

    let mut bus = FlatTestBus::new(0x20000);
    install_ivt_entry(&mut bus, vector, 0x0000, 0x1234);
    let mut cpu = Vcpu::new(core, bus);

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
    assert_eq!(
        cpu.cpu.pending.interrupt_inhibit(),
        0,
        "rollback must not create an interrupt shadow"
    );

    // If the shadow was incorrectly applied, this interrupt would be delayed.
    cpu.cpu.pending.inject_external_interrupt(vector);
    assert_eq!(dispatcher.step(&mut cpu), StepOutcome::InterruptDelivered);
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
    let mut dispatcher = ExecDispatcher::new(NoopInterpreter, jit);

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
    let mut dispatcher = ExecDispatcher::new(NoopInterpreter, jit);

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
    let mut dispatcher = ExecDispatcher::new(NoopInterpreter, jit);

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
