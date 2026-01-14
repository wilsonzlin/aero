mod common;

use std::collections::HashMap;

use aero_d3d11::runtime::aerogpu_resources::{
    AerogpuResourceManager, DirtyRange, Texture2dCreateDesc, TextureUploadTransform,
};
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::AEROGPU_RESOURCE_USAGE_TEXTURE;
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::aerogpu_ring::AerogpuAllocEntry;
use anyhow::{anyhow, Context, Result};

async fn read_texture_rgba8_subresource(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    mip_level: u32,
    array_layer: u32,
    width: u32,
    height: u32,
) -> Result<Vec<u8>> {
    let bytes_per_pixel = 4u32;
    let unpadded_bytes_per_row = width
        .checked_mul(bytes_per_pixel)
        .ok_or_else(|| anyhow!("bytes_per_row overflow"))?;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(align) * align;
    let buffer_size = padded_bytes_per_row as u64 * height as u64;

    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("aero-d3d11 b5 read_texture staging"),
        size: buffer_size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("aero-d3d11 b5 read_texture encoder"),
    });
    encoder.copy_texture_to_buffer(
        wgpu::ImageCopyTexture {
            texture,
            mip_level,
            origin: wgpu::Origin3d {
                x: 0,
                y: 0,
                z: array_layer,
            },
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
    #[cfg(not(target_arch = "wasm32"))]
    device.poll(wgpu::Maintain::Wait);
    #[cfg(target_arch = "wasm32")]
    device.poll(wgpu::Maintain::Poll);

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

fn push_b5_subresource(
    dst: &mut Vec<u8>,
    width: u32,
    height: u32,
    row_pitch_bytes: u32,
    pixel_le16: [u8; 2],
    pad_pattern: [u8; 2],
) -> Result<()> {
    let unpadded_bpr = width
        .checked_mul(2)
        .ok_or_else(|| anyhow!("unpadded bytes_per_row overflow"))?;
    if row_pitch_bytes < unpadded_bpr {
        return Err(anyhow!("row_pitch_bytes too small for b5 subresource"));
    }
    let pad_len = row_pitch_bytes - unpadded_bpr;
    for y in 0..height {
        for _ in 0..width {
            dst.extend_from_slice(&pixel_le16);
        }
        for i in 0..pad_len {
            // Vary padding per row to make it obvious if the upload path accidentally consumes it.
            let b = if (y + i) % 2 == 0 { pad_pattern[0] } else { pad_pattern[1] };
            dst.push(b);
        }
    }
    Ok(())
}

#[test]
fn upload_resource_b5_formats_expand_to_rgba8() -> Result<()> {
    pollster::block_on(async {
        let (device, queue, _supports_compute) =
            match common::wgpu::create_device_queue("aero-d3d11 b5 format test device").await {
                Ok(v) => v,
                Err(err) => {
                    common::skip_or_panic(module_path!(), &format!("{err:#}"));
                    return Ok(());
                }
            };

        let mut resources = AerogpuResourceManager::new(device, queue);

        // ---- B5G6R5Unorm (with per-row padding) ----
        {
            let tex_handle = 1;
            let width = 2u32;
            let height = 2u32;
            let row_pitch_bytes = 8u32; // 4 bytes pixels + 4 bytes padding

            resources.create_texture2d(
                tex_handle,
                Texture2dCreateDesc {
                    usage_flags: AEROGPU_RESOURCE_USAGE_TEXTURE,
                    format: AerogpuFormat::B5G6R5Unorm as u32,
                    width,
                    height,
                    mip_levels: 1,
                    array_layers: 1,
                    row_pitch_bytes,
                    backing_alloc_id: 0,
                    backing_offset_bytes: 0,
                },
            )?;

            // 2x2 pixels, row-major:
            // row0: red, green
            // row1: blue, white
            let mut b5 = Vec::new();
            // row0
            b5.extend_from_slice(&[0x00, 0xF8, 0xE0, 0x07]);
            b5.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]); // padding (must be ignored)
            // row1
            b5.extend_from_slice(&[0x1F, 0x00, 0xFF, 0xFF]);
            b5.extend_from_slice(&[0xFE, 0xED, 0xFA, 0xCE]); // padding (must be ignored)
            assert_eq!(b5.len(), row_pitch_bytes as usize * height as usize);

            resources.upload_resource(
                tex_handle,
                DirtyRange {
                    offset_bytes: 0,
                    size_bytes: b5.len() as u64,
                },
                &b5,
            )?;

            let tex = resources.texture2d(tex_handle)?;
            assert_eq!(tex.desc.texture_format, wgpu::TextureFormat::Rgba8Unorm);
            assert_eq!(
                tex.desc.upload_transform,
                TextureUploadTransform::B5G6R5ToRgba8
            );

            let pixels = common::wgpu::read_texture_rgba8(
                resources.device(),
                resources.queue(),
                &tex.texture,
                width,
                height,
            )
            .await?;
            assert_eq!(
                pixels,
                vec![
                    255, 0, 0, 255, // red
                    0, 255, 0, 255, // green
                    0, 0, 255, 255, // blue
                    255, 255, 255, 255, // white
                ]
            );
        }

        // ---- B5G5R5A1Unorm (alpha=0 and alpha=1, with per-row padding) ----
        {
            let tex_handle = 2;
            let width = 2u32;
            let height = 2u32;
            let row_pitch_bytes = 8u32; // 4 bytes pixels + 4 bytes padding

            resources.create_texture2d(
                tex_handle,
                Texture2dCreateDesc {
                    usage_flags: AEROGPU_RESOURCE_USAGE_TEXTURE,
                    format: AerogpuFormat::B5G5R5A1Unorm as u32,
                    width,
                    height,
                    mip_levels: 1,
                    array_layers: 1,
                    row_pitch_bytes,
                    backing_alloc_id: 0,
                    backing_offset_bytes: 0,
                },
            )?;

            // row0: red (a=1), green (a=0)
            // row1: blue (a=1), white (a=0)
            let mut b5 = Vec::new();
            b5.extend_from_slice(&[
                0x00, 0xFC, // red, a=1
                0xE0, 0x03, // green, a=0
            ]);
            b5.extend_from_slice(&[0x11, 0x22, 0x33, 0x44]); // padding (must be ignored)
            b5.extend_from_slice(&[
                0x1F, 0x80, // blue, a=1
                0xFF, 0x7F, // white, a=0
            ]);
            b5.extend_from_slice(&[0x55, 0x66, 0x77, 0x88]); // padding (must be ignored)
            assert_eq!(b5.len(), row_pitch_bytes as usize * height as usize);

            resources.upload_resource(
                tex_handle,
                DirtyRange {
                    offset_bytes: 0,
                    size_bytes: b5.len() as u64,
                },
                &b5,
            )?;

            let tex = resources.texture2d(tex_handle)?;
            assert_eq!(tex.desc.texture_format, wgpu::TextureFormat::Rgba8Unorm);
            assert_eq!(
                tex.desc.upload_transform,
                TextureUploadTransform::B5G5R5A1ToRgba8
            );

            let pixels = common::wgpu::read_texture_rgba8(
                resources.device(),
                resources.queue(),
                &tex.texture,
                width,
                height,
            )
            .await?;
            assert_eq!(
                pixels,
                vec![
                    255, 0, 0, 255, // red, a=1
                    0, 255, 0, 0, // green, a=0
                    0, 0, 255, 255, // blue, a=1
                    255, 255, 255, 0, // white, a=0
                ]
            );
        }

        Ok(())
    })
}

