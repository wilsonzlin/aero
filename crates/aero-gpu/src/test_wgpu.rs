//! Shared `wgpu` device/queue for unit tests.
//!
//! `wgpu` device/instance creation can be expensive and, on some CI/sandbox environments,
//! repeatedly creating and dropping devices across many tests has been observed to segfault.
//!
//! To keep the test harness robust, we lazily create (and intentionally keep alive) a small set of
//! headless `wgpu` devices keyed by the requested feature bits.
#![allow(dead_code)]

use std::sync::OnceLock;

pub(crate) struct TestWgpuDevice {
    pub(crate) device: wgpu::Device,
    pub(crate) queue: wgpu::Queue,
    pub(crate) downlevel_flags: wgpu::DownlevelFlags,
    pub(crate) backend: wgpu::Backend,
}

fn ensure_xdg_runtime_dir(tag: &str) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let needs_runtime_dir = match std::env::var("XDG_RUNTIME_DIR") {
            Ok(dir) if !dir.is_empty() => match std::fs::metadata(&dir) {
                Ok(meta) => !meta.is_dir() || (meta.permissions().mode() & 0o077) != 0,
                Err(_) => true,
            },
            _ => true,
        };

        if needs_runtime_dir {
            let dir = std::env::temp_dir().join(format!(
                "aero-wgpu-xdg-runtime-{}-{tag}",
                std::process::id()
            ));
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
            std::env::set_var("XDG_RUNTIME_DIR", &dir);
        }
    }
}

async fn request_adapter(backends: wgpu::Backends) -> Option<wgpu::Adapter> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends,
        ..Default::default()
    });

    // Prefer a "fallback" software adapter when available; it's typically more predictable in CI
    // environments.
    let opts = |force_fallback_adapter| wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        compatible_surface: None,
        force_fallback_adapter,
    };

    match instance.request_adapter(&opts(true)).await {
        Some(adapter) => Some(adapter),
        None => instance.request_adapter(&opts(false)).await,
    }
}

pub(crate) fn ensure_runtime_dir() {
    // Ensure `wgpu` has somewhere to put its runtime files on Unix CI.
    static RUNTIME_DIR: OnceLock<()> = OnceLock::new();
    RUNTIME_DIR.get_or_init(|| ensure_xdg_runtime_dir("aero-gpu-tests"));
}

async fn select_adapter() -> Option<wgpu::Adapter> {
    ensure_runtime_dir();

    // On Linux, prefer the native ("primary") backends first and fall back to GL if Vulkan (etc)
    // isn't available.
    if cfg!(target_os = "linux") {
        match request_adapter(wgpu::Backends::PRIMARY).await {
            Some(adapter) => Some(adapter),
            None => request_adapter(wgpu::Backends::GL).await,
        }
    } else {
        request_adapter(wgpu::Backends::PRIMARY).await
    }
}

pub(crate) async fn create_device_exact(
    required_features: wgpu::Features,
) -> Option<TestWgpuDevice> {
    let adapter = select_adapter().await?;

    if !adapter.features().contains(required_features) {
        return None;
    }

    let downlevel_flags = adapter.get_downlevel_capabilities().flags;
    let backend = adapter.get_info().backend;

    let (device, queue) = adapter
        .request_device(
            &wgpu::DeviceDescriptor {
                label: Some("aero-gpu unit-test device"),
                required_features,
                required_limits: wgpu::Limits::downlevel_defaults(),
            },
            None,
        )
        .await
        .ok()?;

    Some(TestWgpuDevice {
        device,
        queue,
        downlevel_flags,
        backend,
    })
}
