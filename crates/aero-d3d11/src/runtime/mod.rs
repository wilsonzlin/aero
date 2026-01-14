pub mod aerogpu_cmd_executor;
pub mod aerogpu_execute;
pub mod aerogpu_resources;
pub mod aerogpu_state;
pub mod bindings;
pub mod execute;
pub mod expansion_scratch;
pub mod gs_translate;
pub mod index_pulling;
pub mod indirect_args;
pub mod pipeline_layout_cache;
mod reflection_bindings;
pub mod resources;
pub mod scratch_allocator;
pub mod tessellation;
pub mod tessellator;
// Persistent shader translation cache is only available in the browser/WASM build.
#[cfg(target_arch = "wasm32")]
mod shader_cache;
#[cfg(target_arch = "wasm32")]
pub use shader_cache::{
    PersistedBinding, PersistedBindingKind, PersistedShaderArtifact, PersistedShaderStage,
    PersistedVsInputSignatureElement, ShaderCache, ShaderCacheSource, ShaderCacheStats,
    ShaderTranslationFlags, D3D11_TRANSLATOR_CACHE_VERSION,
};
pub mod state;
pub mod strip_to_list;
pub mod vertex_pulling;
mod wgsl_link;

use anyhow::{bail, Result};

fn wgpu_texture_compression_disabled() -> bool {
    // CI sometimes uses flaky/buggy software adapters. Allow forcing compression features off so
    // tests remain stable.
    env_var_truthy("AERO_DISABLE_WGPU_TEXTURE_COMPRESSION")
}

fn env_var_truthy(name: &str) -> bool {
    let Ok(raw) = std::env::var(name) else {
        return false;
    };

    let v = raw.trim();
    v == "1"
        || v.eq_ignore_ascii_case("true")
        || v.eq_ignore_ascii_case("yes")
        || v.eq_ignore_ascii_case("on")
}

/// Select optional wgpu features that should be enabled when available.
///
/// This follows the same pattern as `aero-webgpu` feature negotiation: query adapter support and
/// request only the subset that is supported to keep device creation robust across platforms.
fn negotiated_features(adapter: &wgpu::Adapter) -> wgpu::Features {
    let available = adapter.features();
    let backend_is_gl = adapter.get_info().backend == wgpu::Backend::Gl;

    negotiated_features_for_available(
        available,
        backend_is_gl,
        wgpu_texture_compression_disabled(),
    )
}

fn negotiated_features_for_available(
    available: wgpu::Features,
    backend_is_gl: bool,
    disable_texture_compression: bool,
) -> wgpu::Features {
    let mut requested = wgpu::Features::empty();

    // wgpu's GL backend has had correctness issues with native block-compressed texture paths on
    // some platforms (notably Linux CI software adapters). Treat compression as disabled
    // regardless of adapter feature bits to keep tests deterministic.
    //
    // Note: callers can still explicitly enable `TEXTURE_COMPRESSION_BC` on GL (bypassing this
    // negotiated path). In that configuration, Aero must avoid relying on
    // `CommandEncoder::copy_texture_to_texture` for BC textures; see the CPU/staging fallbacks in
    // `aerogpu_cmd_executor`.
    if !disable_texture_compression && !backend_is_gl {
        // Texture compression is optional but beneficial (guest textures, DDS, etc).
        // Request only features the adapter advertises, otherwise `request_device` will fail.
        for feature in [
            wgpu::Features::TEXTURE_COMPRESSION_BC,
            wgpu::Features::TEXTURE_COMPRESSION_ETC2,
            wgpu::Features::TEXTURE_COMPRESSION_ASTC_HDR,
        ] {
            if available.contains(feature) {
                requested |= feature;
            }
        }
    }

    // Enable indirect draws with non-zero `first_instance` when supported. This is required for
    // correct D3D-style instancing semantics in emulation/prepass paths that rely on indirect draw
    // argument buffers.
    if available.contains(wgpu::Features::INDIRECT_FIRST_INSTANCE) {
        requested |= wgpu::Features::INDIRECT_FIRST_INSTANCE;
    }

    // Enable `@builtin(primitive_index)` when supported so SM4/5 `SV_PrimitiveID` can be mapped to
    // a native WebGPU builtin in fragment shaders.
    if available.contains(wgpu::Features::SHADER_PRIMITIVE_INDEX) {
        requested |= wgpu::Features::SHADER_PRIMITIVE_INDEX;
    }

    requested
}

fn supports_indirect_execution_from_downlevel_flags(flags: wgpu::DownlevelFlags) -> bool {
    flags.contains(wgpu::DownlevelFlags::INDIRECT_EXECUTION)
}

/// Geometry-shader emulation relies on GPU-written draw arguments and `draw_indirect`.
///
/// Some downlevel backends/devices (notably wgpu's GL/WebGL paths) do not support indirect
/// execution (`DownlevelFlags::INDIRECT_EXECUTION`). Until a CPU-readback slow-path is
/// implemented, fail fast to avoid silent rendering corruption.
#[allow(dead_code)]
fn require_indirect_execution_for_gs_emulation(supports_indirect_execution: bool) -> Result<()> {
    if supports_indirect_execution {
        Ok(())
    } else {
        bail!("geometry shader emulation requires indirect draws on this backend (wgpu DownlevelFlags::INDIRECT_EXECUTION is not supported)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negotiated_features_disables_compression_on_gl_backend() {
        let compression = wgpu::Features::TEXTURE_COMPRESSION_BC
            | wgpu::Features::TEXTURE_COMPRESSION_ETC2
            | wgpu::Features::TEXTURE_COMPRESSION_ASTC_HDR;

        let requested = negotiated_features_for_available(compression, true, false);
        assert!(
            !requested.intersects(compression),
            "compression features must not be requested on the wgpu GL backend"
        );
    }

    #[test]
    fn supports_indirect_execution_is_derived_from_downlevel_flags() {
        assert!(!supports_indirect_execution_from_downlevel_flags(
            wgpu::DownlevelFlags::empty()
        ));
        assert!(supports_indirect_execution_from_downlevel_flags(
            wgpu::DownlevelFlags::INDIRECT_EXECUTION
        ));
    }

    #[test]
    fn gs_emulation_indirect_execution_policy_errors_when_unsupported() {
        let err = require_indirect_execution_for_gs_emulation(false).expect_err("should fail fast");
        assert!(
            err.to_string()
                .contains("geometry shader emulation requires indirect draws"),
            "unexpected error: {err:#}"
        );

        require_indirect_execution_for_gs_emulation(true)
            .expect("should succeed when indirect execution is supported");
    }
}
