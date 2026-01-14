use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use aero_cpu_core::exec::{
    ExecCpu, ExecDispatcher, ExecutedTier, Interpreter, InterpreterBlockExit, StepOutcome,
};
use aero_cpu_core::jit::cache::{CodeCache, CompiledBlockHandle, CompiledBlockMeta};
use aero_cpu_core::jit::profile::HotnessProfile;
use aero_cpu_core::jit::runtime::{
    CompileRequestSink, JitBackend, JitBlockExit, JitConfig, JitRuntime, DEFAULT_CODE_VERSION_MAX_PAGES,
};
use aero_cpu_core::jit::JitMetricsSink;

type JitBlockFn = Box<dyn FnMut(&mut TestCpu) -> JitBlockExit>;
type InterpreterStepFn = Box<dyn FnMut(&mut TestCpu) -> u64>;

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

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct JitMetricCounts {
    cache_hits: u64,
    cache_misses: u64,
    installs: u64,
    evictions: u64,
    invalidations: u64,
    stale_install_rejects: u64,
    compile_requests: u64,
}

#[derive(Debug, Default)]
struct RecordingMetricsSink {
    cache_hits: AtomicU64,
    cache_misses: AtomicU64,
    installs: AtomicU64,
    evictions: AtomicU64,
    invalidations: AtomicU64,
    stale_install_rejects: AtomicU64,
    compile_requests: AtomicU64,
}

impl RecordingMetricsSink {
    fn snapshot(&self) -> JitMetricCounts {
        JitMetricCounts {
            cache_hits: self.cache_hits.load(Ordering::Relaxed),
            cache_misses: self.cache_misses.load(Ordering::Relaxed),
            installs: self.installs.load(Ordering::Relaxed),
            evictions: self.evictions.load(Ordering::Relaxed),
            invalidations: self.invalidations.load(Ordering::Relaxed),
            stale_install_rejects: self.stale_install_rejects.load(Ordering::Relaxed),
            compile_requests: self.compile_requests.load(Ordering::Relaxed),
        }
    }
}

impl JitMetricsSink for RecordingMetricsSink {
    fn record_cache_hit(&self) {
        self.cache_hits.fetch_add(1, Ordering::Relaxed);
    }

    fn record_cache_miss(&self) {
        self.cache_misses.fetch_add(1, Ordering::Relaxed);
    }

    fn record_install(&self) {
        self.installs.fetch_add(1, Ordering::Relaxed);
    }

    fn record_evict(&self, n: u64) {
        self.evictions.fetch_add(n, Ordering::Relaxed);
    }

    fn record_invalidate(&self) {
        self.invalidations.fetch_add(1, Ordering::Relaxed);
    }

    fn record_stale_install_reject(&self) {
        self.stale_install_rejects
            .fetch_add(1, Ordering::Relaxed);
    }

    fn record_compile_request(&self) {
        self.compile_requests.fetch_add(1, Ordering::Relaxed);
    }

    fn set_cache_bytes(&self, _used: u64, _capacity: u64) {}
}

#[derive(Default)]
struct TestJitBackend {
    blocks: HashMap<u32, JitBlockFn>,
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
    steps: HashMap<u64, InterpreterStepFn>,
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
    fn exec_block(&mut self, cpu: &mut TestCpu) -> InterpreterBlockExit {
        let rip = cpu.rip();
        cpu.begin_instruction();
        let next = self.steps.get_mut(&rip).expect("no interp step")(cpu);
        cpu.end_instruction();
        cpu.maybe_deliver_interrupt();
        InterpreterBlockExit {
            next_rip: next,
            instructions_retired: 1,
        }
    }
}

