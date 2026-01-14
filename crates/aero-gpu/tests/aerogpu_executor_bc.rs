#![allow(clippy::await_holding_lock)]

mod common;

use aero_gpu::aerogpu_executor::{AllocEntry, AllocTable};
use aero_gpu::{readback_buffer, readback_rgba8, GuestMemory, TextureRegion, VecGuestMemory};
use aero_protocol::aerogpu::{
    aerogpu_cmd::{
        AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode,
        AerogpuCmdStreamHeader as ProtocolCmdStreamHeader, AerogpuShaderStage,
        AEROGPU_CMD_STREAM_MAGIC, AEROGPU_COPY_FLAG_WRITEBACK_DST,
        AEROGPU_RESOURCE_USAGE_RENDER_TARGET, AEROGPU_RESOURCE_USAGE_TEXTURE,
        AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
    },
    aerogpu_pci::{AerogpuFormat, AEROGPU_ABI_VERSION_U32},
};

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

fn fullscreen_triangle_vb_bytes() -> Vec<u8> {
    // Full-screen triangle (pos: vec2<f32>).
    let verts: [f32; 6] = [-1.0, -1.0, 3.0, -1.0, -1.0, 3.0];
    let mut vb_bytes = Vec::new();
    for v in verts {
        vb_bytes.extend_from_slice(&v.to_le_bytes());
    }
    vb_bytes
}

fn build_bc_sample_stream(format: AerogpuFormat, bc_data: &[u8], vb_bytes: &[u8]) -> Vec<u8> {
    build_stream(|out| {
        // CREATE_BUFFER (handle=1) vertex buffer.
        emit_packet(out, AerogpuCmdOpcode::CreateBuffer as u32, |out| {
            push_u32(out, 1); // buffer_handle
            push_u32(out, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER); // usage_flags
            push_u64(out, vb_bytes.len() as u64); // size_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        // CREATE_TEXTURE2D (handle=2) 4x4 BC texture (sampled).
        emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
            push_u32(out, 2); // texture_handle
            push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE); // usage_flags
            push_u32(out, format as u32); // format
            push_u32(out, 4); // width
            push_u32(out, 4); // height
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, 0); // row_pitch_bytes (unused when backing_alloc_id == 0)
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        // UPLOAD_RESOURCE: BC bytes for texture 2.
        emit_packet(out, AerogpuCmdOpcode::UploadResource as u32, |out| {
            push_u32(out, 2); // resource_handle
            push_u32(out, 0); // reserved0
            push_u64(out, 0); // offset_bytes
            push_u64(out, bc_data.len() as u64); // size_bytes
            out.extend_from_slice(bc_data);
        });

        // CREATE_TEXTURE2D (handle=3) 1x1 render target.
        emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
            push_u32(out, 3); // texture_handle
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

        // SET_RENDER_TARGETS: color0 = texture 3.
        emit_packet(out, AerogpuCmdOpcode::SetRenderTargets as u32, |out| {
            push_u32(out, 1); // color_count
            push_u32(out, 0); // depth_stencil
            push_u32(out, 3); // colors[0]
            for _ in 1..8 {
                push_u32(out, 0);
            }
        });

        // CLEAR to black.
        emit_packet(out, AerogpuCmdOpcode::Clear as u32, |out| {
            push_u32(out, 1); // flags: CLEAR_COLOR
            push_f32_bits(out, 0.0);
            push_f32_bits(out, 0.0);
            push_f32_bits(out, 0.0);
            push_f32_bits(out, 1.0);
            push_f32_bits(out, 1.0); // depth (unused)
            push_u32(out, 0); // stencil
        });

        // SET_TEXTURE (ps slot 0) = texture 2.
        emit_packet(out, AerogpuCmdOpcode::SetTexture as u32, |out| {
            push_u32(out, AerogpuShaderStage::Pixel as u32);
            push_u32(out, 0); // slot
            push_u32(out, 2); // texture handle
            push_u32(out, 0); // reserved0
        });

        // UPLOAD_RESOURCE vertex buffer bytes.
        emit_packet(out, AerogpuCmdOpcode::UploadResource as u32, |out| {
            push_u32(out, 1); // resource_handle
            push_u32(out, 0); // reserved0
            push_u64(out, 0); // offset_bytes
            push_u64(out, vb_bytes.len() as u64); // size_bytes
            out.extend_from_slice(vb_bytes);
        });

        // SET_VERTEX_BUFFERS: slot 0 = buffer 1.
        emit_packet(out, AerogpuCmdOpcode::SetVertexBuffers as u32, |out| {
            push_u32(out, 0); // start_slot
            push_u32(out, 1); // buffer_count
            push_u32(out, 1); // binding[0].buffer
            push_u32(out, 8); // binding[0].stride_bytes
            push_u32(out, 0); // binding[0].offset_bytes
            push_u32(out, 0); // binding[0].reserved0
        });

        // DRAW: 3 vertices.
        emit_packet(out, AerogpuCmdOpcode::Draw as u32, |out| {
            push_u32(out, 3); // vertex_count
            push_u32(out, 1); // instance_count
            push_u32(out, 0); // first_vertex
            push_u32(out, 0); // first_instance
        });
    })
}

async fn assert_wgpu_texture_is_native_bc(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    tex: &wgpu::Texture,
) {
    // A 4x4 BC texture is a single 4x4 block, so the copy uses rows_per_image=1 (block row).
    //
    // If the executor fell back to an RGBA8 texture, `rows_per_image=1` would be interpreted as 1
    // texel-row and validation should fail because the copy height is 4 texel-rows.
    device.push_error_scope(wgpu::ErrorFilter::Validation);

    // Allocate enough room to satisfy both interpretations of the copy:
    // - RGBA8: 4 rows => at least bytes_per_row*(4-1)+16
    // - BC: 1 block-row => at least 8/16 bytes
    // (Buffer size isn't the thing we're trying to validate here.)
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("aerogpu.executor.bc.native_copy_probe"),
        size: 1024,
        usage: wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("aerogpu.executor.bc.native_copy_probe.encoder"),
    });
    encoder.copy_texture_to_buffer(
        wgpu::ImageCopyTexture {
            texture: tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::ImageCopyBuffer {
            buffer: &buffer,
            layout: wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(256),
                rows_per_image: Some(1),
            },
        },
        wgpu::Extent3d {
            width: 4,
            height: 4,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);
    #[cfg(not(target_arch = "wasm32"))]
    device.poll(wgpu::Maintain::Wait);

    let err = device.pop_error_scope().await;
    assert!(
        err.is_none(),
        "expected native BC texture, got wgpu validation error: {err:?}"
    );
}

