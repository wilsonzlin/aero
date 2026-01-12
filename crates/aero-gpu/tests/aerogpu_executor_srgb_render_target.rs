mod common;

use aero_gpu::aerogpu_executor::AeroGpuExecutor;
use aero_gpu::{readback_rgba8, TextureRegion, VecGuestMemory};
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode,
    AerogpuCmdStreamHeader as ProtocolCmdStreamHeader, AerogpuShaderStage, AEROGPU_CLEAR_COLOR,
    AEROGPU_CMD_STREAM_MAGIC, AEROGPU_RESOURCE_USAGE_RENDER_TARGET, AEROGPU_RESOURCE_USAGE_TEXTURE,
    AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::{AerogpuFormat, AEROGPU_ABI_VERSION_U32};

const CMD_STREAM_SIZE_BYTES_OFFSET: usize =
    core::mem::offset_of!(ProtocolCmdStreamHeader, size_bytes);
const CMD_HDR_SIZE_BYTES_OFFSET: usize = core::mem::offset_of!(ProtocolCmdHdr, size_bytes);

fn push_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_f32_bits(out: &mut Vec<u8>, v: f32) {
    push_u32(out, v.to_bits());
}

fn emit_packet(out: &mut Vec<u8>, opcode: u32, payload: impl FnOnce(&mut Vec<u8>)) {
    let start = out.len();
    push_u32(out, opcode);
    push_u32(out, 0); // size_bytes placeholder
    payload(out);

    while !(out.len() - start).is_multiple_of(4) {
        out.push(0);
    }

    let size_bytes = (out.len() - start) as u32;
    out[start + CMD_HDR_SIZE_BYTES_OFFSET..start + CMD_HDR_SIZE_BYTES_OFFSET + 4]
        .copy_from_slice(&size_bytes.to_le_bytes());
}

fn build_stream(packets: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    let mut out = Vec::new();

    // aerogpu_cmd_stream_header (24 bytes)
    push_u32(&mut out, AEROGPU_CMD_STREAM_MAGIC);
    push_u32(&mut out, AEROGPU_ABI_VERSION_U32);
    push_u32(&mut out, 0); // size_bytes placeholder
    push_u32(&mut out, 0); // flags
    push_u32(&mut out, 0); // reserved0
    push_u32(&mut out, 0); // reserved1

    packets(&mut out);

    let size_bytes = out.len() as u32;
    out[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
        .copy_from_slice(&size_bytes.to_le_bytes());
    out
}

async fn create_device_queue() -> Option<(wgpu::Device, wgpu::Queue)> {
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

    let (device, queue) = adapter
        .request_device(
            &wgpu::DeviceDescriptor {
                label: Some("aerogpu executor srgb rt test device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults(),
            },
            None,
        )
        .await
        .ok()?;

    Some((device, queue))
}

#[test]
fn executor_clear_srgb_render_target_is_supported() {
    pollster::block_on(async {
        let (device, queue) = match create_device_queue().await {
            Some(v) => v,
            None => {
                common::skip_or_panic(module_path!(), "no wgpu adapter available");
                return;
            }
        };

        let mut exec = AeroGpuExecutor::new(device, queue).expect("create executor");
        let mut guest = VecGuestMemory::new(0x1000);

        const RT_HANDLE: u32 = 1;

        let stream = build_stream(|out| {
            // CREATE_TEXTURE2D sRGB render target.
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, RT_HANDLE); // texture_handle
                push_u32(out, AEROGPU_RESOURCE_USAGE_RENDER_TARGET); // usage_flags
                push_u32(out, AerogpuFormat::R8G8B8A8UnormSrgb as u32); // format
                push_u32(out, 1); // width
                push_u32(out, 1); // height
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 0); // row_pitch_bytes
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // SET_RENDER_TARGETS: color0 = RT_HANDLE.
            emit_packet(out, AerogpuCmdOpcode::SetRenderTargets as u32, |out| {
                push_u32(out, 1); // color_count
                push_u32(out, 0); // depth_stencil
                push_u32(out, RT_HANDLE); // colors[0]
                for _ in 1..8 {
                    push_u32(out, 0);
                }
            });

            // CLEAR to solid red.
            emit_packet(out, AerogpuCmdOpcode::Clear as u32, |out| {
                push_u32(out, AEROGPU_CLEAR_COLOR);
                // Clear colors are specified in linear space, so 0.5 should be encoded to roughly
                // 188 in an sRGB render target.
                push_f32_bits(out, 0.5);
                push_f32_bits(out, 0.5);
                push_f32_bits(out, 0.5);
                push_f32_bits(out, 1.0);
                push_f32_bits(out, 1.0); // depth (unused)
                push_u32(out, 0); // stencil
            });
        });

        let report = exec.process_cmd_stream(&stream, &mut guest, None);
        assert!(report.is_ok(), "executor reported errors: {report:?}");

        let rt = exec.texture(RT_HANDLE).expect("render target texture");
        let rgba = readback_rgba8(
            exec.device(),
            exec.queue(),
            rt,
            TextureRegion {
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
            },
        )
        .await;
        let px = &rgba[0..4];
        assert!(
            (185..=190).contains(&px[0]) && px[0] == px[1] && px[1] == px[2],
            "expected sRGB-encoded ~188 gray, got {px:?}"
        );
        assert_eq!(px[3], 255);
    });
}

