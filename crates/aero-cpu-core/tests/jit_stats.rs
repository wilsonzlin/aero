use aero_cpu_core::jit::cache::CompiledBlockHandle;
use aero_cpu_core::jit::runtime::{
    CompileRequestSink, JitBackend, JitBlockExit, JitConfig, JitRuntime,
};

#[derive(Default)]
struct NullCompileSink;

impl CompileRequestSink for NullCompileSink {
    fn request_compile(&mut self, _entry_rip: u64) {}
}

#[derive(Default)]
struct NullBackend;

impl JitBackend for NullBackend {
    type Cpu = ();

    fn execute(&mut self, _table_index: u32, _cpu: &mut Self::Cpu) -> JitBlockExit {
        JitBlockExit {
            next_rip: 0,
            exit_to_interpreter: true,
            committed: false,
        }
    }
}

fn base_config() -> JitConfig {
    JitConfig {
        // Keep the hotness threshold unreachable in unit tests so cache miss paths don't
        // implicitly generate compile requests.
        hot_threshold: u32::MAX,
        cache_max_blocks: 16,
        ..Default::default()
    }
}

#[test]
fn hit_miss_counting() {
    let mut jit = JitRuntime::new(base_config(), NullBackend, NullCompileSink);
    let entry_rip = 0x1000u64;

    assert!(jit.prepare_block(entry_rip).is_none());
    let stats = jit.stats_snapshot();
    assert_eq!(stats.cache_hit, 0);
    assert_eq!(stats.cache_miss, 1);

    // Installing a valid handle should turn the next lookup into a hit.
    let meta = jit.make_meta(0, 0);
    jit.install_handle(CompiledBlockHandle {
        entry_rip,
        table_index: 0,
        meta,
    });
    assert!(jit.prepare_block(entry_rip).is_some());

    let stats = jit.stats_snapshot();
    assert_eq!(stats.cache_hit, 1);
    assert_eq!(stats.cache_miss, 1);
}

#[test]
fn install_and_evict_counting() {
    let config = JitConfig {
        cache_max_blocks: 1,
        ..base_config()
    };
    let mut jit = JitRuntime::new(config, NullBackend, NullCompileSink);

    let meta0 = jit.make_meta(0, 0);
    jit.install_handle(CompiledBlockHandle {
        entry_rip: 0x1000,
        table_index: 0,
        meta: meta0,
    });
    let meta1 = jit.make_meta(0, 0);
    jit.install_handle(CompiledBlockHandle {
        entry_rip: 0x2000,
        table_index: 1,
        meta: meta1,
    });

    let stats = jit.stats_snapshot();
    assert_eq!(stats.install_ok, 2);
    assert_eq!(stats.evictions, 1);
    assert_eq!(jit.cache_len(), 1);
}

#[test]
fn stale_install_rejection_increments() {
    let mut jit = JitRuntime::new(base_config(), NullBackend, NullCompileSink);
    let entry_rip = 0x1000u64;
    let code_paddr = 0x2000u64;
    let byte_len = 1u32;

    let stale_meta = jit.snapshot_meta(code_paddr, byte_len);
    // Make the snapshot stale before installing the handle.
    jit.on_guest_write(code_paddr, byte_len as usize);

    jit.install_handle(CompiledBlockHandle {
        entry_rip,
        table_index: 0,
        meta: stale_meta,
    });

    let stats = jit.stats_snapshot();
    assert_eq!(stats.install_rejected_stale, 1);
    assert_eq!(stats.install_ok, 0);
    assert_eq!(stats.compile_requests, 1);
}

#[test]
fn invalidation_counting() {
    let mut jit = JitRuntime::new(base_config(), NullBackend, NullCompileSink);
    let entry_rip = 0x1234u64;
    let code_paddr = 0x4000u64;
    let byte_len = 1u32;

    // Install a block and then invalidate it via the page-version mechanism.
    let meta = jit.snapshot_meta(code_paddr, byte_len);
    jit.install_handle(CompiledBlockHandle {
        entry_rip,
        table_index: 0,
        meta,
    });
    jit.on_guest_write(code_paddr, byte_len as usize);
    assert!(jit.prepare_block(entry_rip).is_none());

    // Explicit invalidation should also bump the counter (and be idempotent for missing blocks).
    let meta = jit.snapshot_meta(code_paddr, byte_len);
    jit.install_handle(CompiledBlockHandle {
        entry_rip,
        table_index: 0,
        meta,
    });
    assert!(jit.invalidate_block(entry_rip));
    assert!(!jit.invalidate_block(entry_rip));

    let stats = jit.stats_snapshot();
    assert_eq!(stats.invalidations, 2);
    assert_eq!(
        stats.compile_requests, 1,
        "only the stale invalidation should trigger a compilation request"
    );
}
