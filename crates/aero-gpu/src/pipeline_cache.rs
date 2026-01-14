use std::collections::HashMap;
use std::hash::Hash;
use std::num::NonZeroUsize;

use lru::LruCache;
#[cfg(not(target_arch = "wasm32"))]
use tracing::warn;

use crate::error::GpuError;
use crate::passthrough_vs::PassthroughVertexShaderKey;
use crate::pipeline_key::{
    hash_wgsl, ComputePipelineKey, RenderPipelineKey, ShaderHash, ShaderModuleKey, ShaderStage,
};
use crate::stats::PipelineCacheStats;
use crate::GpuCapabilities;

#[derive(Clone, Debug)]
pub struct PipelineCacheConfig {
    pub max_shader_modules: Option<NonZeroUsize>,
    pub max_render_pipelines: Option<NonZeroUsize>,
    pub max_compute_pipelines: Option<NonZeroUsize>,
}

impl PipelineCacheConfig {
    const DEFAULT_MAX_SHADER_MODULES: usize = 2048;
    const DEFAULT_MAX_RENDER_PIPELINES: usize = 1024;
    const DEFAULT_MAX_COMPUTE_PIPELINES: usize = 256;

    #[cfg(not(target_arch = "wasm32"))]
    fn parse_env_nonzero_usize(var: &'static str) -> Option<NonZeroUsize> {
        let raw = match std::env::var(var) {
            Ok(v) => v,
            Err(std::env::VarError::NotPresent) => return None,
            Err(std::env::VarError::NotUnicode(_)) => {
                warn!(
                    env_var = var,
                    "Ignoring {var} because it is not valid unicode"
                );
                return None;
            }
        };

        let trimmed = raw.trim();
        if trimmed.is_empty() {
            warn!(env_var = var, value = %raw, "Ignoring empty {var}");
            return None;
        }

        match trimmed.parse::<usize>() {
            Ok(value) => match NonZeroUsize::new(value) {
                Some(v) => Some(v),
                None => {
                    warn!(
                        env_var = var,
                        value = %raw,
                        "Ignoring {var} because 0 is not a valid cache size"
                    );
                    None
                }
            },
            Err(err) => {
                warn!(
                    env_var = var,
                    value = %raw,
                    error = %err,
                    "Ignoring {var} because it is not a valid integer"
                );
                None
            }
        }
    }
}

impl Default for PipelineCacheConfig {
    fn default() -> Self {
        #[cfg_attr(target_arch = "wasm32", allow(unused_mut))]
        let mut config = Self {
            max_shader_modules: Some(
                NonZeroUsize::new(Self::DEFAULT_MAX_SHADER_MODULES)
                    .expect("DEFAULT_MAX_SHADER_MODULES must be non-zero"),
            ),
            max_render_pipelines: Some(
                NonZeroUsize::new(Self::DEFAULT_MAX_RENDER_PIPELINES)
                    .expect("DEFAULT_MAX_RENDER_PIPELINES must be non-zero"),
            ),
            max_compute_pipelines: Some(
                NonZeroUsize::new(Self::DEFAULT_MAX_COMPUTE_PIPELINES)
                    .expect("DEFAULT_MAX_COMPUTE_PIPELINES must be non-zero"),
            ),
        };

        // Allow cache sizes to be tuned in production without code changes.
        //
        // Note: Environment variables are not supported on `wasm32-unknown-unknown`,
        // so we only apply overrides on native targets.
        #[cfg(not(target_arch = "wasm32"))]
        {
            if let Some(v) = Self::parse_env_nonzero_usize("AERO_PIPELINE_CACHE_MAX_SHADER_MODULES")
            {
                config.max_shader_modules = Some(v);
            }
            if let Some(v) =
                Self::parse_env_nonzero_usize("AERO_PIPELINE_CACHE_MAX_RENDER_PIPELINES")
            {
                config.max_render_pipelines = Some(v);
            }
            if let Some(v) =
                Self::parse_env_nonzero_usize("AERO_PIPELINE_CACHE_MAX_COMPUTE_PIPELINES")
            {
                config.max_compute_pipelines = Some(v);
            }
        }

        config
    }
}

#[derive(Debug)]
struct CachedShaderModule {
    module: std::sync::Arc<wgpu::ShaderModule>,
    #[cfg(debug_assertions)]
    source: String,
}

