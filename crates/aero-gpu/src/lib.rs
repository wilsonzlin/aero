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
//! - Reliability primitives (structured [`GpuErrorEvent`]s, device-lost recovery,
//!   and present/surface retry helpers) used by the browser GPU subsystem.

mod buffer_arena;
mod context;
mod dirty_rect;
mod error;
mod error_event;
mod present;
#[cfg(feature = "diff-engine")]
mod tile_diff;
pub mod frame_source;
mod recovery;
mod surface;
mod time;
mod upload;
mod wgpu_integration;

mod bc_decompress;
mod readback;
mod texture_format;
mod texture_manager;

pub mod bindings;
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
pub use error_event::{GpuErrorCategory, GpuErrorEvent, GpuErrorSeverity, GpuErrorSeverityKind};
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
pub use recovery::{BackendAvailability, GpuRecoveryMachine, RecoveryOutcome, RecoveryState};
pub use surface::{
    present_with_retry, GpuPresenter, GpuSurfaceError, PresentOutcome, SimulatedSurface,
    SurfaceFrame, SurfaceProvider,
};
pub use time::now_ms;
pub use upload::{
    BufferSliceHandle, DynamicOffset, GpuCapabilities, UploadRingBuffer, UploadRingBufferDescriptor,
    UploadStats,
};
pub use wgpu_integration::{register_wgpu_uncaptured_error_handler, wgpu_error_to_event};
