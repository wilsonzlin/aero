use crate::jit::cache::{CodeCache, CompiledBlockHandle, CompiledBlockMeta, PageVersionSnapshot};
use crate::jit::profile::HotnessProfile;
use std::sync::Arc;

use core::cell::Cell;

/// Optional, low-overhead hook for embedders to collect JIT runtime metrics.
///
/// All methods must be infallible and cheap to call. The JIT runtime will only invoke these
/// methods when a sink is installed, and will not allocate on the hot path.
pub trait JitMetricsSink {
    fn record_cache_hit(&self);
    fn record_cache_miss(&self);
    fn record_install(&self);
    fn record_evict(&self, n: u64);
    fn record_invalidate(&self);
    fn record_stale_install_reject(&self);
    fn record_compile_request(&self);
    fn set_cache_bytes(&self, used: u64, capacity: u64);
}

// Allow embedders to directly reuse `aero_perf::Telemetry` / `aero_perf::jit::JitMetrics` as the
// runtime metrics sink without introducing a dependency cycle (cpu-core already depends on
// aero-perf, but aero-perf does not depend on cpu-core).
impl JitMetricsSink for aero_perf::jit::JitMetrics {
    #[inline]
    fn record_cache_hit(&self) {
        aero_perf::jit::JitMetrics::record_cache_hit(self);
    }

    #[inline]
    fn record_cache_miss(&self) {
        aero_perf::jit::JitMetrics::record_cache_miss(self);
    }

    #[inline]
    fn record_install(&self) {
        aero_perf::jit::JitMetrics::record_cache_install(self);
    }

    #[inline]
    fn record_evict(&self, n: u64) {
        aero_perf::jit::JitMetrics::record_cache_evict(self, n);
    }

    #[inline]
    fn record_invalidate(&self) {
        aero_perf::jit::JitMetrics::record_cache_invalidate(self);
    }

    #[inline]
    fn record_stale_install_reject(&self) {
        aero_perf::jit::JitMetrics::record_cache_stale_install_reject(self);
    }

    #[inline]
    fn record_compile_request(&self) {
        aero_perf::jit::JitMetrics::record_compile_request(self);
    }

    #[inline]
    fn set_cache_bytes(&self, used: u64, capacity: u64) {
        aero_perf::jit::JitMetrics::set_cache_used_bytes(self, used);
        aero_perf::jit::JitMetrics::set_cache_capacity_bytes(self, capacity);
    }
}

pub const PAGE_SIZE: u64 = 4096;
pub const PAGE_SHIFT: u32 = 12;

/// Default number of guest 4KiB pages tracked by the page-version table.
///
/// This covers 4GiB of guest physical address space:
/// `4GiB / 4KiB = 1,048,576` pages, requiring a 4MiB `u32` table.
pub const DEFAULT_CODE_VERSION_MAX_PAGES: usize = 1_048_576;

const _: () = {
    // Ensure the JIT-visible table layout matches `u32[]` so WASM `i32.load`/`i32.store` works.
    use core::mem::{align_of, size_of};
    assert!(size_of::<Cell<u32>>() == size_of::<u32>());
    assert!(align_of::<Cell<u32>>() == align_of::<u32>());
};

#[derive(Debug, Clone)]
pub struct JitConfig {
    pub enabled: bool,
    pub hot_threshold: u32,
    pub cache_max_blocks: usize,
    pub cache_max_bytes: usize,
    /// Maximum number of 4KiB guest pages tracked by the page-version table.
    ///
    /// The table is exposed to generated JIT code as a contiguous `u32` array with one entry per
    /// page (`paddr >> 12`). Pages outside this range implicitly have version 0 and writes to them
    /// do not update the table.
    pub code_version_max_pages: usize,
}

impl Default for JitConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            hot_threshold: 32,
            cache_max_blocks: 1024,
            cache_max_bytes: 0,
            code_version_max_pages: DEFAULT_CODE_VERSION_MAX_PAGES,
        }
    }
}

