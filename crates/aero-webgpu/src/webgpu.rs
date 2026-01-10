use crate::{BackendCaps, WebGpuInitError};

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

/// A WebGPU adapter/device/queue bundle with negotiated capabilities.
pub struct WebGpuContext {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    caps: BackendCaps,
}

impl WebGpuContext {
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
        let backends = if cfg!(target_arch = "wasm32") {
            wgpu::Backends::BROWSER_WEBGPU
        } else {
            // Avoid initializing the GL backend in headless CI environments; it
            // can emit noisy display-system errors (Wayland/X11) even when Vulkan
            // is available.
            wgpu::Backends::PRIMARY
        };
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor { backends, ..Default::default() });
        Self::request_internal(instance, options, None).await
    }

    pub(crate) async fn request_with_surface<'a>(
        instance: wgpu::Instance,
        options: WebGpuInitOptions,
        surface: &'a wgpu::Surface<'a>,
    ) -> Result<Self, WebGpuInitError> {
        Self::request_internal(instance, options, Some(surface)).await
    }

    async fn request_internal<'a>(
        instance: wgpu::Instance,
        options: WebGpuInitOptions,
        compatible_surface: Option<&'a wgpu::Surface<'a>>,
    ) -> Result<Self, WebGpuInitError> {
        let adapter = request_adapter_robust(&instance, compatible_surface, options.power_preference)
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

        let caps = BackendCaps::from_webgpu(&device);

        Ok(Self {
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

    let mut requested = wgpu::Features::empty();

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

    requested
}

fn negotiated_limits(adapter: &wgpu::Adapter, desired_max_buffer_size: u64) -> wgpu::Limits {
    let adapter_limits = adapter.limits();

    let mut limits = wgpu::Limits::default();

    // Clamp requested limits to what the adapter reports.
    limits.max_buffer_size = desired_max_buffer_size.min(adapter_limits.max_buffer_size);
    let desired_storage_binding_size = limits
        .max_buffer_size
        .min(u64::from(adapter_limits.max_storage_buffer_binding_size))
        .min(u64::from(u32::MAX));
    limits.max_storage_buffer_binding_size = desired_storage_binding_size as u32;

    limits
}
