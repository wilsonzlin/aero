use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use aero_cpu_core::exec::{ExecCpu, ExecDispatcher, ExecutedTier, Interpreter, StepOutcome};
use aero_cpu_core::jit::cache::{CodeCache, CompiledBlockHandle, CompiledBlockMeta};
use aero_cpu_core::jit::runtime::{
    CompileRequestSink, JitBackend, JitBlockExit, JitConfig, JitRuntime,
};

#[derive(Debug, Default, Clone)]
struct TestCpu {
    rip: u64,
    acc: u64,
    interrupts_enabled: bool,
    interrupt_shadow: u8,
    pending_interrupts: u32,
    delivered_interrupts: u32,
}

impl TestCpu {
    fn request_interrupt(&mut self) {
        self.pending_interrupts = self.pending_interrupts.saturating_add(1);
    }

    fn begin_instruction(&mut self) {}

    fn end_instruction(&mut self) {
        if self.interrupt_shadow > 0 {
            self.interrupt_shadow -= 1;
        }
    }
}

impl ExecCpu for TestCpu {
    fn rip(&self) -> u64 {
        self.rip
    }

    fn set_rip(&mut self, rip: u64) {
        self.rip = rip;
    }

    fn maybe_deliver_interrupt(&mut self) -> bool {
        if self.pending_interrupts == 0 {
            return false;
        }
        if !self.interrupts_enabled {
            return false;
        }
        if self.interrupt_shadow != 0 {
            return false;
        }

        self.pending_interrupts -= 1;
        self.delivered_interrupts = self.delivered_interrupts.saturating_add(1);
        true
    }
}

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

#[derive(Default)]
struct TestJitBackend {
    blocks: HashMap<u32, Box<dyn FnMut(&mut TestCpu) -> JitBlockExit>>,
}

impl TestJitBackend {
    fn install<F>(&mut self, table_index: u32, f: F)
    where
        F: FnMut(&mut TestCpu) -> JitBlockExit + 'static,
    {
        self.blocks.insert(table_index, Box::new(f));
    }
}

impl JitBackend for TestJitBackend {
    type Cpu = TestCpu;

    fn execute(&mut self, table_index: u32, cpu: &mut TestCpu) -> JitBlockExit {
        self.blocks
            .get_mut(&table_index)
            .expect("missing table entry")(cpu)
    }
}

#[derive(Default)]
struct TestInterpreter {
    steps: HashMap<u64, Box<dyn FnMut(&mut TestCpu) -> u64>>,
}

impl TestInterpreter {
    fn install<F>(&mut self, entry_rip: u64, f: F)
    where
        F: FnMut(&mut TestCpu) -> u64 + 'static,
    {
        self.steps.insert(entry_rip, Box::new(f));
    }
}

impl Interpreter<TestCpu> for TestInterpreter {
    fn exec_block(&mut self, cpu: &mut TestCpu) -> u64 {
        let rip = cpu.rip();
        cpu.begin_instruction();
        let next = self.steps.get_mut(&rip).expect("no interp step")(cpu);
        cpu.end_instruction();
        cpu.maybe_deliver_interrupt();
        next
    }
}

#[test]
fn hotness_threshold_triggers_compile_request_once() {
    let config = JitConfig {
        enabled: true,
        hot_threshold: 3,
        cache_max_blocks: 16,
        cache_max_bytes: 0,
    };
    let compile = RecordingCompileSink::default();
    let mut jit = JitRuntime::new(config, TestJitBackend::default(), compile.clone());

    for _ in 0..5 {
        assert!(jit.prepare_block(0).is_none());
    }

    assert_eq!(compile.snapshot(), vec![0]);
}