async fn run_bc_cpu_fallback_sample_test(
    test_name: &str,
    format: AerogpuFormat,
    bc_data: &[u8],
    expected_rgba: [u8; 4],
) {
    let mut exec = match common::aerogpu_executor_or_skip(test_name) {
        Some(exec) => exec,
        None => return,
    };
    let mut guest = VecGuestMemory::new(0x1000);

    let vb_bytes = fullscreen_triangle_vb_bytes();
    let stream = build_bc_sample_stream(format, bc_data, &vb_bytes);

    let report = exec.process_cmd_stream(&stream, &mut guest, None);
    assert!(report.is_ok(), "report had errors: {:#?}", report.events);

    let rt_tex = exec.texture(3).expect("render target texture");
    let rgba = readback_rgba8(
        exec.device(),
        exec.queue(),
        rt_tex,
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
    assert_eq!(&rgba[0..4], &expected_rgba);
}

#[allow(clippy::await_holding_lock)]
async fn run_bc_direct_sample_test(
    test_name: &str,
    format: AerogpuFormat,
    bc_data: &[u8],
    expected_rgba: [u8; 4],
) {
    if env_truthy("AERO_DISABLE_WGPU_TEXTURE_COMPRESSION") {
        common::skip_or_panic(
            test_name,
            "AERO_DISABLE_WGPU_TEXTURE_COMPRESSION is set; skipping native BC path tests",
        );
        return;
    }

    let mut exec = match common::aerogpu_executor_bc_or_skip(test_name).await {
        Some(exec) => exec,
        None => return,
    };
    let mut guest = VecGuestMemory::new(0x1000);

    let vb_bytes = fullscreen_triangle_vb_bytes();
    let stream = build_bc_sample_stream(format, bc_data, &vb_bytes);

    let report = exec.process_cmd_stream(&stream, &mut guest, None);
    assert!(report.is_ok(), "report had errors: {:#?}", report.events);

    let bc_tex = exec.texture(2).expect("sampled BC texture");
    assert_wgpu_texture_is_native_bc(exec.device(), exec.queue(), bc_tex).await;

    let rt_tex = exec.texture(3).expect("render target texture");
    let rgba = readback_rgba8(
        exec.device(),
        exec.queue(),
        rt_tex,
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
    assert_eq!(&rgba[0..4], &expected_rgba);
}

#[test]
fn executor_upload_bc1_cpu_fallback_and_sample() {
    const TEST_NAME: &str = concat!(
        module_path!(),
        "::executor_upload_bc1_cpu_fallback_and_sample"
    );
    pollster::block_on(async {
        // Single BC1 block for a 4x4 texture. Make it solid red by setting
        // color0=color1=RGB565(255,0,0)=0xF800 and all indices to 0.
        let bc1_solid_red = [0x00, 0xF8, 0x00, 0xF8, 0x00, 0x00, 0x00, 0x00];
        run_bc_cpu_fallback_sample_test(
            TEST_NAME,
            AerogpuFormat::BC1RgbaUnorm,
            &bc1_solid_red,
            [255, 0, 0, 255],
        )
        .await;
    });
}

#[test]
fn executor_upload_bc1_srgb_cpu_fallback_decodes_on_sample() {
    const TEST_NAME: &str = concat!(
        module_path!(),
        "::executor_upload_bc1_srgb_cpu_fallback_decodes_on_sample"
    );
    pollster::block_on(async {
        let mut exec = match common::aerogpu_executor_or_skip(TEST_NAME) {
            Some(exec) => exec,
            None => return,
        };
        let mut guest = VecGuestMemory::new(0x1000);

        // Full-screen triangle (pos: vec2<f32>).
        let verts: [f32; 6] = [-1.0, -1.0, 3.0, -1.0, -1.0, 3.0];
        let mut vb_bytes = Vec::new();
        for v in verts {
            vb_bytes.extend_from_slice(&v.to_le_bytes());
        }

        // A single BC1 block with color0=white, color1=black, and all indices set to 2 (the 2/3
        // white entry in 4-color mode). This decompresses to an sRGB value of 170.
        //
        // When sampled from an sRGB BC1 texture, 170 should decode to linear ~0.402 and store to an
        // UNORM render target as ~103.
        let bc1_srgb_gray_170 = [
            0xff, 0xff, // color0 (white)
            0x00, 0x00, // color1 (black)
            0xaa, 0xaa, 0xaa, 0xaa, // indices: all 2
        ];

        let stream = build_stream(|out| {
            // CREATE_BUFFER (handle=1) vertex buffer.
            emit_packet(out, AerogpuCmdOpcode::CreateBuffer as u32, |out| {
                push_u32(out, 1); // buffer_handle
                push_u32(out, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER); // usage_flags
                push_u64(out, vb_bytes.len() as u64); // size_bytes
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // CREATE_TEXTURE2D (handle=2) 4x4 BC1 sRGB texture (sampled).
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 2); // texture_handle
                push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE); // usage_flags
                push_u32(out, AerogpuFormat::BC1RgbaUnormSrgb as u32); // format
                push_u32(out, 4); // width
                push_u32(out, 4); // height
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 0); // row_pitch_bytes
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // UPLOAD_RESOURCE: BC bytes for texture 2.
            emit_packet(out, AerogpuCmdOpcode::UploadResource as u32, |out| {
                push_u32(out, 2); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, bc1_srgb_gray_170.len() as u64); // size_bytes
                out.extend_from_slice(&bc1_srgb_gray_170);
            });

            // CREATE_TEXTURE2D (handle=3) 1x1 linear render target.
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 3); // texture_handle
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

            // SET_RENDER_TARGETS: color0 = texture 3.
            emit_packet(out, AerogpuCmdOpcode::SetRenderTargets as u32, |out| {
                push_u32(out, 1); // color_count
                push_u32(out, 0); // depth_stencil
                push_u32(out, 3); // colors[0]
                for _ in 1..8 {
                    push_u32(out, 0);
                }
            });

            // SET_TEXTURE (ps slot 0) = texture 2.
            emit_packet(out, AerogpuCmdOpcode::SetTexture as u32, |out| {
                push_u32(out, AerogpuShaderStage::Pixel as u32);
                push_u32(out, 0); // slot
                push_u32(out, 2); // texture handle
                push_u32(out, 0); // reserved0
            });

            // UPLOAD_RESOURCE vertex buffer bytes.
            emit_packet(out, AerogpuCmdOpcode::UploadResource as u32, |out| {
                push_u32(out, 1); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, vb_bytes.len() as u64); // size_bytes
                out.extend_from_slice(&vb_bytes);
            });

            // SET_VERTEX_BUFFERS: slot 0 = buffer 1.
            emit_packet(out, AerogpuCmdOpcode::SetVertexBuffers as u32, |out| {
                push_u32(out, 0); // start_slot
                push_u32(out, 1); // buffer_count
                push_u32(out, 1); // binding[0].buffer
                push_u32(out, 8); // binding[0].stride_bytes
                push_u32(out, 0); // binding[0].offset_bytes
                push_u32(out, 0); // binding[0].reserved0
            });

            // DRAW: 3 vertices.
            emit_packet(out, AerogpuCmdOpcode::Draw as u32, |out| {
                push_u32(out, 3); // vertex_count
                push_u32(out, 1); // instance_count
                push_u32(out, 0); // first_vertex
                push_u32(out, 0); // first_instance
            });
        });

        let report = exec.process_cmd_stream(&stream, &mut guest, None);
        assert!(report.is_ok(), "report had errors: {:#?}", report.events);

        let rt_tex = exec.texture(3).expect("render target texture");
        let rgba = readback_rgba8(
            exec.device(),
            exec.queue(),
            rt_tex,
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
            (95..=110).contains(&px[0]) && px[0] == px[1] && px[1] == px[2],
            "expected decoded ~103 gray, got {px:?}"
        );
        assert_eq!(px[3], 255);
    });
}

