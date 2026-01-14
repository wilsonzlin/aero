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

/// Return a shared, leaked `WgpuBackend` for this integration-test binary.
///
/// Like [`d3d9_executor`], this avoids wgpu backend/driver instability caused by repeatedly
/// creating/dropping `wgpu::Device`s across many `#[test]` cases in a single process.
#[allow(dead_code)]
#[cfg(target_arch = "wasm32")]
pub fn wgpu_backend_webgpu(
    test_name: &str,
) -> Option<std::sync::MutexGuard<'static, aero_gpu::backend::WgpuBackend>> {
    // The wgpu backend relies on JS-backed WebGPU handles on wasm32 which are not Send/Sync. Keep
    // host-style integration tests buildable by treating them as skipped.
    skip_or_panic(test_name, "headless wgpu backend is host-only");
    None
}

#[allow(dead_code)]
#[cfg(not(target_arch = "wasm32"))]
pub fn wgpu_backend_webgpu(
    test_name: &str,
) -> Option<std::sync::MutexGuard<'static, aero_gpu::backend::WgpuBackend>> {
    use std::sync::{Mutex, OnceLock};

    static BACKEND: OnceLock<Result<&'static Mutex<aero_gpu::backend::WgpuBackend>, String>> =
        OnceLock::new();

    let backend = BACKEND.get_or_init(|| {
        ensure_xdg_runtime_dir();
        match pollster::block_on(aero_gpu::backend::WgpuBackend::new_headless(
            aero_gpu::hal::BackendKind::WebGpu,
        )) {
            Ok(backend) => Ok(Box::leak(Box::new(Mutex::new(backend)))),
            Err(err) => Err(err.to_string()),
        }
    });

    match backend {
        Ok(backend) => Some(backend.lock().unwrap_or_else(|poison| poison.into_inner())),
        Err(err) => {
            skip_or_panic(test_name, &format!("wgpu backend init failed: {err}"));
            None
        }
    }
}

/// Return a shared, leaked D3D9 executor for this integration-test binary.
///
/// Some wgpu backends/drivers have been observed to crash inside the allocator when repeatedly
/// creating/dropping `wgpu::Device`s across many `#[test]` cases in a single process. Integration
/// tests often instantiate a new headless executor per test, so we centralize executor creation
/// here and reuse it across tests.
#[allow(dead_code)]
#[cfg(not(target_arch = "wasm32"))]
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

    let mut exec = exec.lock().unwrap_or_else(|poison| poison.into_inner());
    exec.reset();
    Some(exec)
}

#[allow(dead_code)]
#[cfg(target_arch = "wasm32")]
pub fn d3d9_executor(
    test_name: &str,
) -> Option<std::sync::MutexGuard<'static, aero_gpu::AerogpuD3d9Executor>> {
    // The headless D3D9 executor uses non-Send/Sync WebGPU handles on wasm32, so we cannot share a
    // `static` executor cache like we do on native targets.
    skip_or_panic(
        test_name,
        "shared D3D9 executor cache is not available on wasm32",
    );
    None
}

/// Return a shared, leaked stable-protocol (`AeroGpuExecutor`) for this integration-test binary.
///
/// Like [`d3d9_executor`], this avoids wgpu backend/driver instability (including crashes or LLVM
/// OOMs in some software adapters) caused by repeatedly creating/dropping `wgpu::Device`s across
/// many `#[test]` cases in a single process.
#[allow(dead_code)]
#[cfg(target_arch = "wasm32")]
pub async fn aerogpu_executor(
    test_name: &str,
) -> Option<futures_intrusive::sync::MutexGuard<'static, aero_gpu::aerogpu_executor::AeroGpuExecutor>>
{
    // `AeroGpuExecutor` uses JS-backed WebGPU handles on wasm32 which are not Send/Sync, so we
    // cannot share a `static` executor cache like we do on native targets.
    skip_or_panic(
        test_name,
        "shared AeroGpuExecutor cache is not available on wasm32",
    );
    None
}

#[allow(dead_code)]
#[cfg(not(target_arch = "wasm32"))]
pub async fn aerogpu_executor(
    test_name: &str,
) -> Option<futures_intrusive::sync::MutexGuard<'static, aero_gpu::aerogpu_executor::AeroGpuExecutor>>
{
    use futures_intrusive::sync::Mutex;
    use std::sync::OnceLock;

    static EXEC: OnceLock<Option<&'static Mutex<aero_gpu::aerogpu_executor::AeroGpuExecutor>>> =
        OnceLock::new();

    let exec = EXEC.get_or_init(|| {
        ensure_xdg_runtime_dir();

        // Prefer wgpu's GL backend on Linux CI for stability. Vulkan software adapters have been a
        // recurring source of flakes/crashes in headless sandboxes.
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: if cfg!(target_os = "linux") {
                wgpu::Backends::GL
            } else {
                wgpu::Backends::all()
            },
            ..Default::default()
        });

        let adapter =
            match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: None,
                force_fallback_adapter: true,
            })) {
                Some(adapter) => Some(adapter),
                None => {
                    pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                        power_preference: wgpu::PowerPreference::LowPower,
                        compatible_surface: None,
                        force_fallback_adapter: false,
                    }))
                }
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
        Some(Box::leak(Box::new(Mutex::new(exec, true))))
    });

    let Some(exec) = exec.as_ref() else {
        skip_or_panic(test_name, "no wgpu adapter available");
        return None;
    };

    let mut exec = exec.lock().await;
    exec.reset();
    Some(exec)
}