#[test]
fn executor_samples_srgb_texture_and_decodes_texels() {
    pollster::block_on(async {
        let (device, queue) = match create_device_queue().await {
            Some(v) => v,
            None => {
                common::skip_or_panic(module_path!(), "no wgpu adapter available");
                return;
            }
        };

        let mut exec = AeroGpuExecutor::new(device, queue).expect("create executor");
        let mut guest = VecGuestMemory::new(0x1000);

        // Full-screen triangle (pos: vec2<f32>).
        let verts: [f32; 6] = [-1.0, -1.0, 3.0, -1.0, -1.0, 3.0];
        let mut vb_bytes = Vec::new();
        for v in verts {
            vb_bytes.extend_from_slice(&v.to_le_bytes());
        }

        // sRGB-encoded ~0.5 gray (188). When sampled from an sRGB texture, this should decode to
        // linear ~0.5, which then stores as ~128 in an UNORM render target.
        let srgb_gray = [188u8, 188u8, 188u8, 255u8];

        const VB_HANDLE: u32 = 1;
        const TEX_HANDLE: u32 = 2;
        const RT_HANDLE: u32 = 3;

        let stream = build_stream(|out| {
            // CREATE_BUFFER (handle=1) vertex buffer.
            emit_packet(out, AerogpuCmdOpcode::CreateBuffer as u32, |out| {
                push_u32(out, VB_HANDLE); // buffer_handle
                push_u32(out, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER); // usage_flags
                push_u64(out, vb_bytes.len() as u64); // size_bytes
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // CREATE_TEXTURE2D (handle=2) 1x1 sRGB texture (sampled).
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, TEX_HANDLE); // texture_handle
                push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE); // usage_flags
                push_u32(out, AerogpuFormat::R8G8B8A8UnormSrgb as u32); // format
                push_u32(out, 1); // width
                push_u32(out, 1); // height
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 0); // row_pitch_bytes
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });
            emit_packet(out, AerogpuCmdOpcode::UploadResource as u32, |out| {
                push_u32(out, TEX_HANDLE); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, srgb_gray.len() as u64); // size_bytes
                out.extend_from_slice(&srgb_gray);
            });

            // CREATE_TEXTURE2D (handle=3) 1x1 linear render target.
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, RT_HANDLE); // texture_handle
                push_u32(out, AEROGPU_RESOURCE_USAGE_RENDER_TARGET); // usage_flags
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32); // format
                push_u32(out, 1); // width
                push_u32(out, 1); // height
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 0); // row_pitch_bytes
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // SET_RENDER_TARGETS: color0 = RT_HANDLE.
            emit_packet(out, AerogpuCmdOpcode::SetRenderTargets as u32, |out| {
                push_u32(out, 1); // color_count
                push_u32(out, 0); // depth_stencil
                push_u32(out, RT_HANDLE); // colors[0]
                for _ in 1..8 {
                    push_u32(out, 0);
                }
            });

            // SET_TEXTURE (ps slot 0) = TEX_HANDLE.
            emit_packet(out, AerogpuCmdOpcode::SetTexture as u32, |out| {
                push_u32(out, AerogpuShaderStage::Pixel as u32);
                push_u32(out, 0); // slot
                push_u32(out, TEX_HANDLE); // texture handle
                push_u32(out, 0); // reserved0
            });

            // UPLOAD_RESOURCE vertex buffer bytes.
            emit_packet(out, AerogpuCmdOpcode::UploadResource as u32, |out| {
                push_u32(out, VB_HANDLE); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, vb_bytes.len() as u64); // size_bytes
                out.extend_from_slice(&vb_bytes);
            });

            // SET_VERTEX_BUFFERS: slot 0 = buffer 1.
            emit_packet(out, AerogpuCmdOpcode::SetVertexBuffers as u32, |out| {
                push_u32(out, 0); // start_slot
                push_u32(out, 1); // buffer_count
                push_u32(out, VB_HANDLE); // binding[0].buffer
                push_u32(out, 8); // binding[0].stride_bytes
                push_u32(out, 0); // binding[0].offset_bytes
                push_u32(out, 0); // binding[0].reserved0
            });

            // DRAW: full-screen triangle.
            emit_packet(out, AerogpuCmdOpcode::Draw as u32, |out| {
                push_u32(out, 3); // vertex_count
                push_u32(out, 1); // instance_count
                push_u32(out, 0); // first_vertex
                push_u32(out, 0); // first_instance
            });
        });

        let report = exec.process_cmd_stream(&stream, &mut guest, None);
        assert!(report.is_ok(), "executor reported errors: {report:?}");

        let rt = exec.texture(RT_HANDLE).expect("render target texture");
        let rgba = readback_rgba8(
            exec.device(),
            exec.queue(),
            rt,
            TextureRegion {
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
            },
        )
        .await;

        let px = &rgba[0..4];
        assert!(
            (120..=135).contains(&px[0]) && px[0] == px[1] && px[1] == px[2],
            "expected sRGB texel to decode to ~128 linear, got {px:?}"
        );
        assert_eq!(px[3], 255);
    });
}

