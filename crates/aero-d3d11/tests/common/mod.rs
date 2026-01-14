#![allow(dead_code)]

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
#[allow(dead_code)]
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
    // Some downlevel backends expose compute+indirect execution but have very low per-stage storage
    // buffer limits (e.g. `Limits::downlevel_defaults()` sets
    // `max_storage_buffers_per_shader_stage = 4`).
    //
    // The GS/HS/DS compute-prepass path relies on storage buffers, so treat these failures as
    // "emulation unsupported" for tests.
    if msg.contains("max_storage_buffers_per_shader_stage")
        || msg.contains("Too many StorageBuffers")
        || msg.contains("Too many bindings of type StorageBuffers")
        || msg.contains("too many storage buffers")
        || msg.contains("storage buffers per shader stage")
        || msg.contains("Storage buffers per shader stage")
    {
        skip_or_panic(
            test_name,
            "storage buffer limit too low for compute prepass",
        );
        return true;
    }

    false
}

#[allow(dead_code)]
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
    // The GS/HS/DS emulation paths rely on storage buffers. WebGPU guarantees at least 4 storage
    // buffers per stage, but some downlevel configurations expose fewer. Skip these tests up front
    // to avoid wgpu validation errors during pipeline/bind group creation.
    //
    // Note: some specific tests (e.g. vertex pulling smoke tests) may require *more* than 4 storage
    // buffers per stage; those tests should perform their own limit checks.
    let max_storage = exec.device().limits().max_storage_buffers_per_shader_stage;
    if max_storage < 4 {
        skip_or_panic(
            test_name,
            &format!(
                "geometry shader prepass requires >=4 storage buffers per shader stage, but device limit max_storage_buffers_per_shader_stage={max_storage}"
            ),
        );
        return false;
    }
    true
}

/// Helper for tests that exercise `SV_PrimitiveID` / `@builtin(primitive_index)` paths.
///
/// The WebGPU primitive-index builtin is behind the optional `SHADER_PRIMITIVE_INDEX` feature, and
/// is not available on all backends (notably some wgpu-GL paths used in CI).
#[allow(dead_code)]
pub fn require_shader_primitive_index_or_skip(
    exec: &aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor,
    test_name: &str,
) -> bool {
    if !exec
        .device()
        .features()
        .contains(::wgpu::Features::SHADER_PRIMITIVE_INDEX)
    {
        skip_or_panic(
            test_name,
            "SV_PrimitiveID requires wgpu::Features::SHADER_PRIMITIVE_INDEX, but this backend/device does not support it",
        );
        return false;
    }
    true
}

pub mod wgpu;
