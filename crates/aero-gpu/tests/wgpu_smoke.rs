use aero_gpu::backend::WgpuBackend;
use aero_gpu::hal::*;
use aero_gpu::GpuError;

#[test]
fn wgpu_backend_create_destroy_smoke() {
    let mut backend = match pollster::block_on(WgpuBackend::new_headless(BackendKind::WebGpu)) {
        Ok(backend) => backend,
        Err(err) => {
            eprintln!("skipping wgpu smoke test: {err}");
            return;
        }
    };

    let buffer = backend
        .create_buffer(BufferDesc {
            label: Some("buf".into()),
            size: 16,
            usage: BufferUsages::COPY_DST,
        })
        .unwrap();
    backend.write_buffer(buffer, 0, &[1, 2, 3, 4]).unwrap();
    backend.destroy_buffer(buffer).unwrap();
    assert!(matches!(
        backend.destroy_buffer(buffer),
        Err(GpuError::InvalidHandle { .. })
    ));

    let texture = backend
        .create_texture(TextureDesc {
            label: Some("tex".into()),
            size: Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format: TextureFormat::Rgba8Unorm,
            usage: TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST,
        })
        .unwrap();
    let view = backend
        .create_texture_view(texture, TextureViewDesc::default())
        .unwrap();
    backend.destroy_texture_view(view).unwrap();
    backend.destroy_texture(texture).unwrap();

    let sampler = backend.create_sampler(SamplerDesc::default()).unwrap();
    backend.destroy_sampler(sampler).unwrap();

    let cmd = backend.create_command_buffer(&[]).unwrap();
    backend.submit(&[cmd]).unwrap();
    assert!(matches!(
        backend.submit(&[cmd]),
        Err(GpuError::InvalidHandle { .. })
    ));
}
