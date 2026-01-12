mod common;

use aero_gpu::{AerogpuD3d9Error, AerogpuD3d9Executor};
use aero_protocol::aerogpu::{
    aerogpu_cmd::{
        AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode,
        AerogpuCmdStreamHeader as ProtocolCmdStreamHeader, AerogpuPrimitiveTopology,
        AEROGPU_CLEAR_COLOR, AEROGPU_CLEAR_DEPTH, AEROGPU_CLEAR_STENCIL, AEROGPU_CMD_STREAM_MAGIC,
        AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL, AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
        AEROGPU_RESOURCE_USAGE_TEXTURE, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
    },
    aerogpu_pci::{AerogpuFormat, AEROGPU_ABI_VERSION_U32},
};

const CMD_STREAM_SIZE_BYTES_OFFSET: usize =
    core::mem::offset_of!(ProtocolCmdStreamHeader, size_bytes);
const CMD_HDR_SIZE_BYTES_OFFSET: usize = core::mem::offset_of!(ProtocolCmdHdr, size_bytes);

// D3D9 render state IDs (subset).
const D3DRS_SCISSORTESTENABLE: u32 = 174;
const D3DRS_SRGBWRITEENABLE: u32 = 194;
const D3DRS_ZENABLE: u32 = 7;
const D3DRS_ZWRITEENABLE: u32 = 14;
const D3DRS_ZFUNC: u32 = 23;

const D3DCMP_LESSEQUAL: u32 = 4;
const D3DCMP_GREATER: u32 = 5;

fn push_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_i32(out: &mut Vec<u8>, v: i32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_f32(out: &mut Vec<u8>, v: f32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_u8(out: &mut Vec<u8>, v: u8) {
    out.push(v);
}

fn align4(v: usize) -> usize {
    (v + 3) & !3
}

fn build_stream(packets: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    let mut out = Vec::new();

    // aerogpu_cmd_stream_header (24 bytes)
    push_u32(&mut out, AEROGPU_CMD_STREAM_MAGIC);
    push_u32(&mut out, AEROGPU_ABI_VERSION_U32);
    push_u32(&mut out, 0); // size_bytes (patch later)
    push_u32(&mut out, 0); // flags
    push_u32(&mut out, 0); // reserved0
    push_u32(&mut out, 0); // reserved1

    packets(&mut out);

    let size_bytes = out.len() as u32;
    out[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
        .copy_from_slice(&size_bytes.to_le_bytes());
    out
}

fn emit_packet(out: &mut Vec<u8>, opcode: u32, payload: impl FnOnce(&mut Vec<u8>)) {
    let start = out.len();
    push_u32(out, opcode);
    push_u32(out, 0); // size_bytes placeholder
    payload(out);
    let end_aligned = align4(out.len());
    out.resize(end_aligned, 0);
    let size_bytes = (end_aligned - start) as u32;
    out[start + CMD_HDR_SIZE_BYTES_OFFSET..start + CMD_HDR_SIZE_BYTES_OFFSET + 4]
        .copy_from_slice(&size_bytes.to_le_bytes());
}

fn enc_reg_type(ty: u8) -> u32 {
    let low = (ty & 0x7) as u32;
    let high = (ty & 0x18) as u32;
    (low << 28) | (high << 8)
}

fn enc_src(reg_type: u8, reg_num: u16, swizzle: u8) -> u32 {
    enc_reg_type(reg_type) | (reg_num as u32) | ((swizzle as u32) << 16)
}

fn enc_dst(reg_type: u8, reg_num: u16, mask: u8) -> u32 {
    enc_reg_type(reg_type) | (reg_num as u32) | ((mask as u32) << 16)
}

fn enc_inst(opcode: u16, params: &[u32]) -> Vec<u32> {
    let token = (opcode as u32) | ((params.len() as u32) << 24);
    let mut v = vec![token];
    v.extend_from_slice(params);
    v
}

fn to_bytes(words: &[u32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(words.len() * 4);
    for w in words {
        bytes.extend_from_slice(&w.to_le_bytes());
    }
    bytes
}

fn assemble_vs_passthrough_pos() -> Vec<u8> {
    // vs_2_0: mov oPos, v0; end
    let mut words = vec![0xFFFE_0200];
    words.extend(enc_inst(0x0001, &[enc_dst(4, 0, 0xF), enc_src(1, 0, 0xE4)]));
    words.push(0x0000_FFFF);
    to_bytes(&words)
}

fn assemble_ps_solid_color_c0() -> Vec<u8> {
    // ps_2_0: mov oC0, c0; end
    let mut words = vec![0xFFFF_0200];
    words.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(2, 0, 0xE4)]));
    words.push(0x0000_FFFF);
    to_bytes(&words)
}

fn vertex_decl_pos4() -> Vec<u8> {
    // D3DVERTEXELEMENT9 stream (little-endian).
    // Element 0: POSITION0 float4 at stream 0 offset 0.
    // End marker: stream 0xFF, type UNUSED.
    let mut vertex_decl = Vec::new();
    push_u16(&mut vertex_decl, 0); // stream
    push_u16(&mut vertex_decl, 0); // offset
    push_u8(&mut vertex_decl, 3); // type = FLOAT4
    push_u8(&mut vertex_decl, 0); // method
    push_u8(&mut vertex_decl, 0); // usage = POSITION
    push_u8(&mut vertex_decl, 0); // usage_index
    push_u16(&mut vertex_decl, 0x00FF); // stream = 0xFF
    push_u16(&mut vertex_decl, 0); // offset
    push_u8(&mut vertex_decl, 17); // type = UNUSED
    push_u8(&mut vertex_decl, 0); // method
    push_u8(&mut vertex_decl, 0); // usage
    push_u8(&mut vertex_decl, 0); // usage_index
    vertex_decl
}

fn pixel_at(pixels: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let idx = ((y * width + x) * 4) as usize;
    [
        pixels[idx],
        pixels[idx + 1],
        pixels[idx + 2],
        pixels[idx + 3],
    ]
}

fn stencil_at(stencil: &[u8], width: u32, x: u32, y: u32) -> u8 {
    stencil[(y * width + x) as usize]
}

#[test]
fn d3d9_cmd_stream_clear_respects_scissor_rect() {
    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(module_path!(), "wgpu adapter not found");
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    const RT_HANDLE: u32 = 1;

    let width = 64u32;
    let height = 64u32;

    let scissor_x = 8i32;
    let scissor_y = 8i32;
    let scissor_w = 16i32;
    let scissor_h = 16i32;

    let stream = build_stream(|out| {
        emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
            push_u32(out, RT_HANDLE);
            push_u32(
                out,
                AEROGPU_RESOURCE_USAGE_TEXTURE | AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
            );
            push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32);
            push_u32(out, width);
            push_u32(out, height);
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, width * 4); // row_pitch_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, AerogpuCmdOpcode::SetRenderTargets as u32, |out| {
            push_u32(out, 1); // color_count
            push_u32(out, 0); // depth_stencil
            push_u32(out, RT_HANDLE);
            for _ in 0..7 {
                push_u32(out, 0);
            }
        });

        emit_packet(out, AerogpuCmdOpcode::SetViewport as u32, |out| {
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, width as f32);
            push_f32(out, height as f32);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
        });

        // Full target clear to red using the fast-path load-op clear.
        emit_packet(out, AerogpuCmdOpcode::Clear as u32, |out| {
            push_u32(out, AEROGPU_CLEAR_COLOR);
            push_f32(out, 1.0);
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 1.0); // depth
            push_u32(out, 0); // stencil
        });

        emit_packet(out, AerogpuCmdOpcode::SetRenderState as u32, |out| {
            push_u32(out, D3DRS_SCISSORTESTENABLE);
            push_u32(out, 1);
        });

        emit_packet(out, AerogpuCmdOpcode::SetScissor as u32, |out| {
            push_i32(out, scissor_x);
            push_i32(out, scissor_y);
            push_i32(out, scissor_w);
            push_i32(out, scissor_h);
        });

        // Scissored clear to green.
        emit_packet(out, AerogpuCmdOpcode::Clear as u32, |out| {
            push_u32(out, AEROGPU_CLEAR_COLOR);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 1.0); // depth
            push_u32(out, 0); // stencil
        });
    });

    exec.execute_cmd_stream(&stream)
        .expect("execute should succeed");

    let (_out_w, _out_h, rgba) = pollster::block_on(exec.readback_texture_rgba8(RT_HANDLE))
        .expect("readback should succeed");

    let red = [255, 0, 0, 255];
    let green = [0, 255, 0, 255];

    // Inside scissor should be green.
    assert_eq!(
        pixel_at(&rgba, width, (scissor_x + 1) as u32, (scissor_y + 1) as u32),
        green
    );
    assert_eq!(
        pixel_at(
            &rgba,
            width,
            (scissor_x + scissor_w - 1) as u32,
            (scissor_y + scissor_h - 1) as u32
        ),
        green
    );

    // Outside scissor should remain red.
    assert_eq!(pixel_at(&rgba, width, 0, 0), red);
    assert_eq!(pixel_at(&rgba, width, width - 1, height - 1), red);
    assert_eq!(
        pixel_at(&rgba, width, (scissor_x - 1) as u32, scissor_y as u32),
        red
    );
    assert_eq!(
        pixel_at(
            &rgba,
            width,
            (scissor_x + scissor_w) as u32,
            scissor_y as u32
        ),
        red
    );
}