#[allow(dead_code)]
pub fn aerogpu_executor_or_skip(
    test_name: &str,
) -> Option<futures_intrusive::sync::MutexGuard<'static, aero_gpu::aerogpu_executor::AeroGpuExecutor>>
{
    pollster::block_on(aerogpu_executor(test_name))
}

#[allow(dead_code)]
#[cfg(target_arch = "wasm32")]
pub async fn aerogpu_executor_bc(
) -> Option<futures_intrusive::sync::MutexGuard<'static, aero_gpu::aerogpu_executor::AeroGpuExecutor>>
{
    // `AeroGpuExecutor` stores WebGPU handles that are not `Send`/`Sync` on wasm32, so we cannot
    // safely cache a shared executor behind a `static` mutex.
    None
}

#[allow(dead_code)]
#[cfg(not(target_arch = "wasm32"))]
pub async fn aerogpu_executor_bc(
) -> Option<futures_intrusive::sync::MutexGuard<'static, aero_gpu::aerogpu_executor::AeroGpuExecutor>>
{
    use futures_intrusive::sync::Mutex;
    use std::sync::OnceLock;

    static EXECUTOR: OnceLock<Option<&'static Mutex<aero_gpu::aerogpu_executor::AeroGpuExecutor>>> =
        OnceLock::new();

    let exec = EXECUTOR.get_or_init(|| {
        let exec = pollster::block_on(async {
            ensure_xdg_runtime_dir();

            // Avoid wgpu's GL backend on Linux: wgpu-hal's GLES pipeline reflection can panic for
            // some shader pipelines (observed in CI sandboxes), which turns these tests into hard
            // failures.
            let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
                backends: if cfg!(target_os = "linux") {
                    wgpu::Backends::PRIMARY
                } else {
                    wgpu::Backends::all()
                },
                ..Default::default()
            });

            // Try a couple different adapter options; the default request may land on an adapter
            // that doesn't support BC compression even when another does (e.g. integrated vs
            // discrete).
            let adapter_opts = [
                wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::LowPower,
                    compatible_surface: None,
                    force_fallback_adapter: true,
                },
                wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::LowPower,
                    compatible_surface: None,
                    force_fallback_adapter: false,
                },
                wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::HighPerformance,
                    compatible_surface: None,
                    force_fallback_adapter: false,
                },
            ];

            for opts in adapter_opts {
                let Some(adapter) = instance.request_adapter(&opts).await else {
                    continue;
                };
                if !adapter
                    .features()
                    .contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
                {
                    continue;
                }
                // Avoid CPU software adapters on Linux for native BC paths; they are a common
                // source of flakes and crashes (even if they advertise TEXTURE_COMPRESSION_BC).
                if cfg!(target_os = "linux")
                    && adapter.get_info().device_type == wgpu::DeviceType::Cpu
                {
                    continue;
                }

                let Ok((device, queue)) = adapter
                    .request_device(
                        &wgpu::DeviceDescriptor {
                            label: Some("aerogpu executor test device (BC)"),
                            required_features: wgpu::Features::TEXTURE_COMPRESSION_BC,
                            required_limits: wgpu::Limits::downlevel_defaults(),
                        },
                        None,
                    )
                    .await
                else {
                    continue;
                };

                let Ok(exec) = aero_gpu::aerogpu_executor::AeroGpuExecutor::new(device, queue)
                else {
                    continue;
                };
                return Some(Box::leak(Box::new(Mutex::new(exec, true))));
            }

            None
        });
        exec.map(|mutex| &*mutex)
    });

    let mutex = exec.as_ref()?;
    let mut guard = mutex.lock().await;
    guard.reset();
    Some(guard)
}

#[allow(dead_code)]
#[cfg(target_arch = "wasm32")]
pub async fn aerogpu_executor_bc_or_skip(
    test_name: &str,
) -> Option<futures_intrusive::sync::MutexGuard<'static, aero_gpu::aerogpu_executor::AeroGpuExecutor>>
{
    skip_or_panic(test_name, "BC-only executor is host-only");
    None
}

#[allow(dead_code)]
#[cfg(not(target_arch = "wasm32"))]
pub async fn aerogpu_executor_bc_or_skip(
    test_name: &str,
) -> Option<futures_intrusive::sync::MutexGuard<'static, aero_gpu::aerogpu_executor::AeroGpuExecutor>>
{
    match aerogpu_executor_bc().await {
        Some(exec) => Some(exec),
        None => {
            skip_or_panic(test_name, "no wgpu adapter supports TEXTURE_COMPRESSION_BC");
            None
        }
    }
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
