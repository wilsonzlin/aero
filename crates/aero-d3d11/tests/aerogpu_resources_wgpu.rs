use aero_d3d11::runtime::aerogpu_resources::{AerogpuResourceManager, DirtyRange};
use aero_protocol::aerogpu::aerogpu_cmd::{
    AEROGPU_INPUT_LAYOUT_BLOB_MAGIC, AEROGPU_INPUT_LAYOUT_BLOB_VERSION,
    AEROGPU_RESOURCE_USAGE_TEXTURE, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use anyhow::{anyhow, Context, Result};

async fn create_device_queue() -> Result<(wgpu::Device, wgpu::Queue)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let needs_runtime_dir = std::env::var("XDG_RUNTIME_DIR")
            .ok()
            .map(|v| v.is_empty())
            .unwrap_or(true);

        if needs_runtime_dir {
            let dir =
                std::env::temp_dir().join(format!("aero-d3d11-xdg-runtime-{}", std::process::id()));
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
            std::env::set_var("XDG_RUNTIME_DIR", &dir);
        }
    }

    let instance = wgpu::Instance::default();
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
    }
    .ok_or_else(|| anyhow!("wgpu: no suitable adapter found"))?;

    let (device, queue) = adapter
        .request_device(
            &wgpu::DeviceDescriptor {
                label: Some("aero-d3d11 aerogpu_resources test device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults(),
            },
            None,
        )
        .await
        .map_err(|e| anyhow!("wgpu: request_device failed: {e:?}"))?;

    Ok((device, queue))
}

async fn read_buffer(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    buffer: &wgpu::Buffer,
) -> Result<Vec<u8>> {
    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("aerogpu_resources read_buffer staging"),
        size: buffer.size(),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("aerogpu_resources read_buffer encoder"),
    });
    encoder.copy_buffer_to_buffer(buffer, 0, &staging, 0, buffer.size());
    queue.submit([encoder.finish()]);

    let slice = staging.slice(..);
    let (sender, receiver) = futures_intrusive::channel::shared::oneshot_channel();
    slice.map_async(wgpu::MapMode::Read, move |v| {
        sender.send(v).ok();
    });
    device.poll(wgpu::Maintain::Wait);
    receiver
        .receive()
        .await
        .ok_or_else(|| anyhow!("wgpu: map_async dropped"))?
        .context("wgpu: map_async failed")?;

    let data = slice.get_mapped_range().to_vec();
    staging.unmap();
    Ok(data)
}