#[test]
fn d3d9_cmd_stream_clear_scissored_respects_srgb_write_enable() {
    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(
                concat!(
                    module_path!(),
                    "::d3d9_cmd_stream_clear_scissored_respects_srgb_write_enable"
                ),
                "wgpu adapter not found",
            );
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    if !exec.supports_view_formats() {
        common::skip_or_panic(
            concat!(
                module_path!(),
                "::d3d9_cmd_stream_clear_scissored_respects_srgb_write_enable"
            ),
            "DownlevelFlags::VIEW_FORMATS not supported",
        );
        return;
    }

    const RT_HANDLE: u32 = 1;

    let width = 16u32;
    let height = 16u32;

    let scissor_x = 4i32;
    let scissor_y = 4i32;
    let scissor_w = 8i32;
    let scissor_h = 8i32;

    let stream = build_stream(|out| {
        emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
            push_u32(out, RT_HANDLE);
            push_u32(
                out,
                AEROGPU_RESOURCE_USAGE_TEXTURE | AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
            );
            push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32);
            push_u32(out, width);
            push_u32(out, height);
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, width * 4); // row_pitch_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, AerogpuCmdOpcode::SetRenderTargets as u32, |out| {
            push_u32(out, 1); // color_count
            push_u32(out, 0); // depth_stencil
            push_u32(out, RT_HANDLE);
            for _ in 0..7 {
                push_u32(out, 0);
            }
        });

        emit_packet(out, AerogpuCmdOpcode::SetViewport as u32, |out| {
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, width as f32);
            push_f32(out, height as f32);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
        });

        // Full target clear to black using the fast-path load-op clear.
        emit_packet(out, AerogpuCmdOpcode::Clear as u32, |out| {
            push_u32(out, AEROGPU_CLEAR_COLOR);
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 1.0); // depth
            push_u32(out, 0); // stencil
        });

        emit_packet(out, AerogpuCmdOpcode::SetRenderState as u32, |out| {
            push_u32(out, D3DRS_SCISSORTESTENABLE);
            push_u32(out, 1);
        });

        emit_packet(out, AerogpuCmdOpcode::SetScissor as u32, |out| {
            push_i32(out, scissor_x);
            push_i32(out, scissor_y);
            push_i32(out, scissor_w);
            push_i32(out, scissor_h);
        });

        emit_packet(out, AerogpuCmdOpcode::SetRenderState as u32, |out| {
            push_u32(out, D3DRS_SRGBWRITEENABLE);
            push_u32(out, 1);
        });

        // Scissored clear to 0.5 gray. With sRGB write enabled, 0.5 (linear) becomes ~188 (sRGB).
        emit_packet(out, AerogpuCmdOpcode::Clear as u32, |out| {
            push_u32(out, AEROGPU_CLEAR_COLOR);
            push_f32(out, 0.5);
            push_f32(out, 0.5);
            push_f32(out, 0.5);
            push_f32(out, 1.0);
            push_f32(out, 1.0); // depth
            push_u32(out, 0); // stencil
        });
    });

    exec.execute_cmd_stream(&stream)
        .expect("execute should succeed");

    let (_out_w, _out_h, rgba) = pollster::block_on(exec.readback_texture_rgba8(RT_HANDLE))
        .expect("readback should succeed");

    let inside = pixel_at(&rgba, width, (scissor_x + 1) as u32, (scissor_y + 1) as u32);
    assert!(
        (185..=190).contains(&inside[0]) && inside[0] == inside[1] && inside[1] == inside[2],
        "expected srgb-encoded ~188 gray, got {inside:?}"
    );
    assert_eq!(inside[3], 255);

    // Outside scissor should remain black.
    assert_eq!(pixel_at(&rgba, width, 0, 0), [0, 0, 0, 255]);
    assert_eq!(
        pixel_at(&rgba, width, width - 1, height - 1),
        [0, 0, 0, 255]
    );
    assert_eq!(
        pixel_at(&rgba, width, (scissor_x - 1) as u32, scissor_y as u32),
        [0, 0, 0, 255]
    );
    assert_eq!(
        pixel_at(
            &rgba,
            width,
            (scissor_x + scissor_w) as u32,
            scissor_y as u32
        ),
        [0, 0, 0, 255]
    );
}

#[test]
fn d3d9_cmd_stream_clear_respects_scissor_rect_with_negative_origin() {
    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(
                concat!(
                    module_path!(),
                    "::d3d9_cmd_stream_clear_respects_scissor_rect_with_negative_origin"
                ),
                "wgpu adapter not found",
            );
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    const RT_HANDLE: u32 = 1;

    let width = 64u32;
    let height = 64u32;

    // x is negative but (x + width) is still positive: the visible rect should clamp to [0, 16).
    let scissor_x = -16i32;
    let scissor_y = 0i32;
    let scissor_w = 32i32;
    let scissor_h = height as i32;

    let stream = build_stream(|out| {
        emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
            push_u32(out, RT_HANDLE);
            push_u32(
                out,
                AEROGPU_RESOURCE_USAGE_TEXTURE | AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
            );
            push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32);
            push_u32(out, width);
            push_u32(out, height);
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, width * 4); // row_pitch_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, AerogpuCmdOpcode::SetRenderTargets as u32, |out| {
            push_u32(out, 1); // color_count
            push_u32(out, 0); // depth_stencil
            push_u32(out, RT_HANDLE);
            for _ in 0..7 {
                push_u32(out, 0);
            }
        });

        emit_packet(out, AerogpuCmdOpcode::SetViewport as u32, |out| {
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, width as f32);
            push_f32(out, height as f32);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
        });

        // Full target clear to red using the fast-path load-op clear.
        emit_packet(out, AerogpuCmdOpcode::Clear as u32, |out| {
            push_u32(out, AEROGPU_CLEAR_COLOR);
            push_f32(out, 1.0);
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 1.0); // depth
            push_u32(out, 0); // stencil
        });

        emit_packet(out, AerogpuCmdOpcode::SetRenderState as u32, |out| {
            push_u32(out, D3DRS_SCISSORTESTENABLE);
            push_u32(out, 1);
        });

        emit_packet(out, AerogpuCmdOpcode::SetScissor as u32, |out| {
            push_i32(out, scissor_x);
            push_i32(out, scissor_y);
            push_i32(out, scissor_w);
            push_i32(out, scissor_h);
        });

        // Scissored clear to green.
        emit_packet(out, AerogpuCmdOpcode::Clear as u32, |out| {
            push_u32(out, AEROGPU_CLEAR_COLOR);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 1.0); // depth
            push_u32(out, 0); // stencil
        });
    });

    exec.execute_cmd_stream(&stream)
        .expect("execute should succeed");

    let (_out_w, _out_h, rgba) = pollster::block_on(exec.readback_texture_rgba8(RT_HANDLE))
        .expect("readback should succeed");

    let red = [255, 0, 0, 255];
    let green = [0, 255, 0, 255];

    // Inside: scissor clamps to x=0..16.
    assert_eq!(pixel_at(&rgba, width, 0, 0), green);
    assert_eq!(pixel_at(&rgba, width, 15, 0), green);

    // Outside: should remain red.
    assert_eq!(pixel_at(&rgba, width, 16, 0), red);
    assert_eq!(pixel_at(&rgba, width, width - 1, height - 1), red);
}