#[test]
fn upload_resource_b5_formats_expand_to_rgba8_for_mips_and_array_layers() -> Result<()> {
    pollster::block_on(async {
        let (device, queue, _supports_compute) =
            match common::wgpu::create_device_queue("aero-d3d11 b5 mip/array test device").await {
                Ok(v) => v,
                Err(err) => {
                    common::skip_or_panic(module_path!(), &format!("{err:#}"));
                    return Ok(());
                }
            };

        let mut resources = AerogpuResourceManager::new(device, queue);

        // ---- B5G6R5Unorm: 2 array layers, 3 mips, padded mip0 row_pitch ----
        {
            let tex_handle = 10;
            let width = 4u32;
            let height = 4u32;
            let mip_levels = 3u32;
            let array_layers = 2u32;
            let row_pitch_bytes = 10u32; // 8 bytes pixels + 2 bytes padding

            resources.create_texture2d(
                tex_handle,
                Texture2dCreateDesc {
                    usage_flags: AEROGPU_RESOURCE_USAGE_TEXTURE,
                    format: AerogpuFormat::B5G6R5Unorm as u32,
                    width,
                    height,
                    mip_levels,
                    array_layers,
                    row_pitch_bytes,
                    backing_alloc_id: 0,
                    backing_offset_bytes: 0,
                },
            )?;

            // Build a full linear upload buffer in layer-major, mip-major order.
            let mut bytes = Vec::new();
            for layer in 0..array_layers {
                // mip0 4x4, padded rows
                let mip0_color = match layer {
                    0 => [0x00, 0xF8], // red
                    _ => [0xFF, 0xFF], // white
                };
                push_b5_subresource(
                    &mut bytes,
                    4,
                    4,
                    row_pitch_bytes,
                    mip0_color,
                    [0xDE, 0xAD],
                )?;

                // mip1 2x2, tight rows
                let mip1_color = match layer {
                    0 => [0xE0, 0x07], // green
                    _ => [0x00, 0x00], // black
                };
                push_b5_subresource(&mut bytes, 2, 2, 4, mip1_color, [0xBE, 0xEF])?;

                // mip2 1x1, tight rows
                let mip2_color = match layer {
                    0 => [0x1F, 0x00], // blue
                    _ => [0x00, 0xF8], // red
                };
                push_b5_subresource(&mut bytes, 1, 1, 2, mip2_color, [0xFE, 0xED])?;
            }

            resources.upload_resource(
                tex_handle,
                DirtyRange {
                    offset_bytes: 0,
                    size_bytes: bytes.len() as u64,
                },
                &bytes,
            )?;

            let tex = resources.texture2d(tex_handle)?;
            assert_eq!(tex.desc.texture_format, wgpu::TextureFormat::Rgba8Unorm);
            assert_eq!(
                tex.desc.upload_transform,
                TextureUploadTransform::B5G6R5ToRgba8
            );

            // Validate a few subresources (layer 0/1, mip 0/1/2).
            for (layer, mip, w, h, expected_px) in [
                (0u32, 0u32, 4u32, 4u32, [255u8, 0, 0, 255]),     // red
                (0u32, 1u32, 2u32, 2u32, [0u8, 255, 0, 255]),     // green
                (0u32, 2u32, 1u32, 1u32, [0u8, 0, 255, 255]),     // blue
                (1u32, 0u32, 4u32, 4u32, [255u8, 255, 255, 255]), // white
                (1u32, 1u32, 2u32, 2u32, [0u8, 0, 0, 255]),       // black
                (1u32, 2u32, 1u32, 1u32, [255u8, 0, 0, 255]),     // red
            ] {
                let readback = read_texture_rgba8_subresource(
                    resources.device(),
                    resources.queue(),
                    &tex.texture,
                    mip,
                    layer,
                    w,
                    h,
                )
                .await?;
                assert_eq!(readback, expected_px.repeat((w * h) as usize));
            }
        }

        // ---- B5G5R5A1Unorm: alpha handling across mips/layers ----
        {
            let tex_handle = 11;
            let width = 4u32;
            let height = 4u32;
            let mip_levels = 3u32;
            let array_layers = 2u32;
            let row_pitch_bytes = 10u32;

            resources.create_texture2d(
                tex_handle,
                Texture2dCreateDesc {
                    usage_flags: AEROGPU_RESOURCE_USAGE_TEXTURE,
                    format: AerogpuFormat::B5G5R5A1Unorm as u32,
                    width,
                    height,
                    mip_levels,
                    array_layers,
                    row_pitch_bytes,
                    backing_alloc_id: 0,
                    backing_offset_bytes: 0,
                },
            )?;

            let mut bytes = Vec::new();
            for layer in 0..array_layers {
                // mip0 4x4
                let mip0_color = match layer {
                    0 => [0x00, 0xFC], // red, a=1
                    _ => [0xFF, 0x7F], // white, a=0
                };
                push_b5_subresource(
                    &mut bytes,
                    4,
                    4,
                    row_pitch_bytes,
                    mip0_color,
                    [0x11, 0x22],
                )?;

                // mip1 2x2
                let mip1_color = match layer {
                    0 => [0xE0, 0x03], // green, a=0
                    _ => [0x00, 0x80], // black, a=1
                };
                push_b5_subresource(&mut bytes, 2, 2, 4, mip1_color, [0x33, 0x44])?;

                // mip2 1x1
                let mip2_color = match layer {
                    0 => [0x1F, 0x80], // blue, a=1
                    _ => [0x00, 0x7C], // red, a=0
                };
                push_b5_subresource(&mut bytes, 1, 1, 2, mip2_color, [0x55, 0x66])?;
            }

            resources.upload_resource(
                tex_handle,
                DirtyRange {
                    offset_bytes: 0,
                    size_bytes: bytes.len() as u64,
                },
                &bytes,
            )?;

            let tex = resources.texture2d(tex_handle)?;
            assert_eq!(tex.desc.texture_format, wgpu::TextureFormat::Rgba8Unorm);
            assert_eq!(
                tex.desc.upload_transform,
                TextureUploadTransform::B5G5R5A1ToRgba8
            );

            for (layer, mip, w, h, expected_px) in [
                (0u32, 0u32, 4u32, 4u32, [255u8, 0, 0, 255]),     // red, a=1
                (0u32, 1u32, 2u32, 2u32, [0u8, 255, 0, 0]),       // green, a=0
                (0u32, 2u32, 1u32, 1u32, [0u8, 0, 255, 255]),     // blue, a=1
                (1u32, 0u32, 4u32, 4u32, [255u8, 255, 255, 0]),   // white, a=0
                (1u32, 1u32, 2u32, 2u32, [0u8, 0, 0, 255]),       // black, a=1
                (1u32, 2u32, 1u32, 1u32, [255u8, 0, 0, 0]),       // red, a=0
            ] {
                let readback = read_texture_rgba8_subresource(
                    resources.device(),
                    resources.queue(),
                    &tex.texture,
                    mip,
                    layer,
                    w,
                    h,
                )
                .await?;
                assert_eq!(readback, expected_px.repeat((w * h) as usize));
            }
        }

        Ok(())
    })
}

