use std::collections::HashMap;

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

#[derive(Debug, Default)]
pub struct PageVersionTracker {
    versions: HashMap<u64, u32>,
}

impl PageVersionTracker {
    pub fn version(&self, page: u64) -> u32 {
        self.versions.get(&page).copied().unwrap_or(0)
    }

    pub fn bump_write(&mut self, paddr: u64, len: usize) {
        if len == 0 {
            return;
        }

        let start_page = paddr >> PAGE_SHIFT;
        let end = paddr.checked_add(len as u64 - 1).unwrap_or(u64::MAX);
        let end_page = end >> PAGE_SHIFT;

        for page in start_page..=end_page {
            let v = self.versions.entry(page).or_insert(0);
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

    pub fn make_meta(&self, code_paddr: u64, byte_len: u32) -> CompiledBlockMeta {
        CompiledBlockMeta {
            code_paddr,
            byte_len,
            page_versions: self.page_versions.snapshot(code_paddr, byte_len),
        }
    }

    pub fn install_block(
        &mut self,
        entry_rip: u64,
        table_index: u32,
        code_paddr: u64,
        byte_len: u32,
    ) -> Vec<u64> {
        let handle = CompiledBlockHandle {
            entry_rip,
            table_index,
            meta: self.make_meta(code_paddr, byte_len),
        };

        let evicted = self.cache.insert(handle);
        for rip in &evicted {
            self.profile.clear_requested(*rip);
        }
        evicted
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
