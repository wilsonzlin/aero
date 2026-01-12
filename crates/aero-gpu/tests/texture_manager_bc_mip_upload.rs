mod common;

use aero_gpu::{GpuCapabilities, TextureDesc, TextureFormat, TextureManager, TextureRegion};

async fn create_device_queue_bc() -> Option<(wgpu::Device, wgpu::Queue)> {
    common::ensure_xdg_runtime_dir();

    // Prefer GL on Linux CI to avoid crashes in some Vulkan software adapters.
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: if cfg!(target_os = "linux") {
            wgpu::Backends::GL
        } else {
            wgpu::Backends::all()
        },
        ..Default::default()
    });

    let adapter = match instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: true,
        })
        .await
    {
        Some(adapter) => Some(adapter),
        None => {
            instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::LowPower,
                    compatible_surface: None,
                    force_fallback_adapter: false,
                })
                .await
        }
    }?;

    if !adapter
        .features()
        .contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
    {
        return None;
    }

    adapter
        .request_device(
            &wgpu::DeviceDescriptor {
                label: Some("aero-gpu TextureManager BC mip upload test device"),
                required_features: wgpu::Features::TEXTURE_COMPRESSION_BC,
                required_limits: wgpu::Limits::downlevel_defaults(),
            },
            None,
        )
        .await
        .ok()
}

fn env_truthy(name: &str) -> bool {
    let Ok(raw) = std::env::var(name) else {
        return false;
    };
    let v = raw.trim();
    v == "1"
        || v.eq_ignore_ascii_case("true")
        || v.eq_ignore_ascii_case("yes")
        || v.eq_ignore_ascii_case("on")
}

#[test]
fn texture_manager_bc_mip_upload_pads_small_mips() {
    const TEST_NAME: &str = concat!(
        module_path!(),
        "::texture_manager_bc_mip_upload_pads_small_mips"
    );

    pollster::block_on(async {
        if env_truthy("AERO_DISABLE_WGPU_TEXTURE_COMPRESSION") {
            common::skip_or_panic(
                TEST_NAME,
                "AERO_DISABLE_WGPU_TEXTURE_COMPRESSION is set; skipping native BC path tests",
            );
            return;
        }

        let (device, queue) = match create_device_queue_bc().await {
            Some(v) => v,
            None => {
                common::skip_or_panic(TEST_NAME, "TEXTURE_COMPRESSION_BC not supported");
                return;
            }
        };

        let caps = GpuCapabilities::from_device(&device);
        assert!(
            caps.supports_bc_texture_compression,
            "device should have TEXTURE_COMPRESSION_BC enabled"
        );

        let mut textures = TextureManager::new(&device, &queue, caps);

        let tex_key = 0xBADC0FFEu64;
        textures.create_texture(
            tex_key,
            TextureDesc {
                size: wgpu::Extent3d {
                    width: 4,
                    height: 4,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 2,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: TextureFormat::Bc1RgbaUnorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                label: Some("bc1-mip-upload".to_string()),
            },
        );

        // Upload mip0 (4x4) and mip1 (2x2). WebGPU requires the BC mip1 upload to use the
        // physical 4x4 block extent, so this is a regression test for small mip uploads.
        device.push_error_scope(wgpu::ErrorFilter::Validation);

        textures
            .write_texture_region(
                tex_key,
                TextureRegion {
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    size: wgpu::Extent3d {
                        width: 4,
                        height: 4,
                        depth_or_array_layers: 1,
                    },
                },
                &[0xAA; 8],
            )
            .unwrap();
        textures
            .write_texture_region(
                tex_key,
                TextureRegion {
                    mip_level: 1,
                    origin: wgpu::Origin3d::ZERO,
                    size: wgpu::Extent3d {
                        width: 2,
                        height: 2,
                        depth_or_array_layers: 1,
                    },
                },
                &[0x55; 8],
            )
            .unwrap();

        #[cfg(not(target_arch = "wasm32"))]
        device.poll(wgpu::Maintain::Wait);

        let err = device.pop_error_scope().await;
        assert!(
            err.is_none(),
            "expected BC mip upload to succeed without validation errors, got: {err:?}"
        );
    });
}
