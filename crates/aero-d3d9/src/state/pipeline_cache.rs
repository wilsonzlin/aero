use crate::state::tracker::PipelineKey;
use lru::LruCache;
use std::num::NonZeroUsize;
use std::sync::Arc;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PipelineCacheStats {
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub current_size: usize,
    pub max_entries: usize,
}

/// LRU cache for `wgpu::RenderPipeline` objects keyed by translated D3D9 state.
///
/// In WebGPU, pipeline creation is expensive. D3D9 applications frequently
/// re-issue the same state over and over, so caching is critical for stable
/// frame times.
pub struct PipelineCache {
    cache: LruCache<PipelineKey, Arc<wgpu::RenderPipeline>>,
    hits: u64,
    misses: u64,
    evictions: u64,
}

impl PipelineCache {
    pub fn new(max_entries: usize) -> Self {
        let max_entries = max_entries.max(1);
        Self {
            cache: LruCache::new(NonZeroUsize::new(max_entries).expect("max_entries >= 1")),
            hits: 0,
            misses: 0,
            evictions: 0,
        }
    }

    pub fn stats(&self) -> PipelineCacheStats {
        PipelineCacheStats {
            hits: self.hits,
            misses: self.misses,
            evictions: self.evictions,
            current_size: self.cache.len(),
            max_entries: self.cache.cap().get(),
        }
    }

    /// Look up the pipeline for `key`, creating it via `create` if missing.
    ///
    /// Returns a clone of the internal `wgpu::RenderPipeline` handle, which is
    /// cheap (refcounted).
    pub fn get_or_create(
        &mut self,
        key: PipelineKey,
        create: impl FnOnce() -> wgpu::RenderPipeline,
    ) -> Arc<wgpu::RenderPipeline> {
        if let Some(existing) = self.cache.get(&key) {
            self.hits += 1;
            return Arc::clone(existing);
        }

        self.misses += 1;
        let pipeline = Arc::new(create());
        if self.cache.put(key, Arc::clone(&pipeline)).is_some() {
            self.evictions += 1;
        }
        pipeline
    }

    pub fn clear(&mut self) {
        self.cache.clear();
    }
}

impl std::fmt::Debug for PipelineCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PipelineCache")
            .field("stats", &self.stats())
            .finish_non_exhaustive()
    }
}
