use std::sync::atomic::{AtomicU64, Ordering};

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

/// Telemetry counters for the GPU subsystem (presentation + recovery).
///
/// These counters are designed to be cheap to update on the render thread and
/// are safe to read from another thread when forwarded over IPC.
#[derive(Debug, Default)]
pub struct GpuStats {
    presents_attempted: AtomicU64,
    presents_succeeded: AtomicU64,
    recoveries_attempted: AtomicU64,
    recoveries_succeeded: AtomicU64,
    surface_reconfigures: AtomicU64,

    // ---------------------------------------------------------------------
    // D3D9 shader translation + cache counters (WG-010(D))
    // ---------------------------------------------------------------------
    d3d9_shader_translate_calls: AtomicU64,
    d3d9_shader_cache_persistent_hits: AtomicU64,
    d3d9_shader_cache_persistent_misses: AtomicU64,
    d3d9_shader_cache_memory_hits: AtomicU64,
    /// 0/1 flag: whether the persistent shader cache is disabled/unavailable.
    d3d9_shader_cache_disabled: AtomicU64,
}

impl GpuStats {
    pub fn new() -> Self {
        let stats = Self::default();

        // Persistent caching is only supported in the browser/WASM build; native builds always
        // report `disabled=1`.
        #[cfg(not(target_arch = "wasm32"))]
        {
            stats
                .d3d9_shader_cache_disabled
                .store(1, Ordering::Relaxed);
        }
        stats
    }

    pub fn inc_presents_attempted(&self) {
        self.presents_attempted.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_presents_succeeded(&self) {
        self.presents_succeeded.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_recoveries_attempted(&self) {
        self.recoveries_attempted.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_recoveries_succeeded(&self) {
        self.recoveries_succeeded.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_surface_reconfigures(&self) {
        self.surface_reconfigures.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_d3d9_shader_translate_calls(&self) {
        self.d3d9_shader_translate_calls
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_d3d9_shader_cache_persistent_hits(&self) {
        self.d3d9_shader_cache_persistent_hits
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_d3d9_shader_cache_persistent_misses(&self) {
        self.d3d9_shader_cache_persistent_misses
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_d3d9_shader_cache_memory_hits(&self) {
        self.d3d9_shader_cache_memory_hits
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn set_d3d9_shader_cache_disabled(&self, disabled: bool) {
        self.d3d9_shader_cache_disabled
            .store(disabled as u64, Ordering::Relaxed);
    }

    // Compatibility aliases (older call sites).
    pub fn inc_d3d9_shader_persistent_hits(&self) {
        self.inc_d3d9_shader_cache_persistent_hits();
    }

    pub fn inc_d3d9_shader_persistent_misses(&self) {
        self.inc_d3d9_shader_cache_persistent_misses();
    }

    pub fn snapshot(&self) -> GpuStatsSnapshot {
        GpuStatsSnapshot {
            presents_attempted: self.presents_attempted.load(Ordering::Relaxed),
            presents_succeeded: self.presents_succeeded.load(Ordering::Relaxed),
            recoveries_attempted: self.recoveries_attempted.load(Ordering::Relaxed),
            recoveries_succeeded: self.recoveries_succeeded.load(Ordering::Relaxed),
            surface_reconfigures: self.surface_reconfigures.load(Ordering::Relaxed),

            d3d9_shader_translate_calls: self.d3d9_shader_translate_calls.load(Ordering::Relaxed),
            d3d9_shader_cache_persistent_hits: self
                .d3d9_shader_cache_persistent_hits
                .load(Ordering::Relaxed),
            d3d9_shader_cache_persistent_misses: self
                .d3d9_shader_cache_persistent_misses
                .load(Ordering::Relaxed),
            d3d9_shader_cache_memory_hits: self
                .d3d9_shader_cache_memory_hits
                .load(Ordering::Relaxed),
            d3d9_shader_cache_disabled: self.d3d9_shader_cache_disabled.load(Ordering::Relaxed),
        }
    }

    /// Returns a JSON object as a string.
    pub fn to_json(&self) -> String {
        self.snapshot().to_json()
    }

    /// Compatibility name for callers that expect a `get_gpu_stats()` method.
    pub fn get_gpu_stats(&self) -> String {
        self.to_json()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpuStatsSnapshot {
    pub presents_attempted: u64,
    pub presents_succeeded: u64,
    pub recoveries_attempted: u64,
    pub recoveries_succeeded: u64,
    pub surface_reconfigures: u64,

    pub d3d9_shader_translate_calls: u64,
    pub d3d9_shader_cache_persistent_hits: u64,
    pub d3d9_shader_cache_persistent_misses: u64,
    pub d3d9_shader_cache_memory_hits: u64,
    pub d3d9_shader_cache_disabled: u64,
}

impl GpuStatsSnapshot {
    pub fn to_json(self) -> String {
        // Note: This is hand-built JSON for speed/alloc avoidance on the render thread.
        format!(
            "{{\"presents_attempted\":{},\"presents_succeeded\":{},\"recoveries_attempted\":{},\"recoveries_succeeded\":{},\"surface_reconfigures\":{},\"d3d9_shader_translate_calls\":{},\"d3d9_shader_cache_persistent_hits\":{},\"d3d9_shader_cache_persistent_misses\":{},\"d3d9_shader_cache_memory_hits\":{},\"d3d9_shader_cache_disabled\":{},\"d3d9_shader_cache\":{{\"translate_calls\":{},\"persistent_hits\":{},\"persistent_misses\":{},\"memory_hits\":{},\"disabled\":{}}}}}",
            self.presents_attempted,
            self.presents_succeeded,
            self.recoveries_attempted,
            self.recoveries_succeeded,
            self.surface_reconfigures,
            self.d3d9_shader_translate_calls,
            self.d3d9_shader_cache_persistent_hits,
            self.d3d9_shader_cache_persistent_misses,
            self.d3d9_shader_cache_memory_hits,
            self.d3d9_shader_cache_disabled,
            self.d3d9_shader_translate_calls,
            self.d3d9_shader_cache_persistent_hits,
            self.d3d9_shader_cache_persistent_misses,
            self.d3d9_shader_cache_memory_hits,
            self.d3d9_shader_cache_disabled,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_json_contains_counters() {
        let stats = GpuStats::new();
        stats.inc_presents_attempted();
        stats.inc_d3d9_shader_translate_calls();
        stats.inc_surface_reconfigures();
        stats.inc_d3d9_shader_translate_calls();
        stats.inc_d3d9_shader_cache_memory_hits();
        let json = stats.to_json();
        assert!(json.contains("\"presents_attempted\":1"));
        assert!(json.contains("\"surface_reconfigures\":1"));
        assert!(json.contains("\"d3d9_shader_translate_calls\":2"));
        assert!(json.contains("\"d3d9_shader_cache_memory_hits\":1"));
        // Nested object (used by browser E2E harnesses).
        assert!(json.contains("\"translate_calls\":2"));
        assert!(json.contains("\"memory_hits\":1"));
    }
}