#[test]
fn code_cache_eviction_is_lru_and_size_capped() {
    fn handle(entry_rip: u64, byte_len: u32) -> CompiledBlockHandle {
        CompiledBlockHandle {
            entry_rip,
            table_index: entry_rip as u32,
            meta: CompiledBlockMeta {
                code_paddr: entry_rip,
                byte_len,
                page_versions: Vec::new(),
            },
        }
    }

    let mut cache = CodeCache::new(2, 0);
    assert!(cache.insert(handle(0, 10)).is_empty());
    assert!(cache.insert(handle(1, 10)).is_empty());

    cache.get_cloned(0);

    let evicted = cache.insert(handle(2, 10));
    assert_eq!(evicted, vec![1]);
    assert!(cache.contains(0));
    assert!(!cache.contains(1));
    assert!(cache.contains(2));

    let mut cache = CodeCache::new(10, 15);
    assert!(cache.insert(handle(10, 10)).is_empty());
    let evicted = cache.insert(handle(11, 10));
    assert_eq!(evicted, vec![10]);
    assert!(!cache.contains(10));
    assert!(cache.contains(11));
    assert!(cache.current_bytes() <= 15);
}

#[test]
fn page_version_invalidation_evicts_and_requests_recompile() {
    let config = JitConfig {
        enabled: true,
        hot_threshold: 1_000,
        cache_max_blocks: 16,
        cache_max_bytes: 0,
    };
    let compile = RecordingCompileSink::default();
    let mut jit = JitRuntime::new(config, TestJitBackend::default(), compile.clone());

    jit.install_block(0, 0, 0x1000, 8);
    assert!(jit.is_compiled(0));

    assert!(jit.prepare_block(0).is_some());
    assert!(compile.snapshot().is_empty());

    jit.on_guest_write(0x1004, 1);
    assert!(jit.prepare_block(0).is_none());
    assert!(!jit.is_compiled(0));

    assert_eq!(compile.snapshot(), vec![0]);
}

#[test]
fn stale_page_version_snapshot_rejected_on_install() {
    let config = JitConfig {
        enabled: true,
        hot_threshold: 1_000,
        cache_max_blocks: 16,
        cache_max_bytes: 0,
    };

    let compile = RecordingCompileSink::default();
    let mut jit = JitRuntime::new(config, TestJitBackend::default(), compile.clone());

    let meta = jit.snapshot_meta(0x6000, 8);
    jit.on_guest_write(0x6000, 1);

    let handle = CompiledBlockHandle {
        entry_rip: 0,
        table_index: 0,
        meta,
    };
    jit.install_handle(handle);

    assert!(!jit.is_compiled(0));
    assert_eq!(compile.snapshot(), vec![0]);
}

#[test]
fn stale_install_does_not_evict_newer_valid_block() {
    let config = JitConfig {
        enabled: true,
        hot_threshold: 1_000,
        cache_max_blocks: 16,
        cache_max_bytes: 0,
    };

    let compile = RecordingCompileSink::default();
    let mut jit = JitRuntime::new(config, TestJitBackend::default(), compile.clone());

    // Capture a snapshot before the code page changes (simulating a background compilation job).
    let stale_meta = jit.snapshot_meta(0x7000, 8);

    // Code page changes, invalidating the snapshot.
    jit.on_guest_write(0x7000, 1);

    // Install a newer (valid) compiled block that matches the current version.
    jit.install_block(0, 0, 0x7000, 8);
    assert!(jit.prepare_block(0).is_some());

    // A stale compilation result arrives late; it must not replace/evict the valid block.
    let stale_handle = CompiledBlockHandle {
        entry_rip: 0,
        table_index: 123,
        meta: stale_meta,
    };
    jit.install_handle(stale_handle);

    assert!(jit.prepare_block(0).is_some());
    assert!(compile.snapshot().is_empty());
}