#[test]
fn d3d9_cmd_stream_clear_skips_when_scissor_rect_has_no_intersection_due_to_negative_origin() {
    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(
                concat!(
                    module_path!(),
                    "::d3d9_cmd_stream_clear_skips_when_scissor_rect_has_no_intersection_due_to_negative_origin"
                ),
                "wgpu adapter not found",
            );
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    const RT_HANDLE: u32 = 1;

    let width = 64u32;
    let height = 64u32;

    // x is negative and (x + width) is still negative: the scissor rect has no intersection.
    let scissor_x = -16i32;
    let scissor_y = 0i32;
    let scissor_w = 8i32;
    let scissor_h = height as i32;

    let stream = build_stream(|out| {
        emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
            push_u32(out, RT_HANDLE);
            push_u32(
                out,
                AEROGPU_RESOURCE_USAGE_TEXTURE | AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
            );
            push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32);
            push_u32(out, width);
            push_u32(out, height);
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, width * 4); // row_pitch_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, AerogpuCmdOpcode::SetRenderTargets as u32, |out| {
            push_u32(out, 1); // color_count
            push_u32(out, 0); // depth_stencil
            push_u32(out, RT_HANDLE);
            for _ in 0..7 {
                push_u32(out, 0);
            }
        });

        emit_packet(out, AerogpuCmdOpcode::SetViewport as u32, |out| {
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, width as f32);
            push_f32(out, height as f32);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
        });

        // Full target clear to red.
        emit_packet(out, AerogpuCmdOpcode::Clear as u32, |out| {
            push_u32(out, AEROGPU_CLEAR_COLOR);
            push_f32(out, 1.0);
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 1.0); // depth
            push_u32(out, 0); // stencil
        });

        emit_packet(out, AerogpuCmdOpcode::SetRenderState as u32, |out| {
            push_u32(out, D3DRS_SCISSORTESTENABLE);
            push_u32(out, 1);
        });

        emit_packet(out, AerogpuCmdOpcode::SetScissor as u32, |out| {
            push_i32(out, scissor_x);
            push_i32(out, scissor_y);
            push_i32(out, scissor_w);
            push_i32(out, scissor_h);
        });

        // Scissored clear to green should have no effect.
        emit_packet(out, AerogpuCmdOpcode::Clear as u32, |out| {
            push_u32(out, AEROGPU_CLEAR_COLOR);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 1.0); // depth
            push_u32(out, 0); // stencil
        });
    });

    exec.execute_cmd_stream(&stream)
        .expect("execute should succeed");

    let (_out_w, _out_h, rgba) = pollster::block_on(exec.readback_texture_rgba8(RT_HANDLE))
        .expect("readback should succeed");

    let red = [255, 0, 0, 255];

    assert_eq!(pixel_at(&rgba, width, 0, 0), red);
    assert_eq!(pixel_at(&rgba, width, width - 1, height - 1), red);
}

#[test]
fn d3d9_cmd_stream_draw_skips_when_scissor_rect_has_no_intersection() {
    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(
                concat!(
                    module_path!(),
                    "::d3d9_cmd_stream_draw_skips_when_scissor_rect_has_no_intersection"
                ),
                "wgpu adapter not found",
            );
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    const RT_HANDLE: u32 = 1;
    const VB_HANDLE: u32 = 2;
    const VS_HANDLE: u32 = 3;
    const PS_HANDLE: u32 = 4;
    const IL_HANDLE: u32 = 5;

    let width = 32u32;
    let height = 32u32;

    // Full-screen triangle (POSITION float4).
    //
    // Note: Keep clockwise winding so the draw stays visible even if culling is enabled (D3D9
    // defaults to `D3DCULL_CCW` with clockwise front faces).
    let vertices: [f32; 12] = [
        -1.0, -1.0, 0.0, 1.0, //
        -1.0, 3.0, 0.0, 1.0, //
        3.0, -1.0, 0.0, 1.0, //
    ];
    let vb_bytes: &[u8] = bytemuck::cast_slice(&vertices);

    let vs_bytes = assemble_vs_passthrough_pos();
    let ps_bytes = assemble_ps_solid_color_c0();
    let vertex_decl = vertex_decl_pos4();

    let stream = build_stream(|out| {
        emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
            push_u32(out, RT_HANDLE);
            push_u32(
                out,
                AEROGPU_RESOURCE_USAGE_TEXTURE | AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
            );
            push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32);
            push_u32(out, width);
            push_u32(out, height);
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, width * 4); // row_pitch_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, AerogpuCmdOpcode::CreateBuffer as u32, |out| {
            push_u32(out, VB_HANDLE);
            push_u32(out, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER);
            push_u64(out, vb_bytes.len() as u64);
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, AerogpuCmdOpcode::UploadResource as u32, |out| {
            push_u32(out, VB_HANDLE);
            push_u32(out, 0); // reserved0
            push_u64(out, 0); // offset_bytes
            push_u64(out, vb_bytes.len() as u64);
            out.extend_from_slice(vb_bytes);
        });

        emit_packet(out, AerogpuCmdOpcode::CreateShaderDxbc as u32, |out| {
            push_u32(out, VS_HANDLE);
            push_u32(out, 0); // AEROGPU_SHADER_STAGE_VERTEX
            push_u32(out, vs_bytes.len() as u32);
            push_u32(out, 0); // reserved0
            out.extend_from_slice(&vs_bytes);
        });

        emit_packet(out, AerogpuCmdOpcode::CreateShaderDxbc as u32, |out| {
            push_u32(out, PS_HANDLE);
            push_u32(out, 1); // AEROGPU_SHADER_STAGE_PIXEL
            push_u32(out, ps_bytes.len() as u32);
            push_u32(out, 0); // reserved0
            out.extend_from_slice(&ps_bytes);
        });

        emit_packet(out, AerogpuCmdOpcode::BindShaders as u32, |out| {
            push_u32(out, VS_HANDLE);
            push_u32(out, PS_HANDLE);
            push_u32(out, 0); // cs
            push_u32(out, 0); // reserved0
        });

        emit_packet(out, AerogpuCmdOpcode::CreateInputLayout as u32, |out| {
            push_u32(out, IL_HANDLE);
            push_u32(out, vertex_decl.len() as u32);
            push_u32(out, 0); // reserved0
            out.extend_from_slice(&vertex_decl);
        });

        emit_packet(out, AerogpuCmdOpcode::SetInputLayout as u32, |out| {
            push_u32(out, IL_HANDLE);
            push_u32(out, 0); // reserved0
        });

        emit_packet(out, AerogpuCmdOpcode::SetVertexBuffers as u32, |out| {
            push_u32(out, 0); // start_slot
            push_u32(out, 1); // buffer_count
            push_u32(out, VB_HANDLE);
            push_u32(out, 16); // stride_bytes
            push_u32(out, 0); // offset_bytes
            push_u32(out, 0); // reserved0
        });

        emit_packet(out, AerogpuCmdOpcode::SetPrimitiveTopology as u32, |out| {
            push_u32(out, AerogpuPrimitiveTopology::TriangleList as u32);
            push_u32(out, 0); // reserved0
        });

        emit_packet(out, AerogpuCmdOpcode::SetRenderTargets as u32, |out| {
            push_u32(out, 1); // color_count
            push_u32(out, 0); // depth_stencil
            push_u32(out, RT_HANDLE);
            for _ in 0..7 {
                push_u32(out, 0);
            }
        });

        emit_packet(out, AerogpuCmdOpcode::SetViewport as u32, |out| {
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, width as f32);
            push_f32(out, height as f32);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
        });

        // Clear to red.
        emit_packet(out, AerogpuCmdOpcode::Clear as u32, |out| {
            push_u32(out, AEROGPU_CLEAR_COLOR);
            push_f32(out, 1.0);
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 1.0);
            push_u32(out, 0);
        });

        // Enable scissor testing but set an out-of-bounds rect that has no intersection.
        emit_packet(out, AerogpuCmdOpcode::SetRenderState as u32, |out| {
            push_u32(out, D3DRS_SCISSORTESTENABLE);
            push_u32(out, 1);
        });
        emit_packet(out, AerogpuCmdOpcode::SetScissor as u32, |out| {
            push_i32(out, width as i32); // x (at the right edge, outside)
            push_i32(out, 0);
            push_i32(out, 1);
            push_i32(out, 1);
        });

        // c0 = green.
        emit_packet(out, AerogpuCmdOpcode::SetShaderConstantsF as u32, |out| {
            push_u32(out, 1); // AEROGPU_SHADER_STAGE_PIXEL
            push_u32(out, 0); // start_register
            push_u32(out, 1); // vec4_count
            push_u32(out, 0); // reserved0
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
        });

        emit_packet(out, AerogpuCmdOpcode::Draw as u32, |out| {
            push_u32(out, 3); // vertex_count
            push_u32(out, 1); // instance_count
            push_u32(out, 0); // first_vertex
            push_u32(out, 0); // first_instance
        });
    });

    exec.execute_cmd_stream(&stream)
        .expect("execute should succeed");

    let (_out_w, _out_h, rgba) = pollster::block_on(exec.readback_texture_rgba8(RT_HANDLE))
        .expect("readback should succeed");

    // Scissor is completely out of bounds; draw should have produced no fragments.
    assert_eq!(
        pixel_at(&rgba, width, width / 2, height / 2),
        [255, 0, 0, 255]
    );
}

