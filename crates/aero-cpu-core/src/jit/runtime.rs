use crate::jit::cache::{CodeCache, CompiledBlockHandle, CompiledBlockMeta, PageVersionSnapshot};
use crate::jit::profile::HotnessProfile;

pub const PAGE_SIZE: u64 = 4096;
pub const PAGE_SHIFT: u32 = 12;

#[derive(Debug, Clone)]
pub struct JitConfig {
    pub enabled: bool,
    pub hot_threshold: u32,
    pub cache_max_blocks: usize,
    pub cache_max_bytes: usize,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct JitRuntimeStats {
    pub cache_lookup_hit_total: u64,
    pub cache_lookup_miss_total: u64,
    /// Total number of `CompileRequestSink::request_compile` calls issued by the runtime.
    pub compile_requests_total: u64,
    pub blocks_installed_total: u64,
    /// Number of blocks evicted as a result of `CodeCache::insert` capacity pressure.
    pub blocks_evicted_total: u64,
    /// Number of blocks invalidated (explicitly via `invalidate_block` or implicitly due to stale
    /// page-version checks).
    pub blocks_invalidated_total: u64,
    /// Number of compilation results rejected because their page-version snapshots were stale at
    /// install time.
    pub stale_install_rejected_total: u64,
}

impl Default for JitConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            hot_threshold: 32,
            cache_max_blocks: 1024,
            cache_max_bytes: 0,
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

#[derive(Debug, Default, Clone)]
pub struct PageVersionTracker {
    /// Page version table indexed by 4KiB physical page number.
    ///
    /// This is intentionally a dense table so it can be exposed to generated JIT code as a
    /// contiguous `u32` array (one entry per page). Pages outside the table implicitly have
    /// version 0.
    versions: Vec<u32>,
}

impl PageVersionTracker {
    /// Hard cap on the number of tracked 4KiB pages in [`Self::versions`].
    ///
    /// The table is dense and grows on-demand when guest writes are observed. Without a cap, a
    /// hostile/buggy caller could pass an absurd guest physical address (e.g. `u64::MAX`) to
    /// [`Self::bump_write`] and force `Vec::resize()` to attempt allocating terabytes.
    ///
    /// `4_194_304` pages = 16GiB of guest-physical address space, and requires at most 16MiB of host
    /// memory for the version table (`u32` per page). This comfortably covers realistic guests
    /// while remaining safe for CI / memory-limited sandboxes.
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

    pub fn version(&self, page: u64) -> u32 {
        if page >= Self::MAX_TRACKED_PAGES as u64 {
            return 0;
        }
        let Ok(idx) = usize::try_from(page) else {
            return 0;
        };
        self.versions.get(idx).copied().unwrap_or(0)
    }

    /// Sets an explicit version for a page.
    ///
    /// This is primarily used by unit tests and tooling; normal execution should use
    /// [`Self::bump_write`].
    pub fn set_version(&mut self, page: u64, version: u32) {
        if page >= Self::MAX_TRACKED_PAGES as u64 {
            return;
        }
        let Ok(idx) = usize::try_from(page) else {
            return;
        };
        if self.versions.len() <= idx {
            self.versions.resize(idx + 1, 0);
        }
        self.versions[idx] = version;
    }

    pub fn bump_write(&mut self, paddr: u64, len: usize) {
        if len == 0 {
            return;
        }

        let start_page = paddr >> PAGE_SHIFT;
        let end = paddr.saturating_add(len as u64 - 1);
        let end_page = end >> PAGE_SHIFT;

        let max_page = (Self::MAX_TRACKED_PAGES as u64).saturating_sub(1);
        if start_page > max_page {
            return;
        };
        let clamped_end_page = end_page.min(max_page);

        let Ok(end_idx) = usize::try_from(clamped_end_page) else {
            return;
        };

        if self.versions.len() <= end_idx {
            self.versions.resize(end_idx + 1, 0);
        }

        let start_idx = start_page as usize;
        for v in &mut self.versions[start_idx..=end_idx] {
            *v = v.saturating_add(1);
        }
    }