#[test]
fn executor_upload_bc2_srgb_cpu_fallback_decodes_on_sample() {
    const TEST_NAME: &str = concat!(
        module_path!(),
        "::executor_upload_bc2_srgb_cpu_fallback_decodes_on_sample"
    );
    pollster::block_on(async {
        let mut exec = match common::aerogpu_executor_or_skip(TEST_NAME) {
            Some(exec) => exec,
            None => return,
        };
        let mut guest = VecGuestMemory::new(0x1000);

        // Full-screen triangle (pos: vec2<f32>).
        let verts: [f32; 6] = [-1.0, -1.0, 3.0, -1.0, -1.0, 3.0];
        let mut vb_bytes = Vec::new();
        for v in verts {
            vb_bytes.extend_from_slice(&v.to_le_bytes());
        }

        // A single BC2 block with:
        // - explicit alpha set to 255 for all texels
        // - BC1-like color block: color0=white, color1=black, indices all 2 -> sRGB gray 170
        //
        // When sampled from an sRGB BC2 texture, 170 should decode to linear ~0.402 and store to an
        // UNORM render target as ~103.
        let bc2_srgb_gray_170 = [
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // alpha bits (LE u64): all 0xF
            0xff, 0xff, // color0 (white)
            0x00, 0x00, // color1 (black)
            0xaa, 0xaa, 0xaa, 0xaa, // indices: all 2
        ];

        let stream = build_stream(|out| {
            // CREATE_BUFFER (handle=1) vertex buffer.
            emit_packet(out, AerogpuCmdOpcode::CreateBuffer as u32, |out| {
                push_u32(out, 1); // buffer_handle
                push_u32(out, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER); // usage_flags
                push_u64(out, vb_bytes.len() as u64); // size_bytes
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // CREATE_TEXTURE2D (handle=2) 4x4 BC2 sRGB texture (sampled).
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 2); // texture_handle
                push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE); // usage_flags
                push_u32(out, AerogpuFormat::BC2RgbaUnormSrgb as u32); // format
                push_u32(out, 4); // width
                push_u32(out, 4); // height
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 0); // row_pitch_bytes
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // UPLOAD_RESOURCE: BC bytes for texture 2.
            emit_packet(out, AerogpuCmdOpcode::UploadResource as u32, |out| {
                push_u32(out, 2); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, bc2_srgb_gray_170.len() as u64); // size_bytes
                out.extend_from_slice(&bc2_srgb_gray_170);
            });

            // CREATE_TEXTURE2D (handle=3) 1x1 linear render target.
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 3); // texture_handle
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

            // SET_RENDER_TARGETS: color0 = texture 3.
            emit_packet(out, AerogpuCmdOpcode::SetRenderTargets as u32, |out| {
                push_u32(out, 1); // color_count
                push_u32(out, 0); // depth_stencil
                push_u32(out, 3); // colors[0]
                for _ in 1..8 {
                    push_u32(out, 0);
                }
            });

            // SET_TEXTURE (ps slot 0) = texture 2.
            emit_packet(out, AerogpuCmdOpcode::SetTexture as u32, |out| {
                push_u32(out, AerogpuShaderStage::Pixel as u32);
                push_u32(out, 0); // slot
                push_u32(out, 2); // texture handle
                push_u32(out, 0); // reserved0
            });

            // UPLOAD_RESOURCE vertex buffer bytes.
            emit_packet(out, AerogpuCmdOpcode::UploadResource as u32, |out| {
                push_u32(out, 1); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, vb_bytes.len() as u64); // size_bytes
                out.extend_from_slice(&vb_bytes);
            });

            // SET_VERTEX_BUFFERS: slot 0 = buffer 1.
            emit_packet(out, AerogpuCmdOpcode::SetVertexBuffers as u32, |out| {
                push_u32(out, 0); // start_slot
                push_u32(out, 1); // buffer_count
                push_u32(out, 1); // binding[0].buffer
                push_u32(out, 8); // binding[0].stride_bytes
                push_u32(out, 0); // binding[0].offset_bytes
                push_u32(out, 0); // binding[0].reserved0
            });

            // DRAW: 3 vertices.
            emit_packet(out, AerogpuCmdOpcode::Draw as u32, |out| {
                push_u32(out, 3); // vertex_count
                push_u32(out, 1); // instance_count
                push_u32(out, 0); // first_vertex
                push_u32(out, 0); // first_instance
            });
        });

        let report = exec.process_cmd_stream(&stream, &mut guest, None);
        assert!(report.is_ok(), "report had errors: {:#?}", report.events);

        let rt_tex = exec.texture(3).expect("render target texture");
        let rgba = readback_rgba8(
            exec.device(),
            exec.queue(),
            rt_tex,
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
            (95..=110).contains(&px[0]) && px[0] == px[1] && px[1] == px[2],
            "expected decoded ~103 gray, got {px:?}"
        );
        assert_eq!(px[3], 255);
    });
}