/// JIT runtime counters (non-atomic).
///
/// These counters are meant for instrumentation and testing. They intentionally use plain `u64`
/// fields and require `&mut self` to update; callers that need cross-thread aggregation should do
/// it at a higher level.
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct JitRuntimeStats {
    cache_hit: u64,
    cache_miss: u64,
    install_ok: u64,
    install_rejected_stale: u64,
    evictions: u64,
    /// Number of blocks invalidated (explicitly via `invalidate_block` or implicitly due to stale
    /// page-version checks).
    invalidations: u64,
    invalidate_calls: u64,
    /// Total number of `CompileRequestSink::request_compile` calls issued by the runtime.
    compile_requests: u64,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct JitRuntimeStatsSnapshot {
    pub cache_hit: u64,
    pub cache_miss: u64,
    pub install_ok: u64,
    pub install_rejected_stale: u64,
    pub evictions: u64,
    pub invalidations: u64,
    pub invalidate_calls: u64,
    pub compile_requests: u64,
}

impl JitRuntimeStats {
    pub fn snapshot(&self) -> JitRuntimeStatsSnapshot {
        JitRuntimeStatsSnapshot {
            cache_hit: self.cache_hit,
            cache_miss: self.cache_miss,
            install_ok: self.install_ok,
            install_rejected_stale: self.install_rejected_stale,
            evictions: self.evictions,
            invalidations: self.invalidations,
            invalidate_calls: self.invalidate_calls,
            compile_requests: self.compile_requests,
        }
    }
}

pub trait CompileRequestSink {
    fn request_compile(&mut self, entry_rip: u64);
}

pub trait JitBackend {
    type Cpu;

    fn execute(&mut self, table_index: u32, cpu: &mut Self::Cpu) -> JitBlockExit;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JitBlockExit {
    pub next_rip: u64,
    pub exit_to_interpreter: bool,
    /// Whether the block committed architectural side effects (register/memory updates) and thus
    /// retired guest instructions.
    ///
    /// Some backends may speculatively execute a block and then roll back guest state when the
    /// block performs a runtime/MMIO/page-fault exit without deoptimization metadata. In those
    /// cases, the execution engine must *not* advance time/TSC or age interrupt-shadow state.
    pub committed: bool,
}

pub struct PageVersionTracker {
    /// Page version table indexed by 4KiB physical page number.
    ///
    /// This is intentionally a dense table so it can be exposed to generated JIT code as a
    /// contiguous `u32` array (one entry per page). Pages outside the table implicitly have
    /// version 0.
    ///
    /// Versions are treated as modulo-2^32 counters. Every observed write bumps the version for
    /// each touched page by `1` using wrapping arithmetic (`u32::MAX + 1 == 0`). Compiled blocks
    /// snapshot the current versions of the pages they cover and later validate them by simple
    /// equality checks.
    ///
    /// Wraparound could in theory cause a stale snapshot to appear valid again, but this would
    /// require `2^32` writes to the same page between taking a snapshot and validating it. In
    /// practice this is vanishingly unlikely, and will become even less so if we only bump code
    /// pages in the future.
    versions: Box<[Cell<u32>]>,

    /// Snapshot generation counter.
    ///
    /// [`Self::reset`] increments this so compilation results derived from pre-reset snapshots are
    /// rejected even if the per-page version values happen to match (e.g. all zeros).
    generation: Cell<u64>,
}

impl Default for PageVersionTracker {
    fn default() -> Self {
        Self::new(DEFAULT_CODE_VERSION_MAX_PAGES)
    }
}

impl core::fmt::Debug for PageVersionTracker {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PageVersionTracker")
            .field("max_pages", &self.versions.len())
            .field("generation", &self.generation.get())
            .finish()
    }
}

impl Clone for PageVersionTracker {
    fn clone(&self) -> Self {
        let out = Self::new(self.versions.len());
        for i in 0..self.versions.len() {
            out.versions[i].set(self.versions[i].get());
        }
        out.generation.set(self.generation.get());
        out
    }
}

impl PageVersionTracker {
    /// Hard cap on the number of tracked 4KiB pages in the dense page-version table.
    ///
    /// The table is exposed to JIT code as a dense `u32[]`. Without a cap, an embedder could
    /// configure an absurd page count and force an attempt to allocate terabytes of host memory.
    ///
    /// `4_194_304` pages = 16GiB of guest-physical address space, requiring at most 16MiB of host
    /// memory for the version table (`u32` per page).
    pub const MAX_TRACKED_PAGES: usize = 4_194_304;

    /// Maximum number of page-version entries returned by [`Self::snapshot`].
    ///
    /// A snapshot is stored in every compiled block's metadata. Even though `byte_len` is a `u32`,
    /// an absurd value could otherwise result in allocating and copying a multi-megabyte
    /// `Vec<PageVersionSnapshot>` per block. Basic blocks are expected to span *very* few pages, so
    /// capping snapshots keeps metadata bounded.
    ///
    /// If the requested code span covers more than `MAX_SNAPSHOT_PAGES`, the snapshot is truncated
    /// to the first `MAX_SNAPSHOT_PAGES` pages starting at `code_paddr`. Such an incomplete
    /// snapshot cannot safely validate self-modifying code; [`JitRuntime`] treats blocks whose
    /// snapshot does not cover the full `byte_len` span as stale and rejects them.
    pub const MAX_SNAPSHOT_PAGES: usize = 4096;

    /// Creates a new bounded tracker with a stable backing table.
    ///
    /// `max_pages` is the number of 4KiB pages tracked. The backing storage is allocated once and
    /// never resized, so the table pointer returned by [`Self::table_ptr_len`] remains stable for
    /// the lifetime of the tracker.
    pub fn new(max_pages: usize) -> Self {
        // Safety/DoS hardening: clamp to the hard cap so callers can't trigger absurd allocations.
        let max_pages = max_pages.min(Self::MAX_TRACKED_PAGES);
        let mut versions = Vec::with_capacity(max_pages);
        versions.resize_with(max_pages, || Cell::new(0));
        Self {
            versions: versions.into_boxed_slice(),
            generation: Cell::new(0),
        }
    }

    /// Ensure the internal dense version table contains at least `len` entries.
    ///
    /// The tracker uses a fixed-size, pointer-stable table, so the table length is determined at
    /// construction time (via `max_pages` / [`JitConfig::code_version_max_pages`]) and cannot grow
    /// later without invalidating the exported pointer.
    ///
    /// This method therefore acts as a *check*: it ensures `len <= table_len`, panicking if the
    /// configured table is too small.
    #[track_caller]
    pub fn ensure_table_len(&self, len: usize) {
        assert!(
            len <= self.versions.len(),
            "requested code-version table length ({len}) exceeds configured max_pages ({})",
            self.versions.len()
        );
    }

    /// Backwards-compatible alias for [`Self::ensure_table_len`].
    #[track_caller]
    pub fn ensure_tracked_pages(&self, pages: usize) {
        self.ensure_table_len(pages);
    }

    /// Backwards-compatible alias for [`Self::ensure_table_len`].
    #[track_caller]
    pub fn ensure_len(&self, len: usize) {
        self.ensure_table_len(len);
    }

    /// Returns `(ptr, len_entries)` for the JIT-visible page-version table.
    ///
    /// The table is a contiguous `u32` array with one entry per 4KiB page (`paddr >> 12`).
    ///
    /// Pointer stability: `ptr` remains valid for the lifetime of the tracker (no reallocations).
    pub fn table_ptr_len(&self) -> (*mut u32, usize) {
        let len = self.versions.len();
        if len == 0 {
            // `Box<[T]>::as_ptr()` returns a non-null dangling pointer for empty slices, but JIT
            // callers typically treat `len == 0` as "table disabled". Return a null pointer to
            // make that intent explicit and avoid accidental dereference by embedding code.
            (core::ptr::null_mut(), 0)
        } else {
            (self.versions.as_ptr() as *mut u32, len)
        }
    }

    /// Pointer to the start of the dense page-version table (`u32` entries).
    ///
    /// The returned pointer is valid for `self.versions_len()` entries. It remains valid for the
    /// lifetime of the tracker (no reallocations).
    pub fn table_ptr(&self) -> *const u32 {
        if self.versions.is_empty() {
            core::ptr::null()
        } else {
            self.versions.as_ptr() as *const u32
        }
    }

    /// WASM32 helper returning the page-version table pointer + length as `u32`.
    ///
    /// `ptr` is a wasm linear-memory byte offset.
    #[cfg(target_arch = "wasm32")]
    pub fn table_ptr_len_u32(&self) -> (u32, u32) {
        let (ptr, len) = self.table_ptr_len();
        (ptr as u32, len as u32)
    }

    /// wasm32 helper: returns [`Self::table_ptr`] as a linear-memory byte offset.
    #[cfg(target_arch = "wasm32")]
    pub fn table_ptr_u32(&self) -> u32 {
        self.table_ptr() as u32
    }

    pub fn version(&self, page: u64) -> u32 {
        let len = self.versions.len() as u64;
        if page >= len {
            return 0;
        }
        let idx = page as usize;
        self.versions[idx].get()
    }

    /// Sets an explicit version for a page.
    ///
    /// This is primarily used by unit tests and tooling; normal execution should use
    /// [`Self::bump_write`].
    pub fn set_version(&self, page: u64, version: u32) {
        let len = self.versions.len() as u64;
        if page >= len {
            return;
        }
        let idx = page as usize;
        self.versions[idx].set(version);
    }

    pub fn bump_write(&self, paddr: u64, len: usize) {
        if len == 0 {
            return;
        }

        let max_pages = self.versions.len() as u64;
        if max_pages == 0 {
            return;
        }

        let start_page = paddr >> PAGE_SHIFT;
        let end = paddr.saturating_add((len as u64).saturating_sub(1));
        let end_page = end >> PAGE_SHIFT;

        if start_page >= max_pages {
            return;
        }

        let end_page = end_page.min(max_pages - 1);
        let start_idx = start_page as usize;
        let end_idx = end_page as usize;
        for i in start_idx..=end_idx {
            let cell = &self.versions[i];
            cell.set(cell.get().wrapping_add(1));
        }
    }

    pub fn snapshot(&self, code_paddr: u64, byte_len: u32) -> Vec<PageVersionSnapshot> {
        if byte_len == 0 {
            return Vec::new();
        }
        let start_page = code_paddr >> PAGE_SHIFT;
        let end = code_paddr.saturating_add(u64::from(byte_len).saturating_sub(1));
        let end_page = end >> PAGE_SHIFT;

        let page_count = end_page.saturating_sub(start_page).saturating_add(1);
        let clamped_pages = page_count.min(Self::MAX_SNAPSHOT_PAGES as u64);
        // `byte_len != 0` implies at least one spanned page, but keep the logic robust.
        if clamped_pages == 0 {
            return Vec::new();
        }
        let clamped_end_page = start_page.saturating_add(clamped_pages - 1);

        let mut out = Vec::with_capacity(clamped_pages as usize);
        for page in start_page..=clamped_end_page {
            out.push(PageVersionSnapshot {
                page,
                version: self.version(page),
            });
        }
        out
    }

    /// Number of entries currently allocated in the dense page-version table.
    ///
    /// This is primarily intended for tests and tooling.
    pub fn versions_len(&self) -> usize {
        self.versions.len()
    }

    /// Snapshot generation for page-version validation.
    pub fn generation(&self) -> u64 {
        self.generation.get()
    }

    /// Zero all tracked page versions in-place.
    ///
    /// This preserves the exported table pointer/length: any generated JIT code that has cached
    /// the pointer returned by [`Self::table_ptr_len`] will continue to observe a valid table
    /// after reset, with all entries set to 0.
    pub fn reset(&self) {
        self.generation.set(self.generation.get().wrapping_add(1));
        for v in self.versions.iter() {
            v.set(0);
        }
    }
}

pub struct JitRuntime<B, C> {
    config: JitConfig,
    stats: JitRuntimeStats,
    backend: B,
    compile: C,
    cache: CodeCache,
    profile: HotnessProfile,
    page_versions: PageVersionTracker,
    metrics_sink: Option<Arc<dyn JitMetricsSink + Send + Sync>>,
}

impl<B, C> JitRuntime<B, C>
where
    B: JitBackend,
    C: CompileRequestSink,
{
    pub fn new(config: JitConfig, backend: B, compile: C) -> Self {
        let mut config = config;
        // Clamp the configured table size to the hard safety cap so `JitRuntime::config()` always
        // reflects the actual JIT-visible table length.
        config.code_version_max_pages = config
            .code_version_max_pages
            .min(PageVersionTracker::MAX_TRACKED_PAGES);

        let page_versions = PageVersionTracker::new(config.code_version_max_pages);
        let cache = CodeCache::new(config.cache_max_blocks, config.cache_max_bytes);
        let profile_capacity = HotnessProfile::recommended_capacity(config.cache_max_blocks);
        let profile = HotnessProfile::new_with_capacity(config.hot_threshold, profile_capacity);
        Self {
            config,
            stats: JitRuntimeStats::default(),
            backend,
            compile,
            cache,
            profile,
            page_versions,
            metrics_sink: None,
        }
    }

    pub fn set_metrics_sink(&mut self, sink: Option<Arc<dyn JitMetricsSink + Send + Sync>>) {
        self.metrics_sink = sink;
        if let Some(sink) = self.metrics_sink.as_deref() {
            sink.set_cache_bytes(
                self.cache.current_bytes() as u64,
                self.config.cache_max_bytes as u64,
            );
        }
    }

    pub fn with_metrics_sink(mut self, sink: Arc<dyn JitMetricsSink + Send + Sync>) -> Self {
        self.set_metrics_sink(Some(sink));
        self
    }

    pub fn config(&self) -> &JitConfig {
        &self.config
    }

    #[inline]
    pub fn stats(&self) -> &JitRuntimeStats {
        &self.stats
    }

    #[inline]
    pub fn stats_snapshot(&self) -> JitRuntimeStatsSnapshot {
        self.stats.snapshot()
    }

    pub fn stats_reset(&mut self) {
        self.stats = JitRuntimeStats::default();
    }

    pub fn cache_len(&self) -> usize {
        self.cache.len()
    }

    pub fn is_compiled(&self, entry_rip: u64) -> bool {
        self.cache.contains(entry_rip)
    }

    pub fn hotness(&self, entry_rip: u64) -> u32 {
        self.profile.counter(entry_rip)
    }

    /// Access the runtime's guest-physical page version tracker.
    ///
    /// The tracker exposes a stable, JIT-visible `u32[]` table via
    /// [`PageVersionTracker::table_ptr_len`].
    pub fn page_versions(&self) -> &PageVersionTracker {
        &self.page_versions
    }

    /// Ensure the internal page-version table has at least `len` entries.
    ///
    /// This is a *check* (not a resize): the table length is fixed at construction time via
    /// [`JitConfig::code_version_max_pages`]. If `len` exceeds the configured length, this method
    /// panics.
    #[track_caller]
    pub fn ensure_page_version_table_len(&self, len: usize) {
        self.page_versions.ensure_len(len);
    }

    /// Return the `(ptr, len)` of the dense page-version table.
    ///
    /// `ptr` points to `len` contiguous `u32` entries, indexed by 4KiB physical page number.
    ///
    /// The pointer/length are stable for the lifetime of the runtime (no reallocations), including
    /// across [`Self::reset`] which clears the table in place.
    pub fn page_version_table_ptr_len(&self) -> (*const u32, usize) {
        (
            self.page_versions.table_ptr(),
            self.page_versions.versions_len(),
        )
    }

    pub fn on_guest_write(&mut self, paddr: u64, len: usize) {
        self.page_versions.bump_write(paddr, len);
    }

    /// Ensure the internal page-version table has at least `len` `u32` entries.
    ///
    /// This is a *check* (not a resize): the table length is fixed at construction time via
    /// [`JitConfig::code_version_max_pages`]. If `len` exceeds the configured length, this method
    /// panics.
    ///
    /// When exposing the table to generated JIT code (e.g. WASM inline stores / code-version
    /// guards), callers should size the table up-front (via
    /// [`JitConfig::code_version_max_pages`]) so it covers all guest-physical pages that need
    /// tracking. Pages outside the table behave as version 0.
    #[track_caller]
    pub fn ensure_code_version_table_len(&self, len: usize) {
        self.page_versions.ensure_table_len(len);
    }

    /// Returns the raw pointer/length of the page-version table.
    ///
    /// The pointer/length are stable for the lifetime of the runtime (no reallocations), including
    /// across [`Self::reset`] which clears the table in place.
    pub fn code_version_table_ptr_len(&self) -> (*mut u32, usize) {
        self.page_versions.table_ptr_len()
    }

    /// Reset all runtime-managed JIT state.
    ///
    /// This is intended for embedders that restore a snapshot of guest memory/state or want to
    /// perform global invalidation without recreating the entire runtime. After calling `reset`,
    /// previously-compiled blocks and compilation results derived from old page-version snapshots
    /// will no longer be considered valid.
    pub fn reset(&mut self) {
        self.cache.clear();
        self.profile.reset();
        self.page_versions.reset();
        self.stats_reset();
        if let Some(metrics) = self.metrics_sink.as_deref() {
            metrics.set_cache_bytes(0, self.config.cache_max_bytes as u64);
        }
    }

    /// Snapshot the current page-version state for a block of guest code.
    ///
    /// The returned metadata should be captured by the compilation pipeline at the time it reads
    /// guest code bytes. Installing a block with a stale snapshot will cause the runtime to reject
    /// the block and request recompilation.
    pub fn snapshot_meta(&self, code_paddr: u64, byte_len: u32) -> CompiledBlockMeta {
        CompiledBlockMeta {
            code_paddr,
            byte_len,
            page_versions_generation: self.page_versions.generation(),
            page_versions: self.page_versions.snapshot(code_paddr, byte_len),
            instruction_count: 0,
            inhibit_interrupts_after_block: false,
        }
    }

    /// Backwards-compatible alias for [`Self::snapshot_meta`].
    pub fn make_meta(&self, code_paddr: u64, byte_len: u32) -> CompiledBlockMeta {
        self.snapshot_meta(code_paddr, byte_len)
    }

    /// Installs a fully-described compiled block into the cache.
    ///
    /// If the block's page-version snapshot is already stale, the block is rejected and a new
    /// compilation request is issued for the same entry RIP.
    pub fn install_handle(&mut self, handle: CompiledBlockHandle) -> Vec<u64> {
        let metrics = self.metrics_sink.as_deref();
        if !self.is_block_valid(&handle) {
            self.stats.install_rejected_stale = self.stats.install_rejected_stale.saturating_add(1);
            // A background compilation result can arrive after the guest has modified the code.
            // Installing such a block would be incorrect; reject it and request recompilation.
            if let Some(metrics) = metrics {
                metrics.record_stale_install_reject();
            }
            let entry_rip = handle.entry_rip;
            // If we already have a valid block for this RIP, ignore the stale result. This can
            // happen if multiple compilation jobs raced and the newest one installed first.
            if let Some(existing) = self.cache.get_cloned(entry_rip) {
                if self.is_block_valid(&existing) {
                    return Vec::new();
                }

                // Existing block is also stale; drop it so we don't keep probing it on every
                // execution attempt.
                if self.cache.remove(entry_rip).is_some() {
                    self.stats.invalidations = self.stats.invalidations.saturating_add(1);
                    if let Some(metrics) = metrics {
                        metrics.record_invalidate();
                    }
                }
                self.profile.clear_requested(entry_rip);
                if let Some(metrics) = metrics {
                    metrics.set_cache_bytes(
                        self.cache.current_bytes() as u64,
                        self.config.cache_max_bytes as u64,
                    );
                }
            }

            self.profile.mark_requested(entry_rip);
            self.request_compile(entry_rip);
            return Vec::new();
        }

        self.stats.install_ok = self.stats.install_ok.saturating_add(1);
        let evicted = self.cache.insert(handle);
        let evicted_count = u64::try_from(evicted.len()).unwrap_or(u64::MAX);
        self.stats.evictions = self.stats.evictions.saturating_add(evicted_count);
        if let Some(metrics) = metrics {
            metrics.record_install();
            if !evicted.is_empty() {
                metrics.record_evict(evicted_count);
            }
            metrics.set_cache_bytes(
                self.cache.current_bytes() as u64,
                self.config.cache_max_bytes as u64,
            );
        }
        for rip in &evicted {
            self.profile.clear_requested(*rip);
        }
        evicted
    }

    pub fn install_block(
        &mut self,
        entry_rip: u64,
        table_index: u32,
        code_paddr: u64,
        byte_len: u32,
    ) -> Vec<u64> {
        self.install_handle(CompiledBlockHandle {
            entry_rip,
            table_index,
            meta: self.snapshot_meta(code_paddr, byte_len),
        })
    }

    pub fn invalidate_block(&mut self, entry_rip: u64) -> bool {
        self.stats.invalidate_calls = self.stats.invalidate_calls.saturating_add(1);
        if self.cache.remove(entry_rip).is_some() {
            self.stats.invalidations = self.stats.invalidations.saturating_add(1);
            self.profile.clear_requested(entry_rip);
            if let Some(metrics) = self.metrics_sink.as_deref() {
                metrics.record_invalidate();
                metrics.set_cache_bytes(
                    self.cache.current_bytes() as u64,
                    self.config.cache_max_bytes as u64,
                );
            }
            return true;
        }
        false
    }

    pub fn prepare_block(&mut self, entry_rip: u64) -> Option<CompiledBlockHandle> {
        if !self.config.enabled {
            return None;
        }

        let metrics = self.metrics_sink.as_deref();
        let mut compile_due_to_stale = false;
        let mut handle = self.cache.get_cloned(entry_rip);
        if let Some(ref h) = handle {
            if !self.is_block_valid(h) {
                if self.cache.remove(entry_rip).is_some() {
                    self.stats.invalidations = self.stats.invalidations.saturating_add(1);
                    if let Some(metrics) = metrics {
                        metrics.record_invalidate();
                    }
                }
                self.profile.clear_requested(entry_rip);
                if let Some(metrics) = metrics {
                    metrics.set_cache_bytes(
                        self.cache.current_bytes() as u64,
                        self.config.cache_max_bytes as u64,
                    );
                }
                self.profile.mark_requested(entry_rip);
                compile_due_to_stale = true;
                handle = None;
            }
        }

        let has_compiled = handle.is_some();
        if has_compiled {
            self.stats.cache_hit = self.stats.cache_hit.saturating_add(1);
            if let Some(metrics) = metrics {
                metrics.record_cache_hit();
            }
        } else {
            self.stats.cache_miss = self.stats.cache_miss.saturating_add(1);
            if let Some(metrics) = metrics {
                metrics.record_cache_miss();
            }
        }

        let compile_due_to_hotness = self.profile.record_hit(entry_rip, has_compiled);
        if compile_due_to_stale || compile_due_to_hotness {
            self.request_compile(entry_rip);
        }

        handle
    }

    pub fn execute_block(
        &mut self,
        cpu: &mut B::Cpu,
        handle: &CompiledBlockHandle,
    ) -> JitBlockExit {
        self.backend.execute(handle.table_index, cpu)
    }

    fn is_block_valid(&self, handle: &CompiledBlockHandle) -> bool {
        // An empty snapshot means "no page-version validation". Some unit tests and embedders
        // intentionally omit metadata for synthetic blocks.
        if handle.meta.page_versions.is_empty() {
            return true;
        }

        if handle.meta.page_versions_generation != self.page_versions.generation() {
            return false;
        }

        // If the snapshot does not cover the full code span (e.g. because [`PageVersionTracker`]
        // clamped it), we conservatively treat the block as stale. Otherwise we'd risk executing a
        // block whose code pages are not fully validated against self-modifying writes.
        let expected_pages = if handle.meta.byte_len == 0 {
            0u64
        } else {
            let start_page = handle.meta.code_paddr >> PAGE_SHIFT;
            let end = handle
                .meta
                .code_paddr
                .saturating_add(handle.meta.byte_len as u64 - 1);
            let end_page = end >> PAGE_SHIFT;
            end_page.saturating_sub(start_page).saturating_add(1)
        };
        if expected_pages > handle.meta.page_versions.len() as u64 {
            return false;
        }

        for snapshot in &handle.meta.page_versions {
            if self.page_versions.version(snapshot.page) != snapshot.version {
                return false;
            }
        }
        true
    }

    #[inline]
    fn request_compile(&mut self, entry_rip: u64) {
        self.stats.compile_requests = self.stats.compile_requests.saturating_add(1);
        if let Some(metrics) = self.metrics_sink.as_deref() {
            metrics.record_compile_request();
        }
        self.compile.request_compile(entry_rip);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::panic;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[derive(Default)]
    struct MockMetricsSink {
        cache_hit: AtomicU64,
        cache_miss: AtomicU64,
        install: AtomicU64,
        evict: AtomicU64,
        invalidate: AtomicU64,
        stale_reject: AtomicU64,
        compile_request: AtomicU64,
        cache_used: AtomicU64,
        cache_capacity: AtomicU64,
    }

    impl MockMetricsSink {
        fn cache_hit(&self) -> u64 {
            self.cache_hit.load(Ordering::Relaxed)
        }
        fn cache_miss(&self) -> u64 {
            self.cache_miss.load(Ordering::Relaxed)
        }
        fn install(&self) -> u64 {
            self.install.load(Ordering::Relaxed)
        }
        fn evict(&self) -> u64 {
            self.evict.load(Ordering::Relaxed)
        }
        fn stale_reject(&self) -> u64 {
            self.stale_reject.load(Ordering::Relaxed)
        }
        fn compile_request(&self) -> u64 {
            self.compile_request.load(Ordering::Relaxed)
        }
        fn cache_used(&self) -> u64 {
            self.cache_used.load(Ordering::Relaxed)
        }
        fn cache_capacity(&self) -> u64 {
            self.cache_capacity.load(Ordering::Relaxed)
        }
    }

    impl JitMetricsSink for MockMetricsSink {
        fn record_cache_hit(&self) {
            self.cache_hit.fetch_add(1, Ordering::Relaxed);
        }

        fn record_cache_miss(&self) {
            self.cache_miss.fetch_add(1, Ordering::Relaxed);
        }

        fn record_install(&self) {
            self.install.fetch_add(1, Ordering::Relaxed);
        }

        fn record_evict(&self, n: u64) {
            self.evict.fetch_add(n, Ordering::Relaxed);
        }

        fn record_invalidate(&self) {
            self.invalidate.fetch_add(1, Ordering::Relaxed);
        }

        fn record_stale_install_reject(&self) {
            self.stale_reject.fetch_add(1, Ordering::Relaxed);
        }

        fn record_compile_request(&self) {
            self.compile_request.fetch_add(1, Ordering::Relaxed);
        }

        fn set_cache_bytes(&self, used: u64, capacity: u64) {
            self.cache_used.store(used, Ordering::Relaxed);
            self.cache_capacity.store(capacity, Ordering::Relaxed);
        }
    }

    #[derive(Default)]
    struct MockCompileSink {
        requests: Vec<u64>,
    }

    impl CompileRequestSink for MockCompileSink {
        fn request_compile(&mut self, entry_rip: u64) {
            self.requests.push(entry_rip);
        }
    }

    struct DummyBackend;

    impl JitBackend for DummyBackend {
        type Cpu = ();

        fn execute(&mut self, _table_index: u32, _cpu: &mut Self::Cpu) -> JitBlockExit {
            JitBlockExit {
                next_rip: 0,
                exit_to_interpreter: true,
                committed: false,
            }
        }
    }

    fn make_runtime(config: JitConfig) -> JitRuntime<DummyBackend, MockCompileSink> {
        JitRuntime::new(config, DummyBackend, MockCompileSink::default())
    }

    thread_local! {
        static LAST_PANIC_LOC: RefCell<Option<(String, u32)>> = RefCell::new(None);
    }

    // `std::panic::set_hook` installs a process-wide hook. Even though our CI/safe-run environment
    // forces single-threaded test execution, other runners may execute tests concurrently. Guard
    // against racy hook replacement by serializing access here.
    static PANIC_HOOK_LOCK: Mutex<()> = Mutex::new(());

    fn capture_panic_location(f: impl FnOnce()) -> (String, u32) {
        let _guard = PANIC_HOOK_LOCK.lock().expect("panic hook lock poisoned");
        LAST_PANIC_LOC.with(|cell| cell.borrow_mut().take());
        let prev = panic::take_hook();
        panic::set_hook(Box::new(|info| {
            if let Some(loc) = info.location() {
                LAST_PANIC_LOC.with(|cell| {
                    *cell.borrow_mut() = Some((loc.file().to_string(), loc.line()))
                });
            }
        }));

        let result = panic::catch_unwind(panic::AssertUnwindSafe(f));
        panic::set_hook(prev);

        assert!(result.is_err(), "expected a panic");
        LAST_PANIC_LOC.with(|cell| cell.borrow().clone().expect("panic hook did not capture a location"))
    }

    #[test]
    fn page_versions_bump_wraps_u32() {
        let tracker = PageVersionTracker::default();
        let page = 0u64;
        tracker.set_version(page, u32::MAX);
        tracker.bump_write(page << PAGE_SHIFT, 1);
        assert_eq!(tracker.version(page), 0);
    }

    #[test]
    fn page_versions_table_ptr_len_is_null_when_len_zero() {
        let tracker = PageVersionTracker::new(0);
        let (ptr, len) = tracker.table_ptr_len();
        assert!(ptr.is_null());
        assert_eq!(len, 0);
    }

    #[test]
    fn page_versions_ensure_tracked_pages_panics_at_call_site() {
        let tracker = PageVersionTracker::new(4);

        let expected_file = file!();
        let expected_line = line!() + 2;
        let (file, line) = capture_panic_location(|| {
            tracker.ensure_tracked_pages(5);
        });
        assert_eq!(file, expected_file);
        assert_eq!(line, expected_line);
    }

    #[test]
    fn jit_runtime_ensure_code_version_table_len_panics_at_call_site() {
        let rt = make_runtime(JitConfig {
            code_version_max_pages: 4,
            ..Default::default()
        });

        let expected_file = file!();
        let expected_line = line!() + 2;
        let (file, line) = capture_panic_location(|| {
            rt.ensure_code_version_table_len(5);
        });
        assert_eq!(file, expected_file);
        assert_eq!(line, expected_line);
    }

    #[test]
    fn jit_runtime_clamps_code_version_max_pages_to_hard_cap() {
        let config = JitConfig {
            code_version_max_pages: PageVersionTracker::MAX_TRACKED_PAGES.saturating_add(123),
            ..Default::default()
        };
        let rt = make_runtime(config);
        assert_eq!(
            rt.config().code_version_max_pages,
            PageVersionTracker::MAX_TRACKED_PAGES
        );
        assert_eq!(
            rt.page_versions().versions_len(),
            PageVersionTracker::MAX_TRACKED_PAGES
        );
    }

    #[test]
    fn page_versions_snapshot_validation_invalidates_on_write() {
        let entry_rip = 0x1000u64;
        let code_paddr = 0x2000u64;
        let config = JitConfig {
            // Keep hotness-based compilation out of the way; we only want requests triggered by
            // invalidation.
            hot_threshold: 100,
            ..Default::default()
        };
        let mut jit = make_runtime(config);

        let meta = jit.snapshot_meta(code_paddr, 1);
        jit.install_handle(CompiledBlockHandle {
            entry_rip,
            table_index: 0,
            meta,
        });
        assert!(jit.is_compiled(entry_rip));
        assert!(jit.prepare_block(entry_rip).is_some());

        // Mutate the page after installation; the cached block should now be considered stale.
        jit.on_guest_write(code_paddr, 1);
        assert!(jit.prepare_block(entry_rip).is_none());
        assert!(!jit.is_compiled(entry_rip));
        assert_eq!(jit.compile.requests, vec![entry_rip]);
    }

    #[test]
    fn metrics_cache_miss() {
        let mut rt = make_runtime(JitConfig {
            hot_threshold: 100,
            ..Default::default()
        });
        let metrics = Arc::new(MockMetricsSink::default());
        rt.set_metrics_sink(Some(metrics.clone()));

        assert!(rt.prepare_block(0x1000).is_none());

        assert_eq!(metrics.cache_hit(), 0);
        assert_eq!(metrics.cache_miss(), 1);
    }

    #[test]
    fn metrics_cache_hit() {
        let mut rt = make_runtime(JitConfig::default());
        let metrics = Arc::new(MockMetricsSink::default());
        rt.set_metrics_sink(Some(metrics.clone()));

        rt.install_block(0x1000, 1, 0, 4);
        let handle = rt.prepare_block(0x1000);
        assert!(handle.is_some());

        assert_eq!(metrics.cache_hit(), 1);
        assert_eq!(metrics.cache_miss(), 0);
    }

    #[test]
    fn metrics_compile_request_only_once_per_entry() {
        let mut rt = make_runtime(JitConfig {
            hot_threshold: 1,
            ..Default::default()
        });
        let metrics = Arc::new(MockMetricsSink::default());
        rt.set_metrics_sink(Some(metrics.clone()));

        for _ in 0..5 {
            assert!(rt.prepare_block(0x2000).is_none());
        }

        assert_eq!(metrics.compile_request(), 1);
        assert_eq!(rt.compile.requests.len(), 1);
        assert_eq!(rt.compile.requests[0], 0x2000);
    }

    #[test]
    fn metrics_eviction_updates_cache_bytes() {
        let mut rt = make_runtime(JitConfig {
            cache_max_bytes: 10,
            ..Default::default()
        });
        let metrics = Arc::new(MockMetricsSink::default());
        rt.set_metrics_sink(Some(metrics.clone()));

        rt.install_block(0x1000, 0, 0, 6);
        assert_eq!(metrics.cache_used(), 6);
        assert_eq!(metrics.cache_capacity(), 10);

        rt.install_block(0x2000, 1, 0, 6);
        // Second install should evict the first (6+6 > 10), leaving one entry.
        assert_eq!(metrics.evict(), 1);
        assert_eq!(metrics.cache_used(), 6);
        assert_eq!(metrics.cache_capacity(), 10);
        assert_eq!(rt.cache_len(), 1);
    }

    #[test]
    fn metrics_stale_install_reject() {
        let mut rt = make_runtime(JitConfig::default());
        let metrics = Arc::new(MockMetricsSink::default());
        rt.set_metrics_sink(Some(metrics.clone()));

        let meta = rt.snapshot_meta(0, 1);
        // Mutate the page versions after taking the snapshot, making `meta` stale.
        rt.on_guest_write(0, 1);

        rt.install_handle(CompiledBlockHandle {
            entry_rip: 0x3000,
            table_index: 0,
            meta,
        });

        assert_eq!(metrics.stale_reject(), 1);
        assert_eq!(metrics.install(), 0);
    }

    #[test]
    fn aero_perf_jit_metrics_can_be_used_as_runtime_metrics_sink() {
        let mut rt = make_runtime(JitConfig {
            hot_threshold: 1,
            cache_max_bytes: 10,
            ..Default::default()
        });
        let metrics = Arc::new(aero_perf::jit::JitMetrics::new(true));
        rt.set_metrics_sink(Some(metrics.clone()));

        // 1) Cache miss + compile request (hot_threshold=1).
        assert!(rt.prepare_block(0x1000).is_none());

        // 2) Install two blocks; the second forces eviction due to max_bytes.
        rt.install_block(0x1000, 0, 0, 6);
        rt.install_block(0x2000, 1, 0, 6);

        // 3) Explicit invalidation.
        assert!(rt.invalidate_block(0x2000));

        // 4) Stale install reject triggers another compile request.
        let meta = rt.snapshot_meta(0x3000, 1);
        rt.on_guest_write(0x3000, 1);
        rt.install_handle(CompiledBlockHandle {
            entry_rip: 0x3000,
            table_index: 0,
            meta,
        });

        let totals = metrics.snapshot_totals();
        assert_eq!(totals.cache_lookup_hit_total, 0);
        assert_eq!(totals.cache_lookup_miss_total, 1);
        assert_eq!(totals.cache_install_total, 2);
        assert_eq!(totals.cache_evict_total, 1);
        assert_eq!(totals.cache_invalidate_total, 1);
        assert_eq!(totals.cache_stale_install_reject_total, 1);
        assert_eq!(totals.compile_request_total, 2);
        assert_eq!(totals.code_cache_used_bytes, 0);
        assert_eq!(totals.code_cache_capacity_bytes, 10);
    }

    #[test]
    fn reset_clears_cache_and_hotness_profile() {
        let entry_rip = 0x1000u64;
        let config = JitConfig {
            hot_threshold: 2,
            cache_max_blocks: 16,
            ..Default::default()
        };

        let mut jit = make_runtime(config);

        // Warm the profile and requested set by making the block hot without a compiled handle.
        assert!(jit.prepare_block(entry_rip).is_none());
        assert_eq!(jit.hotness(entry_rip), 1);
        assert!(jit.compile.requests.is_empty());

        assert!(jit.prepare_block(entry_rip).is_none());
        assert_eq!(jit.hotness(entry_rip), 2);
        assert_eq!(jit.compile.requests, vec![entry_rip]);

        // Install a compiled block and ensure it is now returned by `prepare_block`.
        jit.install_block(entry_rip, 0, 0x2000, 16);
        assert_eq!(jit.cache_len(), 1);
        assert!(jit.is_compiled(entry_rip));
        assert_eq!(jit.cache.current_bytes(), 16);
        assert!(jit.prepare_block(entry_rip).is_some());

        // Reset should behave like a cold start: cache empty, counters zero, and compile requests
        // can be re-issued even if they were previously requested.
        assert_ne!(jit.stats_snapshot(), JitRuntimeStatsSnapshot::default());
        jit.reset();
        assert_eq!(jit.cache_len(), 0);
        assert!(!jit.is_compiled(entry_rip));
        assert_eq!(jit.hotness(entry_rip), 0);
        assert!(jit.cache.is_empty());
        assert_eq!(jit.cache.current_bytes(), 0);
        assert_eq!(jit.stats_snapshot(), JitRuntimeStatsSnapshot::default());

        assert!(jit.prepare_block(entry_rip).is_none());
        assert_eq!(jit.hotness(entry_rip), 1);
        assert_eq!(jit.compile.requests, vec![entry_rip]);

        assert!(jit.prepare_block(entry_rip).is_none());
        assert_eq!(jit.hotness(entry_rip), 2);
        assert_eq!(jit.compile.requests, vec![entry_rip, entry_rip]);
    }

    #[test]
    fn reset_clears_page_versions_and_invalidates_old_snapshots() {
        let entry_rip = 0x3000u64;
        let code_paddr = 0x4000u64;
        let max_pages = 64usize;
        let config = JitConfig {
            hot_threshold: 1,
            cache_max_blocks: 16,
            code_version_max_pages: max_pages,
            ..Default::default()
        };

        let mut jit = make_runtime(config);

        // Simulate guest code modification so the page version is non-zero.
        jit.on_guest_write(code_paddr, 1);
        let code_page = code_paddr >> PAGE_SHIFT;
        assert_eq!(jit.page_versions().version(code_page), 1);

        // A handle compiled against the old page-version snapshot should be rejected after reset.
        let old_meta = jit.snapshot_meta(code_paddr, 1);
        assert_eq!(old_meta.page_versions.len(), 1);
        assert_eq!(old_meta.page_versions[0].version, 1);

        jit.reset();
        assert_eq!(
            jit.page_versions().version(code_page),
            0,
            "reset should restore all pages to version 0"
        );
        assert_eq!(jit.page_versions().versions_len(), max_pages);

        let old_handle = CompiledBlockHandle {
            entry_rip,
            table_index: 0,
            meta: old_meta,
        };
        jit.install_handle(old_handle);

        assert_eq!(jit.cache_len(), 0);
        assert!(!jit.is_compiled(entry_rip));
        assert_eq!(
            jit.compile.requests,
            vec![entry_rip],
            "stale compilation results must be rejected after reset"
        );
    }

    #[test]
    fn reset_updates_metrics_cache_bytes() {
        let mut rt = make_runtime(JitConfig {
            cache_max_bytes: 10,
            code_version_max_pages: 64,
            ..Default::default()
        });
        let metrics = Arc::new(MockMetricsSink::default());
        rt.set_metrics_sink(Some(metrics.clone()));

        rt.install_block(0x1000, 0, 0, 6);
        assert_eq!(metrics.cache_used(), 6);
        assert_eq!(metrics.cache_capacity(), 10);

        rt.reset();
        assert_eq!(metrics.cache_used(), 0);
        assert_eq!(metrics.cache_capacity(), 10);
    }
}
