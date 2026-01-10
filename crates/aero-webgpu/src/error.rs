use thiserror::Error;

#[derive(Debug, Error)]
pub enum WebGpuInitError {
    #[error("no compatible GPU adapter was found (adapter request returned None)")]
    NoAdapter,

    #[error("requesting a GPU device failed: {0}")]
    RequestDevice(#[from] wgpu::RequestDeviceError),

    #[error("creating a GPU surface failed: {0}")]
    CreateSurface(String),
}

#[derive(Debug, Error)]
pub enum BackendError {
    #[error(transparent)]
    WebGpu(#[from] WebGpuInitError),

    #[error("failed to initialize a usable GPU backend (WebGPU error: {webgpu}; WebGL2 error: {webgl2})")]
    NoUsableBackend { webgpu: String, webgl2: String },

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
