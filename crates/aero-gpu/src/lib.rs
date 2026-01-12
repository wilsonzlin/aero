//! `aero-gpu` contains GPU-side utilities and a backend-agnostic HAL used by Aero.
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
//! - A stable, backend-agnostic HAL (see [`hal`]) so higher-level rendering code does not fork per
//!   backend.

mod aerogpu_d3d9_executor;
mod buffer_arena;
mod command_processor;
mod context;
mod dirty_rect;
mod error;
mod error_event;
pub mod guest_memory;
mod present;
mod shared_surface;
#[cfg(feature = "diff-engine")]
mod tile_diff;

pub mod frame_source;
mod protocol;
mod recovery;
pub mod shader_lib;
mod surface;
mod time;
mod upload;
mod wgpu_integration;

mod acmd_executor;
mod bc_decompress;
mod readback;
mod texture_format;
mod texture_manager;
mod wgpu_features;

pub mod aerogpu_d3d9;
pub mod aerogpu_executor;
pub mod backend;
pub mod bindings;
pub mod cmd;
pub mod command_processor_d3d9;
pub mod hal;
pub mod pipeline_cache;
pub mod pipeline_key;
pub mod profiler;
pub mod protocol_d3d11;
pub mod protocol_d3d9;
pub mod stats;

pub use acmd_executor::{AeroGpuAcmdExecutor, AeroGpuAcmdExecutorError};
pub use aerogpu_d3d9_executor::{AerogpuD3d9Error, AerogpuD3d9Executor, PresentedScanout};
pub use bc_decompress::{
    decompress_bc1_rgba8, decompress_bc2_rgba8, decompress_bc3_rgba8, decompress_bc7_rgba8,
};
pub use buffer_arena::BufferArena;
pub use command_processor::{
    AeroGpuCommandProcessor, AeroGpuEvent, AeroGpuSubmissionAllocation, CommandProcessorError,
};
pub use context::WgpuContext;
pub use dirty_rect::{merge_and_cap_rects, Rect, RectMergeOutcome};
pub use error::GpuError;
pub use error_event::{GpuErrorCategory, GpuErrorEvent, GpuErrorSeverity, GpuErrorSeverityKind};
pub use guest_memory::{GuestMemory, GuestMemoryError, VecGuestMemory};
pub use present::{PresentError, PresentTelemetry, Presenter, TextureWriter};
pub use profiler::{
    FrameTimingsReport, GpuBackendKind, GpuProfiler, GpuProfilerConfig, GpuTimestampPhase,
};
pub use protocol::{
    parse_cmd_stream, AeroGpuBlendState, AeroGpuCmd, AeroGpuCmdHdr, AeroGpuCmdStreamHeader,
    AeroGpuCmdStreamParseError, AeroGpuCmdStreamView, AeroGpuDepthStencilState,
    AeroGpuInputLayoutBlobHeader, AeroGpuInputLayoutElementDxgi, AeroGpuOpcode,
    AeroGpuRasterizerState, AEROGPU_CMD_STREAM_MAGIC, AEROGPU_INPUT_LAYOUT_BLOB_MAGIC,
    AEROGPU_INPUT_LAYOUT_BLOB_VERSION, AEROGPU_MAX_RENDER_TARGETS,
};
pub use readback::{readback_buffer, readback_depth32f, readback_rgba8, readback_stencil8};
pub use recovery::{BackendAvailability, GpuRecoveryMachine, RecoveryOutcome, RecoveryState};
pub use surface::{
    present_with_retry, GpuPresenter, GpuSurfaceError, PresentOutcome, SimulatedSurface,
    SurfaceFrame, SurfaceProvider,
};
pub use texture_format::{TextureFormat, TextureFormatSelection, TextureUploadTransform};
pub use texture_format::wgpu_bc_texture_dimensions_compatible;
pub use texture_manager::{
    SamplerDesc, TextureDesc, TextureKey, TextureManager, TextureManagerError, TextureManagerStats,
    TextureRegion, TextureViewDesc,
};
pub use time::now_ms;
pub use upload::{
    BufferSliceHandle, DynamicOffset, GpuCapabilities, UploadRingBuffer,
    UploadRingBufferDescriptor, UploadStats,
};
pub use wgpu_integration::{register_wgpu_uncaptured_error_handler, wgpu_error_to_event};

use hal::GpuBackend;

/// Stable entry-point owned by the rest of the system.
///
/// `GpuContext` is responsible for owning the chosen backend implementation and exposing a stable
/// API upward via a `dyn GpuBackend` trait object.
pub struct GpuContext {
    backend: Box<dyn GpuBackend>,
}