#[test]
fn d3d9_cmd_stream_clear_respects_scissor_rect_mrt() {
    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(
                concat!(
                    module_path!(),
                    "::d3d9_cmd_stream_clear_respects_scissor_rect_mrt"
                ),
                "wgpu adapter not found",
            );
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    const RT0_HANDLE: u32 = 1;
    const RT1_HANDLE: u32 = 2;

    let width = 64u32;
    let height = 64u32;

    let scissor_x = 8i32;
    let scissor_y = 8i32;
    let scissor_w = 16i32;
    let scissor_h = 16i32;

    let stream = build_stream(|out| {
        for handle in [RT0_HANDLE, RT1_HANDLE] {
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, handle);
                push_u32(
                    out,
                    AEROGPU_RESOURCE_USAGE_TEXTURE | AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
                );
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32);
                push_u32(out, width);
                push_u32(out, height);
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, width * 4); // row_pitch_bytes
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });
        }

        emit_packet(out, AerogpuCmdOpcode::SetRenderTargets as u32, |out| {
            push_u32(out, 2); // color_count
            push_u32(out, 0); // depth_stencil
            push_u32(out, RT0_HANDLE);
            push_u32(out, RT1_HANDLE);
            for _ in 0..6 {
                push_u32(out, 0);
            }
        });

        emit_packet(out, AerogpuCmdOpcode::SetViewport as u32, |out| {
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, width as f32);
            push_f32(out, height as f32);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
        });

        // Full target clear to red using the fast-path load-op clear.
        emit_packet(out, AerogpuCmdOpcode::Clear as u32, |out| {
            push_u32(out, AEROGPU_CLEAR_COLOR);
            push_f32(out, 1.0);
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 1.0); // depth
            push_u32(out, 0); // stencil
        });

        emit_packet(out, AerogpuCmdOpcode::SetRenderState as u32, |out| {
            push_u32(out, D3DRS_SCISSORTESTENABLE);
            push_u32(out, 1);
        });

        emit_packet(out, AerogpuCmdOpcode::SetScissor as u32, |out| {
            push_i32(out, scissor_x);
            push_i32(out, scissor_y);
            push_i32(out, scissor_w);
            push_i32(out, scissor_h);
        });

        // Scissored clear to green.
        emit_packet(out, AerogpuCmdOpcode::Clear as u32, |out| {
            push_u32(out, AEROGPU_CLEAR_COLOR);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 1.0); // depth
            push_u32(out, 0); // stencil
        });
    });

    exec.execute_cmd_stream(&stream)
        .expect("execute should succeed");

    let red = [255, 0, 0, 255];
    let green = [0, 255, 0, 255];

    for handle in [RT0_HANDLE, RT1_HANDLE] {
        let (_out_w, _out_h, rgba) = pollster::block_on(exec.readback_texture_rgba8(handle))
            .expect("readback should succeed");

        // Inside scissor should be green.
        assert_eq!(
            pixel_at(&rgba, width, (scissor_x + 1) as u32, (scissor_y + 1) as u32),
            green
        );

        // Outside scissor should remain red.
        assert_eq!(pixel_at(&rgba, width, 0, 0), red);
        assert_eq!(pixel_at(&rgba, width, width - 1, height - 1), red);
    }
}

#[test]
fn d3d9_cmd_stream_clear_stencil_respects_scissor_rect() {
    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(
                concat!(
                    module_path!(),
                    "::d3d9_cmd_stream_clear_stencil_respects_scissor_rect"
                ),
                "wgpu adapter not found",
            );
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    if !exec.supports_depth_texture_and_buffer_copies() {
        common::skip_or_panic(
            concat!(
                module_path!(),
                "::d3d9_cmd_stream_clear_stencil_respects_scissor_rect"
            ),
            "DownlevelFlags::DEPTH_TEXTURE_AND_BUFFER_COPIES not supported",
        );
        return;
    }

    const RT_HANDLE: u32 = 1;
    const DS_HANDLE: u32 = 2;

    let width = 64u32;
    let height = 64u32;

    let scissor_x = 8i32;
    let scissor_y = 8i32;
    let scissor_w = 16i32;
    let scissor_h = 16i32;

    let stream = build_stream(|out| {
        emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
            push_u32(out, RT_HANDLE);
            push_u32(
                out,
                AEROGPU_RESOURCE_USAGE_TEXTURE | AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
            );
            push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32);
            push_u32(out, width);
            push_u32(out, height);
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, width * 4); // row_pitch_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
            push_u32(out, DS_HANDLE);
            push_u32(
                out,
                AEROGPU_RESOURCE_USAGE_TEXTURE | AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL,
            );
            push_u32(out, AerogpuFormat::D24UnormS8Uint as u32);
            push_u32(out, width);
            push_u32(out, height);
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, 0); // row_pitch_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, AerogpuCmdOpcode::SetRenderTargets as u32, |out| {
            push_u32(out, 1); // color_count
            push_u32(out, DS_HANDLE); // depth_stencil
            push_u32(out, RT_HANDLE);
            for _ in 0..7 {
                push_u32(out, 0);
            }
        });

        // Full target clear to stencil=0.
        emit_packet(out, AerogpuCmdOpcode::Clear as u32, |out| {
            push_u32(out, AEROGPU_CLEAR_STENCIL);
            // Color is ignored for stencil-only clear, but still part of the packet.
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 1.0); // depth
            push_u32(out, 0); // stencil
        });

        emit_packet(out, AerogpuCmdOpcode::SetRenderState as u32, |out| {
            push_u32(out, D3DRS_SCISSORTESTENABLE);
            push_u32(out, 1);
        });

        emit_packet(out, AerogpuCmdOpcode::SetScissor as u32, |out| {
            push_i32(out, scissor_x);
            push_i32(out, scissor_y);
            push_i32(out, scissor_w);
            push_i32(out, scissor_h);
        });

        // Scissored clear to stencil=7.
        emit_packet(out, AerogpuCmdOpcode::Clear as u32, |out| {
            push_u32(out, AEROGPU_CLEAR_STENCIL);
            // Color is ignored for stencil-only clear, but still part of the packet.
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 1.0); // depth
            push_u32(out, 7); // stencil
        });
    });

    exec.execute_cmd_stream(&stream)
        .expect("execute should succeed");

    let (_out_w, _out_h, stencil) = pollster::block_on(exec.readback_texture_stencil8(DS_HANDLE))
        .expect("stencil readback should succeed");
    assert_eq!(stencil.len(), (width * height) as usize);

    let stencil_at = |x: u32, y: u32| stencil[(y * width + x) as usize];

    assert_eq!(
        stencil_at((scissor_x + 1) as u32, (scissor_y + 1) as u32),
        7
    );
    assert_eq!(stencil_at(0, 0), 0);
    assert_eq!(stencil_at(width - 1, height - 1), 0);
}