#[test]
fn executor_upload_bc3_srgb_cpu_fallback_decodes_on_sample() {
    const TEST_NAME: &str = concat!(
        module_path!(),
        "::executor_upload_bc3_srgb_cpu_fallback_decodes_on_sample"
    );
    pollster::block_on(async {
        let mut exec = match common::aerogpu_executor_or_skip(TEST_NAME) {
            Some(exec) => exec,
            None => return,
        };
        let mut guest = VecGuestMemory::new(0x1000);

        // Full-screen triangle (pos: vec2<f32>).
        let verts: [f32; 6] = [-1.0, -1.0, 3.0, -1.0, -1.0, 3.0];
        let mut vb_bytes = Vec::new();
        for v in verts {
            vb_bytes.extend_from_slice(&v.to_le_bytes());
        }

        // A single BC3 block with:
        // - alpha0=255, alpha1=0, indices all 0 -> alpha 255
        // - BC1-like color block: color0=white, color1=black, indices all 2 -> sRGB gray 170
        //
        // When sampled from an sRGB BC3 texture, 170 should decode to linear ~0.402 and store to an
        // UNORM render target as ~103.
        let bc3_srgb_gray_170 = [
            0xff, 0x00, // alpha0, alpha1
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // alpha indices (48-bit LE): all 0
            0xff, 0xff, // color0 (white)
            0x00, 0x00, // color1 (black)
            0xaa, 0xaa, 0xaa, 0xaa, // indices: all 2
        ];

        let stream = build_stream(|out| {
            // CREATE_BUFFER (handle=1) vertex buffer.
            emit_packet(out, AerogpuCmdOpcode::CreateBuffer as u32, |out| {
                push_u32(out, 1); // buffer_handle
                push_u32(out, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER); // usage_flags
                push_u64(out, vb_bytes.len() as u64); // size_bytes
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // CREATE_TEXTURE2D (handle=2) 4x4 BC3 sRGB texture (sampled).
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 2); // texture_handle
                push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE); // usage_flags
                push_u32(out, AerogpuFormat::BC3RgbaUnormSrgb as u32); // format
                push_u32(out, 4); // width
                push_u32(out, 4); // height
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 0); // row_pitch_bytes
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // UPLOAD_RESOURCE: BC bytes for texture 2.
            emit_packet(out, AerogpuCmdOpcode::UploadResource as u32, |out| {
                push_u32(out, 2); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, bc3_srgb_gray_170.len() as u64); // size_bytes
                out.extend_from_slice(&bc3_srgb_gray_170);
            });

            // CREATE_TEXTURE2D (handle=3) 1x1 linear render target.
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 3); // texture_handle
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

            // SET_RENDER_TARGETS: color0 = texture 3.
            emit_packet(out, AerogpuCmdOpcode::SetRenderTargets as u32, |out| {
                push_u32(out, 1); // color_count
                push_u32(out, 0); // depth_stencil
                push_u32(out, 3); // colors[0]
                for _ in 1..8 {
                    push_u32(out, 0);
                }
            });

            // SET_TEXTURE (ps slot 0) = texture 2.
            emit_packet(out, AerogpuCmdOpcode::SetTexture as u32, |out| {
                push_u32(out, AerogpuShaderStage::Pixel as u32);
                push_u32(out, 0); // slot
                push_u32(out, 2); // texture handle
                push_u32(out, 0); // reserved0
            });

            // UPLOAD_RESOURCE vertex buffer bytes.
            emit_packet(out, AerogpuCmdOpcode::UploadResource as u32, |out| {
                push_u32(out, 1); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, vb_bytes.len() as u64); // size_bytes
                out.extend_from_slice(&vb_bytes);
            });

            // SET_VERTEX_BUFFERS: slot 0 = buffer 1.
            emit_packet(out, AerogpuCmdOpcode::SetVertexBuffers as u32, |out| {
                push_u32(out, 0); // start_slot
                push_u32(out, 1); // buffer_count
                push_u32(out, 1); // binding[0].buffer
                push_u32(out, 8); // binding[0].stride_bytes
                push_u32(out, 0); // binding[0].offset_bytes
                push_u32(out, 0); // binding[0].reserved0
            });

            // DRAW: 3 vertices.
            emit_packet(out, AerogpuCmdOpcode::Draw as u32, |out| {
                push_u32(out, 3); // vertex_count
                push_u32(out, 1); // instance_count
                push_u32(out, 0); // first_vertex
                push_u32(out, 0); // first_instance
            });
        });

        let report = exec.process_cmd_stream(&stream, &mut guest, None);
        assert!(report.is_ok(), "report had errors: {:#?}", report.events);

        let rt_tex = exec.texture(3).expect("render target texture");
        let rgba = readback_rgba8(
            exec.device(),
            exec.queue(),
            rt_tex,
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
            (95..=110).contains(&px[0]) && px[0] == px[1] && px[1] == px[2],
            "expected decoded ~103 gray, got {px:?}"
        );
        assert_eq!(px[3], 255);
    });
}

#[test]
fn executor_upload_bc7_srgb_cpu_fallback_decodes_on_sample() {
    const TEST_NAME: &str = concat!(
        module_path!(),
        "::executor_upload_bc7_srgb_cpu_fallback_decodes_on_sample"
    );
    pollster::block_on(async {
        let mut exec = match common::aerogpu_executor_or_skip(TEST_NAME) {
            Some(exec) => exec,
            None => return,
        };
        let mut guest = VecGuestMemory::new(0x1000);

        // Full-screen triangle (pos: vec2<f32>).
        let verts: [f32; 6] = [-1.0, -1.0, 3.0, -1.0, -1.0, 3.0];
        let mut vb_bytes = Vec::new();
        for v in verts {
            vb_bytes.extend_from_slice(&v.to_le_bytes());
        }

        // A single BC7 block where texel (0,0) decodes to sRGB gray 170.
        //
        // When sampled from an sRGB BC7 texture, 170 should decode to linear ~0.402 and store to an
        // UNORM render target as ~103.
        let bc7_srgb_texel00_gray_170: [u8; 16] = [
            0x0d, 0x9b, 0x60, 0x7f, 0x13, 0x62, 0x68, 0x33, 0x8f, 0xde, 0x1d, 0x56, 0x21, 0xdd,
            0x30, 0xc1,
        ];
        let decompressed = aero_gpu::decompress_bc7_rgba8(4, 4, &bc7_srgb_texel00_gray_170);
        assert_eq!(&decompressed[0..4], &[170, 170, 170, 255]);

        let stream = build_stream(|out| {
            // CREATE_BUFFER (handle=1) vertex buffer.
            emit_packet(out, AerogpuCmdOpcode::CreateBuffer as u32, |out| {
                push_u32(out, 1); // buffer_handle
                push_u32(out, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER); // usage_flags
                push_u64(out, vb_bytes.len() as u64); // size_bytes
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // CREATE_TEXTURE2D (handle=2) 4x4 BC7 sRGB texture (sampled).
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 2); // texture_handle
                push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE); // usage_flags
                push_u32(out, AerogpuFormat::BC7RgbaUnormSrgb as u32); // format
                push_u32(out, 4); // width
                push_u32(out, 4); // height
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 0); // row_pitch_bytes
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // UPLOAD_RESOURCE: BC bytes for texture 2.
            emit_packet(out, AerogpuCmdOpcode::UploadResource as u32, |out| {
                push_u32(out, 2); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, bc7_srgb_texel00_gray_170.len() as u64); // size_bytes
                out.extend_from_slice(&bc7_srgb_texel00_gray_170);
            });

            // CREATE_TEXTURE2D (handle=3) 1x1 linear render target.
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 3); // texture_handle
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

            // SET_RENDER_TARGETS: color0 = texture 3.
            emit_packet(out, AerogpuCmdOpcode::SetRenderTargets as u32, |out| {
                push_u32(out, 1); // color_count
                push_u32(out, 0); // depth_stencil
                push_u32(out, 3); // colors[0]
                for _ in 1..8 {
                    push_u32(out, 0);
                }
            });

            // SET_TEXTURE (ps slot 0) = texture 2.
            emit_packet(out, AerogpuCmdOpcode::SetTexture as u32, |out| {
                push_u32(out, AerogpuShaderStage::Pixel as u32);
                push_u32(out, 0); // slot
                push_u32(out, 2); // texture handle
                push_u32(out, 0); // reserved0
            });

            // UPLOAD_RESOURCE vertex buffer bytes.
            emit_packet(out, AerogpuCmdOpcode::UploadResource as u32, |out| {
                push_u32(out, 1); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, vb_bytes.len() as u64); // size_bytes
                out.extend_from_slice(&vb_bytes);
            });

            // SET_VERTEX_BUFFERS: slot 0 = buffer 1.
            emit_packet(out, AerogpuCmdOpcode::SetVertexBuffers as u32, |out| {
                push_u32(out, 0); // start_slot
                push_u32(out, 1); // buffer_count
                push_u32(out, 1); // binding[0].buffer
                push_u32(out, 8); // binding[0].stride_bytes
                push_u32(out, 0); // binding[0].offset_bytes
                push_u32(out, 0); // binding[0].reserved0
            });

            // DRAW: 3 vertices.
            emit_packet(out, AerogpuCmdOpcode::Draw as u32, |out| {
                push_u32(out, 3); // vertex_count
                push_u32(out, 1); // instance_count
                push_u32(out, 0); // first_vertex
                push_u32(out, 0); // first_instance
            });
        });

        let report = exec.process_cmd_stream(&stream, &mut guest, None);
        assert!(report.is_ok(), "report had errors: {:#?}", report.events);

        let rt_tex = exec.texture(3).expect("render target texture");
        let rgba = readback_rgba8(
            exec.device(),
            exec.queue(),
            rt_tex,
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
            (95..=110).contains(&px[0]) && px[0] == px[1] && px[1] == px[2],
            "expected decoded ~103 gray, got {px:?}"
        );
        assert_eq!(px[3], 255);
    });
}