#[test]
fn ensure_texture_uploaded_guest_backed_b5_formats_expand_to_rgba8() -> Result<()> {
    pollster::block_on(async {
        let (device, queue, _supports_compute) = match common::wgpu::create_device_queue(
            "aero-d3d11 b5 guest-backed test device",
        )
        .await
        {
            Ok(v) => v,
            Err(err) => {
                common::skip_or_panic(module_path!(), &format!("{err:#}"));
                return Ok(());
            }
        };

        let mut resources = AerogpuResourceManager::new(device, queue);

        // Two allocations with distinct GPAs so we can host both textures in one VecGuestMemory.
        let mut alloc_table: HashMap<u32, AerogpuAllocEntry> = HashMap::new();
        alloc_table.insert(
            1,
            AerogpuAllocEntry {
                alloc_id: 1,
                flags: 0,
                gpa: 0,
                size_bytes: 0x100,
                reserved0: 0,
            },
        );
        alloc_table.insert(
            2,
            AerogpuAllocEntry {
                alloc_id: 2,
                flags: 0,
                gpa: 0x100,
                size_bytes: 0x100,
                reserved0: 0,
            },
        );

        let mut guest_mem = VecGuestMemory::new(0x200);

        // ---- B5G6R5Unorm (guest-backed) ----
        {
            let tex_handle = 20;
            let width = 2u32;
            let height = 2u32;
            let row_pitch_bytes = 8u32; // 4 bytes pixels + 4 bytes padding

            resources.create_texture2d(
                tex_handle,
                Texture2dCreateDesc {
                    usage_flags: AEROGPU_RESOURCE_USAGE_TEXTURE,
                    format: AerogpuFormat::B5G6R5Unorm as u32,
                    width,
                    height,
                    mip_levels: 1,
                    array_layers: 1,
                    row_pitch_bytes,
                    backing_alloc_id: 1,
                    backing_offset_bytes: 0,
                },
            )?;

            // Same pattern as the host-owned regression test; includes non-zero padding bytes that
            // must be ignored by the upload path.
            let b5: [u8; 16] = [
                // row0: red, green
                0x00, 0xF8, 0xE0, 0x07, // pixels
                0xDE, 0xAD, 0xBE, 0xEF, // padding
                // row1: blue, white
                0x1F, 0x00, 0xFF, 0xFF, // pixels
                0xFE, 0xED, 0xFA, 0xCE, // padding
            ];
            guest_mem
                .write(0, &b5)
                .context("write guest memory for B5G6R5 texture")?;

            resources.ensure_texture_uploaded(
                tex_handle,
                DirtyRange {
                    offset_bytes: 0,
                    size_bytes: b5.len() as u64,
                },
                &mut guest_mem,
                &alloc_table,
            )?;

            let tex = resources.texture2d(tex_handle)?;
            assert_eq!(tex.desc.texture_format, wgpu::TextureFormat::Rgba8Unorm);
            assert_eq!(
                tex.desc.upload_transform,
                TextureUploadTransform::B5G6R5ToRgba8
            );

            let pixels = common::wgpu::read_texture_rgba8(
                resources.device(),
                resources.queue(),
                &tex.texture,
                width,
                height,
            )
            .await?;
            assert_eq!(
                pixels,
                vec![
                    255, 0, 0, 255, // red
                    0, 255, 0, 255, // green
                    0, 0, 255, 255, // blue
                    255, 255, 255, 255, // white
                ]
            );
        }

        // ---- B5G5R5A1Unorm (guest-backed) ----
        {
            let tex_handle = 21;
            let width = 2u32;
            let height = 2u32;
            let row_pitch_bytes = 8u32; // 4 bytes pixels + 4 bytes padding

            resources.create_texture2d(
                tex_handle,
                Texture2dCreateDesc {
                    usage_flags: AEROGPU_RESOURCE_USAGE_TEXTURE,
                    format: AerogpuFormat::B5G5R5A1Unorm as u32,
                    width,
                    height,
                    mip_levels: 1,
                    array_layers: 1,
                    row_pitch_bytes,
                    backing_alloc_id: 2,
                    backing_offset_bytes: 0,
                },
            )?;

            let b5: [u8; 16] = [
                // row0: red(a=1), green(a=0)
                0x00, 0xFC, 0xE0, 0x03, // pixels
                0x11, 0x22, 0x33, 0x44, // padding
                // row1: blue(a=1), white(a=0)
                0x1F, 0x80, 0xFF, 0x7F, // pixels
                0x55, 0x66, 0x77, 0x88, // padding
            ];
            guest_mem
                .write(0x100, &b5)
                .context("write guest memory for B5G5R5A1 texture")?;

            resources.ensure_texture_uploaded(
                tex_handle,
                DirtyRange {
                    offset_bytes: 0,
                    size_bytes: b5.len() as u64,
                },
                &mut guest_mem,
                &alloc_table,
            )?;

            let tex = resources.texture2d(tex_handle)?;
            assert_eq!(tex.desc.texture_format, wgpu::TextureFormat::Rgba8Unorm);
            assert_eq!(
                tex.desc.upload_transform,
                TextureUploadTransform::B5G5R5A1ToRgba8
            );

            let pixels = common::wgpu::read_texture_rgba8(
                resources.device(),
                resources.queue(),
                &tex.texture,
                width,
                height,
            )
            .await?;
            assert_eq!(
                pixels,
                vec![
                    255, 0, 0, 255, // red, a=1
                    0, 255, 0, 0, // green, a=0
                    0, 0, 255, 255, // blue, a=1
                    255, 255, 255, 0, // white, a=0
                ]
            );
        }

        Ok(())
    })
}