#[test]
fn d3d9_cmd_stream_clear_depth_respects_scissor_rect() {
    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(
                concat!(
                    module_path!(),
                    "::d3d9_cmd_stream_clear_depth_respects_scissor_rect"
                ),
                "wgpu adapter not found",
            );
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    const RT_HANDLE: u32 = 1;
    const DS_HANDLE: u32 = 2;
    const VB_HANDLE: u32 = 3;
    const VS_HANDLE: u32 = 4;
    const PS_HANDLE: u32 = 5;
    const IL_HANDLE: u32 = 6;

    let width = 64u32;
    let height = 64u32;

    let scissor_x = 8i32;
    let scissor_y = 8i32;
    let scissor_w = 16i32;
    let scissor_h = 16i32;

    // Full-screen triangle (POSITION float4) at z=0.5.
    // Note: D3D9 defaults to clockwise front faces. Arrange the full-screen triangle with
    // clockwise winding so it isn't culled by default state.
    let vertices: [f32; 12] = [
        -1.0, -1.0, 0.5, 1.0, //
        -1.0, 3.0, 0.5, 1.0, //
        3.0, -1.0, 0.5, 1.0, //
    ];
    let vb_bytes: &[u8] = bytemuck::cast_slice(&vertices);

    let vs_bytes = assemble_vs_passthrough_pos();
    let ps_bytes = assemble_ps_solid_color_c0();
    let vertex_decl = vertex_decl_pos4();

    let stream = build_stream(|out| {
        emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
            push_u32(out, RT_HANDLE);
            push_u32(
                out,
                AEROGPU_RESOURCE_USAGE_TEXTURE | AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
            );
            push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32);
            push_u32(out, width);
            push_u32(out, height);
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, width * 4); // row_pitch_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
            push_u32(out, DS_HANDLE);
            push_u32(
                out,
                AEROGPU_RESOURCE_USAGE_TEXTURE | AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL,
            );
            push_u32(out, AerogpuFormat::D32Float as u32);
            push_u32(out, width);
            push_u32(out, height);
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, 0); // row_pitch_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, AerogpuCmdOpcode::CreateBuffer as u32, |out| {
            push_u32(out, VB_HANDLE);
            push_u32(out, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER);
            push_u64(out, vb_bytes.len() as u64);
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, AerogpuCmdOpcode::UploadResource as u32, |out| {
            push_u32(out, VB_HANDLE);
            push_u32(out, 0); // reserved0
            push_u64(out, 0); // offset_bytes
            push_u64(out, vb_bytes.len() as u64);
            out.extend_from_slice(vb_bytes);
        });

        emit_packet(out, AerogpuCmdOpcode::CreateShaderDxbc as u32, |out| {
            push_u32(out, VS_HANDLE);
            push_u32(out, 0); // AEROGPU_SHADER_STAGE_VERTEX
            push_u32(out, vs_bytes.len() as u32);
            push_u32(out, 0); // reserved0
            out.extend_from_slice(&vs_bytes);
        });

        emit_packet(out, AerogpuCmdOpcode::CreateShaderDxbc as u32, |out| {
            push_u32(out, PS_HANDLE);
            push_u32(out, 1); // AEROGPU_SHADER_STAGE_PIXEL
            push_u32(out, ps_bytes.len() as u32);
            push_u32(out, 0); // reserved0
            out.extend_from_slice(&ps_bytes);
        });

        emit_packet(out, AerogpuCmdOpcode::BindShaders as u32, |out| {
            push_u32(out, VS_HANDLE);
            push_u32(out, PS_HANDLE);
            push_u32(out, 0); // cs
            push_u32(out, 0); // reserved0
        });

        emit_packet(out, AerogpuCmdOpcode::CreateInputLayout as u32, |out| {
            push_u32(out, IL_HANDLE);
            push_u32(out, vertex_decl.len() as u32);
            push_u32(out, 0); // reserved0
            out.extend_from_slice(&vertex_decl);
        });

        emit_packet(out, AerogpuCmdOpcode::SetInputLayout as u32, |out| {
            push_u32(out, IL_HANDLE);
            push_u32(out, 0); // reserved0
        });

        emit_packet(out, AerogpuCmdOpcode::SetVertexBuffers as u32, |out| {
            push_u32(out, 0); // start_slot
            push_u32(out, 1); // buffer_count
            push_u32(out, VB_HANDLE);
            push_u32(out, 16); // stride_bytes
            push_u32(out, 0); // offset_bytes
            push_u32(out, 0); // reserved0
        });

        emit_packet(out, AerogpuCmdOpcode::SetPrimitiveTopology as u32, |out| {
            push_u32(out, AerogpuPrimitiveTopology::TriangleList as u32);
            push_u32(out, 0); // reserved0
        });

        emit_packet(out, AerogpuCmdOpcode::SetRenderTargets as u32, |out| {
            push_u32(out, 1); // color_count
            push_u32(out, DS_HANDLE); // depth_stencil
            push_u32(out, RT_HANDLE);
            for _ in 0..7 {
                push_u32(out, 0);
            }
        });

        emit_packet(out, AerogpuCmdOpcode::SetViewport as u32, |out| {
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, width as f32);
            push_f32(out, height as f32);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
        });

        // Full target clear to red + depth=0.0.
        emit_packet(out, AerogpuCmdOpcode::Clear as u32, |out| {
            push_u32(out, AEROGPU_CLEAR_COLOR | AEROGPU_CLEAR_DEPTH);
            push_f32(out, 1.0);
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 0.0); // depth
            push_u32(out, 0); // stencil
        });

        // Enable scissor and clear depth to 1.0 inside the rect.
        emit_packet(out, AerogpuCmdOpcode::SetRenderState as u32, |out| {
            push_u32(out, D3DRS_SCISSORTESTENABLE);
            push_u32(out, 1);
        });
        emit_packet(out, AerogpuCmdOpcode::SetScissor as u32, |out| {
            push_i32(out, scissor_x);
            push_i32(out, scissor_y);
            push_i32(out, scissor_w);
            push_i32(out, scissor_h);
        });
        emit_packet(out, AerogpuCmdOpcode::Clear as u32, |out| {
            push_u32(out, AEROGPU_CLEAR_DEPTH);
            // Color is ignored for depth-only clear, but still part of the packet.
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 1.0); // depth
            push_u32(out, 0); // stencil
        });

        // Disable scissor for the test draw so we can observe the depth test result across the
        // whole render target.
        emit_packet(out, AerogpuCmdOpcode::SetRenderState as u32, |out| {
            push_u32(out, D3DRS_SCISSORTESTENABLE);
            push_u32(out, 0);
        });

        // Enable depth testing: draw at z=0.5 with LessEqual. This should pass only where the
        // cleared depth is 1.0.
        emit_packet(out, AerogpuCmdOpcode::SetRenderState as u32, |out| {
            push_u32(out, D3DRS_ZENABLE);
            push_u32(out, 1);
        });
        emit_packet(out, AerogpuCmdOpcode::SetRenderState as u32, |out| {
            push_u32(out, D3DRS_ZWRITEENABLE);
            push_u32(out, 0);
        });
        emit_packet(out, AerogpuCmdOpcode::SetRenderState as u32, |out| {
            push_u32(out, D3DRS_ZFUNC);
            push_u32(out, D3DCMP_LESSEQUAL);
        });

        // c0 = green
        emit_packet(out, AerogpuCmdOpcode::SetShaderConstantsF as u32, |out| {
            push_u32(out, 1); // AEROGPU_SHADER_STAGE_PIXEL
            push_u32(out, 0); // start_register
            push_u32(out, 1); // vec4_count
            push_u32(out, 0); // reserved0
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
        });

        emit_packet(out, AerogpuCmdOpcode::Draw as u32, |out| {
            push_u32(out, 3); // vertex_count
            push_u32(out, 1); // instance_count
            push_u32(out, 0); // first_vertex
            push_u32(out, 0); // first_instance
        });
    });

    exec.execute_cmd_stream(&stream)
        .expect("execute should succeed");

    let (_out_w, _out_h, rgba) = pollster::block_on(exec.readback_texture_rgba8(RT_HANDLE))
        .expect("readback should succeed");

    let red = [255, 0, 0, 255];
    let green = [0, 255, 0, 255];

    // Inside scissor: depth was cleared to 1.0 so z=0.5 passes.
    assert_eq!(
        pixel_at(&rgba, width, (scissor_x + 1) as u32, (scissor_y + 1) as u32),
        green
    );

    // Outside scissor: depth remained 0.0 so z=0.5 fails and we keep the red clear color.
    assert_eq!(pixel_at(&rgba, width, 0, 0), red);
    assert_eq!(pixel_at(&rgba, width, width - 1, height - 1), red);
}

