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
    pub fn version(&self, page: u64) -> u32 {
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
        let end = paddr.checked_add(len as u64 - 1).unwrap_or(u64::MAX);
        let end_page = end >> PAGE_SHIFT;

        let Ok(end_idx) = usize::try_from(end_page) else {
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
        let end = code_paddr
            .checked_add(byte_len as u64 - 1)
            .unwrap_or(u64::MAX);
        let end_page = end >> PAGE_SHIFT;

        (start_page..=end_page)
            .map(|page| PageVersionSnapshot {
                page,
                version: self.version(page),
            })
            .collect()
    }
}

pub struct JitRuntime<B, C> {
    config: JitConfig,
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
        let profile = HotnessProfile::new(config.hot_threshold);
        Self {
            config,
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

    pub fn cache_len(&self) -> usize {
        self.cache.len()
    }

    pub fn is_compiled(&self, entry_rip: u64) -> bool {
        self.cache.contains(entry_rip)
    }

    pub fn hotness(&self, entry_rip: u64) -> u32 {
        self.profile.counter(entry_rip)
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
                self.cache.remove(entry_rip);
                self.profile.clear_requested(entry_rip);
            }

            self.profile.mark_requested(entry_rip);
            self.compile.request_compile(entry_rip);
            return Vec::new();
        }

        let evicted = self.cache.insert(handle);
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
                self.cache.remove(entry_rip);
                self.profile.clear_requested(entry_rip);
                self.profile.mark_requested(entry_rip);
                self.compile.request_compile(entry_rip);
                handle = None;
            }
        }

        let has_compiled = handle.is_some();
        if self.profile.record_hit(entry_rip, has_compiled) {
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
        for snapshot in &handle.meta.page_versions {
            if self.page_versions.version(snapshot.page) != snapshot.version {
                return false;
            }
        }
        true
    }
}
