//! `aero-gpu` contains GPU-side utilities used by Aero.
//!
//! Currently this crate provides:
//! - High-throughput dynamic buffer upload helpers (see [`UploadRingBuffer`]).
//! - Centralized caching of WGSL shader modules and render/compute pipelines
//!   (see [`pipeline_cache::PipelineCache`]).
//! - An internal GPU command stream format plus CPU-side optimization and wgpu
//!   encoding (see [`cmd`]).
//! - Optional GPU/CPU frame timing collection (see [`profiler`]).
//! - Texture management with BCn CPU fallback + readback utilities.

mod buffer_arena;
mod context;
mod dirty_rect;
mod error;
mod present;
#[cfg(feature = "diff-engine")]
mod tile_diff;
mod upload;

mod bc_decompress;
mod readback;
mod texture_format;
mod texture_manager;

pub mod cmd;
pub mod pipeline_cache;
pub mod pipeline_key;
pub mod protocol_d3d11;
pub mod profiler;
pub mod stats;

pub use bc_decompress::{decompress_bc1_rgba8, decompress_bc3_rgba8, decompress_bc7_rgba8};
pub use buffer_arena::BufferArena;
pub use context::GpuContext;
pub use dirty_rect::{merge_and_cap_rects, Rect, RectMergeOutcome};
pub use error::GpuError;
pub use present::{PresentError, PresentTelemetry, Presenter, TextureWriter};
pub use profiler::{
    FrameTimingsReport, GpuBackendKind, GpuProfiler, GpuProfilerConfig, GpuTimestampPhase,
};
pub use readback::readback_rgba8;
pub use texture_format::{TextureFormat, TextureFormatSelection, TextureUploadTransform};
pub use texture_manager::{
    SamplerDesc, TextureDesc, TextureKey, TextureManager, TextureManagerError, TextureManagerStats,
    TextureRegion, TextureViewDesc,
};
pub use upload::{
    BufferSliceHandle, DynamicOffset, GpuCapabilities, UploadRingBuffer, UploadRingBufferDescriptor,
    UploadStats,
};

