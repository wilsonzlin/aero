use crate::pipeline_cache::{PipelineCache, PipelineCacheConfig};
use crate::GpuCapabilities;

/// Lightweight wrapper around a `wgpu::Device` that owns a [`PipelineCache`].
///
/// When a `wgpu::Device` is lost and recreated, pipelines and shader modules from
/// the previous device become invalid. `GpuContext::replace_device` clears caches
/// to ensure we never reuse old objects.
pub struct GpuContext {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub capabilities: GpuCapabilities,
    pub pipelines: PipelineCache,

    pipeline_cache_config: PipelineCacheConfig,
}

impl GpuContext {
    pub fn new(
        device: wgpu::Device,
        queue: wgpu::Queue,
        capabilities: GpuCapabilities,
        pipeline_cache_config: PipelineCacheConfig,
    ) -> Self {
        let pipelines = PipelineCache::new(pipeline_cache_config.clone(), capabilities);
        Self {
            device,
            queue,
            capabilities,
            pipelines,
            pipeline_cache_config,
        }
    }

    /// Replace the underlying device/queue (e.g. after device-lost recovery).
    ///
    /// This clears pipeline/shader caches and re-applies capabilities.
    pub fn replace_device(
        &mut self,
        device: wgpu::Device,
        queue: wgpu::Queue,
        capabilities: GpuCapabilities,
    ) {
        self.device = device;
        self.queue = queue;
        self.capabilities = capabilities;

        // Pipelines and shader modules are tied to the old device.
        self.pipelines = PipelineCache::new(self.pipeline_cache_config.clone(), capabilities);
    }
}

