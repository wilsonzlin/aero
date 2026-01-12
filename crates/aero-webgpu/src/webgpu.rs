use crate::{BackendCaps, BackendKind, WebGpuInitError};

/// WebGPU init options shared across headless and presentation paths.
#[derive(Debug, Clone)]
pub struct WebGpuInitOptions {
    pub power_preference: wgpu::PowerPreference,

    /// Desired `max_buffer_size` (clamped to adapter limits).
    ///
    /// Aero's guest workloads (textures, command buffers, staging) benefit from
    /// larger buffers when available. Browsers typically default to 256MiB, but
    /// many native adapters can support far larger.
    pub desired_max_buffer_size: u64,
}

impl Default for WebGpuInitOptions {
    fn default() -> Self {
        Self {
            power_preference: wgpu::PowerPreference::HighPerformance,
            desired_max_buffer_size: 1024 * 1024 * 1024, // 1GiB (clamped at runtime)
        }
    }
}

/// A `wgpu` adapter/device/queue bundle with negotiated capabilities.
///
/// On `wasm32`, this can represent either a WebGPU backend (`Backends::BROWSER_WEBGPU`)
/// or a WebGL2 backend (`Backends::BROWSER_WEBGL`). Use [`WebGpuContext::kind`] to
/// distinguish between them.
pub struct WebGpuContext {
    kind: BackendKind,
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    caps: BackendCaps,
}

impl WebGpuContext {
    pub fn kind(&self) -> BackendKind {
        self.kind
    }

    pub fn instance(&self) -> &wgpu::Instance {
        &self.instance
    }

    pub fn adapter(&self) -> &wgpu::Adapter {
        &self.adapter
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    pub fn caps(&self) -> &BackendCaps {
        &self.caps
    }

    /// Acquire a device/queue without creating a presentation surface.
    pub async fn request_headless(options: WebGpuInitOptions) -> Result<Self, WebGpuInitError> {
        if cfg!(target_arch = "wasm32") {
            let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
                backends: wgpu::Backends::BROWSER_WEBGPU,
                ..Default::default()
            });
            return Self::request_internal(instance, BackendKind::WebGpu, options, None).await;
        }

        #[cfg(all(unix, not(target_arch = "wasm32")))]
        {
            use std::os::unix::fs::PermissionsExt;

            // Some wgpu backends (notably GL/WAYLAND) are noisy if `XDG_RUNTIME_DIR` is unset or
            // invalid. Use a per-process temp dir to keep headless/test contexts quiet.
            let needs_runtime_dir = std::env::var("XDG_RUNTIME_DIR")
                .ok()
                .map(|v| v.is_empty())
                .unwrap_or(true);
            if needs_runtime_dir {
                let dir = std::env::temp_dir()
                    .join(format!("aero-webgpu-xdg-runtime-{}", std::process::id()));
                let _ = std::fs::create_dir_all(&dir);
                let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
                std::env::set_var("XDG_RUNTIME_DIR", &dir);
            }
        }

        // Prefer GL on Linux CI to avoid crashes in some Vulkan software adapters (lavapipe/llvmpipe).
        // If no GL adapter is available, fall back to the native backends.
        if cfg!(target_os = "linux") {
            let gl_instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
                backends: wgpu::Backends::GL,
                ..Default::default()
            });
            match Self::request_internal(gl_instance, BackendKind::WebGpu, options.clone(), None)
                .await
            {
                Ok(ctx) => Ok(ctx),
                Err(WebGpuInitError::NoAdapter) => {
                    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
                        backends: wgpu::Backends::PRIMARY,
                        ..Default::default()
                    });
                    Self::request_internal(instance, BackendKind::WebGpu, options, None).await
                }
                Err(err) => Err(err),
            }
        } else {
            // Avoid initializing the GL backend in headless environments; it can emit noisy
            // display-system errors (Wayland/X11) even when a native backend is available.
            let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
                backends: wgpu::Backends::PRIMARY,
                ..Default::default()
            });
            Self::request_internal(instance, BackendKind::WebGpu, options, None).await
        }
    }

    pub(crate) async fn request_with_surface<'a>(
        instance: wgpu::Instance,
        kind: BackendKind,
        options: WebGpuInitOptions,
        surface: &'a wgpu::Surface<'a>,
    ) -> Result<Self, WebGpuInitError> {
        Self::request_internal(instance, kind, options, Some(surface)).await
    }

    async fn request_internal<'a>(
        instance: wgpu::Instance,
        kind: BackendKind,
        options: WebGpuInitOptions,
        compatible_surface: Option<&'a wgpu::Surface<'a>>,
    ) -> Result<Self, WebGpuInitError> {
        let adapter =
            request_adapter_robust(&instance, compatible_surface, options.power_preference)
                .await
                .ok_or(WebGpuInitError::NoAdapter)?;

        let requested_features = negotiated_features(&adapter);
        let requested_limits = negotiated_limits(&adapter, options.desired_max_buffer_size);

        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("aero-webgpu device"),
                    required_features: requested_features,
                    required_limits: requested_limits,
                },
                None,
            )
            .await?;

        let caps = BackendCaps::from_wgpu(&device, kind);

        Ok(Self {
            kind,
            instance,
            adapter,
            device,
            queue,
            caps,
        })
    }
}