#[test]
fn executor_upload_bc1_direct_and_sample() {
    const TEST_NAME: &str = concat!(module_path!(), "::executor_upload_bc1_direct_and_sample");
    pollster::block_on(async {
        // Single BC1 block for a 4x4 texture. Make it solid red by setting
        // color0=color1=RGB565(255,0,0)=0xF800 and all indices to 0.
        let bc1_solid_red = [0x00, 0xF8, 0x00, 0xF8, 0x00, 0x00, 0x00, 0x00];
        let decompressed = aero_gpu::decompress_bc1_rgba8(4, 4, &bc1_solid_red);
        let expected_rgba: [u8; 4] = decompressed[0..4].try_into().unwrap();

        // Ensure the block is truly solid so our 1x1 sample result is stable.
        for px in decompressed.chunks_exact(4) {
            assert_eq!(px, &expected_rgba);
        }
        assert_ne!(expected_rgba, [0, 0, 0, 255]);

        run_bc_direct_sample_test(
            TEST_NAME,
            AerogpuFormat::BC1RgbaUnorm,
            &bc1_solid_red,
            expected_rgba,
        )
        .await;
    });
}

#[test]
fn executor_upload_bc2_cpu_fallback_and_sample() {
    const TEST_NAME: &str = concat!(
        module_path!(),
        "::executor_upload_bc2_cpu_fallback_and_sample"
    );
    pollster::block_on(async {
        // BC2 / DXT3 block for a 4x4 texture.
        // Alpha block: explicit 4-bit alpha with all texels set to 0xF (255).
        // Color: solid red with color0=color1=RGB565(255,0,0)=0xF800 and all indices 0.
        let bc2_solid_red = [
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // alpha (64 bits)
            0x00, 0xf8, // color0
            0x00, 0xf8, // color1
            0x00, 0x00, 0x00, 0x00, // indices
        ];
        run_bc_cpu_fallback_sample_test(
            TEST_NAME,
            AerogpuFormat::BC2RgbaUnorm,
            &bc2_solid_red,
            [255, 0, 0, 255],
        )
        .await;
    });
}

#[test]
fn executor_upload_bc2_direct_and_sample() {
    const TEST_NAME: &str = concat!(module_path!(), "::executor_upload_bc2_direct_and_sample");
    pollster::block_on(async {
        // BC2 / DXT3 block for a 4x4 texture.
        // Alpha block: explicit 4-bit alpha with all texels set to 0xF (255).
        // Color: solid red with color0=color1=RGB565(255,0,0)=0xF800 and all indices 0.
        let bc2_solid_red = [
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // alpha (64 bits)
            0x00, 0xf8, // color0
            0x00, 0xf8, // color1
            0x00, 0x00, 0x00, 0x00, // indices
        ];
        let decompressed = aero_gpu::decompress_bc2_rgba8(4, 4, &bc2_solid_red);
        let expected_rgba: [u8; 4] = decompressed[0..4].try_into().unwrap();
        for px in decompressed.chunks_exact(4) {
            assert_eq!(px, &expected_rgba);
        }
        assert_ne!(expected_rgba, [0, 0, 0, 255]);

        run_bc_direct_sample_test(
            TEST_NAME,
            AerogpuFormat::BC2RgbaUnorm,
            &bc2_solid_red,
            expected_rgba,
        )
        .await;
    });
}

#[test]
fn executor_upload_bc3_cpu_fallback_and_sample() {
    const TEST_NAME: &str = concat!(
        module_path!(),
        "::executor_upload_bc3_cpu_fallback_and_sample"
    );
    pollster::block_on(async {
        // BC3 / DXT5 block for a 4x4 texture.
        // Alpha: alpha0=255, alpha1=0, indices all 0 -> alpha 255.
        // Color: solid red with color0=color1=RGB565(255,0,0)=0xF800 and all indices 0.
        let bc3_solid_red = [
            0xff, 0x00, // alpha0, alpha1
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // alpha indices (48-bit LE)
            0x00, 0xf8, // color0
            0x00, 0xf8, // color1
            0x00, 0x00, 0x00, 0x00, // color indices
        ];
        run_bc_cpu_fallback_sample_test(
            TEST_NAME,
            AerogpuFormat::BC3RgbaUnorm,
            &bc3_solid_red,
            [255, 0, 0, 255],
        )
        .await;
    });
}

#[test]
fn executor_upload_bc3_direct_and_sample() {
    const TEST_NAME: &str = concat!(module_path!(), "::executor_upload_bc3_direct_and_sample");
    pollster::block_on(async {
        // BC3 / DXT5 block for a 4x4 texture.
        // Alpha: alpha0=255, alpha1=0, indices all 0 -> alpha 255.
        // Color: solid red with color0=color1=RGB565(255,0,0)=0xF800 and all indices 0.
        let bc3_solid_red = [
            0xff, 0x00, // alpha0, alpha1
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // alpha indices (48-bit LE)
            0x00, 0xf8, // color0
            0x00, 0xf8, // color1
            0x00, 0x00, 0x00, 0x00, // color indices
        ];
        let decompressed = aero_gpu::decompress_bc3_rgba8(4, 4, &bc3_solid_red);
        let expected_rgba: [u8; 4] = decompressed[0..4].try_into().unwrap();
        for px in decompressed.chunks_exact(4) {
            assert_eq!(px, &expected_rgba);
        }
        assert_ne!(expected_rgba, [0, 0, 0, 255]);

        run_bc_direct_sample_test(
            TEST_NAME,
            AerogpuFormat::BC3RgbaUnorm,
            &bc3_solid_red,
            expected_rgba,
        )
        .await;
    });
}