impl GpuContext {
    pub fn new(backend: Box<dyn GpuBackend>) -> Self {
        Self { backend }
    }

    pub fn backend(&self) -> &dyn GpuBackend {
        self.backend.as_ref()
    }

    pub fn backend_mut(&mut self) -> &mut dyn GpuBackend {
        self.backend.as_mut()
    }
}

impl GpuBackend for GpuContext {
    fn kind(&self) -> hal::BackendKind {
        self.backend.kind()
    }

    fn capabilities(&self) -> &GpuCapabilities {
        self.backend.capabilities()
    }

    fn create_buffer(&mut self, desc: hal::BufferDesc) -> Result<hal::BufferId, GpuError> {
        self.backend.create_buffer(desc)
    }

    fn destroy_buffer(&mut self, id: hal::BufferId) -> Result<(), GpuError> {
        self.backend.destroy_buffer(id)
    }

    fn write_buffer(
        &mut self,
        buffer: hal::BufferId,
        offset: u64,
        data: &[u8],
    ) -> Result<(), GpuError> {
        self.backend.write_buffer(buffer, offset, data)
    }

    fn create_texture(&mut self, desc: hal::TextureDesc) -> Result<hal::TextureId, GpuError> {
        self.backend.create_texture(desc)
    }

    fn destroy_texture(&mut self, id: hal::TextureId) -> Result<(), GpuError> {
        self.backend.destroy_texture(id)
    }

    fn write_texture(&mut self, desc: hal::TextureWriteDesc, data: &[u8]) -> Result<(), GpuError> {
        self.backend.write_texture(desc, data)
    }

    fn create_texture_view(
        &mut self,
        texture: hal::TextureId,
        desc: hal::TextureViewDesc,
    ) -> Result<hal::TextureViewId, GpuError> {
        self.backend.create_texture_view(texture, desc)
    }

    fn destroy_texture_view(&mut self, id: hal::TextureViewId) -> Result<(), GpuError> {
        self.backend.destroy_texture_view(id)
    }

    fn create_sampler(&mut self, desc: hal::SamplerDesc) -> Result<hal::SamplerId, GpuError> {
        self.backend.create_sampler(desc)
    }

    fn destroy_sampler(&mut self, id: hal::SamplerId) -> Result<(), GpuError> {
        self.backend.destroy_sampler(id)
    }

    fn create_bind_group_layout(
        &mut self,
        desc: hal::BindGroupLayoutDesc,
    ) -> Result<hal::BindGroupLayoutId, GpuError> {
        self.backend.create_bind_group_layout(desc)
    }

    fn destroy_bind_group_layout(&mut self, id: hal::BindGroupLayoutId) -> Result<(), GpuError> {
        self.backend.destroy_bind_group_layout(id)
    }

    fn create_bind_group(
        &mut self,
        desc: hal::BindGroupDesc,
    ) -> Result<hal::BindGroupId, GpuError> {
        self.backend.create_bind_group(desc)
    }

    fn destroy_bind_group(&mut self, id: hal::BindGroupId) -> Result<(), GpuError> {
        self.backend.destroy_bind_group(id)
    }

    fn create_render_pipeline(
        &mut self,
        desc: hal::RenderPipelineDesc,
    ) -> Result<hal::PipelineId, GpuError> {
        self.backend.create_render_pipeline(desc)
    }

    fn create_compute_pipeline(
        &mut self,
        desc: hal::ComputePipelineDesc,
    ) -> Result<hal::PipelineId, GpuError> {
        self.backend.create_compute_pipeline(desc)
    }

    fn destroy_pipeline(&mut self, id: hal::PipelineId) -> Result<(), GpuError> {
        self.backend.destroy_pipeline(id)
    }

    fn create_command_buffer(
        &mut self,
        commands: &[hal::GpuCommand],
    ) -> Result<hal::CommandBufferId, GpuError> {
        self.backend.create_command_buffer(commands)
    }

    fn submit(&mut self, command_buffers: &[hal::CommandBufferId]) -> Result<(), GpuError> {
        self.backend.submit(command_buffers)
    }

    fn present(&mut self) -> Result<(), GpuError> {
        self.backend.present()
    }

    fn present_rgba8_framebuffer(
        &mut self,
        width: u32,
        height: u32,
        rgba8: &[u8],
    ) -> Result<(), GpuError> {
        self.backend.present_rgba8_framebuffer(width, height, rgba8)
    }

    fn screenshot_rgba8(&mut self) -> Result<Vec<u8>, GpuError> {
        self.backend.screenshot_rgba8()
    }
}

#[cfg(test)]
mod tests;
