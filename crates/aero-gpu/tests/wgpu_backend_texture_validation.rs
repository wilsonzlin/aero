mod common;

use aero_gpu::backend::WgpuBackend;
use aero_gpu::hal::*;
use aero_gpu::GpuError;

#[test]
fn wgpu_backend_create_texture_rejects_excessive_mip_level_count() {
    common::ensure_xdg_runtime_dir();
    let mut backend = match pollster::block_on(WgpuBackend::new_headless(BackendKind::WebGpu)) {
        Ok(backend) => backend,
        Err(err) => {
            common::skip_or_panic(module_path!(), &format!("wgpu backend init failed: {err}"));
            return;
        }
    };

    let err = backend
        .create_texture(TextureDesc {
            label: Some("tex".into()),
            size: Extent3d {
                width: 4,
                height: 4,
                depth_or_array_layers: 1,
            },
            mip_level_count: 10,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format: TextureFormat::Rgba8Unorm,
            usage: TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST,
        })
        .expect_err("expected invalid mip_level_count to be rejected");
    assert!(matches!(err, GpuError::Backend(ref msg) if msg.contains("mip_level_count")));
}

#[test]
fn wgpu_backend_write_texture_rejects_out_of_range_mip_level() {
    common::ensure_xdg_runtime_dir();
    let mut backend = match pollster::block_on(WgpuBackend::new_headless(BackendKind::WebGpu)) {
        Ok(backend) => backend,
        Err(err) => {
            common::skip_or_panic(module_path!(), &format!("wgpu backend init failed: {err}"));
            return;
        }
    };

    let texture = backend
        .create_texture(TextureDesc {
            label: Some("tex".into()),
            size: Extent3d {
                width: 4,
                height: 4,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format: TextureFormat::Rgba8Unorm,
            usage: TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST,
        })
        .unwrap();

    let err = backend
        .write_texture(
            TextureWriteDesc {
                texture,
                mip_level: 1,
                origin: Origin3d::ZERO,
                layout: ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(4 * 4),
                    rows_per_image: Some(1),
                },
                size: Extent3d {
                    width: 4,
                    height: 1,
                    depth_or_array_layers: 1,
                },
            },
            &[0u8; 4 * 4],
        )
        .expect_err("expected out-of-range mip_level to be rejected");
    assert!(matches!(err, GpuError::Backend(ref msg) if msg.contains("mip_level")));

    // Ensure any previous uploads are flushed before destruction to avoid wgpu validation errors.
    backend.submit(&[]).unwrap();
    backend.destroy_texture(texture).unwrap();
}