#[test]
fn executor_upload_bc7_cpu_fallback_and_sample() {
    const TEST_NAME: &str = concat!(
        module_path!(),
        "::executor_upload_bc7_cpu_fallback_and_sample"
    );
    pollster::block_on(async {
        // Single BC7 block for a 4x4 texture. Use a simple known vector that decodes to a solid
        // color, so the executor's fixed sampling UV (0.5, 0.5) is deterministic across backends.
        let bc7_solid = [0xffu8; 16];
        let decompressed = aero_gpu::decompress_bc7_rgba8(4, 4, &bc7_solid);
        let expected_rgba: [u8; 4] = decompressed[0..4].try_into().unwrap();

        // Ensure the block is truly solid so our 1x1 sample result is stable.
        for px in decompressed.chunks_exact(4) {
            assert_eq!(px, &expected_rgba);
        }
        // Also ensure the chosen block differs from the clear color, so the draw is observable.
        assert_ne!(expected_rgba, [0, 0, 0, 255]);

        run_bc_cpu_fallback_sample_test(
            TEST_NAME,
            AerogpuFormat::BC7RgbaUnorm,
            &bc7_solid,
            expected_rgba,
        )
        .await;
    });
}

#[test]
fn executor_upload_bc7_direct_and_sample() {
    const TEST_NAME: &str = concat!(module_path!(), "::executor_upload_bc7_direct_and_sample");
    pollster::block_on(async {
        // Single BC7 block for a 4x4 texture. Use a simple known vector that decodes to a solid
        // color, so the executor's fixed sampling UV (0.5, 0.5) is deterministic across backends.
        let bc7_solid = [0xffu8; 16];
        let decompressed = aero_gpu::decompress_bc7_rgba8(4, 4, &bc7_solid);
        let expected_rgba: [u8; 4] = decompressed[0..4].try_into().unwrap();

        // Ensure the block is truly solid so our 1x1 sample result is stable.
        for px in decompressed.chunks_exact(4) {
            assert_eq!(px, &expected_rgba);
        }
        // Also ensure the chosen block differs from the clear color, so the draw is observable.
        assert_ne!(expected_rgba, [0, 0, 0, 255]);

        run_bc_direct_sample_test(
            TEST_NAME,
            AerogpuFormat::BC7RgbaUnorm,
            &bc7_solid,
            expected_rgba,
        )
        .await;
    });
}

#[test]
#[allow(clippy::await_holding_lock)]
fn executor_bc_non_multiple_dimensions_use_physical_copy_extents() {
    const TEST_NAME: &str = concat!(
        module_path!(),
        "::executor_bc_non_multiple_dimensions_use_physical_copy_extents"
    );
    pollster::block_on(async {
        if env_truthy("AERO_DISABLE_WGPU_TEXTURE_COMPRESSION") {
            common::skip_or_panic(
                TEST_NAME,
                "AERO_DISABLE_WGPU_TEXTURE_COMPRESSION is set; skipping native BC path tests",
            );
            return;
        }

        let mut exec = match common::aerogpu_executor_bc_or_skip(TEST_NAME).await {
            Some(exec) => exec,
            None => return,
        };
        let mut guest = VecGuestMemory::new(0x1000);

        let stream = build_stream(|out| {
            // CREATE_TEXTURE2D src (handle=1) BC1 4x4 with mip1 = 2x2.
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 1);
                push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE);
                push_u32(out, AerogpuFormat::BC1RgbaUnorm as u32);
                push_u32(out, 4); // width
                push_u32(out, 4); // height
                push_u32(out, 2); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 0); // row_pitch_bytes
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // CREATE_TEXTURE2D dst (handle=2) BC1 4x4 with mip1 = 2x2.
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 2);
                push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE);
                push_u32(out, AerogpuFormat::BC1RgbaUnorm as u32);
                push_u32(out, 4); // width
                push_u32(out, 4); // height
                push_u32(out, 2); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 0); // row_pitch_bytes
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // COPY_TEXTURE2D: copy the full mip1 (2x2 logical). wgpu/WebGPU validation expects the
            // copy extent to be the physical block-rounded size (4x4 for BC1).
            emit_packet(out, AerogpuCmdOpcode::CopyTexture2d as u32, |out| {
                push_u32(out, 2); // dst_texture
                push_u32(out, 1); // src_texture
                push_u32(out, 1); // dst_mip_level
                push_u32(out, 0); // dst_array_layer
                push_u32(out, 1); // src_mip_level
                push_u32(out, 0); // src_array_layer
                push_u32(out, 0); // dst_x
                push_u32(out, 0); // dst_y
                push_u32(out, 0); // src_x
                push_u32(out, 0); // src_y
                push_u32(out, 2); // width (logical mip1)
                push_u32(out, 2); // height (logical mip1)
                push_u32(out, 0); // flags
                push_u32(out, 0); // reserved0
            });
        });

        exec.device()
            .push_error_scope(wgpu::ErrorFilter::Validation);
        let report = exec.process_cmd_stream(&stream, &mut guest, None);
        assert!(report.is_ok(), "report had errors: {:#?}", report.events);

        #[cfg(not(target_arch = "wasm32"))]
        exec.device().poll(wgpu::Maintain::Wait);
        #[cfg(target_arch = "wasm32")]
        exec.device().poll(wgpu::Maintain::Poll);

        let err = exec.device().pop_error_scope().await;
        assert!(
            err.is_none(),
            "expected no wgpu validation error for BC mip copies with non-multiple dimensions, got {err:?}"
        );

        // Ensure we actually exercised the native BC path; otherwise the copy wouldn't require any
        // block padding.
        let bc_tex = exec.texture(1).expect("BC source texture");
        assert_wgpu_texture_is_native_bc(exec.device(), exec.queue(), bc_tex).await;
    });
}

