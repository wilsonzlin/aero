/// Snapshot of pipeline cache counters, suitable for profiling/telemetry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PipelineCacheStats {
    pub shader_module_hits: u64,
    pub shader_module_misses: u64,
    pub shader_module_evictions: u64,
    pub shader_modules: u64,

    pub render_pipeline_hits: u64,
    pub render_pipeline_misses: u64,
    pub render_pipeline_evictions: u64,
    pub render_pipelines: u64,

    pub compute_pipeline_hits: u64,
    pub compute_pipeline_misses: u64,
    pub compute_pipeline_evictions: u64,
    pub compute_pipelines: u64,
}

