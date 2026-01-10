use futures_intrusive::channel::shared::oneshot_channel;

use crate::texture_manager::TextureRegion;

fn align_to(value: u32, alignment: u32) -> u32 {
    debug_assert!(alignment.is_power_of_two());
    (value + alignment - 1) & !(alignment - 1)
}

/// Read back an RGBA8 region from a texture.
///
/// This is intended for tests and debugging; it uses a staging buffer and
/// `map_async`, and works on both native and WASM targets.
///
/// # Panics
/// Panics if mapping fails. (This is test-facing API.)
pub async fn readback_rgba8(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    region: TextureRegion,
) -> Vec<u8> {
    let width = region.size.width;
    let height = region.size.height;
    assert!(width > 0 && height > 0, "readback region must be non-empty");

    let unpadded_bpr = width * 4;
    let padded_bpr = align_to(unpadded_bpr, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let buffer_size = padded_bpr as u64 * height as u64;

    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("aero-gpu.readback_rgba8"),
        size: buffer_size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("aero-gpu.readback_rgba8.encoder"),
    });

    encoder.copy_texture_to_buffer(
        wgpu::ImageCopyTexture {
            texture,
            mip_level: region.mip_level,
            origin: region.origin,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::ImageCopyBuffer {
            buffer: &buffer,
            layout: wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(padded_bpr),
                rows_per_image: Some(height),
            },
        },
        region.size,
    );

    queue.submit([encoder.finish()]);

    let slice = buffer.slice(..);
    let (sender, receiver) = oneshot_channel();
    slice.map_async(wgpu::MapMode::Read, move |res| {
        sender.send(res).ok();
    });

    #[cfg(not(target_arch = "wasm32"))]
    device.poll(wgpu::Maintain::Wait);

    #[cfg(target_arch = "wasm32")]
    device.poll(wgpu::Maintain::Poll);

    receiver
        .receive()
        .await
        .expect("map_async sender dropped")
        .expect("map_async failed");

    let mapped = slice.get_mapped_range();
    let mut out = vec![0u8; (width * height * 4) as usize];
    for row in 0..height as usize {
        let src_start = row * padded_bpr as usize;
        let src_end = src_start + unpadded_bpr as usize;
        let dst_start = row * unpadded_bpr as usize;
        out[dst_start..dst_start + unpadded_bpr as usize]
            .copy_from_slice(&mapped[src_start..src_end]);
    }
    drop(mapped);
    buffer.unmap();
    out
}