#[test]
#[allow(clippy::await_holding_lock)]
fn executor_bc_writeback_uses_physical_copy_extents() {
    const TEST_NAME: &str = concat!(
        module_path!(),
        "::executor_bc_writeback_uses_physical_copy_extents"
    );
    pollster::block_on(async {
        if env_truthy("AERO_DISABLE_WGPU_TEXTURE_COMPRESSION") {
            common::skip_or_panic(
                TEST_NAME,
                "AERO_DISABLE_WGPU_TEXTURE_COMPRESSION is set; skipping native BC path tests",
            );
            return;
        }

        let mut exec = match common::aerogpu_executor_bc_or_skip(TEST_NAME).await {
            Some(exec) => exec,
            None => return,
        };

        const TEX_GPA: u64 = 0x1000;
        const TEX_ALLOC_ID: u32 = 1;

        let mip0_size_bytes: usize = 8; // 4x4 BC1 mip0 = 1 block
        let mip1_size_bytes: usize = 8; // 2x2 BC1 mip1 = 1 block
        let backing_size_bytes: usize = mip0_size_bytes + mip1_size_bytes;

        let mut guest = VecGuestMemory::new(0x4000);
        // Fill with a known value so the test can detect incorrect writeback offsets.
        guest
            .write(TEX_GPA, &vec![0xAAu8; backing_size_bytes])
            .unwrap();

        let mip0_block = [0u8; 8];
        let mip1_block = [0x5Au8; 8];
        let mut src_backing = Vec::new();
        src_backing.extend_from_slice(&mip0_block);
        src_backing.extend_from_slice(&mip1_block);

        const SRC_GPA: u64 = 0x2000;
        const SRC_ALLOC_ID: u32 = 2;
        guest.write(SRC_GPA, &src_backing).unwrap();

        let alloc_table = AllocTable::new([
            (
                TEX_ALLOC_ID,
                AllocEntry {
                    flags: 0,
                    gpa: TEX_GPA,
                    size_bytes: backing_size_bytes as u64,
                },
            ),
            (
                SRC_ALLOC_ID,
                AllocEntry {
                    flags: 0,
                    gpa: SRC_GPA,
                    size_bytes: src_backing.len() as u64,
                },
            ),
        ])
        .expect("alloc table");

        let stream = build_stream(|out| {
            // CREATE_TEXTURE2D dst (handle=1) BC1 4x4 with mip1=2x2, guest-backed.
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 1); // texture_handle
                push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE); // usage_flags
                push_u32(out, AerogpuFormat::BC1RgbaUnorm as u32); // format
                push_u32(out, 4); // width
                push_u32(out, 4); // height
                push_u32(out, 2); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, mip0_size_bytes as u32); // row_pitch_bytes (mip0 only)
                push_u32(out, TEX_ALLOC_ID); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // CREATE_TEXTURE2D src (handle=2) BC1 4x4 with mip1=2x2, guest-backed.
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 2); // texture_handle
                push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE); // usage_flags
                push_u32(out, AerogpuFormat::BC1RgbaUnorm as u32); // format
                push_u32(out, 4); // width
                push_u32(out, 4); // height
                push_u32(out, 2); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, mip0_size_bytes as u32); // row_pitch_bytes (mip0 only)
                push_u32(out, SRC_ALLOC_ID); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // RESOURCE_DIRTY_RANGE: mark the src mip1 as dirty so it is uploaded from guest memory
            // before the copy reads it.
            emit_packet(out, AerogpuCmdOpcode::ResourceDirtyRange as u32, |out| {
                push_u32(out, 2); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, mip0_size_bytes as u64); // offset_bytes (mip1 offset)
                push_u64(out, mip1_size_bytes as u64); // size_bytes
            });

            // COPY_TEXTURE2D: copy the full mip1 (2x2 logical) and write it back to guest memory.
            emit_packet(out, AerogpuCmdOpcode::CopyTexture2d as u32, |out| {
                push_u32(out, 1); // dst_texture
                push_u32(out, 2); // src_texture
                push_u32(out, 1); // dst_mip_level
                push_u32(out, 0); // dst_array_layer
                push_u32(out, 1); // src_mip_level
                push_u32(out, 0); // src_array_layer
                push_u32(out, 0); // dst_x
                push_u32(out, 0); // dst_y
                push_u32(out, 0); // src_x
                push_u32(out, 0); // src_y
                push_u32(out, 2); // width (logical mip1)
                push_u32(out, 2); // height (logical mip1)
                push_u32(out, AEROGPU_COPY_FLAG_WRITEBACK_DST); // flags
                push_u32(out, 0); // reserved0
            });
        });

        exec.device()
            .push_error_scope(wgpu::ErrorFilter::Validation);
        let report = exec.process_cmd_stream(&stream, &mut guest, Some(&alloc_table));
        assert!(report.is_ok(), "report had errors: {:#?}", report.events);

        #[cfg(not(target_arch = "wasm32"))]
        exec.device().poll(wgpu::Maintain::Wait);
        #[cfg(target_arch = "wasm32")]
        exec.device().poll(wgpu::Maintain::Poll);

        let err = exec.device().pop_error_scope().await;
        assert!(
            err.is_none(),
            "expected no wgpu validation error for BC mip writeback with non-multiple dimensions, got {err:?}"
        );

        // Ensure we actually exercised the native BC path; otherwise neither the copy nor writeback
        // would require block rounding.
        let dst_tex = exec.texture(1).expect("BC dst texture");
        assert_wgpu_texture_is_native_bc(exec.device(), exec.queue(), dst_tex).await;

        // mip0 should be unchanged, while mip1 should reflect the written back BC block.
        let mut mip0 = vec![0u8; mip0_size_bytes];
        guest.read(TEX_GPA, &mut mip0).unwrap();
        assert_eq!(mip0, vec![0xAAu8; mip0_size_bytes]);

        let mut mip1 = vec![0u8; mip1_size_bytes];
        guest
            .read(TEX_GPA + mip0_size_bytes as u64, &mut mip1)
            .unwrap();
        assert_eq!(mip1, mip1_block);
    });
}

