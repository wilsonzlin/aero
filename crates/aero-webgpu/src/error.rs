use thiserror::Error;

#[derive(Debug, Error)]
pub enum WebGpuInitError {
    #[error("no compatible WebGPU adapter was found (adapter request returned None)")]
    NoAdapter,

    #[error("requesting a WebGPU device failed: {0}")]
    RequestDevice(#[from] wgpu::RequestDeviceError),

    #[error("creating a WebGPU surface failed: {0}")]
    CreateSurface(String),
}

#[derive(Debug, Error)]
pub enum BackendError {
    #[error(transparent)]
    WebGpu(#[from] WebGpuInitError),

    #[error("WebGL2 fallback was selected but is not implemented yet ({reason})")]
    WebGl2Stub { reason: String },
}

#[derive(Debug, Error)]
pub enum PresentError {
    #[error("framebuffer dimensions must be non-zero")]
    InvalidFramebufferSize,

    #[error("framebuffer length mismatch (expected {expected} bytes, got {actual} bytes)")]
    InvalidFramebufferLength { expected: usize, actual: usize },

    #[error("surface error: {0}")]
    Surface(#[from] wgpu::SurfaceError),

    #[error("WebGL2 presentation is not implemented yet")]
    WebGl2NotImplemented,
}

