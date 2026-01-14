use aero_cpu_core::jit::runtime::{
    CompileRequestSink, JitBackend, JitBlockExit, JitConfig, JitRuntime, PageVersionTracker,
    PAGE_SHIFT,
};

#[derive(Default)]
struct NullBackend;

impl JitBackend for NullBackend {
    type Cpu = ();

    fn execute(&mut self, _table_index: u32, _cpu: &mut Self::Cpu) -> JitBlockExit {
        panic!("NullBackend::execute should not be called by this test");
    }
}

#[derive(Default)]
struct NullCompileSink;

impl CompileRequestSink for NullCompileSink {
    fn request_compile(&mut self, _entry_rip: u64) {}
}

#[test]
fn huge_guest_write_does_not_grow_page_versions_table_unbounded() {
    let max_pages = 64usize;
    let config = JitConfig {
        enabled: true,
        hot_threshold: 1,
        cache_max_blocks: 16,
        cache_max_bytes: 0,
        code_version_max_pages: max_pages,
    };
    let mut jit = JitRuntime::new(config, NullBackend, NullCompileSink);

    // Regression target: this must not attempt to resize the dense version table to a gigantic
    // length or OOM.
    let huge_paddr = u64::MAX - 0x1000;
    jit.on_guest_write(huge_paddr, 1);

    assert_eq!(
        jit.page_versions().versions_len(),
        max_pages,
        "page version table length must remain bounded"
    );

    let huge_page = huge_paddr >> PAGE_SHIFT;
    assert_eq!(
        jit.page_versions().version(huge_page),
        0,
        "out-of-range pages must read as version 0"
    );
}

#[test]
fn snapshot_meta_is_bounded_for_absurd_byte_len() {
    let max_pages = 64usize;
    let config = JitConfig {
        enabled: true,
        hot_threshold: 1,
        cache_max_blocks: 16,
        cache_max_bytes: 0,
        code_version_max_pages: max_pages,
    };
    let jit = JitRuntime::new(config, NullBackend, NullCompileSink);

    // `byte_len` is a u32, but u32::MAX still spans ~1M pages. Ensure snapshot sizing is clamped.
    let meta = jit.snapshot_meta(0, u32::MAX);
    assert!(
        meta.page_versions.len() <= PageVersionTracker::MAX_SNAPSHOT_PAGES,
        "snapshot_meta returned an unexpectedly large page_versions vector: {} entries",
        meta.page_versions.len()
    );
}