#[test]
fn d3d9_cmd_stream_clear_stencil_masks_to_8_bits() {
    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(
                concat!(
                    module_path!(),
                    "::d3d9_cmd_stream_clear_stencil_masks_to_8_bits"
                ),
                "wgpu adapter not found",
            );
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    if !exec.supports_depth_texture_and_buffer_copies() {
        common::skip_or_panic(
            concat!(
                module_path!(),
                "::d3d9_cmd_stream_clear_stencil_masks_to_8_bits"
            ),
            "DownlevelFlags::DEPTH_TEXTURE_AND_BUFFER_COPIES not supported",
        );
        return;
    }

    const RT_HANDLE: u32 = 1;
    const DS_HANDLE: u32 = 2;

    let width = 64u32;
    let height = 64u32;

    // D3D9 takes a 32-bit stencil clear value but only the low 8 bits apply for D24S8.
    let stencil_value = 0x1234u32;
    let expected = (stencil_value & 0xFF) as u8;

    let stream = build_stream(|out| {
        emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
            push_u32(out, RT_HANDLE);
            push_u32(
                out,
                AEROGPU_RESOURCE_USAGE_TEXTURE | AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
            );
            push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32);
            push_u32(out, width);
            push_u32(out, height);
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, width * 4); // row_pitch_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
            push_u32(out, DS_HANDLE);
            push_u32(
                out,
                AEROGPU_RESOURCE_USAGE_TEXTURE | AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL,
            );
            push_u32(out, AerogpuFormat::D24UnormS8Uint as u32);
            push_u32(out, width);
            push_u32(out, height);
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, 0); // row_pitch_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, AerogpuCmdOpcode::SetRenderTargets as u32, |out| {
            push_u32(out, 1); // color_count
            push_u32(out, DS_HANDLE); // depth_stencil
            push_u32(out, RT_HANDLE);
            for _ in 0..7 {
                push_u32(out, 0);
            }
        });

        emit_packet(out, AerogpuCmdOpcode::SetViewport as u32, |out| {
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, width as f32);
            push_f32(out, height as f32);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
        });

        // Full target clear with a stencil value that does not fit in 8 bits.
        emit_packet(out, AerogpuCmdOpcode::Clear as u32, |out| {
            push_u32(
                out,
                AEROGPU_CLEAR_COLOR | AEROGPU_CLEAR_DEPTH | AEROGPU_CLEAR_STENCIL,
            );
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 1.0); // depth
            push_u32(out, stencil_value);
        });
    });

    exec.execute_cmd_stream(&stream)
        .expect("execute should succeed");

    let (_out_w, _out_h, stencil) = pollster::block_on(exec.readback_texture_stencil8(DS_HANDLE))
        .expect("stencil readback should succeed");

    assert_eq!(stencil_at(&stencil, width, 0, 0), expected);
    assert_eq!(stencil_at(&stencil, width, width - 1, height - 1), expected);
}

