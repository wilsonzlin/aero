use aero_webgpu::{Rgba8TextureUploader, WebGpuContext};

mod common;

#[test]
fn rgba8_uploader_noops_on_zero_extent() {
    const TEST_NAME: &str = "rgba8_uploader_noops_on_zero_extent";

    pollster::block_on(async {
        let ctx = match WebGpuContext::request_headless(Default::default()).await {
            Ok(ctx) => ctx,
            Err(err) => {
                common::skip_or_panic(TEST_NAME, &err.to_string());
                return;
            }
        };

        let device = ctx.device();
        let queue = ctx.queue();

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("aero-webgpu rgba8 uploader noop texture"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });

        let mut uploader = Rgba8TextureUploader::new();

        device.push_error_scope(wgpu::ErrorFilter::Validation);
        uploader.write_texture_with_stride(queue, &texture, 0, 1, &[], 0);
        #[cfg(not(target_arch = "wasm32"))]
        device.poll(wgpu::Maintain::Wait);
        let err = device.pop_error_scope().await;
        assert!(
            err.is_none(),
            "expected zero-width upload to be a no-op without validation errors, got: {err:?}"
        );

        device.push_error_scope(wgpu::ErrorFilter::Validation);
        uploader.write_texture_with_stride(queue, &texture, 1, 0, &[], 0);
        #[cfg(not(target_arch = "wasm32"))]
        device.poll(wgpu::Maintain::Wait);
        let err = device.pop_error_scope().await;
        assert!(
            err.is_none(),
            "expected zero-height upload to be a no-op without validation errors, got: {err:?}"
        );
    });
}

