use aero_d3d9::resources::*;

fn require_webgpu() -> bool {
    std::env::var("AERO_REQUIRE_WEBGPU")
        .ok()
        .map(|raw| {
            let v = raw.trim();
            v == "1"
                || v.eq_ignore_ascii_case("true")
                || v.eq_ignore_ascii_case("yes")
                || v.eq_ignore_ascii_case("on")
        })
        .unwrap_or(false)
}

fn skip_or_panic(test_name: &str, reason: &str) {
    if require_webgpu() {
        panic!("AERO_REQUIRE_WEBGPU is enabled but {test_name} cannot run: {reason}");
    }
    eprintln!("skipping {test_name}: {reason}");
}

fn ensure_xdg_runtime_dir() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let needs_runtime_dir = std::env::var("XDG_RUNTIME_DIR")
            .ok()
            .map(|v| v.is_empty())
            .unwrap_or(true);
        if !needs_runtime_dir {
            return;
        }

        let dir = std::env::temp_dir().join(format!(
            "aero-d3d9-xdg-runtime-{}-bc-mip-upload",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        std::env::set_var("XDG_RUNTIME_DIR", &dir);
    }
}

async fn request_device_with_bc_features() -> Option<(wgpu::Device, wgpu::Queue)> {
    ensure_xdg_runtime_dir();

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
                label: Some("aero-d3d9 bc mip upload test device"),
                required_features: wgpu::Features::TEXTURE_COMPRESSION_BC,
                required_limits: wgpu::Limits::downlevel_defaults(),
            },
            None,
        )
        .await
        .ok()
}

#[test]
fn d3d9_bc_mip_upload_pads_small_mips() {
    let (device, queue) = match pollster::block_on(request_device_with_bc_features()) {
        Some(device) => device,
        None => {
            skip_or_panic(module_path!(), "TEXTURE_COMPRESSION_BC not supported");
            return;
        }
    };

    let mut rm = ResourceManager::new(device, queue, ResourceManagerOptions::default());
    rm.begin_frame();

    const TEX: GuestResourceId = 1;

    // 4x4 BC1 with 2 mips -> mip1 is 2x2, but WebGPU requires a 4x4 physical upload for BC.
    rm.create_texture(
        TEX,
        TextureDesc {
            kind: TextureKind::Texture2D {
                width: 4,
                height: 4,
                levels: 2,
            },
            format: D3DFormat::Dxt1,
            pool: D3DPool::Default,
            usage: TextureUsageKind::Sampled,
        },
    )
    .unwrap();

    {
        let locked = rm.lock_texture_rect(TEX, 0, 0, LockFlags::empty()).unwrap();
        assert_eq!(locked.data.len(), 8);
        locked.data.copy_from_slice(&[0xAA; 8]);
    }
    rm.unlock_texture_rect(TEX).unwrap();

    {
        let locked = rm.lock_texture_rect(TEX, 1, 0, LockFlags::empty()).unwrap();
        assert_eq!(locked.data.len(), 8);
        locked.data.copy_from_slice(&[0x55; 8]);
    }
    rm.unlock_texture_rect(TEX).unwrap();

    let mut encoder = rm
        .device()
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("aero-d3d9 bc mip upload test encoder"),
        });
    rm.encode_uploads(&mut encoder);
    rm.submit(encoder);

    #[cfg(not(target_arch = "wasm32"))]
    rm.device().poll(wgpu::Maintain::Wait);
}

#[test]
fn d3d9_bc_create_texture_falls_back_for_non_block_aligned_dimensions() {
    let (device, queue) = match pollster::block_on(request_device_with_bc_features()) {
        Some(device) => device,
        None => {
            skip_or_panic(module_path!(), "TEXTURE_COMPRESSION_BC not supported");
            return;
        }
    };

    let mut rm = ResourceManager::new(device, queue, ResourceManagerOptions::default());
    rm.begin_frame();

    const TEX: GuestResourceId = 1;

    // 9x9 BC1 is not 4x4 block-aligned; the resource manager must fall back to BGRA8 and
    // decompress on upload to avoid wgpu validation errors at texture creation time.
    rm.create_texture(
        TEX,
        TextureDesc {
            kind: TextureKind::Texture2D {
                width: 9,
                height: 9,
                levels: 4,
            },
            format: D3DFormat::Dxt1,
            pool: D3DPool::Default,
            usage: TextureUsageKind::Sampled,
        },
    )
    .unwrap();

    assert_eq!(
        rm.texture(TEX).unwrap().wgpu_format(),
        wgpu::TextureFormat::Bgra8Unorm
    );

    // Upload some dummy BC1 data and flush to ensure the fallback upload path doesn't
    // trigger any further validation errors.
    {
        let locked = rm.lock_texture_rect(TEX, 0, 0, LockFlags::empty()).unwrap();
        // 9x9 BC1 is 3x3 blocks => 72 bytes.
        assert_eq!(locked.data.len(), 72);
        locked.data.copy_from_slice(&[0u8; 72]);
    }
    rm.unlock_texture_rect(TEX).unwrap();

    let mut encoder = rm
        .device()
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("aero-d3d9 bc non-aligned create test encoder"),
        });
    rm.encode_uploads(&mut encoder);
    rm.submit(encoder);

    #[cfg(not(target_arch = "wasm32"))]
    rm.device().poll(wgpu::Maintain::Wait);
}

#[test]
fn d3d9_bc_create_texture_falls_back_for_tiny_dimensions() {
    let (device, queue) = match pollster::block_on(request_device_with_bc_features()) {
        Some(device) => device,
        None => {
            skip_or_panic(module_path!(), "TEXTURE_COMPRESSION_BC not supported");
            return;
        }
    };

    let mut rm = ResourceManager::new(device, queue, ResourceManagerOptions::default());
    rm.begin_frame();

    const TEX: GuestResourceId = 1;

    // wgpu validation rejects creating BC textures whose base mip dimensions are not multiples of
    // 4x4. (e.g. 1x1 BC1). The resource manager must fall back to BGRA8 + CPU decompression.
    rm.create_texture(
        TEX,
        TextureDesc {
            kind: TextureKind::Texture2D {
                width: 1,
                height: 1,
                levels: 1,
            },
            format: D3DFormat::Dxt1,
            pool: D3DPool::Default,
            usage: TextureUsageKind::Sampled,
        },
    )
    .unwrap();

    assert_eq!(
        rm.texture(TEX).unwrap().wgpu_format(),
        wgpu::TextureFormat::Bgra8Unorm
    );

    // 1x1 BC1 is still stored as a single 4x4 block in the D3D/guest layout: 8 bytes.
    {
        let locked = rm.lock_texture_rect(TEX, 0, 0, LockFlags::empty()).unwrap();
        assert_eq!(locked.data.len(), 8);
        locked.data.copy_from_slice(&[0u8; 8]);
    }
    rm.unlock_texture_rect(TEX).unwrap();

    let mut encoder = rm
        .device()
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("aero-d3d9 bc tiny create test encoder"),
        });
    rm.encode_uploads(&mut encoder);
    rm.submit(encoder);

    #[cfg(not(target_arch = "wasm32"))]
    rm.device().poll(wgpu::Maintain::Wait);
}