#[test]
fn d3d9_cmd_stream_clear_depth_d24s8_respects_scissor_rect() {
    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(
                concat!(
                    module_path!(),
                    "::d3d9_cmd_stream_clear_depth_d24s8_respects_scissor_rect"
                ),
                "wgpu adapter not found",
            );
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    const RT_HANDLE: u32 = 1;
    const DS_HANDLE: u32 = 2;
    const VB_HANDLE: u32 = 3;
    const VS_HANDLE: u32 = 4;
    const PS_HANDLE: u32 = 5;
    const IL_HANDLE: u32 = 6;

    let width = 64u32;
    let height = 64u32;

    let scissor_x = 8i32;
    let scissor_y = 8i32;
    let scissor_w = 16i32;
    let scissor_h = 16i32;

    // Full-screen triangle (POSITION float4) at z=0.5.
    // Note: D3D9 defaults to clockwise front faces. Arrange the full-screen triangle with
    // clockwise winding so it isn't culled by default state.
    let vertices: [f32; 12] = [
        -1.0, -1.0, 0.5, 1.0, //
        -1.0, 3.0, 0.5, 1.0, //
        3.0, -1.0, 0.5, 1.0, //
    ];
    let vb_bytes: &[u8] = bytemuck::cast_slice(&vertices);

    let vs_bytes = assemble_vs_passthrough_pos();
    let ps_bytes = assemble_ps_solid_color_c0();
    let vertex_decl = vertex_decl_pos4();

    let stream = build_stream(|out| {
        emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
            push_u32(out, RT_HANDLE);
            push_u32(
                out,
                AEROGPU_RESOURCE_USAGE_TEXTURE | AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
            );
            push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32);
            push_u32(out, width);
            push_u32(out, height);
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, width * 4); // row_pitch_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
            push_u32(out, DS_HANDLE);
            push_u32(
                out,
                AEROGPU_RESOURCE_USAGE_TEXTURE | AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL,
            );
            push_u32(out, AerogpuFormat::D24UnormS8Uint as u32);
            push_u32(out, width);
            push_u32(out, height);
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, 0); // row_pitch_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, AerogpuCmdOpcode::CreateBuffer as u32, |out| {
            push_u32(out, VB_HANDLE);
            push_u32(out, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER);
            push_u64(out, vb_bytes.len() as u64);
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, AerogpuCmdOpcode::UploadResource as u32, |out| {
            push_u32(out, VB_HANDLE);
            push_u32(out, 0); // reserved0
            push_u64(out, 0); // offset_bytes
            push_u64(out, vb_bytes.len() as u64);
            out.extend_from_slice(vb_bytes);
        });

        emit_packet(out, AerogpuCmdOpcode::CreateShaderDxbc as u32, |out| {
            push_u32(out, VS_HANDLE);
            push_u32(out, 0); // AEROGPU_SHADER_STAGE_VERTEX
            push_u32(out, vs_bytes.len() as u32);
            push_u32(out, 0); // reserved0
            out.extend_from_slice(&vs_bytes);
        });

        emit_packet(out, AerogpuCmdOpcode::CreateShaderDxbc as u32, |out| {
            push_u32(out, PS_HANDLE);
            push_u32(out, 1); // AEROGPU_SHADER_STAGE_PIXEL
            push_u32(out, ps_bytes.len() as u32);
            push_u32(out, 0); // reserved0
            out.extend_from_slice(&ps_bytes);
        });

        emit_packet(out, AerogpuCmdOpcode::BindShaders as u32, |out| {
            push_u32(out, VS_HANDLE);
            push_u32(out, PS_HANDLE);
            push_u32(out, 0); // cs
            push_u32(out, 0); // reserved0
        });

        emit_packet(out, AerogpuCmdOpcode::CreateInputLayout as u32, |out| {
            push_u32(out, IL_HANDLE);
            push_u32(out, vertex_decl.len() as u32);
            push_u32(out, 0); // reserved0
            out.extend_from_slice(&vertex_decl);
        });

        emit_packet(out, AerogpuCmdOpcode::SetInputLayout as u32, |out| {
            push_u32(out, IL_HANDLE);
            push_u32(out, 0); // reserved0
        });

        emit_packet(out, AerogpuCmdOpcode::SetVertexBuffers as u32, |out| {
            push_u32(out, 0); // start_slot
            push_u32(out, 1); // buffer_count
            push_u32(out, VB_HANDLE);
            push_u32(out, 16); // stride_bytes
            push_u32(out, 0); // offset_bytes
            push_u32(out, 0); // reserved0
        });

        emit_packet(out, AerogpuCmdOpcode::SetPrimitiveTopology as u32, |out| {
            push_u32(out, AerogpuPrimitiveTopology::TriangleList as u32);
            push_u32(out, 0); // reserved0
        });

        emit_packet(out, AerogpuCmdOpcode::SetRenderTargets as u32, |out| {
            push_u32(out, 1); // color_count
            push_u32(out, DS_HANDLE); // depth_stencil
            push_u32(out, RT_HANDLE);
            for _ in 0..7 {
                push_u32(out, 0);
            }
        });

        emit_packet(out, AerogpuCmdOpcode::SetViewport as u32, |out| {
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, width as f32);
            push_f32(out, height as f32);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
        });

        // Full target clear to red + depth=0.0.
        emit_packet(out, AerogpuCmdOpcode::Clear as u32, |out| {
            push_u32(out, AEROGPU_CLEAR_COLOR | AEROGPU_CLEAR_DEPTH);
            push_f32(out, 1.0);
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 0.0); // depth
            push_u32(out, 0); // stencil
        });

        // Enable scissor and clear depth to 1.0 inside the rect.
        emit_packet(out, AerogpuCmdOpcode::SetRenderState as u32, |out| {
            push_u32(out, D3DRS_SCISSORTESTENABLE);
            push_u32(out, 1);
        });
        emit_packet(out, AerogpuCmdOpcode::SetScissor as u32, |out| {
            push_i32(out, scissor_x);
            push_i32(out, scissor_y);
            push_i32(out, scissor_w);
            push_i32(out, scissor_h);
        });
        emit_packet(out, AerogpuCmdOpcode::Clear as u32, |out| {
            push_u32(out, AEROGPU_CLEAR_DEPTH);
            // Color is ignored for depth-only clear, but still part of the packet.
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 1.0); // depth
            push_u32(out, 0); // stencil
        });

        // Disable scissor for the test draw so we can observe the depth test result across the
        // whole render target.
        emit_packet(out, AerogpuCmdOpcode::SetRenderState as u32, |out| {
            push_u32(out, D3DRS_SCISSORTESTENABLE);
            push_u32(out, 0);
        });

        // Enable depth testing: draw at z=0.5 with LessEqual. This should pass only where the
        // cleared depth is 1.0.
        emit_packet(out, AerogpuCmdOpcode::SetRenderState as u32, |out| {
            push_u32(out, D3DRS_ZENABLE);
            push_u32(out, 1);
        });
        emit_packet(out, AerogpuCmdOpcode::SetRenderState as u32, |out| {
            push_u32(out, D3DRS_ZWRITEENABLE);
            push_u32(out, 0);
        });
        emit_packet(out, AerogpuCmdOpcode::SetRenderState as u32, |out| {
            push_u32(out, D3DRS_ZFUNC);
            push_u32(out, D3DCMP_LESSEQUAL);
        });

        // c0 = green
        emit_packet(out, AerogpuCmdOpcode::SetShaderConstantsF as u32, |out| {
            push_u32(out, 1); // AEROGPU_SHADER_STAGE_PIXEL
            push_u32(out, 0); // start_register
            push_u32(out, 1); // vec4_count
            push_u32(out, 0); // reserved0
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
        });

        emit_packet(out, AerogpuCmdOpcode::Draw as u32, |out| {
            push_u32(out, 3); // vertex_count
            push_u32(out, 1); // instance_count
            push_u32(out, 0); // first_vertex
            push_u32(out, 0); // first_instance
        });
    });

    exec.execute_cmd_stream(&stream)
        .expect("execute should succeed");

    let (_out_w, _out_h, rgba) = pollster::block_on(exec.readback_texture_rgba8(RT_HANDLE))
        .expect("readback should succeed");

    let red = [255, 0, 0, 255];
    let green = [0, 255, 0, 255];

    // Inside scissor: depth was cleared to 1.0 so z=0.5 passes.
    assert_eq!(
        pixel_at(&rgba, width, (scissor_x + 1) as u32, (scissor_y + 1) as u32),
        green
    );

    // Outside scissor: depth remained 0.0 so z=0.5 fails and we keep the red clear color.
    assert_eq!(pixel_at(&rgba, width, 0, 0), red);
    assert_eq!(pixel_at(&rgba, width, width - 1, height - 1), red);
}