#[test]
#[allow(clippy::await_holding_lock)]
fn executor_bc_dirty_range_upload_pads_small_mips() {
    const TEST_NAME: &str = concat!(
        module_path!(),
        "::executor_bc_dirty_range_upload_pads_small_mips"
    );
    pollster::block_on(async {
        if env_truthy("AERO_DISABLE_WGPU_TEXTURE_COMPRESSION") {
            common::skip_or_panic(
                TEST_NAME,
                "AERO_DISABLE_WGPU_TEXTURE_COMPRESSION is set; skipping native BC path tests",
            );
            return;
        }

        let mut exec = match common::aerogpu_executor_bc_or_skip(TEST_NAME).await {
            Some(exec) => exec,
            None => return,
        };

        const TEX_GPA: u64 = 0x1000;
        const TEX_ALLOC_ID: u32 = 1;

        // BC1 4x4 mip chain (mip0 4x4, mip1 2x2): 1 block each.
        let mip0_block = [0xAAu8; 8];
        let mip1_block = [0x5Au8; 8];
        let mut backing_bytes = Vec::new();
        backing_bytes.extend_from_slice(&mip0_block);
        backing_bytes.extend_from_slice(&mip1_block);

        let mut guest = VecGuestMemory::new(0x4000);
        guest.write(TEX_GPA, &backing_bytes).unwrap();

        let alloc_table = AllocTable::new([(
            TEX_ALLOC_ID,
            AllocEntry {
                flags: 0,
                gpa: TEX_GPA,
                size_bytes: backing_bytes.len() as u64,
            },
        )])
        .expect("alloc table");

        let stream = build_stream(|out| {
            // CREATE_TEXTURE2D src (handle=1) BC1 4x4 with mip1=2x2, guest-backed.
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 1); // texture_handle
                push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE); // usage_flags
                push_u32(out, AerogpuFormat::BC1RgbaUnorm as u32); // format
                push_u32(out, 4); // width
                push_u32(out, 4); // height
                push_u32(out, 2); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 8); // row_pitch_bytes (mip0 block row)
                push_u32(out, TEX_ALLOC_ID); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // CREATE_TEXTURE2D dst (handle=2) BC1 4x4 with mip1=2x2, host-owned.
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 2); // texture_handle
                push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE); // usage_flags
                push_u32(out, AerogpuFormat::BC1RgbaUnorm as u32); // format
                push_u32(out, 4); // width
                push_u32(out, 4); // height
                push_u32(out, 2); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 0); // row_pitch_bytes (unused when backing_alloc_id == 0)
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // RESOURCE_DIRTY_RANGE: mark mip1 as dirty, so the executor must upload it from guest
            // memory using a 4x4 physical copy extent (even though the mip is 2x2 logical).
            emit_packet(out, AerogpuCmdOpcode::ResourceDirtyRange as u32, |out| {
                push_u32(out, 1); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, 8); // offset_bytes (mip1 offset)
                push_u64(out, 8); // size_bytes (one BC1 block)
            });

            // COPY_TEXTURE2D: copy the full mip1 (2x2 logical). This forces the executor to flush
            // the dirty guest-backed mip before issuing the copy.
            emit_packet(out, AerogpuCmdOpcode::CopyTexture2d as u32, |out| {
                push_u32(out, 2); // dst_texture
                push_u32(out, 1); // src_texture
                push_u32(out, 1); // dst_mip_level
                push_u32(out, 0); // dst_array_layer
                push_u32(out, 1); // src_mip_level
                push_u32(out, 0); // src_array_layer
                push_u32(out, 0); // dst_x
                push_u32(out, 0); // dst_y
                push_u32(out, 0); // src_x
                push_u32(out, 0); // src_y
                push_u32(out, 2); // width (logical mip1)
                push_u32(out, 2); // height (logical mip1)
                push_u32(out, 0); // flags
                push_u32(out, 0); // reserved0
            });
        });

        exec.device()
            .push_error_scope(wgpu::ErrorFilter::Validation);
        let report = exec.process_cmd_stream(&stream, &mut guest, Some(&alloc_table));
        assert!(report.is_ok(), "report had errors: {:#?}", report.events);

        #[cfg(not(target_arch = "wasm32"))]
        exec.device().poll(wgpu::Maintain::Wait);
        #[cfg(target_arch = "wasm32")]
        exec.device().poll(wgpu::Maintain::Poll);

        let err = exec.device().pop_error_scope().await;
        assert!(
            err.is_none(),
            "expected no wgpu validation error for BC mip dirty-range uploads with non-multiple dimensions, got {err:?}"
        );

        // Ensure we actually exercised the native BC path; otherwise neither the upload nor copy
        // would require block rounding.
        let src_tex = exec.texture(1).expect("BC src texture");
        assert_wgpu_texture_is_native_bc(exec.device(), exec.queue(), src_tex).await;

        // Read back dst mip1 and verify that the copied block matches the guest backing.
        let dst_tex = exec.texture(2).expect("BC dst texture");
        let buffer = exec.device().create_buffer(&wgpu::BufferDescriptor {
            label: Some("aerogpu.executor.bc.dirty_range_mip1_readback"),
            size: 256, // aligned bytes_per_row * 1 block row
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = exec
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aerogpu.executor.bc.dirty_range_mip1_readback.encoder"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture: dst_tex,
                mip_level: 1,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &buffer,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: Some(1),
                },
            },
            wgpu::Extent3d {
                width: 4,
                height: 4,
                depth_or_array_layers: 1,
            },
        );
        exec.queue().submit([encoder.finish()]);

        let readback = readback_buffer(exec.device(), &buffer, 0..8).await;
        assert_eq!(readback, mip1_block);
    });
}

#[test]
fn executor_rejects_misaligned_bc_copy_region() {
    pollster::block_on(async {
        let mut exec = match common::aerogpu_executor_or_skip(module_path!()) {
            Some(exec) => exec,
            None => return,
        };
        let mut guest = VecGuestMemory::new(0x1000);

        let stream = build_stream(|out| {
            // CREATE_TEXTURE2D src (handle=1) BC1.
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 1);
                push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE);
                push_u32(out, AerogpuFormat::BC1RgbaUnorm as u32);
                push_u32(out, 8);
                push_u32(out, 8);
                push_u32(out, 1);
                push_u32(out, 1);
                push_u32(out, 0);
                push_u32(out, 0);
                push_u32(out, 0);
                push_u64(out, 0);
            });

            // CREATE_TEXTURE2D dst (handle=2) BC1.
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 2);
                push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE);
                push_u32(out, AerogpuFormat::BC1RgbaUnorm as u32);
                push_u32(out, 8);
                push_u32(out, 8);
                push_u32(out, 1);
                push_u32(out, 1);
                push_u32(out, 0);
                push_u32(out, 0);
                push_u32(out, 0);
                push_u64(out, 0);
            });

            // COPY_TEXTURE2D: misaligned src_x=2 (must be multiple of 4 for BC).
            emit_packet(out, AerogpuCmdOpcode::CopyTexture2d as u32, |out| {
                push_u32(out, 2); // dst_texture
                push_u32(out, 1); // src_texture
                push_u32(out, 0); // dst_mip_level
                push_u32(out, 0); // dst_array_layer
                push_u32(out, 0); // src_mip_level
                push_u32(out, 0); // src_array_layer
                push_u32(out, 0); // dst_x
                push_u32(out, 0); // dst_y
                push_u32(out, 2); // src_x (misaligned)
                push_u32(out, 0); // src_y
                push_u32(out, 4); // width
                push_u32(out, 4); // height
                push_u32(out, 0); // flags
                push_u32(out, 0); // reserved0
            });
        });

        let report = exec.process_cmd_stream(&stream, &mut guest, None);
        assert!(
            !report.is_ok(),
            "expected error for misaligned BC copy, got ok report"
        );
        let msg = match report.events.first() {
            Some(aero_gpu::aerogpu_executor::ExecutorEvent::Error { message, .. }) => message,
            other => panic!("expected error event, got {other:?}"),
        };
        assert!(
            msg.contains("BC origin") || msg.contains("BC width") || msg.contains("BC height"),
            "unexpected error message: {msg}"
        );
    });
}

#[test]
fn executor_falls_back_for_unaligned_bc_texture_dimensions_even_when_bc_is_enabled() {
    const TEST_NAME: &str = concat!(
        module_path!(),
        "::executor_falls_back_for_unaligned_bc_texture_dimensions_even_when_bc_is_enabled"
    );

    pollster::block_on(async {
        if env_truthy("AERO_DISABLE_WGPU_TEXTURE_COMPRESSION") {
            common::skip_or_panic(
                TEST_NAME,
                "AERO_DISABLE_WGPU_TEXTURE_COMPRESSION is set; skipping BC-enabled test",
            );
            return;
        }

        let mut exec = match common::aerogpu_executor_bc_or_skip(TEST_NAME).await {
            Some(exec) => exec,
            None => return,
        };
        let mut guest = VecGuestMemory::new(0x1000);

        // Regression: wgpu/WebGPU rejects BC textures unless the base dimensions are multiples of
        // the 4x4 block size. Creating a 9x9 BC1 texture must not panic; the executor should
        // transparently fall back to an RGBA8 texture + CPU decompression.
        let stream = build_stream(|out| {
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 1); // texture_handle
                push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE); // usage_flags
                push_u32(out, AerogpuFormat::BC1RgbaUnorm as u32); // format
                push_u32(out, 9); // width (not block-aligned)
                push_u32(out, 9); // height (not block-aligned)
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 0); // row_pitch_bytes
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });
        });

        let report = exec.process_cmd_stream(&stream, &mut guest, None);
        assert!(report.is_ok(), "report had errors: {:#?}", report.events);
    });
}
