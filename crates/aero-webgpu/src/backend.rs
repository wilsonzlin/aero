use crate::{BackendCaps, BackendError, TextureCompressionCaps, WebGpuContext, WebGpuInitOptions, WebGl2Stub};

/// High-level backend selection options.
#[derive(Debug, Clone)]
pub struct BackendOptions {
    /// If `true`, WebGPU init failures will fall back to a WebGL2 stub backend.
    ///
    /// This keeps higher layers from needing to branch on API availability.
    pub allow_webgl2_fallback: bool,

    pub webgpu: WebGpuInitOptions,
}

impl Default for BackendOptions {
    fn default() -> Self {
        Self {
            allow_webgl2_fallback: true,
            webgpu: WebGpuInitOptions::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    WebGpu,
    WebGl2,
}

/// A backend context that higher layers can use without caring about the underlying API.
///
/// For now, the WebGL2 variant is a stub that only exposes negotiated capabilities.
pub enum Backend {
    WebGpu(WebGpuContext),
    WebGl2(WebGl2Stub),
}

impl Backend {
    pub fn kind(&self) -> BackendKind {
        match self {
            Backend::WebGpu(_) => BackendKind::WebGpu,
            Backend::WebGl2(_) => BackendKind::WebGl2,
        }
    }

    pub fn caps(&self) -> &BackendCaps {
        match self {
            Backend::WebGpu(ctx) => ctx.caps(),
            Backend::WebGl2(stub) => stub.caps(),
        }
    }

    /// Acquire a backend without creating a presentation surface (headless).
    pub async fn request_headless(options: BackendOptions) -> Result<Self, BackendError> {
        match WebGpuContext::request_headless(options.webgpu.clone()).await {
            Ok(ctx) => Ok(Backend::WebGpu(ctx)),
            Err(err) if options.allow_webgl2_fallback => Ok(Backend::WebGl2(WebGl2Stub::new(err.to_string()))),
            Err(err) => Err(err.into()),
        }
    }

    /// Negotiated capability summary for a future WebGL2 implementation.
    ///
    /// This is intentionally conservative. A real WebGL2 backend will likely
    /// diverge in subtle ways (sRGB formats, compressed textures, etc).
    pub(crate) fn conservative_webgl2_caps() -> BackendCaps {
        BackendCaps {
            kind: BackendKind::WebGl2,
            texture_compression: TextureCompressionCaps::default(),
            max_buffer_size: 128 * 1024 * 1024,
            max_texture_dimension_2d: 4096,
        }
    }
}