#[test]
fn d3d9_cmd_stream_clear_color_depth_stencil_d24s8_respects_scissor_rect() {
    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(
                concat!(
                    module_path!(),
                    "::d3d9_cmd_stream_clear_color_depth_stencil_d24s8_respects_scissor_rect"
                ),
                "wgpu adapter not found",
            );
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    if !exec.supports_depth_texture_and_buffer_copies() {
        common::skip_or_panic(
            concat!(
                module_path!(),
                "::d3d9_cmd_stream_clear_color_depth_stencil_d24s8_respects_scissor_rect"
            ),
            "DownlevelFlags::DEPTH_TEXTURE_AND_BUFFER_COPIES not supported",
        );
        return;
    }

    const RT_HANDLE: u32 = 1;
    const DS_HANDLE: u32 = 2;
    const VB_HANDLE: u32 = 3;
    const VS_HANDLE: u32 = 4;
    const PS_HANDLE: u32 = 5;
    const IL_HANDLE: u32 = 6;

    let width = 64u32;
    let height = 64u32;

    let scissor_x = 8i32;
    let scissor_y = 8i32;
    let scissor_w = 16i32;
    let scissor_h = 16i32;

    // Full-screen triangle (POSITION float4) at z=0.5.
    //
    // Note: Keep clockwise winding so the draw stays visible under the default D3D9 cull mode
    // (`D3DCULL_CCW` with clockwise front faces).
    let vertices: [f32; 12] = [
        -1.0, -1.0, 0.5, 1.0, //
        -1.0, 3.0, 0.5, 1.0, //
        3.0, -1.0, 0.5, 1.0, //
    ];
    let vb_bytes: &[u8] = bytemuck::cast_slice(&vertices);

    let vs_bytes = assemble_vs_passthrough_pos();
    let ps_bytes = assemble_ps_solid_color_c0();
    let vertex_decl = vertex_decl_pos4();

    let stream = build_stream(|out| {
        emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
            push_u32(out, RT_HANDLE);
            push_u32(
                out,
                AEROGPU_RESOURCE_USAGE_TEXTURE | AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
            );
            push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32);
            push_u32(out, width);
            push_u32(out, height);
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, width * 4); // row_pitch_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
            push_u32(out, DS_HANDLE);
            push_u32(
                out,
                AEROGPU_RESOURCE_USAGE_TEXTURE | AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL,
            );
            push_u32(out, AerogpuFormat::D24UnormS8Uint as u32);
            push_u32(out, width);
            push_u32(out, height);
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, 0); // row_pitch_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, AerogpuCmdOpcode::CreateBuffer as u32, |out| {
            push_u32(out, VB_HANDLE);
            push_u32(out, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER);
            push_u64(out, vb_bytes.len() as u64);
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, AerogpuCmdOpcode::UploadResource as u32, |out| {
            push_u32(out, VB_HANDLE);
            push_u32(out, 0); // reserved0
            push_u64(out, 0); // offset_bytes
            push_u64(out, vb_bytes.len() as u64);
            out.extend_from_slice(vb_bytes);
        });

        emit_packet(out, AerogpuCmdOpcode::CreateShaderDxbc as u32, |out| {
            push_u32(out, VS_HANDLE);
            push_u32(out, 0); // AEROGPU_SHADER_STAGE_VERTEX
            push_u32(out, vs_bytes.len() as u32);
            push_u32(out, 0); // reserved0
            out.extend_from_slice(&vs_bytes);
        });

        emit_packet(out, AerogpuCmdOpcode::CreateShaderDxbc as u32, |out| {
            push_u32(out, PS_HANDLE);
            push_u32(out, 1); // AEROGPU_SHADER_STAGE_PIXEL
            push_u32(out, ps_bytes.len() as u32);
            push_u32(out, 0); // reserved0
            out.extend_from_slice(&ps_bytes);
        });

        emit_packet(out, AerogpuCmdOpcode::BindShaders as u32, |out| {
            push_u32(out, VS_HANDLE);
            push_u32(out, PS_HANDLE);
            push_u32(out, 0); // cs
            push_u32(out, 0); // reserved0
        });

        emit_packet(out, AerogpuCmdOpcode::CreateInputLayout as u32, |out| {
            push_u32(out, IL_HANDLE);
            push_u32(out, vertex_decl.len() as u32);
            push_u32(out, 0); // reserved0
            out.extend_from_slice(&vertex_decl);
        });

        emit_packet(out, AerogpuCmdOpcode::SetInputLayout as u32, |out| {
            push_u32(out, IL_HANDLE);
            push_u32(out, 0); // reserved0
        });

        emit_packet(out, AerogpuCmdOpcode::SetVertexBuffers as u32, |out| {
            push_u32(out, 0); // start_slot
            push_u32(out, 1); // buffer_count
            push_u32(out, VB_HANDLE);
            push_u32(out, 16); // stride_bytes
            push_u32(out, 0); // offset_bytes
            push_u32(out, 0); // reserved0
        });

        emit_packet(out, AerogpuCmdOpcode::SetPrimitiveTopology as u32, |out| {
            push_u32(out, AerogpuPrimitiveTopology::TriangleList as u32);
            push_u32(out, 0); // reserved0
        });

        emit_packet(out, AerogpuCmdOpcode::SetRenderTargets as u32, |out| {
            push_u32(out, 1); // color_count
            push_u32(out, DS_HANDLE); // depth_stencil
            push_u32(out, RT_HANDLE);
            for _ in 0..7 {
                push_u32(out, 0);
            }
        });

        emit_packet(out, AerogpuCmdOpcode::SetViewport as u32, |out| {
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, width as f32);
            push_f32(out, height as f32);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
        });

        // Full target clear to red + depth=0.0 + stencil=0.
        emit_packet(out, AerogpuCmdOpcode::Clear as u32, |out| {
            push_u32(
                out,
                AEROGPU_CLEAR_COLOR | AEROGPU_CLEAR_DEPTH | AEROGPU_CLEAR_STENCIL,
            );
            push_f32(out, 1.0);
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 0.0); // depth
            push_u32(out, 0); // stencil
        });

        // Scissored clear to green + depth=1.0 + stencil=7.
        emit_packet(out, AerogpuCmdOpcode::SetRenderState as u32, |out| {
            push_u32(out, D3DRS_SCISSORTESTENABLE);
            push_u32(out, 1);
        });
        emit_packet(out, AerogpuCmdOpcode::SetScissor as u32, |out| {
            push_i32(out, scissor_x);
            push_i32(out, scissor_y);
            push_i32(out, scissor_w);
            push_i32(out, scissor_h);
        });
        emit_packet(out, AerogpuCmdOpcode::Clear as u32, |out| {
            push_u32(
                out,
                AEROGPU_CLEAR_COLOR | AEROGPU_CLEAR_DEPTH | AEROGPU_CLEAR_STENCIL,
            );
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 1.0); // depth
            push_u32(out, 7); // stencil
        });

        // Disable scissor for the depth-test draw so we can observe outside the rect.
        emit_packet(out, AerogpuCmdOpcode::SetRenderState as u32, |out| {
            push_u32(out, D3DRS_SCISSORTESTENABLE);
            push_u32(out, 0);
        });

        // Enable depth testing: draw at z=0.5 with Greater. This should pass only where the
        // depth remained 0.0 (outside the scissor rect), leaving the green clear inside.
        emit_packet(out, AerogpuCmdOpcode::SetRenderState as u32, |out| {
            push_u32(out, D3DRS_ZENABLE);
            push_u32(out, 1);
        });
        emit_packet(out, AerogpuCmdOpcode::SetRenderState as u32, |out| {
            push_u32(out, D3DRS_ZWRITEENABLE);
            push_u32(out, 0);
        });
        emit_packet(out, AerogpuCmdOpcode::SetRenderState as u32, |out| {
            push_u32(out, D3DRS_ZFUNC);
            push_u32(out, D3DCMP_GREATER);
        });

        // c0 = blue
        emit_packet(out, AerogpuCmdOpcode::SetShaderConstantsF as u32, |out| {
            push_u32(out, 1); // AEROGPU_SHADER_STAGE_PIXEL
            push_u32(out, 0); // start_register
            push_u32(out, 1); // vec4_count
            push_u32(out, 0); // reserved0
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 1.0);
        });

        emit_packet(out, AerogpuCmdOpcode::Draw as u32, |out| {
            push_u32(out, 3); // vertex_count
            push_u32(out, 1); // instance_count
            push_u32(out, 0); // first_vertex
            push_u32(out, 0); // first_instance
        });
    });

    exec.execute_cmd_stream(&stream)
        .expect("execute should succeed");

    let (_out_w, _out_h, rgba) = pollster::block_on(exec.readback_texture_rgba8(RT_HANDLE))
        .expect("readback should succeed");
    let (_out_w, _out_h, stencil) = pollster::block_on(exec.readback_texture_stencil8(DS_HANDLE))
        .expect("stencil readback should succeed");

    let blue = [0, 0, 255, 255];
    let green = [0, 255, 0, 255];

    // Inside scissor: clear set depth=1.0 so z=0.5 fails Greater, leaving green.
    assert_eq!(
        pixel_at(&rgba, width, (scissor_x + 1) as u32, (scissor_y + 1) as u32),
        green
    );

    // Outside scissor: depth remained 0.0 so z=0.5 passes Greater, drawing blue.
    assert_eq!(pixel_at(&rgba, width, 0, 0), blue);
    assert_eq!(pixel_at(&rgba, width, width - 1, height - 1), blue);
    assert_eq!(
        pixel_at(&rgba, width, (scissor_x - 1) as u32, scissor_y as u32),
        blue
    );

    assert_eq!(
        stencil_at(
            &stencil,
            width,
            (scissor_x + 1) as u32,
            (scissor_y + 1) as u32
        ),
        7
    );
    assert_eq!(stencil_at(&stencil, width, 0, 0), 0);
    assert_eq!(stencil_at(&stencil, width, width - 1, height - 1), 0);
}
