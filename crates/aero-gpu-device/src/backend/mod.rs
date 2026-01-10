//! GPU backend abstraction.
//!
//! The emulator's command processor is backend-agnostic; in production it will
//! forward into the DirectX→WebGPU translation layer. For tests we provide a
//! deterministic software backend.

mod soft;
mod webgpu;

use core::fmt;

pub use soft::SoftGpuBackend;
pub use webgpu::WebGpuBackend;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PresentedFrame {
    pub width: u32,
    pub height: u32,
    /// RGBA8, row-major, origin top-left.
    pub rgba8: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Viewport {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Debug)]
pub enum BackendError {
    InvalidResource,
    InvalidState(&'static str),
    OutOfBounds,
    Unsupported,
    Internal(&'static str),
}

impl fmt::Display for BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidResource => write!(f, "invalid resource"),
            Self::InvalidState(msg) => write!(f, "invalid state: {msg}"),
            Self::OutOfBounds => write!(f, "out of bounds"),
            Self::Unsupported => write!(f, "unsupported"),
            Self::Internal(msg) => write!(f, "internal error: {msg}"),
        }
    }
}

impl std::error::Error for BackendError {}

pub trait GpuBackend {
    fn create_buffer(&mut self, id: u32, size_bytes: u64, usage: u32) -> Result<(), BackendError>;
    fn destroy_buffer(&mut self, id: u32) -> Result<(), BackendError>;
    fn write_buffer(&mut self, id: u32, dst_offset: u64, data: &[u8]) -> Result<(), BackendError>;
    fn read_buffer(
        &self,
        id: u32,
        src_offset: u64,
        size_bytes: usize,
    ) -> Result<Vec<u8>, BackendError>;

    fn create_texture2d(
        &mut self,
        id: u32,
        width: u32,
        height: u32,
        format: u32,
        usage: u32,
    ) -> Result<(), BackendError>;
    fn destroy_texture(&mut self, id: u32) -> Result<(), BackendError>;
    fn write_texture2d(
        &mut self,
        id: u32,
        mip_level: u32,
        width: u32,
        height: u32,
        bytes_per_row: u32,
        data: &[u8],
    ) -> Result<(), BackendError>;
    fn read_texture2d(
        &self,
        id: u32,
        mip_level: u32,
        width: u32,
        height: u32,
        bytes_per_row: u32,
    ) -> Result<Vec<u8>, BackendError>;

    fn set_render_target(&mut self, texture_id: u32) -> Result<(), BackendError>;
    fn clear(&mut self, rgba: [f32; 4]) -> Result<(), BackendError>;
    fn set_viewport(&mut self, viewport: Viewport) -> Result<(), BackendError>;

    fn set_pipeline(&mut self, pipeline_id: u32) -> Result<(), BackendError>;
    fn set_vertex_buffer(
        &mut self,
        buffer_id: u32,
        offset: u64,
        stride: u32,
    ) -> Result<(), BackendError>;
    fn draw(&mut self, vertex_count: u32, first_vertex: u32) -> Result<(), BackendError>;

    fn present(&mut self, texture_id: u32) -> Result<(), BackendError>;

    fn take_presented_frame(&mut self) -> Option<PresentedFrame>;
}