#[test]
fn mixed_mode_exit_to_interpreter_forces_one_interpreter_block() {
    let config = JitConfig {
        enabled: true,
        hot_threshold: 1_000,
        cache_max_blocks: 16,
        cache_max_bytes: 0,
    };

    let mut backend = TestJitBackend::default();
    backend.install(0, |cpu: &mut TestCpu| {
        cpu.acc += 10;
        JitBlockExit {
            next_rip: 1,
            exit_to_interpreter: true,
        }
    });
    backend.install(1, |cpu: &mut TestCpu| {
        cpu.acc += 100;
        JitBlockExit {
            next_rip: 2,
            exit_to_interpreter: false,
        }
    });

    let compile = RecordingCompileSink::default();
    let mut jit = JitRuntime::new(config, backend, compile);
    jit.install_block(0, 0, 0x2000, 4);
    jit.install_block(1, 1, 0x3000, 4);

    let mut interp = TestInterpreter::default();
    interp.install(1, |cpu: &mut TestCpu| {
        cpu.acc += 1;
        2
    });

    let mut dispatcher = ExecDispatcher::new(interp, jit);
    let mut cpu = TestCpu {
        rip: 0,
        ..TestCpu::default()
    };

    match dispatcher.step(&mut cpu) {
        StepOutcome::Block { tier, next_rip, .. } => {
            assert_eq!(tier, ExecutedTier::Jit);
            assert_eq!(next_rip, 1);
        }
        _ => panic!("expected block execution"),
    }

    match dispatcher.step(&mut cpu) {
        StepOutcome::Block { tier, next_rip, .. } => {
            assert_eq!(tier, ExecutedTier::Interpreter);
            assert_eq!(next_rip, 2);
        }
        _ => panic!("expected block execution"),
    }

    assert_eq!(cpu.acc, 11);
    assert_eq!(cpu.rip, 2);
}

#[test]
fn interrupt_shadow_is_respected_across_jit_blocks() {
    let config = JitConfig {
        enabled: true,
        hot_threshold: 1_000,
        cache_max_blocks: 16,
        cache_max_bytes: 0,
    };

    let mut backend = TestJitBackend::default();
    backend.install(0, |cpu: &mut TestCpu| {
        cpu.interrupts_enabled = true;
        cpu.interrupt_shadow = 1;
        JitBlockExit {
            next_rip: 1,
            exit_to_interpreter: false,
        }
    });

    let compile = RecordingCompileSink::default();
    let mut jit = JitRuntime::new(config, backend, compile);
    jit.install_block(0, 0, 0x4000, 4);

    let mut interp = TestInterpreter::default();
    interp.install(1, |_cpu: &mut TestCpu| 2);

    let mut dispatcher = ExecDispatcher::new(interp, jit);
    let mut cpu = TestCpu {
        rip: 0,
        interrupts_enabled: false,
        pending_interrupts: 1,
        ..TestCpu::default()
    };

    dispatcher.step(&mut cpu);
    assert_eq!(cpu.rip, 1);
    assert_eq!(cpu.pending_interrupts, 1);
    assert_eq!(cpu.delivered_interrupts, 0);
    assert_eq!(cpu.interrupt_shadow, 1);

    dispatcher.step(&mut cpu);
    assert_eq!(cpu.pending_interrupts, 0);
    assert_eq!(cpu.delivered_interrupts, 1);
    assert_eq!(cpu.interrupt_shadow, 0);
}

#[test]
fn pending_interrupt_delivered_at_jit_block_boundaries() {
    let config = JitConfig {
        enabled: true,
        hot_threshold: 1_000,
        cache_max_blocks: 16,
        cache_max_bytes: 0,
    };

    let mut backend = TestJitBackend::default();
    backend.install(0, |cpu: &mut TestCpu| {
        cpu.request_interrupt();
        JitBlockExit {
            next_rip: 1,
            exit_to_interpreter: false,
        }
    });

    let compile = RecordingCompileSink::default();
    let mut jit = JitRuntime::new(config, backend, compile);
    jit.install_block(0, 0, 0x5000, 4);

    let mut interp = TestInterpreter::default();
    interp.install(1, |_cpu: &mut TestCpu| 2);

    let mut dispatcher = ExecDispatcher::new(interp, jit);
    let mut cpu = TestCpu {
        rip: 0,
        interrupts_enabled: true,
        ..TestCpu::default()
    };

    dispatcher.step(&mut cpu);
    assert_eq!(cpu.rip, 1);
    assert_eq!(cpu.pending_interrupts, 1);

    assert_eq!(dispatcher.step(&mut cpu), StepOutcome::InterruptDelivered);
    assert_eq!(cpu.pending_interrupts, 0);
    assert_eq!(cpu.delivered_interrupts, 1);

    dispatcher.step(&mut cpu);
    assert_eq!(cpu.rip, 2);
}
