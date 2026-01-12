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
    let mut requested = wgpu::Features::empty();

    if !wgpu_texture_compression_disabled()
        && available.contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
    {
        requested |= wgpu::Features::TEXTURE_COMPRESSION_BC;
    }

    requested
}