#[test]
fn hotness_threshold_triggers_compile_request_once() {
    let config = JitConfig {
        enabled: true,
        hot_threshold: 3,
        cache_max_blocks: 16,
        cache_max_bytes: 0,
        code_version_max_pages: DEFAULT_CODE_VERSION_MAX_PAGES,
    };
    let compile = RecordingCompileSink::default();
    let metrics = Arc::new(RecordingMetricsSink::default());
    let mut jit = JitRuntime::new(config, TestJitBackend::default(), compile.clone());
    jit.set_metrics_sink(Some(metrics.clone()));

    for _ in 0..5 {
        assert!(jit.prepare_block(0).is_none());
    }

    assert_eq!(compile.snapshot(), vec![0]);
    let stats = jit.stats().snapshot();
    assert_eq!(stats.cache_hit, 0);
    assert_eq!(stats.cache_miss, 5);
    assert_eq!(stats.compile_requests, 1);

    assert_eq!(
        metrics.snapshot(),
        JitMetricCounts {
            cache_hits: 0,
            cache_misses: 5,
            installs: 0,
            evictions: 0,
            invalidations: 0,
            stale_install_rejects: 0,
            compile_requests: 1,
        }
    );
}

#[test]
fn hotness_profile_is_memory_bounded() {
    let config = JitConfig {
        enabled: true,
        // Avoid triggering compilation; we just want to churn the hotness table.
        hot_threshold: 1_000_000,
        cache_max_blocks: 16,
        cache_max_bytes: 0,
        code_version_max_pages: DEFAULT_CODE_VERSION_MAX_PAGES,
    };
    let mut jit = JitRuntime::new(config.clone(), TestJitBackend::default(), RecordingCompileSink::default());

    let capacity = HotnessProfile::recommended_capacity(config.cache_max_blocks);
    let total = capacity * 2;
    for entry_rip in 0..(total as u64) {
        jit.prepare_block(entry_rip);
    }

    let mut present = 0usize;
    for entry_rip in 0..(total as u64) {
        if jit.hotness(entry_rip) != 0 {
            present += 1;
        }
    }

    assert!(present <= capacity, "hotness table exceeded capacity: {present} > {capacity}");
    assert_eq!(present, capacity);
    assert_eq!(jit.hotness(0), 0, "old entries should be evicted once capacity is exceeded");
}

