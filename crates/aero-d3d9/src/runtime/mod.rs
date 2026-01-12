mod execute;

pub use execute::{
    ColorFormat, D3D9Runtime, IndexFormat, RenderTarget, RuntimeConfig, RuntimeError, ShaderStage,
    SwapChainDesc, TextureDesc, TextureFormat, VertexAttributeDesc, VertexDecl, VertexFormat,
};

// Persistent shader translation cache is only available in the browser/WASM build.
#[cfg(target_arch = "wasm32")]
mod shader_cache;
#[cfg(target_arch = "wasm32")]
pub use shader_cache::{
    PersistedShaderArtifact, ShaderCache, ShaderCacheKey, ShaderCacheSource,
    ShaderTranslationFlags, D3D9_TRANSLATOR_CACHE_VERSION,
};
