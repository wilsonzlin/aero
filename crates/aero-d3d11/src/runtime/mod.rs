pub mod aerogpu_cmd_executor;
pub mod aerogpu_execute;
pub mod aerogpu_resources;
pub mod aerogpu_state;
pub mod bindings;
pub mod execute;
pub mod pipeline_layout_cache;
mod reflection_bindings;
pub mod resources;
pub mod state;
mod wgsl_link;

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

    requested
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
}
