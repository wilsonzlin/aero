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
#[allow(dead_code)]
#[cfg(target_arch = "wasm32")]
pub fn d3d9_executor(
    test_name: &str,
) -> Option<std::sync::MutexGuard<'static, aero_gpu::AerogpuD3d9Executor>> {
    // The headless executor uses non-Send/Sync WebGPU handles on wasm32; keep the host-style
    // integration tests buildable by treating them as skipped.
    skip_or_panic(test_name, "D3D9 headless executor is host-only");
    None
}

#[allow(dead_code)]
#[cfg(not(target_arch = "wasm32"))]
pub fn d3d9_executor(
    test_name: &str,
) -> Option<std::sync::MutexGuard<'static, aero_gpu::AerogpuD3d9Executor>> {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = test_name;
        // `AerogpuD3d9Executor` is not `Send`/`Sync` on wasm32 (it contains WebGPU/wasm-bindgen
        // types like `JsValue`). The integration tests in this crate are not executed for the
        // `wasm32-unknown-unknown` target in CI (they are only compiled via `--no-run`), so return
        // `None` to keep the test crates compiling without pulling non-`Sync` types into `static`
        // variables.
        return None;
    }

    #[cfg(not(target_arch = "wasm32"))]
    use std::sync::{Mutex, OnceLock};

    #[cfg(not(target_arch = "wasm32"))]
    static EXEC: OnceLock<Option<&'static Mutex<aero_gpu::AerogpuD3d9Executor>>> = OnceLock::new();

    #[cfg(not(target_arch = "wasm32"))]
    let exec = EXEC.get_or_init(|| {
        let exec = match pollster::block_on(aero_gpu::AerogpuD3d9Executor::new_headless()) {
            Ok(exec) => exec,
            Err(aero_gpu::AerogpuD3d9Error::AdapterNotFound) => return None,
            Err(err) => panic!("failed to create executor: {err}"),
        };
        Some(Box::leak(Box::new(Mutex::new(exec))))
    });

    #[cfg(not(target_arch = "wasm32"))]
    let Some(exec) = exec.as_ref() else {
        skip_or_panic(test_name, "wgpu adapter not found");
        return None;
    };

    #[cfg(not(target_arch = "wasm32"))]
    let mut exec = exec.lock().unwrap();
    #[cfg(not(target_arch = "wasm32"))]
    exec.reset();
    #[cfg(not(target_arch = "wasm32"))]
    Some(exec)
}

/// Return a shared, leaked stable-protocol (`AeroGpuExecutor`) for this integration-test binary.
///
/// Like [`d3d9_executor`], this avoids wgpu backend/driver instability (including crashes or LLVM
/// OOMs in some software adapters) caused by repeatedly creating/dropping `wgpu::Device`s across
/// many `#[test]` cases in a single process.
#[allow(dead_code)]
#[cfg(target_arch = "wasm32")]
pub fn aerogpu_executor(
    test_name: &str,
) -> Option<std::sync::MutexGuard<'static, aero_gpu::aerogpu_executor::AeroGpuExecutor>> {
    // `AeroGpuExecutor` uses JS-backed WebGPU handles on wasm32 which are not Send/Sync. Keep the
    // host-style integration tests buildable by treating them as skipped.
    skip_or_panic(test_name, "AeroGpuExecutor is host-only");
    None
}

#[allow(dead_code)]
#[cfg(not(target_arch = "wasm32"))]
pub fn aerogpu_executor(
    test_name: &str,
) -> Option<std::sync::MutexGuard<'static, aero_gpu::aerogpu_executor::AeroGpuExecutor>> {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = test_name;
        // `AeroGpuExecutor` stores wgpu `Device`/`Queue` handles which are not `Send`/`Sync` on wasm.
        // The integration tests in this crate are not executed for the `wasm32-unknown-unknown`
        // target in CI (they are only compiled via `--no-run`). Return `None` to keep these test
        // crates compiling without introducing non-`Sync` statics.
        return None;
    }

    #[cfg(not(target_arch = "wasm32"))]
    use std::sync::{Mutex, OnceLock};

    #[cfg(not(target_arch = "wasm32"))]
    static EXEC: OnceLock<Option<&'static Mutex<aero_gpu::aerogpu_executor::AeroGpuExecutor>>> =
        OnceLock::new();

    #[cfg(not(target_arch = "wasm32"))]
    let exec = EXEC.get_or_init(|| {
        ensure_xdg_runtime_dir();

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: if cfg!(target_os = "linux") {
                wgpu::Backends::PRIMARY
            } else {
                wgpu::Backends::all()
            },
            ..Default::default()
        });

        let adapter = match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: true,
        })) {
            Some(adapter) => Some(adapter),
            None => pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: None,
                force_fallback_adapter: false,
            })),
        }?;

        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("aero-gpu AeroGpuExecutor (tests)"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults(),
            },
            None,
        ))
        .ok()?;

        let exec = aero_gpu::aerogpu_executor::AeroGpuExecutor::new(device, queue)
            .expect("create AeroGpuExecutor");
        Some(Box::leak(Box::new(Mutex::new(exec))))
    });

    #[cfg(not(target_arch = "wasm32"))]
    let Some(exec) = exec.as_ref() else {
        skip_or_panic(test_name, "no wgpu adapter available");
        return None;
    };

    #[cfg(not(target_arch = "wasm32"))]
    let mut exec = exec.lock().unwrap();
    #[cfg(not(target_arch = "wasm32"))]
    exec.reset();
    #[cfg(not(target_arch = "wasm32"))]
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
