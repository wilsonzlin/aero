use crate::{
    BackendCaps, BackendError, TextureCompressionCaps, WebGl2Stub, WebGpuContext, WebGpuInitOptions,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestedBackend {
    /// Prefer WebGPU when available, otherwise fall back to WebGL2 (if enabled).
    Auto,
    WebGpu,
    WebGl2,
}

impl Default for RequestedBackend {
    fn default() -> Self {
        Self::Auto
    }
}

/// High-level backend selection options.
#[derive(Debug, Clone)]
pub struct BackendOptions {
    pub requested_backend: RequestedBackend,

    /// If `true`, [`RequestedBackend::Auto`] will fall back to WebGL2 when WebGPU
    /// is unavailable.
    ///
    /// On `wasm32`, the WebGL2 path is intended to use `wgpu`'s `BROWSER_WEBGL`
    /// backend (surface/presentation required). In headless mode we currently
    /// return a stub backend with conservative caps.
    pub allow_webgl2_fallback: bool,

    pub webgpu: WebGpuInitOptions,
}

impl Default for BackendOptions {
    fn default() -> Self {
        Self {
            requested_backend: RequestedBackend::Auto,
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
    Wgpu(WebGpuContext),
    WebGl2Stub(WebGl2Stub),
}

impl Backend {
    pub fn kind(&self) -> BackendKind {
        match self {
            Backend::Wgpu(ctx) => ctx.kind(),
            Backend::WebGl2Stub(_) => BackendKind::WebGl2,
        }
    }

    pub fn caps(&self) -> &BackendCaps {
        match self {
            Backend::Wgpu(ctx) => ctx.caps(),
            Backend::WebGl2Stub(stub) => stub.caps(),
        }
    }

    /// Acquire a backend without creating a presentation surface (headless).
    pub async fn request_headless(options: BackendOptions) -> Result<Self, BackendError> {
        let allow_fallback = options.allow_webgl2_fallback
            && matches!(options.requested_backend, RequestedBackend::Auto);
        if matches!(options.requested_backend, RequestedBackend::WebGl2) {
            return Ok(Backend::WebGl2Stub(WebGl2Stub::new(
                "requested WebGL2 backend is not available in headless mode".to_string(),
            )));
        }

        match WebGpuContext::request_headless(options.webgpu.clone()).await {
            Ok(ctx) => Ok(Backend::Wgpu(ctx)),
            Err(err) if allow_fallback => Ok(Backend::WebGl2Stub(WebGl2Stub::new(err.to_string()))),
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
