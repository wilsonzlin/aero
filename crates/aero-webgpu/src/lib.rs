//! WebGPU backend scaffolding for Aero.
//!
//! This crate is intentionally self-contained: it provides
//! - robust WebGPU adapter/device acquisition with feature/limit negotiation
//! - basic resource helpers (buffers/textures, shader cache)
//! - a presentation path for blitting an RGBA framebuffer onto a surface
//! - an abstraction hook for a future WebGL2 backend

mod backend;
mod caps;
mod error;
mod presenter;
mod resources;
mod shader;
mod webgpu;
mod webgl2;

pub use backend::{Backend, BackendKind, BackendOptions};
pub use caps::{BackendCaps, TextureCompressionCaps};
pub use error::{BackendError, PresentError, WebGpuInitError};
pub use presenter::{AspectMode, FramebufferPresenter, FramebufferSize};
pub use resources::{GpuBufferAllocator, GpuTextureAllocator};
pub use shader::ShaderLibrary;
pub use webgpu::{WebGpuContext, WebGpuInitOptions};
pub use webgl2::WebGl2Stub;

