use std::cell::RefCell;
use std::rc::Rc;

use aero_cpu_core::jit::cache::CompiledBlockHandle;
use aero_cpu_core::jit::runtime::{
    CompileRequestSink, JitBackend, JitBlockExit, JitConfig, JitRuntime, PAGE_SHIFT,
};

#[derive(Default)]
struct NullBackend;

impl JitBackend for NullBackend {
    type Cpu = ();

    fn execute(&mut self, _table_index: u32, _cpu: &mut Self::Cpu) -> JitBlockExit {
        panic!("NullBackend::execute should not be called by this test");
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

#[test]
fn raw_table_mutation_is_observed_by_version_and_snapshot_meta() {
    let config = JitConfig {
        enabled: true,
        hot_threshold: 1,
        cache_max_blocks: 16,
        cache_max_bytes: 0,
        code_version_max_pages: 8,
    };
    let mut jit = JitRuntime::new(config, NullBackend, RecordingCompileSink::default());

    let (ptr, len) = jit.code_version_table_ptr_len();
    assert_eq!(len, 8);

    // Simulate a JIT-side inlined store/bump by writing directly through the raw table pointer.
    // Safety: `ptr` is a contiguous `u32` array of length `len`.
    unsafe {
        ptr.add(3).write(7);
    }

    assert_eq!(jit.page_versions().version(3), 7);

    let meta = jit.snapshot_meta(3u64 << PAGE_SHIFT, 1);
    assert_eq!(meta.page_versions.len(), 1);
    assert_eq!(meta.page_versions[0].page, 3);
    assert_eq!(meta.page_versions[0].version, 7);
}

#[test]
fn code_version_table_pointer_is_stable_across_guest_writes_and_reset() {
    let config = JitConfig {
        enabled: true,
        hot_threshold: 1,
        cache_max_blocks: 16,
        cache_max_bytes: 0,
        code_version_max_pages: 8,
    };
    let mut jit = JitRuntime::new(config, NullBackend, RecordingCompileSink::default());

    let (ptr0, len0) = jit.code_version_table_ptr_len();
    assert_eq!(len0, 8);

    jit.on_guest_write(0, 1);
    let (ptr1, len1) = jit.code_version_table_ptr_len();
    assert_eq!(ptr1, ptr0);
    assert_eq!(len1, len0);

    jit.reset();
    let (ptr2, len2) = jit.code_version_table_ptr_len();
    assert_eq!(ptr2, ptr0);
    assert_eq!(len2, len0);
    assert_eq!(jit.page_versions().version(0), 0);
}

#[test]
fn reset_keeps_table_pointer_stable_and_rejects_old_snapshots() {
    let config = JitConfig {
        enabled: true,
        hot_threshold: 1,
        cache_max_blocks: 16,
        cache_max_bytes: 0,
        code_version_max_pages: 64,
    };
    let compile = RecordingCompileSink::default();
    let compile_log = compile.clone();
    let mut jit = JitRuntime::new(config, NullBackend, compile);

    let code_paddr = 0x4000u64;
    let old_meta = jit.snapshot_meta(code_paddr, 1);
    assert_eq!(old_meta.page_versions.len(), 1);
    assert_eq!(old_meta.page_versions[0].version, 0);

    let (ptr0, len0) = jit.code_version_table_ptr_len();
    jit.reset();
    let (ptr1, len1) = jit.code_version_table_ptr_len();
    assert_eq!(ptr1, ptr0);
    assert_eq!(len1, len0);

    let entry_rip = 0x1000u64;
    jit.install_handle(CompiledBlockHandle {
        entry_rip,
        table_index: 0,
        meta: old_meta,
    });

    assert_eq!(
        compile_log.snapshot(),
        vec![entry_rip],
        "stale compilation results must be rejected after reset"
    );
    assert_eq!(jit.cache_len(), 0);
}
