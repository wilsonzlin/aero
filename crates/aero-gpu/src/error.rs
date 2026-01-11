use crate::pipeline_key::{ShaderHash, ShaderStage};

/// GPU-layer errors that should be actionable for higher layers.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum GpuError {
    /// A capability is required but unavailable on the current backend/device.
    #[error("unsupported GPU feature: {0}")]
    Unsupported(&'static str),

    /// Backend-specific failure that doesn't fit a structured variant.
    #[error("GPU backend error: {0}")]
    Backend(String),

    /// A backend-owned generational handle was invalid (out of bounds, stale generation, or deleted).
    #[error("invalid GPU handle (kind={kind}, index={index}, generation={generation})")]
    InvalidHandle {
        kind: &'static str,
        index: u32,
        generation: u32,
    },

    /// A shader module was referenced by hash/stage but has not been registered in
    /// the shader module cache.
    #[error(
        "shader module not found in cache (stage={stage:?}, hash=0x{hash:032x}). \
         Ensure you called PipelineCache::get_or_create_shader_module for this WGSL."
    )]
    MissingShaderModule {
        stage: ShaderStage,
        hash: ShaderHash,
    },
}