#[derive(Debug)]
enum CacheInner<K: Hash + Eq, V> {
    Unbounded(HashMap<K, V>),
    Lru(LruCache<K, V>),
}

#[derive(Debug)]
struct Cache<K: Hash + Eq, V> {
    inner: CacheInner<K, V>,
}

impl<K, V> Cache<K, V>
where
    K: Hash + Eq,
{
    fn unbounded() -> Self {
        Self {
            inner: CacheInner::Unbounded(HashMap::new()),
        }
    }

    fn lru(cap: NonZeroUsize) -> Self {
        Self {
            inner: CacheInner::Lru(LruCache::new(cap)),
        }
    }

    fn len(&self) -> usize {
        match &self.inner {
            CacheInner::Unbounded(map) => map.len(),
            CacheInner::Lru(cache) => cache.len(),
        }
    }

    fn clear(&mut self) {
        match &mut self.inner {
            CacheInner::Unbounded(map) => map.clear(),
            CacheInner::Lru(cache) => cache.clear(),
        }
    }

    fn peek(&self, key: &K) -> Option<&V> {
        match &self.inner {
            CacheInner::Unbounded(map) => map.get(key),
            CacheInner::Lru(cache) => cache.peek(key),
        }
    }

    fn get(&mut self, key: &K) -> Option<&V> {
        match &mut self.inner {
            CacheInner::Unbounded(map) => map.get(key),
            CacheInner::Lru(cache) => cache.get(key),
        }
    }

    /// Insert and return whether an eviction occurred.
    fn put(&mut self, key: K, value: V) -> bool {
        match &mut self.inner {
            CacheInner::Unbounded(map) => {
                map.insert(key, value);
                false
            }
            CacheInner::Lru(cache) => {
                let existed = cache.peek(&key).is_some();
                let was_full = cache.len() == cache.cap().get();
                cache.put(key, value);
                !existed && was_full
            }
        }
    }
}

/// Central cache for shader modules and render/compute pipelines.
#[derive(Debug)]
pub struct PipelineCache {
    capabilities: GpuCapabilities,
    stats: PipelineCacheStats,

    shader_modules: Cache<ShaderModuleKey, CachedShaderModule>,
    render_pipelines: Cache<RenderPipelineKey, wgpu::RenderPipeline>,
    compute_pipelines: Cache<ComputePipelineKey, wgpu::ComputePipeline>,
}

impl PipelineCache {
    pub fn new(config: PipelineCacheConfig, capabilities: GpuCapabilities) -> Self {
        let shader_modules = match config.max_shader_modules {
            Some(cap) => Cache::lru(cap),
            None => Cache::unbounded(),
        };
        let render_pipelines = match config.max_render_pipelines {
            Some(cap) => Cache::lru(cap),
            None => Cache::unbounded(),
        };
        let compute_pipelines = match config.max_compute_pipelines {
            Some(cap) => Cache::lru(cap),
            None => Cache::unbounded(),
        };

        Self {
            capabilities,
            stats: PipelineCacheStats::default(),
            shader_modules,
            render_pipelines,
            compute_pipelines,
        }
    }

    pub fn stats(&self) -> PipelineCacheStats {
        self.stats
    }

    pub fn clear(&mut self) {
        self.shader_modules.clear();
        self.render_pipelines.clear();
        self.compute_pipelines.clear();

        self.stats.shader_modules = 0;
        self.stats.render_pipelines = 0;
        self.stats.compute_pipelines = 0;
    }

    /// Returns the WGSL source previously registered for this shader, when running
    /// in debug builds.
    #[cfg(debug_assertions)]
    pub fn debug_shader_source(&self, stage: ShaderStage, hash: ShaderHash) -> Option<&str> {
        self.shader_modules
            .peek(&ShaderModuleKey { hash, stage })
            .map(|entry| entry.source.as_str())
    }

    fn update_sizes(&mut self) {
        self.stats.shader_modules = self.shader_modules.len() as u64;
        self.stats.render_pipelines = self.render_pipelines.len() as u64;
        self.stats.compute_pipelines = self.compute_pipelines.len() as u64;
    }