#[test]
fn executor_upload_x8_srgb_forces_opaque_alpha() {
    pollster::block_on(async {
        let (device, queue) = match create_device_queue().await {
            Some(v) => v,
            None => {
                common::skip_or_panic(module_path!(), "no wgpu adapter available");
                return;
            }
        };

        let mut exec = AeroGpuExecutor::new(device, queue).expect("create executor");
        let mut guest = VecGuestMemory::new(0x1000);

        const TEX_HANDLE: u32 = 1;
        let pixel = [10u8, 20u8, 30u8, 0u8]; // alpha should be forced to 255 for X8 formats

        let stream = build_stream(|out| {
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, TEX_HANDLE); // texture_handle
                push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE); // usage_flags
                push_u32(out, AerogpuFormat::R8G8B8X8UnormSrgb as u32); // format
                push_u32(out, 1); // width
                push_u32(out, 1); // height
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 0); // row_pitch_bytes
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            emit_packet(out, AerogpuCmdOpcode::UploadResource as u32, |out| {
                push_u32(out, TEX_HANDLE); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, pixel.len() as u64); // size_bytes
                out.extend_from_slice(&pixel);
            });
        });

        let report = exec.process_cmd_stream(&stream, &mut guest, None);
        assert!(report.is_ok(), "executor reported errors: {report:?}");

        let tex = exec.texture(TEX_HANDLE).expect("texture should exist");
        let rgba = readback_rgba8(
            exec.device(),
            exec.queue(),
            tex,
            TextureRegion {
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
            },
        )
        .await;

        assert_eq!(&rgba[0..4], &[10, 20, 30, 255]);
    });
}