#[test]
fn hot_blocks_still_trigger_compile_requests_under_eviction_pressure() {
    let config = JitConfig {
        enabled: true,
        hot_threshold: 5,
        cache_max_blocks: 16,
        cache_max_bytes: 0,
        code_version_max_pages: DEFAULT_CODE_VERSION_MAX_PAGES,
    };
    let compile = RecordingCompileSink::default();
    let mut jit = JitRuntime::new(config.clone(), TestJitBackend::default(), compile.clone());

    let hot_rips = [0x10u64, 0x20, 0x30];

    // Give the hot entries a small head start so they aren't arbitrarily evicted among the 1-hit
    // cold entries when the table first fills.
    for _ in 0..2 {
        for &rip in &hot_rips {
            jit.prepare_block(rip);
        }
    }

    let capacity = HotnessProfile::recommended_capacity(config.cache_max_blocks);
    for i in 0..(capacity * 4) {
        jit.prepare_block(hot_rips[i % hot_rips.len()]);
        // Unique cold RIPs to force table churn.
        jit.prepare_block(0x1000 + (i as u64));
    }

    let mut requested = compile.snapshot();
    requested.sort_unstable();
    assert_eq!(requested, hot_rips.to_vec());
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
                page_versions_generation: 0,
                page_versions: Vec::new(),
                instruction_count: 0,
                inhibit_interrupts_after_block: false,
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
fn code_cache_touch_many_times_does_not_break_eviction_or_accounting() {
    fn handle(entry_rip: u64) -> CompiledBlockHandle {
        CompiledBlockHandle {
            entry_rip,
            table_index: entry_rip as u32,
            meta: CompiledBlockMeta {
                code_paddr: entry_rip,
                byte_len: 10,
                page_versions_generation: 0,
                page_versions: Vec::new(),
                instruction_count: 0,
                inhibit_interrupts_after_block: false,
            },
        }
    }

    // Size limit is enforced by bytes (not block count) so we can stress insertion/eviction without
    // relying on the block-count path.
    let mut cache = CodeCache::new(1024, 20);
    assert!(cache.insert(handle(0)).is_empty());
    assert!(cache.insert(handle(1)).is_empty());
    assert_eq!(cache.current_bytes(), 20);

    // Repeated touches of the same key should not grow internal bookkeeping (e.g. accidental LRU
    // duplication) and should not affect byte accounting.
    for _ in 0..10_000 {
        assert!(cache.get_cloned(0).is_some());
    }
    assert_eq!(cache.current_bytes(), 20);
    assert_eq!(cache.len(), 2);

    // Inserting new blocks should always evict the least-recently-used (the non-touched entry),
    // while keeping accounting stable.
    let evicted = cache.insert(handle(2));
    assert_eq!(evicted, vec![1]);
    assert!(cache.contains(0));
    assert!(!cache.contains(1));
    assert!(cache.contains(2));
    assert_eq!(cache.len(), 2);
    assert_eq!(cache.current_bytes(), 20);

    for rip in 3..100u64 {
        // Keep entry 0 hot.
        for _ in 0..100 {
            cache.get_cloned(0);
        }
        let evicted = cache.insert(handle(rip));
        assert_eq!(evicted, vec![rip - 1]);
        assert!(cache.contains(0));
        assert!(cache.contains(rip));
        assert!(!cache.contains(rip - 1));
        assert_eq!(cache.len(), 2);
        assert_eq!(cache.current_bytes(), 20);
    }
}

#[test]
fn page_version_invalidation_evicts_and_requests_recompile() {
    let config = JitConfig {
        enabled: true,
        hot_threshold: 1_000,
        cache_max_blocks: 16,
        cache_max_bytes: 0,
        code_version_max_pages: DEFAULT_CODE_VERSION_MAX_PAGES,
    };
    let compile = RecordingCompileSink::default();
    let metrics = Arc::new(RecordingMetricsSink::default());
    let mut jit = JitRuntime::new(config, TestJitBackend::default(), compile.clone());
    jit.set_metrics_sink(Some(metrics.clone()));

    jit.install_block(0, 0, 0x1000, 8);
    assert!(jit.is_compiled(0));

    assert!(jit.prepare_block(0).is_some());
    assert!(compile.snapshot().is_empty());

    jit.on_guest_write(0x1004, 1);
    assert!(jit.prepare_block(0).is_none());
    assert!(!jit.is_compiled(0));

    assert_eq!(compile.snapshot(), vec![0]);
    let stats = jit.stats().snapshot();
    assert_eq!(stats.install_ok, 1);
    assert_eq!(stats.cache_hit, 1);
    assert_eq!(stats.cache_miss, 1);
    assert_eq!(stats.compile_requests, 1);

    assert_eq!(
        metrics.snapshot(),
        JitMetricCounts {
            cache_hits: 1,
            cache_misses: 1,
            installs: 1,
            evictions: 0,
            invalidations: 1,
            stale_install_rejects: 0,
            compile_requests: 1,
        }
    );
}

#[test]
fn stale_page_version_snapshot_rejected_on_install() {
    let config = JitConfig {
        enabled: true,
        hot_threshold: 1_000,
        cache_max_blocks: 16,
        cache_max_bytes: 0,
        code_version_max_pages: DEFAULT_CODE_VERSION_MAX_PAGES,
    };

    let compile = RecordingCompileSink::default();
    let metrics = Arc::new(RecordingMetricsSink::default());
    let mut jit = JitRuntime::new(config, TestJitBackend::default(), compile.clone());
    jit.set_metrics_sink(Some(metrics.clone()));

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
    let stats = jit.stats().snapshot();
    assert_eq!(stats.install_ok, 0);
    assert_eq!(stats.install_rejected_stale, 1);
    assert_eq!(stats.compile_requests, 1);

    assert_eq!(
        metrics.snapshot(),
        JitMetricCounts {
            cache_hits: 0,
            cache_misses: 0,
            installs: 0,
            evictions: 0,
            invalidations: 0,
            stale_install_rejects: 1,
            compile_requests: 1,
        }
    );
}

#[test]
fn stale_install_does_not_evict_newer_valid_block() {
    let config = JitConfig {
        enabled: true,
        hot_threshold: 1_000,
        cache_max_blocks: 16,
        cache_max_bytes: 0,
        code_version_max_pages: DEFAULT_CODE_VERSION_MAX_PAGES,
    };

    let compile = RecordingCompileSink::default();
    let metrics = Arc::new(RecordingMetricsSink::default());
    let mut jit = JitRuntime::new(config, TestJitBackend::default(), compile.clone());
    jit.set_metrics_sink(Some(metrics.clone()));

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
    let stats = jit.stats().snapshot();
    assert_eq!(stats.install_ok, 1);
    assert_eq!(stats.install_rejected_stale, 1);
    assert_eq!(stats.cache_hit, 2);
    assert_eq!(stats.compile_requests, 0);
    assert_eq!(
        metrics.snapshot(),
        JitMetricCounts {
            cache_hits: 2,
            cache_misses: 0,
            installs: 1,
            evictions: 0,
            invalidations: 0,
            stale_install_rejects: 1,
            compile_requests: 0,
        }
    );
}

#[test]
fn runtime_stats_counts_cache_evictions() {
    let config = JitConfig {
        enabled: true,
        hot_threshold: 1_000,
        cache_max_blocks: 1,
        cache_max_bytes: 0,
        code_version_max_pages: DEFAULT_CODE_VERSION_MAX_PAGES,
    };

    let compile = RecordingCompileSink::default();
    let mut jit = JitRuntime::new(config, TestJitBackend::default(), compile);

    assert!(jit.install_block(0, 0, 0x1000, 4).is_empty());
    let evicted = jit.install_block(1, 1, 0x2000, 4);
    assert_eq!(evicted, vec![0]);
    assert!(!jit.is_compiled(0));
    assert!(jit.is_compiled(1));

    let stats = jit.stats().snapshot();
    assert_eq!(stats.install_ok, 2);
    assert_eq!(stats.evictions, 1);
}

#[test]
fn runtime_stats_counts_invalidate_calls() {
    let config = JitConfig {
        enabled: true,
        hot_threshold: 1_000,
        cache_max_blocks: 16,
        cache_max_bytes: 0,
        code_version_max_pages: DEFAULT_CODE_VERSION_MAX_PAGES,
    };

    let compile = RecordingCompileSink::default();
    let mut jit = JitRuntime::new(config, TestJitBackend::default(), compile);

    jit.install_block(0, 0, 0x1000, 4);
    assert!(jit.invalidate_block(0));
    assert!(!jit.is_compiled(0));
    assert!(!jit.invalidate_block(0));

    let stats = jit.stats().snapshot();
    assert_eq!(stats.invalidate_calls, 2);
}

#[test]
fn runtime_reset_clears_cache_hotness_and_page_versions() {
    let config = JitConfig {
        enabled: true,
        hot_threshold: 1_000,
        cache_max_blocks: 16,
        cache_max_bytes: 0,
        code_version_max_pages: DEFAULT_CODE_VERSION_MAX_PAGES,
    };

    let compile = RecordingCompileSink::default();
    let mut jit = JitRuntime::new(config, TestJitBackend::default(), compile);

    jit.install_block(0, 0, 0x1000, 8);
    assert_eq!(jit.cache_len(), 1);
    assert!(jit.is_compiled(0));

    for _ in 0..5 {
        assert!(jit.prepare_block(0).is_some());
    }
    assert_eq!(jit.hotness(0), 5);

    // Mutate code to ensure the page-version tracker has non-zero state.
    jit.on_guest_write(0x1004, 1);
    let before = jit.snapshot_meta(0x1000, 8);
    assert_eq!(before.page_versions.len(), 1);
    assert_ne!(before.page_versions[0].version, 0);

    jit.reset();

    assert_eq!(jit.cache_len(), 0);
    assert!(!jit.is_compiled(0));
    assert_eq!(jit.hotness(0), 0);

    let after = jit.snapshot_meta(0x1000, 8);
    assert_eq!(after.page_versions.len(), 1);
    assert_eq!(after.page_versions[0].version, 0);
}

#[test]
fn runtime_reset_allows_retriggering_compile_requests() {
    let config = JitConfig {
        enabled: true,
        hot_threshold: 3,
        cache_max_blocks: 16,
        cache_max_bytes: 0,
        code_version_max_pages: DEFAULT_CODE_VERSION_MAX_PAGES,
    };

    let compile = RecordingCompileSink::default();
    let mut jit = JitRuntime::new(config, TestJitBackend::default(), compile.clone());

    for _ in 0..3 {
        assert!(jit.prepare_block(0x42).is_none());
    }
    assert_eq!(compile.snapshot(), vec![0x42]);

    jit.reset();

    for _ in 0..3 {
        assert!(jit.prepare_block(0x42).is_none());
    }
    assert_eq!(compile.snapshot(), vec![0x42, 0x42]);
}

#[test]
fn jit_metrics_record_eviction_and_explicit_invalidation() {
    let config = JitConfig {
        enabled: true,
        hot_threshold: 1_000,
        cache_max_blocks: 1,
        cache_max_bytes: 0,
        code_version_max_pages: DEFAULT_CODE_VERSION_MAX_PAGES,
    };

    let compile = RecordingCompileSink::default();
    let metrics = Arc::new(RecordingMetricsSink::default());
    let mut jit = JitRuntime::new(config, TestJitBackend::default(), compile.clone());
    jit.set_metrics_sink(Some(metrics.clone()));

    // First install: no eviction.
    jit.install_block(0, 0, 0x1000, 4);
    // Second install forces eviction due to max_blocks=1.
    jit.install_block(1, 1, 0x2000, 4);

    assert_eq!(jit.cache_len(), 1);

    // Explicit invalidation should record an invalidation event.
    assert!(jit.invalidate_block(1));
    assert!(!jit.invalidate_block(1));

    assert!(compile.snapshot().is_empty());
    assert_eq!(
        metrics.snapshot(),
        JitMetricCounts {
            cache_hits: 0,
            cache_misses: 0,
            installs: 2,
            evictions: 1,
            invalidations: 1,
            stale_install_rejects: 0,
            compile_requests: 0,
        }
    );
}

#[test]
fn jit_metrics_sink_none_emits_no_events() {
    let config = JitConfig {
        enabled: true,
        hot_threshold: 2,
        cache_max_blocks: 1,
        cache_max_bytes: 0,
        code_version_max_pages: DEFAULT_CODE_VERSION_MAX_PAGES,
    };

    let compile = RecordingCompileSink::default();
    let metrics = Arc::new(RecordingMetricsSink::default());

    // Run through some typical runtime events, but do not install a metrics sink.
    let mut jit = JitRuntime::new(config, TestJitBackend::default(), compile.clone());
    assert!(jit.prepare_block(0).is_none());
    assert!(jit.prepare_block(0).is_none());
    jit.install_block(0, 0, 0x1000, 4);
    assert!(jit.prepare_block(0).is_some());
    assert!(jit.invalidate_block(0));

    // The runtime should still have performed its normal compile-request bookkeeping...
    assert_eq!(compile.snapshot(), vec![0]);
    // ...but the external metrics sink must observe nothing.
    assert_eq!(metrics.snapshot(), JitMetricCounts::default());
}

#[test]
fn mixed_mode_exit_to_interpreter_forces_one_interpreter_block() {
    let config = JitConfig {
        enabled: true,
        hot_threshold: 1_000,
        cache_max_blocks: 16,
        cache_max_bytes: 0,
        code_version_max_pages: DEFAULT_CODE_VERSION_MAX_PAGES,
    };

    let mut backend = TestJitBackend::default();
    backend.install(0, |cpu: &mut TestCpu| {
        cpu.acc += 10;
        JitBlockExit {
            next_rip: 1,
            exit_to_interpreter: true,
            committed: true,
        }
    });
    backend.install(1, |cpu: &mut TestCpu| {
        cpu.acc += 100;
        JitBlockExit {
            next_rip: 2,
            exit_to_interpreter: false,
            committed: true,
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
        code_version_max_pages: DEFAULT_CODE_VERSION_MAX_PAGES,
    };

    let mut backend = TestJitBackend::default();
    backend.install(0, |cpu: &mut TestCpu| {
        cpu.interrupts_enabled = true;
        cpu.interrupt_shadow = 1;
        JitBlockExit {
            next_rip: 1,
            exit_to_interpreter: false,
            committed: true,
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
        code_version_max_pages: DEFAULT_CODE_VERSION_MAX_PAGES,
    };

    let mut backend = TestJitBackend::default();
    backend.install(0, |cpu: &mut TestCpu| {
        cpu.request_interrupt();
        JitBlockExit {
            next_rip: 1,
            exit_to_interpreter: false,
            committed: true,
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

#[test]
fn step_reports_retired_instruction_counts_across_tiers() {
    let config = JitConfig {
        enabled: true,
        hot_threshold: 1_000,
        cache_max_blocks: 16,
        cache_max_bytes: 0,
        code_version_max_pages: DEFAULT_CODE_VERSION_MAX_PAGES,
    };

    let mut backend = TestJitBackend::default();
    backend.install(0, |cpu: &mut TestCpu| {
        cpu.acc += 10;
        JitBlockExit {
            next_rip: 2,
            exit_to_interpreter: false,
            committed: true,
        }
    });
    backend.install(1, |_cpu: &mut TestCpu| JitBlockExit {
        // Roll back and re-execute from the same RIP in the interpreter.
        next_rip: 2,
        exit_to_interpreter: true,
        committed: false,
    });

    let compile = RecordingCompileSink::default();
    let mut jit = JitRuntime::new(config, backend, compile);

    // Install a committed block with an instruction count of 5.
    jit.install_handle(CompiledBlockHandle {
        entry_rip: 1,
        table_index: 0,
        meta: CompiledBlockMeta {
            code_paddr: 0x1000,
            byte_len: 4,
            page_versions_generation: 0,
            page_versions: Vec::new(),
            instruction_count: 5,
            inhibit_interrupts_after_block: false,
        },
    });

    // Install a rollback block with a non-zero instruction count; it should not be reported as
    // retired when the exit is not committed.
    jit.install_handle(CompiledBlockHandle {
        entry_rip: 2,
        table_index: 1,
        meta: CompiledBlockMeta {
            code_paddr: 0x2000,
            byte_len: 4,
            page_versions_generation: 0,
            page_versions: Vec::new(),
            instruction_count: 7,
            inhibit_interrupts_after_block: false,
        },
    });

    let mut interp = TestInterpreter::default();
    interp.install(0, |_cpu: &mut TestCpu| 1);

    let mut dispatcher = ExecDispatcher::new(interp, jit);
    let mut cpu = TestCpu {
        rip: 0,
        ..TestCpu::default()
    };

    match dispatcher.step(&mut cpu) {
        StepOutcome::Block {
            tier,
            entry_rip,
            next_rip,
            instructions_retired,
        } => {
            assert_eq!(tier, ExecutedTier::Interpreter);
            assert_eq!(entry_rip, 0);
            assert_eq!(next_rip, 1);
            assert_eq!(instructions_retired, 1);
        }
        other => panic!("expected interpreter block, got {other:?}"),
    }

    match dispatcher.step(&mut cpu) {
        StepOutcome::Block {
            tier,
            entry_rip,
            next_rip,
            instructions_retired,
        } => {
            assert_eq!(tier, ExecutedTier::Jit);
            assert_eq!(entry_rip, 1);
            assert_eq!(next_rip, 2);
            assert_eq!(instructions_retired, 5);
        }
        other => panic!("expected JIT block, got {other:?}"),
    }

    match dispatcher.step(&mut cpu) {
        StepOutcome::Block {
            tier,
            entry_rip,
            next_rip,
            instructions_retired,
        } => {
            assert_eq!(tier, ExecutedTier::Jit);
            assert_eq!(entry_rip, 2);
            assert_eq!(next_rip, 2);
            assert_eq!(instructions_retired, 0);
        }
        other => panic!("expected rollback JIT block, got {other:?}"),
    }
}
