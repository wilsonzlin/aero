mod common;

use aero_gpu::{AerogpuD3d9Error, AerogpuD3d9Executor};

fn push_u8(out: &mut Vec<u8>, v: u8) {
    out.push(v);
}

fn push_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

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

fn align4(v: usize) -> usize {
    (v + 3) & !3
}

fn build_stream(packets: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    let mut out = Vec::new();

    // aerogpu_cmd_stream_header (24 bytes)
    push_u32(&mut out, 0x444D_4341); // "ACMD" little-endian
    push_u32(&mut out, 0x0001_0000); // abi_version (major=1 minor=0)
    push_u32(&mut out, 0); // size_bytes (patch later)
    push_u32(&mut out, 0); // flags
    push_u32(&mut out, 0); // reserved0
    push_u32(&mut out, 0); // reserved1

    packets(&mut out);

    let size_bytes = out.len() as u32;
    out[8..12].copy_from_slice(&size_bytes.to_le_bytes());
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
    out[start + 4..start + 8].copy_from_slice(&size_bytes.to_le_bytes());
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
    let token = (opcode as u32) | (((params.len() as u32) + 1) << 24);
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

fn assemble_vs_passthrough_pos_tex() -> Vec<u8> {
    // vs_2_0: mov oPos, v0; mov oT0, v1; end
    let mut words = vec![0xFFFE_0200];
    words.extend(enc_inst(0x0001, &[enc_dst(4, 0, 0xF), enc_src(1, 0, 0xE4)]));
    words.extend(enc_inst(0x0001, &[enc_dst(6, 0, 0xF), enc_src(1, 1, 0xE4)]));
    words.push(0x0000_FFFF);
    to_bytes(&words)
}

fn assemble_ps_texld() -> Vec<u8> {
    // ps_2_0: texld r0, t0, s0; mov oC0, r0; end
    let mut words = vec![0xFFFF_0200];
    words.extend(enc_inst(
        0x0042,
        &[
            enc_dst(0, 0, 0xF),   // r0
            enc_src(3, 0, 0xE4),  // t0
            enc_src(10, 0, 0xE4), // s0
        ],
    ));
    words.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 0, 0xE4)]));
    words.push(0x0000_FFFF);
    to_bytes(&words)
}

fn pixel_at(pixels: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let idx = ((y * width + x) * 4) as usize;
    pixels[idx..idx + 4].try_into().unwrap()
}

