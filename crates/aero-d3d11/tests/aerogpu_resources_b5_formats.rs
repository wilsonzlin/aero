mod common;

use aero_d3d11::runtime::aerogpu_resources::{
    AerogpuResourceManager, DirtyRange, Texture2dCreateDesc, TextureUploadTransform,
};
use aero_protocol::aerogpu::aerogpu_cmd::AEROGPU_RESOURCE_USAGE_TEXTURE;
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use anyhow::Result;

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

