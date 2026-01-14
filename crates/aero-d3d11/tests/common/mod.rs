pub fn require_webgpu() -> bool {
    let Ok(raw) = std::env::var("AERO_REQUIRE_WEBGPU") else {
        return false;
    };

    let v = raw.trim();
    v == "1"
        || v.eq_ignore_ascii_case("true")
        || v.eq_ignore_ascii_case("yes")
        || v.eq_ignore_ascii_case("on")
}

pub fn skip_or_panic(test_name: &str, reason: &str) {
    if require_webgpu() {
        panic!("AERO_REQUIRE_WEBGPU is enabled but {test_name} cannot run: {reason}");
    }
    eprintln!("skipping {test_name}: {reason}");
}

/// Helper for tests that rely on the GS/HS/DS emulation path (compute prepass + indirect draw).
///
/// Some backends/adapters (notably wgpu-GL/WebGL2 paths) do not support compute shaders and/or
/// indirect execution. Until slow-path fallbacks exist, these tests should skip rather than fail.
pub fn skip_if_compute_or_indirect_unsupported(test_name: &str, err: &anyhow::Error) -> bool {
    let msg = err.to_string();

    // Older code paths may return `GpuError::Unsupported("compute")` through various wrappers.
    // Newer code may fail fast with a more explicit error.
    if msg.contains("Unsupported(\"compute\")")
        || msg.contains("does not support compute")
        || msg.contains("requires compute shaders")
    {
        skip_or_panic(test_name, "compute unsupported");
        return true;
    }

    if msg.contains("does not support indirect")
        || msg.contains("requires indirect draws")
        || msg.contains("INDIRECT_EXECUTION")
    {
        skip_or_panic(test_name, "indirect unsupported");
        return true;
    }

    false
}

pub fn require_gs_prepass_or_skip(
    exec: &aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor,
    test_name: &str,
) -> bool {
    if !exec.caps().supports_compute {
        skip_or_panic(
            test_name,
            "geometry shader prepass requires compute shaders, but this wgpu backend does not support compute",
        );
        return false;
    }
    if !exec.supports_indirect() {
        skip_or_panic(
            test_name,
            "geometry shader prepass requires indirect execution, but this wgpu backend does not support indirect draws",
        );
        return false;
    }
    true
}

pub mod wgpu;