async fn read_texture_rgba8(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    width: u32,
    height: u32,
) -> Result<Vec<u8>> {
    let bytes_per_pixel = 4u32;
    let unpadded_bytes_per_row = width * bytes_per_pixel;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded_bytes_per_row = ((unpadded_bytes_per_row + align - 1) / align) * align;
    let buffer_size = padded_bytes_per_row as u64 * height as u64;

    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("aerogpu_resources read_texture staging"),
        size: buffer_size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("aerogpu_resources read_texture encoder"),
    });
    encoder.copy_texture_to_buffer(
        wgpu::ImageCopyTexture {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::ImageCopyBuffer {
            buffer: &staging,
            layout: wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(padded_bytes_per_row),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);

    let slice = staging.slice(..);
    let (sender, receiver) = futures_intrusive::channel::shared::oneshot_channel();
    slice.map_async(wgpu::MapMode::Read, move |v| {
        sender.send(v).ok();
    });
    device.poll(wgpu::Maintain::Wait);
    receiver
        .receive()
        .await
        .ok_or_else(|| anyhow!("wgpu: map_async dropped"))?
        .context("wgpu: map_async failed")?;

    let mapped = slice.get_mapped_range();
    let mut out = Vec::with_capacity((unpadded_bytes_per_row * height) as usize);
    for row in 0..height as usize {
        let start = row * padded_bytes_per_row as usize;
        out.extend_from_slice(&mapped[start..start + unpadded_bytes_per_row as usize]);
    }
    drop(mapped);
    staging.unmap();
    Ok(out)
}

#[test]
fn upload_resource_buffer_and_texture_roundtrip() -> Result<()> {
    pollster::block_on(async {
        let (device, queue) = create_device_queue().await?;
        let mut resources = AerogpuResourceManager::new(device, queue);

        // Buffer upload.
        let buf_handle = 1;
        resources.create_buffer(buf_handle, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER, 16, 0, 0)?;

        let buf_data: Vec<u8> = (0u8..16u8).collect();
        resources.upload_resource(
            buf_handle,
            DirtyRange {
                offset_bytes: 0,
                size_bytes: 16,
            },
            &buf_data,
        )?;

        let buf = resources.buffer(buf_handle)?;
        let readback = read_buffer(resources.device(), resources.queue(), &buf.buffer).await?;
        assert_eq!(readback[..16], buf_data);

        // Texture upload.
        let tex_handle = 2;
        resources.create_texture2d(
            tex_handle,
            AEROGPU_RESOURCE_USAGE_TEXTURE,
            AerogpuFormat::R8G8B8A8Unorm as u32,
            2,
            2,
            1,
            1,
            8,
            0,
            0,
        )?;

        // 2x2 RGBA8: four pixels.
        let tex_data: Vec<u8> = vec![
            0x01, 0x02, 0x03, 0x04, // p0
            0x05, 0x06, 0x07, 0x08, // p1
            0x09, 0x0A, 0x0B, 0x0C, // p2
            0x0D, 0x0E, 0x0F, 0x10, // p3
        ];

        resources.upload_resource(
            tex_handle,
            DirtyRange {
                offset_bytes: 0,
                size_bytes: tex_data.len() as u64,
            },
            &tex_data,
        )?;

        let tex = resources.texture2d(tex_handle)?;
        assert_eq!(tex.desc.format, wgpu::TextureFormat::Rgba8Unorm);
        let readback =
            read_texture_rgba8(resources.device(), resources.queue(), &tex.texture, 2, 2).await?;
        assert_eq!(readback, tex_data);

        Ok(())
    })
}
#[test]
fn create_texture2d_requires_row_pitch_for_backed_textures() -> Result<()> {
    pollster::block_on(async {
        let (device, queue) = create_device_queue().await?;
        let mut resources = AerogpuResourceManager::new(device, queue);

        let res = resources.create_texture2d(
            42,
            AEROGPU_RESOURCE_USAGE_TEXTURE,
            AerogpuFormat::R8G8B8A8Unorm as u32,
            4,
            4,
            1,
            1,
            0, // row_pitch_bytes
            1, // backing_alloc_id
            0,
        );
        let err = res.expect_err("expected create_texture2d to reject missing row_pitch_bytes");
        assert!(err
            .to_string()
            .contains("row_pitch_bytes is required for allocation-backed textures"));
        Ok(())
    })
}

#[test]
fn handles_are_namespaced_per_object_type() -> Result<()> {
    pollster::block_on(async {
        let (device, queue) = create_device_queue().await?;
        let mut resources = AerogpuResourceManager::new(device, queue);

        // The protocol uses separate handle namespaces for resources, shaders, and input layouts.
        // Ensure we can reuse the same numeric handle across object types.
        let handle = 7u32;

        resources.create_buffer(handle, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER, 16, 0, 0)?;

        let mut ilay = Vec::new();
        ilay.extend_from_slice(&AEROGPU_INPUT_LAYOUT_BLOB_MAGIC.to_le_bytes());
        ilay.extend_from_slice(&AEROGPU_INPUT_LAYOUT_BLOB_VERSION.to_le_bytes());
        ilay.extend_from_slice(&1u32.to_le_bytes()); // element_count
        ilay.extend_from_slice(&0u32.to_le_bytes()); // reserved0
                                                     // element 0
        ilay.extend_from_slice(&0u32.to_le_bytes()); // semantic_name_hash
        ilay.extend_from_slice(&0u32.to_le_bytes()); // semantic_index
        ilay.extend_from_slice(&2u32.to_le_bytes()); // DXGI_FORMAT_R32G32B32A32_FLOAT
        ilay.extend_from_slice(&0u32.to_le_bytes()); // input_slot
        ilay.extend_from_slice(&0u32.to_le_bytes()); // aligned_byte_offset
        ilay.extend_from_slice(&0u32.to_le_bytes()); // input_slot_class
        ilay.extend_from_slice(&0u32.to_le_bytes()); // instance_data_step_rate

        resources.create_input_layout(handle, ilay)?;

        Ok(())
    })
}
