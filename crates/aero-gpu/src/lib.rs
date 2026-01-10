//! `aero-gpu` contains GPU-side utilities used by Aero.
//!
//! Currently this crate provides:
//! - High-throughput dynamic buffer upload helpers (see [`UploadRingBuffer`]).
//! - Centralized caching of WGSL shader modules and render/compute pipelines
//!   (see [`pipeline_cache::PipelineCache`]).
//! - An internal GPU command stream format plus CPU-side optimization and wgpu
//!   encoding (see [`cmd`]).

mod buffer_arena;
mod dirty_rect;
mod present;
#[cfg(feature = "diff-engine")]
mod tile_diff;
mod upload;

mod context;
mod error;

pub mod cmd;
pub mod pipeline_cache;
pub mod pipeline_key;
pub mod protocol_d3d11;
pub mod stats;

pub use buffer_arena::BufferArena;
pub use context::GpuContext;
pub use error::GpuError;
pub use dirty_rect::{merge_and_cap_rects, Rect, RectMergeOutcome};
pub use present::{PresentError, PresentTelemetry, Presenter, TextureWriter};
pub use upload::{
    BufferSliceHandle, DynamicOffset, GpuCapabilities, UploadRingBuffer,
    UploadRingBufferDescriptor, UploadStats,
};
