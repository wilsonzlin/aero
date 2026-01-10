//! `aero-gpu` contains GPU-side utilities used by Aero.
//!
//! This crate is currently focused on high-throughput buffer upload helpers for
//! emulator-style workloads (dynamic vertex/index/uniform updates every frame).

mod buffer_arena;
mod upload;

pub use buffer_arena::BufferArena;
pub use upload::{
    BufferSliceHandle, DynamicOffset, GpuCapabilities, UploadRingBuffer,
    UploadRingBufferDescriptor, UploadStats,
};