    pub fn snapshot(&self, code_paddr: u64, byte_len: u32) -> Vec<PageVersionSnapshot> {
        if byte_len == 0 {
            return Vec::new();
        }
        let start_page = code_paddr >> PAGE_SHIFT;
        let end = code_paddr.saturating_add(byte_len as u64 - 1);
        let end_page = end >> PAGE_SHIFT;

        let page_count = end_page
            .saturating_sub(start_page)
            .saturating_add(1);
        let max_pages = Self::MAX_SNAPSHOT_PAGES as u64;
        let clamped_pages = page_count.min(max_pages);
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
    /// This is primarily intended for tests and tooling; the version table grows on-demand up to
    /// [`Self::MAX_TRACKED_PAGES`].
    pub fn versions_len(&self) -> usize {
        self.versions.len()
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
}

impl<B, C> JitRuntime<B, C>
where
    B: JitBackend,
    C: CompileRequestSink,
{
    pub fn new(config: JitConfig, backend: B, compile: C) -> Self {
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
            page_versions: PageVersionTracker::default(),
        }
    }

    pub fn config(&self) -> &JitConfig {
        &self.config
    }

    #[inline]
    pub fn stats_snapshot(&self) -> JitRuntimeStats {
        self.stats
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
    /// This is primarily intended for debugging and unit tests.
    pub fn page_versions(&self) -> &PageVersionTracker {
        &self.page_versions
    }

    pub fn on_guest_write(&mut self, paddr: u64, len: usize) {
        self.page_versions.bump_write(paddr, len);
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
        if !self.is_block_valid(&handle) {
            self.stats.stale_install_rejected_total =
                self.stats.stale_install_rejected_total.saturating_add(1);
            // A background compilation result can arrive after the guest has modified the code.
            // Installing such a block would be incorrect; reject it and request recompilation.
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
                    self.stats.blocks_invalidated_total =
                        self.stats.blocks_invalidated_total.saturating_add(1);
                }
                self.profile.clear_requested(entry_rip);
            }

            self.profile.mark_requested(entry_rip);
            self.stats.compile_requests_total =
                self.stats.compile_requests_total.saturating_add(1);
            self.compile.request_compile(entry_rip);
            return Vec::new();
        }

        self.stats.blocks_installed_total = self.stats.blocks_installed_total.saturating_add(1);
        let evicted = self.cache.insert(handle);
        let evicted_count = u64::try_from(evicted.len()).unwrap_or(u64::MAX);
        self.stats.blocks_evicted_total = self
            .stats
            .blocks_evicted_total
            .saturating_add(evicted_count);
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
        if self.cache.remove(entry_rip).is_some() {
            self.stats.blocks_invalidated_total =
                self.stats.blocks_invalidated_total.saturating_add(1);
            self.profile.clear_requested(entry_rip);
            return true;
        }
        false
    }

    pub fn prepare_block(&mut self, entry_rip: u64) -> Option<CompiledBlockHandle> {
        if !self.config.enabled {
            return None;
        }

        let mut handle = self.cache.get_cloned(entry_rip);
        if let Some(ref h) = handle {
            if !self.is_block_valid(h) {
                if self.cache.remove(entry_rip).is_some() {
                    self.stats.blocks_invalidated_total =
                        self.stats.blocks_invalidated_total.saturating_add(1);
                }
                self.profile.clear_requested(entry_rip);
                self.profile.mark_requested(entry_rip);
                self.stats.compile_requests_total =
                    self.stats.compile_requests_total.saturating_add(1);
                self.compile.request_compile(entry_rip);
                handle = None;
            }
        }

        if handle.is_some() {
            self.stats.cache_lookup_hit_total =
                self.stats.cache_lookup_hit_total.saturating_add(1);
        } else {
            self.stats.cache_lookup_miss_total =
                self.stats.cache_lookup_miss_total.saturating_add(1);
        }

        let has_compiled = handle.is_some();
        if self.profile.record_hit(entry_rip, has_compiled) {
            self.stats.compile_requests_total =
                self.stats.compile_requests_total.saturating_add(1);
            self.compile.request_compile(entry_rip);
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
            end_page
                .saturating_sub(start_page)
                .saturating_add(1)
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
}