#[test]
fn d3d9_cmd_stream_render_state_and_sampler_state_are_honored() {
    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(
                concat!(
                    module_path!(),
                    "::d3d9_cmd_stream_render_state_and_sampler_state_are_honored"
                ),
                "wgpu adapter not found",
            );
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    // Protocol constants from `drivers/aerogpu/protocol/aerogpu_cmd.h`.
    const OPC_CREATE_BUFFER: u32 = 0x100;
    const OPC_CREATE_TEXTURE2D: u32 = 0x101;
    const OPC_UPLOAD_RESOURCE: u32 = 0x104;
    const OPC_CREATE_SHADER_DXBC: u32 = 0x200;
    const OPC_BIND_SHADERS: u32 = 0x202;
    const OPC_CREATE_INPUT_LAYOUT: u32 = 0x204;
    const OPC_SET_INPUT_LAYOUT: u32 = 0x206;
    const OPC_SET_RENDER_TARGETS: u32 = 0x400;
    const OPC_SET_VIEWPORT: u32 = 0x401;
    const OPC_SET_SCISSOR: u32 = 0x402;
    const OPC_SET_VERTEX_BUFFERS: u32 = 0x500;
    const OPC_SET_PRIMITIVE_TOPOLOGY: u32 = 0x502;
    const OPC_SET_TEXTURE: u32 = 0x510;
    const OPC_SET_SAMPLER_STATE: u32 = 0x511;
    const OPC_SET_RENDER_STATE: u32 = 0x512;
    const OPC_CLEAR: u32 = 0x600;
    const OPC_DRAW: u32 = 0x601;

    const AEROGPU_FORMAT_R8G8B8A8_UNORM: u32 = 3;
    const AEROGPU_RESOURCE_USAGE_TEXTURE: u32 = 1 << 3;
    const AEROGPU_RESOURCE_USAGE_RENDER_TARGET: u32 = 1 << 4;
    const AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER: u32 = 1 << 0;
    const AEROGPU_TOPOLOGY_TRIANGLELIST: u32 = 4;
    const AEROGPU_CLEAR_COLOR: u32 = 1 << 0;

    const STAGE_PIXEL: u32 = 1;

    // D3D9 render state IDs (subset).
    const D3DRS_ALPHABLENDENABLE: u32 = 27;
    const D3DRS_SRCBLEND: u32 = 19;
    const D3DRS_DESTBLEND: u32 = 20;
    const D3DRS_BLENDOP: u32 = 171;
    const D3DRS_SEPARATEALPHABLENDENABLE: u32 = 206;
    const D3DRS_SRCBLENDALPHA: u32 = 207;
    const D3DRS_DESTBLENDALPHA: u32 = 208;
    const D3DRS_BLENDOPALPHA: u32 = 209;
    const D3DRS_SCISSORTESTENABLE: u32 = 174;

    // D3D9 blend factors/ops.
    const D3DBLEND_ONE: u32 = 2;
    const D3DBLEND_SRCALPHA: u32 = 5;
    const D3DBLEND_INVSRCALPHA: u32 = 6;
    const D3DBLENDOP_ADD: u32 = 1;

    // D3D9 sampler state IDs (subset).
    const D3DSAMP_ADDRESSU: u32 = 1;
    const D3DSAMP_ADDRESSV: u32 = 2;
    const D3DSAMP_MAGFILTER: u32 = 5;
    const D3DSAMP_MINFILTER: u32 = 6;
    const D3DSAMP_MIPFILTER: u32 = 7;

    // D3D9 address/filter enums.
    const D3DTADDRESS_CLAMP: u32 = 3;
    const D3DTEXF_NONE: u32 = 0;
    const D3DTEXF_POINT: u32 = 1;
    const D3DTEXF_LINEAR: u32 = 2;

    const RT_HANDLE: u32 = 1;
    const VB_HANDLE: u32 = 2;
    const VS_HANDLE: u32 = 3;
    const PS_HANDLE: u32 = 4;
    const IL_HANDLE: u32 = 5;

    const TEX_RED: u32 = 10;
    const TEX_GREEN_ALPHA: u32 = 11;
    const TEX_BW: u32 = 12;
    const TEX_RED_HALF_ALPHA: u32 = 13;

    let width = 64u32;
    let height = 64u32;

    // Vertex buffer: two fullscreen quads (12 vertices total). Each vertex is:
    //   float4 position, float4 texcoord
    let mut vb_data = Vec::new();
    let quad_pos = [
        [-1.0f32, -1.0, 0.0, 1.0],
        [-1.0, 1.0, 0.0, 1.0],
        [1.0, -1.0, 0.0, 1.0],
        [-1.0, 1.0, 0.0, 1.0],
        [1.0, 1.0, 0.0, 1.0],
        [1.0, -1.0, 0.0, 1.0],
    ];

    // Quad A: UVs span [0, 1] (for alpha-blend test).
    let quad_uv = [
        [0.0f32, 0.0],
        [0.0, 1.0],
        [1.0, 0.0],
        [0.0, 1.0],
        [1.0, 1.0],
        [1.0, 0.0],
    ];

    // Quad B: constant UV near the boundary between two texels (for filter test).
    let uv_const = [0.49f32, 0.5f32];

    for (pos, uv) in quad_pos.iter().zip(quad_uv) {
        for &f in pos {
            push_f32(&mut vb_data, f);
        }
        push_f32(&mut vb_data, uv[0]);
        push_f32(&mut vb_data, uv[1]);
        push_f32(&mut vb_data, 0.0);
        push_f32(&mut vb_data, 0.0);
    }
    for pos in quad_pos {
        for f in pos {
            push_f32(&mut vb_data, f);
        }
        push_f32(&mut vb_data, uv_const[0]);
        push_f32(&mut vb_data, uv_const[1]);
        push_f32(&mut vb_data, 0.0);
        push_f32(&mut vb_data, 0.0);
    }

    let vs_bytes = assemble_vs_passthrough_pos_tex();
    let ps_bytes = assemble_ps_texld();

    // D3DVERTEXELEMENT9 stream (little-endian).
    // Element 0: POSITION0 float4 at stream 0 offset 0.
    // Element 1: TEXCOORD0 float4 at stream 0 offset 16.
    // End marker: stream 0xFF, type UNUSED.
    let mut vertex_decl = Vec::new();
    push_u16(&mut vertex_decl, 0); // stream
    push_u16(&mut vertex_decl, 0); // offset
    push_u8(&mut vertex_decl, 3); // type = FLOAT4
    push_u8(&mut vertex_decl, 0); // method
    push_u8(&mut vertex_decl, 0); // usage = POSITION
    push_u8(&mut vertex_decl, 0); // usage_index
    push_u16(&mut vertex_decl, 0); // stream
    push_u16(&mut vertex_decl, 16); // offset
    push_u8(&mut vertex_decl, 3); // type = FLOAT4
    push_u8(&mut vertex_decl, 0); // method
    push_u8(&mut vertex_decl, 5); // usage = TEXCOORD
    push_u8(&mut vertex_decl, 0); // usage_index
    push_u16(&mut vertex_decl, 0x00FF); // stream = 0xFF
    push_u16(&mut vertex_decl, 0); // offset
    push_u8(&mut vertex_decl, 17); // type = UNUSED
    push_u8(&mut vertex_decl, 0); // method
    push_u8(&mut vertex_decl, 0); // usage
    push_u8(&mut vertex_decl, 0); // usage_index
    assert_eq!(vertex_decl.len(), 24);

    let stream = build_stream(|out| {
        emit_packet(out, OPC_CREATE_TEXTURE2D, |out| {
            push_u32(out, RT_HANDLE);
            push_u32(
                out,
                AEROGPU_RESOURCE_USAGE_TEXTURE | AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
            );
            push_u32(out, AEROGPU_FORMAT_R8G8B8A8_UNORM);
            push_u32(out, width);
            push_u32(out, height);
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, width * 4); // row_pitch_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        // Sampled textures.
        emit_packet(out, OPC_CREATE_TEXTURE2D, |out| {
            push_u32(out, TEX_RED);
            push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE);
            push_u32(out, AEROGPU_FORMAT_R8G8B8A8_UNORM);
            push_u32(out, 1);
            push_u32(out, 1);
            push_u32(out, 1);
            push_u32(out, 1);
            push_u32(out, 4);
            push_u32(out, 0);
            push_u32(out, 0);
            push_u64(out, 0);
        });

        emit_packet(out, OPC_CREATE_TEXTURE2D, |out| {
            push_u32(out, TEX_RED_HALF_ALPHA);
            push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE);
            push_u32(out, AEROGPU_FORMAT_R8G8B8A8_UNORM);
            push_u32(out, 1);
            push_u32(out, 1);
            push_u32(out, 1);
            push_u32(out, 1);
            push_u32(out, 4);
            push_u32(out, 0);
            push_u32(out, 0);
            push_u64(out, 0);
        });

        emit_packet(out, OPC_CREATE_TEXTURE2D, |out| {
            push_u32(out, TEX_GREEN_ALPHA);
            push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE);
            push_u32(out, AEROGPU_FORMAT_R8G8B8A8_UNORM);
            push_u32(out, 2);
            push_u32(out, 1);
            push_u32(out, 1);
            push_u32(out, 1);
            push_u32(out, 8);
            push_u32(out, 0);
            push_u32(out, 0);
            push_u64(out, 0);
        });

        emit_packet(out, OPC_CREATE_TEXTURE2D, |out| {
            push_u32(out, TEX_BW);
            push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE);
            push_u32(out, AEROGPU_FORMAT_R8G8B8A8_UNORM);
            push_u32(out, 2);
            push_u32(out, 1);
            push_u32(out, 1);
            push_u32(out, 1);
            push_u32(out, 8);
            push_u32(out, 0);
            push_u32(out, 0);
            push_u64(out, 0);
        });

        // Upload sample textures.
        emit_packet(out, OPC_UPLOAD_RESOURCE, |out| {
            push_u32(out, TEX_RED);
            push_u32(out, 0);
            push_u64(out, 0);
            push_u64(out, 4);
            out.extend_from_slice(&[255, 0, 0, 255]);
        });
        emit_packet(out, OPC_UPLOAD_RESOURCE, |out| {
            push_u32(out, TEX_RED_HALF_ALPHA);
            push_u32(out, 0);
            push_u64(out, 0);
            push_u64(out, 4);
            out.extend_from_slice(&[255, 0, 0, 128]);
        });
        emit_packet(out, OPC_UPLOAD_RESOURCE, |out| {
            push_u32(out, TEX_GREEN_ALPHA);
            push_u32(out, 0);
            push_u64(out, 0);
            push_u64(out, 8);
            out.extend_from_slice(&[0, 255, 0, 0, 0, 255, 0, 255]);
        });
        emit_packet(out, OPC_UPLOAD_RESOURCE, |out| {
            push_u32(out, TEX_BW);
            push_u32(out, 0);
            push_u64(out, 0);
            push_u64(out, 8);
            out.extend_from_slice(&[
                0, 0, 0, 255, // black
                255, 255, 255, 255, // white
            ]);
        });

        emit_packet(out, OPC_CREATE_BUFFER, |out| {
            push_u32(out, VB_HANDLE);
            push_u32(out, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER);
            push_u64(out, vb_data.len() as u64);
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, OPC_UPLOAD_RESOURCE, |out| {
            push_u32(out, VB_HANDLE);
            push_u32(out, 0); // reserved0
            push_u64(out, 0); // offset_bytes
            push_u64(out, vb_data.len() as u64);
            out.extend_from_slice(&vb_data);
        });

        emit_packet(out, OPC_CREATE_SHADER_DXBC, |out| {
            push_u32(out, VS_HANDLE);
            push_u32(out, 0); // AEROGPU_SHADER_STAGE_VERTEX
            push_u32(out, vs_bytes.len() as u32);
            push_u32(out, 0); // reserved0
            out.extend_from_slice(&vs_bytes);
        });

        emit_packet(out, OPC_CREATE_SHADER_DXBC, |out| {
            push_u32(out, PS_HANDLE);
            push_u32(out, 1); // AEROGPU_SHADER_STAGE_PIXEL
            push_u32(out, ps_bytes.len() as u32);
            push_u32(out, 0); // reserved0
            out.extend_from_slice(&ps_bytes);
        });

        emit_packet(out, OPC_BIND_SHADERS, |out| {
            push_u32(out, VS_HANDLE);
            push_u32(out, PS_HANDLE);
            push_u32(out, 0); // cs
            push_u32(out, 0); // reserved0
        });

        emit_packet(out, OPC_CREATE_INPUT_LAYOUT, |out| {
            push_u32(out, IL_HANDLE);
            push_u32(out, vertex_decl.len() as u32);
            push_u32(out, 0); // reserved0
            out.extend_from_slice(&vertex_decl);
        });

        emit_packet(out, OPC_SET_INPUT_LAYOUT, |out| {
            push_u32(out, IL_HANDLE);
            push_u32(out, 0); // reserved0
        });

        emit_packet(out, OPC_SET_VERTEX_BUFFERS, |out| {
            push_u32(out, 0); // start_slot
            push_u32(out, 1); // buffer_count
            push_u32(out, VB_HANDLE);
            push_u32(out, 32); // stride_bytes
            push_u32(out, 0); // offset_bytes
            push_u32(out, 0); // reserved0
        });

        emit_packet(out, OPC_SET_PRIMITIVE_TOPOLOGY, |out| {
            push_u32(out, AEROGPU_TOPOLOGY_TRIANGLELIST);
            push_u32(out, 0); // reserved0
        });

        emit_packet(out, OPC_SET_RENDER_TARGETS, |out| {
            push_u32(out, 1); // color_count
            push_u32(out, 0); // depth_stencil
            push_u32(out, RT_HANDLE);
            for _ in 0..7 {
                push_u32(out, 0);
            }
        });

        emit_packet(out, OPC_SET_VIEWPORT, |out| {
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, width as f32);
            push_f32(out, height as f32);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
        });

        // Scissor rect is configured once; enable is controlled by render state.
        emit_packet(out, OPC_SET_SCISSOR, |out| {
            push_i32(out, 0);
            push_i32(out, 0);
            push_i32(out, width as i32);
            push_i32(out, height as i32);
        });

        emit_packet(out, OPC_CLEAR, |out| {
            push_u32(out, AEROGPU_CLEAR_COLOR);
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 1.0); // depth
            push_u32(out, 0); // stencil
        });

        // Sampler 0: point sample and clamp.
        for (state, value) in [
            (D3DSAMP_ADDRESSU, D3DTADDRESS_CLAMP),
            (D3DSAMP_ADDRESSV, D3DTADDRESS_CLAMP),
            (D3DSAMP_MINFILTER, D3DTEXF_POINT),
            (D3DSAMP_MAGFILTER, D3DTEXF_POINT),
            (D3DSAMP_MIPFILTER, D3DTEXF_NONE),
        ] {
            emit_packet(out, OPC_SET_SAMPLER_STATE, |out| {
                push_u32(out, STAGE_PIXEL);
                push_u32(out, 0); // slot
                push_u32(out, state);
                push_u32(out, value);
            });
        }

        // Disable scissor for the blend test.
        emit_packet(out, OPC_SET_RENDER_STATE, |out| {
            push_u32(out, D3DRS_SCISSORTESTENABLE);
            push_u32(out, 0);
        });

        // Background: red, blending disabled.
        emit_packet(out, OPC_SET_RENDER_STATE, |out| {
            push_u32(out, D3DRS_ALPHABLENDENABLE);
            push_u32(out, 0);
        });
        emit_packet(out, OPC_SET_TEXTURE, |out| {
            push_u32(out, STAGE_PIXEL);
            push_u32(out, 0); // slot
            push_u32(out, TEX_RED);
            push_u32(out, 0); // reserved0
        });
        emit_packet(out, OPC_DRAW, |out| {
            push_u32(out, 6); // vertex_count
            push_u32(out, 1); // instance_count
            push_u32(out, 0); // first_vertex
            push_u32(out, 0); // first_instance
        });

        // Overlay: green with varying alpha, blending enabled.
        emit_packet(out, OPC_SET_TEXTURE, |out| {
            push_u32(out, STAGE_PIXEL);
            push_u32(out, 0); // slot
            push_u32(out, TEX_GREEN_ALPHA);
            push_u32(out, 0); // reserved0
        });
        emit_packet(out, OPC_SET_RENDER_STATE, |out| {
            push_u32(out, D3DRS_ALPHABLENDENABLE);
            push_u32(out, 1);
        });
        emit_packet(out, OPC_SET_RENDER_STATE, |out| {
            push_u32(out, D3DRS_SRCBLEND);
            push_u32(out, D3DBLEND_SRCALPHA);
        });
        emit_packet(out, OPC_SET_RENDER_STATE, |out| {
            push_u32(out, D3DRS_DESTBLEND);
            push_u32(out, D3DBLEND_INVSRCALPHA);
        });
        emit_packet(out, OPC_SET_RENDER_STATE, |out| {
            push_u32(out, D3DRS_BLENDOP);
            push_u32(out, D3DBLENDOP_ADD);
        });
        emit_packet(out, OPC_DRAW, |out| {
            push_u32(out, 6); // vertex_count
            push_u32(out, 1); // instance_count
            push_u32(out, 0); // first_vertex
            push_u32(out, 0); // first_instance
        });

        // Filter test: draw the constant-UV quad into the top strip, point vs linear.
        emit_packet(out, OPC_SET_RENDER_STATE, |out| {
            push_u32(out, D3DRS_ALPHABLENDENABLE);
            push_u32(out, 0);
        });
        emit_packet(out, OPC_SET_RENDER_STATE, |out| {
            push_u32(out, D3DRS_SCISSORTESTENABLE);
            push_u32(out, 1);
        });
        emit_packet(out, OPC_SET_TEXTURE, |out| {
            push_u32(out, STAGE_PIXEL);
            push_u32(out, 0); // slot
            push_u32(out, TEX_BW);
            push_u32(out, 0); // reserved0
        });

        // Left half: point sampling.
        emit_packet(out, OPC_SET_SCISSOR, |out| {
            push_i32(out, 0);
            push_i32(out, 0);
            push_i32(out, 32);
            push_i32(out, 16);
        });
        for (state, value) in [
            (D3DSAMP_MINFILTER, D3DTEXF_POINT),
            (D3DSAMP_MAGFILTER, D3DTEXF_POINT),
        ] {
            emit_packet(out, OPC_SET_SAMPLER_STATE, |out| {
                push_u32(out, STAGE_PIXEL);
                push_u32(out, 0); // slot
                push_u32(out, state);
                push_u32(out, value);
            });
        }
        emit_packet(out, OPC_DRAW, |out| {
            push_u32(out, 6); // vertex_count
            push_u32(out, 1); // instance_count
            push_u32(out, 6); // first_vertex (constant-UV quad)
            push_u32(out, 0); // first_instance
        });

        // Right half: linear sampling.
        emit_packet(out, OPC_SET_SCISSOR, |out| {
            push_i32(out, 32);
            push_i32(out, 0);
            push_i32(out, 32);
            push_i32(out, 16);
        });
        for (state, value) in [
            (D3DSAMP_MINFILTER, D3DTEXF_LINEAR),
            (D3DSAMP_MAGFILTER, D3DTEXF_LINEAR),
        ] {
            emit_packet(out, OPC_SET_SAMPLER_STATE, |out| {
                push_u32(out, STAGE_PIXEL);
                push_u32(out, 0); // slot
                push_u32(out, state);
                push_u32(out, value);
            });
        }
        emit_packet(out, OPC_DRAW, |out| {
            push_u32(out, 6); // vertex_count
            push_u32(out, 1); // instance_count
            push_u32(out, 6); // first_vertex (constant-UV quad)
            push_u32(out, 0); // first_instance
        });

        // Separate-alpha blend test: write into a small bottom-left region so we don't clobber the
        // previous assertions.
        emit_packet(out, OPC_SET_SCISSOR, |out| {
            push_i32(out, 0);
            push_i32(out, 48);
            push_i32(out, 16);
            push_i32(out, 16);
        });
        emit_packet(out, OPC_CLEAR, |out| {
            push_u32(out, AEROGPU_CLEAR_COLOR);
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 0.0); // alpha = 0
            push_f32(out, 1.0); // depth
            push_u32(out, 0); // stencil
        });
        emit_packet(out, OPC_SET_TEXTURE, |out| {
            push_u32(out, STAGE_PIXEL);
            push_u32(out, 0); // slot
            push_u32(out, TEX_RED_HALF_ALPHA);
            push_u32(out, 0); // reserved0
        });
        for (state, value) in [
            (D3DRS_ALPHABLENDENABLE, 1),
            (D3DRS_SRCBLEND, D3DBLEND_SRCALPHA),
            (D3DRS_DESTBLEND, D3DBLEND_INVSRCALPHA),
            (D3DRS_BLENDOP, D3DBLENDOP_ADD),
            (D3DRS_SEPARATEALPHABLENDENABLE, 1),
            (D3DRS_SRCBLENDALPHA, D3DBLEND_ONE),
            (D3DRS_DESTBLENDALPHA, D3DBLEND_INVSRCALPHA),
            (D3DRS_BLENDOPALPHA, D3DBLENDOP_ADD),
        ] {
            emit_packet(out, OPC_SET_RENDER_STATE, |out| {
                push_u32(out, state);
                push_u32(out, value);
            });
        }
        emit_packet(out, OPC_DRAW, |out| {
            push_u32(out, 6); // vertex_count
            push_u32(out, 1); // instance_count
            push_u32(out, 0); // first_vertex
            push_u32(out, 0); // first_instance
        });
    });

    exec.execute_cmd_stream(&stream)
        .expect("execute should succeed");

    let (out_w, _out_h, rgba) = pollster::block_on(exec.readback_texture_rgba8(RT_HANDLE))
        .expect("readback should succeed");
    assert_eq!(out_w, width);

    // Alpha blend validation: left half samples alpha=0 texel -> should remain red.
    assert_eq!(pixel_at(&rgba, width, 8, 32), [255, 0, 0, 255]);
    // Right half samples alpha=255 texel -> should be green.
    assert_eq!(pixel_at(&rgba, width, 56, 32), [0, 255, 0, 255]);

    // Filter validation: top strip is overwritten by point (left) and linear (right).
    let left = pixel_at(&rgba, width, 16, 8);
    let right = pixel_at(&rgba, width, 48, 8);
    assert_eq!(left, [0, 0, 0, 255]);
    assert!(
        (100..=150).contains(&right[0]) && right[0] == right[1] && right[1] == right[2],
        "expected linear sampling to produce gray, got {right:?}"
    );

    // Separate alpha blend validation: color uses SRCALPHA but alpha should use ONE when separate
    // alpha blending is enabled. Without separate alpha, we'd observe alpha ~= 64 instead of 128.
    let separate_alpha = pixel_at(&rgba, width, 8, 56);
    assert!(
        (110..=145).contains(&separate_alpha[0]),
        "expected blended red channel ~= 128, got {separate_alpha:?}"
    );
    assert!(
        (110..=145).contains(&separate_alpha[3]),
        "expected blended alpha channel ~= 128, got {separate_alpha:?}"
    );
}
