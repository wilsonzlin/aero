//! Shared helpers for `aero-gpu` integration tests.
//!
//! Note: D3D9 defaults to back-face culling with clockwise front faces
//! (`D3DCULL_CCW`). Tests that render triangles without explicitly setting cull
//! state should use clockwise vertex winding to avoid having geometry culled.

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

/// Return a shared, leaked D3D9 executor for this integration-test binary.
///
/// Some wgpu backends/drivers have been observed to crash inside the allocator when repeatedly
/// creating/dropping `wgpu::Device`s across many `#[test]` cases in a single process. Integration
/// tests often instantiate a new headless executor per test, so we centralize executor creation
/// here and reuse it across tests.
pub fn d3d9_executor(
    test_name: &str,
) -> Option<std::sync::MutexGuard<'static, aero_gpu::AerogpuD3d9Executor>> {
    use std::sync::{Mutex, OnceLock};

    static EXEC: OnceLock<Option<&'static Mutex<aero_gpu::AerogpuD3d9Executor>>> = OnceLock::new();

    let exec = EXEC.get_or_init(|| {
        let exec = match pollster::block_on(aero_gpu::AerogpuD3d9Executor::new_headless()) {
            Ok(exec) => exec,
            Err(aero_gpu::AerogpuD3d9Error::AdapterNotFound) => return None,
            Err(err) => panic!("failed to create executor: {err}"),
        };
        Some(Box::leak(Box::new(Mutex::new(exec))))
    });

    let Some(exec) = exec.as_ref() else {
        skip_or_panic(test_name, "wgpu adapter not found");
        return None;
    };

    let mut exec = exec.lock().unwrap();
    exec.reset();
    Some(exec)
}

#[allow(dead_code)]
pub fn ensure_xdg_runtime_dir() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        use std::sync::OnceLock;

        static INIT: OnceLock<()> = OnceLock::new();
        INIT.get_or_init(|| {
            let needs_runtime_dir = std::env::var("XDG_RUNTIME_DIR")
                .ok()
                .map(|v| v.is_empty())
                .unwrap_or(true);
            if !needs_runtime_dir {
                return;
            }

            let dir =
                std::env::temp_dir().join(format!("aero-gpu-xdg-runtime-{}", std::process::id()));
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
            std::env::set_var("XDG_RUNTIME_DIR", &dir);
        });
    }
}
