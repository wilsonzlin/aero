/// Helpers for robust `wgpu` feature negotiation.
///
/// In `wgpu`, `adapter.features()` reports *supported* features, but a `Device` only exposes
/// features that were explicitly requested at device creation time. For optional capabilities like
/// BC texture compression, we want to:
/// - enable them when the adapter supports them (to avoid slow CPU fallbacks), but
/// - keep device creation robust across platforms/adapters that don't support them.
///
/// CI note: some software adapters can be flaky with optional feature paths, so allow forcing
/// compression features off with `AERO_DISABLE_WGPU_TEXTURE_COMPRESSION=1`.
pub(crate) fn negotiated_features(adapter: &wgpu::Adapter) -> wgpu::Features {
    let available = adapter.features();
    let mut requested = wgpu::Features::empty();

    if std::env::var_os("AERO_DISABLE_WGPU_TEXTURE_COMPRESSION").is_none()
        && available.contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
    {
        requested |= wgpu::Features::TEXTURE_COMPRESSION_BC;
    }

    requested
}
