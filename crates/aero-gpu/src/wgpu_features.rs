//! Helpers for robust `wgpu` feature negotiation.
//!
//! In `wgpu`, `adapter.features()` reports *supported* features, but a `Device` only exposes
//! features that were explicitly requested at device creation time. For optional capabilities like
//! texture compression, we want to:
//! - enable them when the adapter supports them (to avoid slow CPU fallbacks), but
//! - keep device creation robust across platforms/adapters that don't support them.
//!
//! CI note: some adapters/backends can be flaky with optional feature paths, so allow forcing
//! compression features off with `AERO_DISABLE_WGPU_TEXTURE_COMPRESSION=1`.

/// Env var that disables requesting any optional texture-compression features.
///
/// This is useful for:
/// - CI stability (avoid exercising driver-specific texture compression paths).
/// - Deterministic fallback testing (force the CPU BCn decompressor path).
pub(crate) const DISABLE_WGPU_TEXTURE_COMPRESSION_ENV: &str =
    "AERO_DISABLE_WGPU_TEXTURE_COMPRESSION";

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

pub(crate) fn negotiated_features(adapter: &wgpu::Adapter) -> wgpu::Features {
    let available = adapter.features();
    negotiated_features_for_available(available, env_var_truthy(DISABLE_WGPU_TEXTURE_COMPRESSION_ENV))
}

fn negotiated_features_for_available(
    available: wgpu::Features,
    disable_texture_compression: bool,
) -> wgpu::Features {
    let mut requested = wgpu::Features::empty();

    if !disable_texture_compression {
        // Texture compression is optional but beneficial (guest textures, DDS, etc).
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
    fn negotiated_features_respects_texture_compression_opt_out() {
        let compression = wgpu::Features::TEXTURE_COMPRESSION_BC
            | wgpu::Features::TEXTURE_COMPRESSION_ETC2
            | wgpu::Features::TEXTURE_COMPRESSION_ASTC_HDR;

        let available = compression;

        let requested = negotiated_features_for_available(available, false);
        assert!(requested.contains(compression));

        let requested = negotiated_features_for_available(available, true);
        assert!(!requested.intersects(compression));
    }

    #[test]
    fn negotiated_features_only_requests_adapter_supported_bits() {
        let requested = negotiated_features_for_available(wgpu::Features::empty(), false);
        assert!(requested.is_empty());
    }
}