async fn request_adapter_robust<'a>(
    instance: &wgpu::Instance,
    compatible_surface: Option<&'a wgpu::Surface<'a>>,
    power_preference: wgpu::PowerPreference,
) -> Option<wgpu::Adapter> {
    // Attempt 1: requested power preference.
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference,
            compatible_surface,
            force_fallback_adapter: false,
        })
        .await;
    if adapter.is_some() {
        return adapter;
    }

    // Attempt 2: low-power adapter.
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface,
            force_fallback_adapter: false,
        })
        .await;
    if adapter.is_some() {
        return adapter;
    }

    // Attempt 3: fallback adapter (often software).
    instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface,
            force_fallback_adapter: true,
        })
        .await
}

fn negotiated_features(adapter: &wgpu::Adapter) -> wgpu::Features {
    let available = adapter.features();
    let backend_is_gl = adapter.get_info().backend == wgpu::Backend::Gl;

    negotiated_features_for_available(
        available,
        backend_is_gl,
        env_var_truthy("AERO_DISABLE_WGPU_TEXTURE_COMPRESSION"),
    )
}

fn negotiated_features_for_available(
    available: wgpu::Features,
    backend_is_gl: bool,
    disable_texture_compression: bool,
) -> wgpu::Features {
    let mut requested = wgpu::Features::empty();

    // Texture compression is optional but beneficial (guest textures, DDS, etc).
    //
    // Note: on the wgpu GL backend, block-compressed texture paths have proven unreliable on some
    // platforms (notably Linux CI adapters). Treat compression as disabled regardless of adapter
    // feature bits to keep behavior deterministic.
    if !disable_texture_compression && !backend_is_gl {
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

    // Timestamp queries are extremely useful for profiling, but are optional and not supported on
    // all platforms (notably some browser/WebGL2 fallbacks). wgpu further splits the capability
    // into finer-grained feature bits; we rely on encoder timestamps.
    if available.contains(wgpu::Features::TIMESTAMP_QUERY)
        && available.contains(wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS)
    {
        requested |=
            wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS;
    }

    requested
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

fn negotiated_limits(adapter: &wgpu::Adapter, desired_max_buffer_size: u64) -> wgpu::Limits {
    let adapter_limits = adapter.limits();

    // Clamp requested limits to what the adapter reports.
    let max_buffer_size = desired_max_buffer_size.min(adapter_limits.max_buffer_size);
    let desired_storage_binding_size = max_buffer_size
        .min(u64::from(adapter_limits.max_storage_buffer_binding_size))
        .min(u64::from(u32::MAX));

    wgpu::Limits {
        max_buffer_size,
        max_storage_buffer_binding_size: desired_storage_binding_size as u32,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negotiated_features_respects_texture_compression_opt_out() {
        let compression = wgpu::Features::TEXTURE_COMPRESSION_BC
            | wgpu::Features::TEXTURE_COMPRESSION_ETC2
            | wgpu::Features::TEXTURE_COMPRESSION_ASTC_HDR;

        let timestamps =
            wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS;

        let available = compression | timestamps;

        let requested = negotiated_features_for_available(available, false, false);
        assert!(requested.contains(compression));
        assert!(requested.contains(timestamps));

        let requested = negotiated_features_for_available(available, false, true);
        assert!(!requested.intersects(compression));
        assert!(requested.contains(timestamps));
    }

    #[test]
    fn negotiated_features_requires_timestamp_inside_encoders() {
        let requested =
            negotiated_features_for_available(wgpu::Features::TIMESTAMP_QUERY, false, false);
        assert!(
            !requested.contains(wgpu::Features::TIMESTAMP_QUERY),
            "should not request TIMESTAMP_QUERY unless TIMESTAMP_QUERY_INSIDE_ENCODERS is also available"
        );

        let requested = negotiated_features_for_available(
            wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS,
            false,
            false,
        );
        assert!(requested.contains(wgpu::Features::TIMESTAMP_QUERY));
        assert!(requested.contains(wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS));
    }

    #[test]
    fn negotiated_features_disables_compression_on_gl_backend() {
        let compression = wgpu::Features::TEXTURE_COMPRESSION_BC
            | wgpu::Features::TEXTURE_COMPRESSION_ETC2
            | wgpu::Features::TEXTURE_COMPRESSION_ASTC_HDR;

        let timestamps =
            wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS;

        let available = compression | timestamps;

        let requested = negotiated_features_for_available(available, true, false);
        assert!(
            !requested.intersects(compression),
            "compression features must not be requested on the wgpu GL backend"
        );
        assert!(requested.contains(timestamps));
    }
}