    /// Get or create a `wgpu::ShaderModule` from WGSL source.
    ///
    /// Returns the shader's `ShaderHash` so callers can embed it into pipeline keys.
    pub fn get_or_create_shader_module(
        &mut self,
        device: &wgpu::Device,
        stage: ShaderStage,
        wgsl: &str,
        label: Option<&str>,
    ) -> (ShaderHash, &wgpu::ShaderModule) {
        let hash = hash_wgsl(wgsl);
        let key = ShaderModuleKey { hash, stage };

        if self.shader_modules.peek(&key).is_some() {
            self.stats.shader_module_hits += 1;
            let entry = self.shader_modules.get(&key).expect("peek reported Some");
            return (hash, entry.module.as_ref());
        }

        self.stats.shader_module_misses += 1;

        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label,
            source: wgpu::ShaderSource::Wgsl(wgsl.into()),
        });

        let evicted = self.shader_modules.put(
            key,
            CachedShaderModule {
                module: std::sync::Arc::new(module),
                #[cfg(debug_assertions)]
                source: wgsl.to_owned(),
            },
        );
        if evicted {
            self.stats.shader_module_evictions += 1;
        }

        self.update_sizes();
        let entry = self
            .shader_modules
            .get(&ShaderModuleKey { hash, stage })
            .expect("just inserted shader module; this is only None if it was immediately evicted");
        (hash, entry.module.as_ref())
    }

    /// Get or create a generated passthrough vertex shader module for the given output signature.
    ///
    /// The shader WGSL is generated deterministically from `signature`, hashed via [`hash_wgsl`],
    /// and cached in the same shader-module cache as application-provided shaders.
    pub fn get_or_create_passthrough_vertex_shader(
        &mut self,
        device: &wgpu::Device,
        signature: &PassthroughVertexShaderKey,
    ) -> (ShaderHash, &wgpu::ShaderModule) {
        let wgsl = signature.wgsl();
        self.get_or_create_shader_module(
            device,
            ShaderStage::Vertex,
            &wgsl,
            Some("aero-gpu passthrough vertex shader"),
        )
    }

    fn get_cached_shader_module_arc(
        &mut self,
        stage: ShaderStage,
        hash: ShaderHash,
    ) -> Result<std::sync::Arc<wgpu::ShaderModule>, GpuError> {
        let key = ShaderModuleKey { hash, stage };
        self.shader_modules
            .get(&key)
            .map(|e| e.module.clone())
            .ok_or(GpuError::MissingShaderModule { stage, hash })
    }

    /// Get or create a `wgpu::RenderPipeline`.
    ///
    /// Shader modules referenced by `key.vertex_shader` and `key.fragment_shader` must
    /// already exist in the shader module cache. Use [`Self::get_or_create_shader_module`]
    /// first.
    pub fn get_or_create_render_pipeline<F>(
        &mut self,
        device: &wgpu::Device,
        key: RenderPipelineKey,
        desc_builder: F,
    ) -> Result<&wgpu::RenderPipeline, GpuError>
    where
        F: FnOnce(&wgpu::Device, &wgpu::ShaderModule, &wgpu::ShaderModule) -> wgpu::RenderPipeline,
    {
        if self.render_pipelines.peek(&key).is_some() {
            self.stats.render_pipeline_hits += 1;
            let pipeline = self.render_pipelines.get(&key).expect("peek reported Some");
            return Ok(pipeline);
        }

        self.stats.render_pipeline_misses += 1;

        let vs = self.get_cached_shader_module_arc(ShaderStage::Vertex, key.vertex_shader)?;
        let fs = self.get_cached_shader_module_arc(ShaderStage::Fragment, key.fragment_shader)?;

        let pipeline = desc_builder(device, vs.as_ref(), fs.as_ref());

        let key_clone = key.clone();
        let evicted = self.render_pipelines.put(key, pipeline);
        if evicted {
            self.stats.render_pipeline_evictions += 1;
        }

        self.update_sizes();
        Ok(self
            .render_pipelines
            .get(&key_clone)
            .expect("just inserted render pipeline"))
    }

    /// Get or create a `wgpu::ComputePipeline`.
    ///
    /// If `GpuCapabilities.supports_compute == false`, this returns
    /// `GpuError::Unsupported(\"compute\")` deterministically and does not attempt any
    /// `wgpu` calls.
    pub fn get_or_create_compute_pipeline<F>(
        &mut self,
        device: &wgpu::Device,
        key: ComputePipelineKey,
        desc_builder: F,
    ) -> Result<&wgpu::ComputePipeline, GpuError>
    where
        F: FnOnce(&wgpu::Device, &wgpu::ShaderModule) -> wgpu::ComputePipeline,
    {
        if !self.capabilities.supports_compute {
            return Err(GpuError::Unsupported("compute"));
        }

        if self.compute_pipelines.peek(&key).is_some() {
            self.stats.compute_pipeline_hits += 1;
            let pipeline = self
                .compute_pipelines
                .get(&key)
                .expect("peek reported Some");
            return Ok(pipeline);
        }

        self.stats.compute_pipeline_misses += 1;

        let cs = self.get_cached_shader_module_arc(ShaderStage::Compute, key.shader)?;

        let pipeline = desc_builder(device, cs.as_ref());

        let key_clone = key.clone();
        let evicted = self.compute_pipelines.put(key, pipeline);
        if evicted {
            self.stats.compute_pipeline_evictions += 1;
        }

        self.update_sizes();
        Ok(self
            .compute_pipelines
            .get(&key_clone)
            .expect("just inserted compute pipeline"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    #[test]
    fn lru_eviction_is_least_recently_used() {
        let mut cache: Cache<u32, &'static str> = Cache::lru(NonZeroUsize::new(2).unwrap());

        assert!(!cache.put(1, "a"));
        assert!(!cache.put(2, "b"));

        // Touch key=1, making it MRU and key=2 the LRU.
        assert_eq!(cache.get(&1), Some(&"a"));

        // Inserting a third entry should evict key=2.
        assert!(cache.put(3, "c"));
        assert!(cache.peek(&2).is_none());
        assert_eq!(cache.peek(&1), Some(&"a"));
        assert_eq!(cache.peek(&3), Some(&"c"));
    }

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    struct EnvVarGuard {
        saved: Vec<(&'static str, Option<String>)>,
    }

    impl EnvVarGuard {
        fn new(vars: &[&'static str]) -> Self {
            let saved = vars
                .iter()
                .map(|&key| (key, std::env::var(key).ok()))
                .collect::<Vec<_>>();
            for &key in vars {
                std::env::remove_var(key);
            }
            Self { saved }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            for &(key, ref value) in &self.saved {
                match value {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    #[test]
    fn pipeline_cache_config_default_is_bounded() {
        let _lock = env_lock().lock().unwrap();
        let _guard = EnvVarGuard::new(&[
            "AERO_PIPELINE_CACHE_MAX_SHADER_MODULES",
            "AERO_PIPELINE_CACHE_MAX_RENDER_PIPELINES",
            "AERO_PIPELINE_CACHE_MAX_COMPUTE_PIPELINES",
        ]);

        let config = PipelineCacheConfig::default();
        assert_eq!(
            config.max_shader_modules.unwrap().get(),
            PipelineCacheConfig::DEFAULT_MAX_SHADER_MODULES
        );
        assert_eq!(
            config.max_render_pipelines.unwrap().get(),
            PipelineCacheConfig::DEFAULT_MAX_RENDER_PIPELINES
        );
        assert_eq!(
            config.max_compute_pipelines.unwrap().get(),
            PipelineCacheConfig::DEFAULT_MAX_COMPUTE_PIPELINES
        );
    }

    #[test]
    fn pipeline_cache_config_env_overrides_are_applied() {
        let _lock = env_lock().lock().unwrap();
        let _guard = EnvVarGuard::new(&[
            "AERO_PIPELINE_CACHE_MAX_SHADER_MODULES",
            "AERO_PIPELINE_CACHE_MAX_RENDER_PIPELINES",
            "AERO_PIPELINE_CACHE_MAX_COMPUTE_PIPELINES",
        ]);

        std::env::set_var("AERO_PIPELINE_CACHE_MAX_SHADER_MODULES", "123");
        std::env::set_var("AERO_PIPELINE_CACHE_MAX_RENDER_PIPELINES", "456");
        std::env::set_var("AERO_PIPELINE_CACHE_MAX_COMPUTE_PIPELINES", "789");

        let config = PipelineCacheConfig::default();
        assert_eq!(config.max_shader_modules.unwrap().get(), 123);
        assert_eq!(config.max_render_pipelines.unwrap().get(), 456);
        assert_eq!(config.max_compute_pipelines.unwrap().get(), 789);
    }
}
