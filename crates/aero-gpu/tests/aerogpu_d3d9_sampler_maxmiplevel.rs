mod common;

use aero_gpu::{AerogpuD3d9Error, AerogpuD3d9Executor};
use aero_protocol::aerogpu::{
    aerogpu_cmd::{
        AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode,
        AerogpuCmdStreamHeader as ProtocolCmdStreamHeader, AerogpuPrimitiveTopology,
        AEROGPU_CLEAR_COLOR, AEROGPU_CMD_STREAM_MAGIC, AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
        AEROGPU_RESOURCE_USAGE_TEXTURE, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
    },
    aerogpu_pci::{AerogpuFormat, AEROGPU_ABI_VERSION_U32},
};

const CMD_STREAM_SIZE_BYTES_OFFSET: usize =
    core::mem::offset_of!(ProtocolCmdStreamHeader, size_bytes);
const CMD_HDR_SIZE_BYTES_OFFSET: usize = core::mem::offset_of!(ProtocolCmdHdr, size_bytes);

fn push_u8(out: &mut Vec<u8>, v: u8) {
    out.push(v);
}

fn push_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_u32(out: &mut Vec<u8>, v: u32) {
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

#[test]
fn d3d9_cmd_stream_sampler_maxmiplevel_skips_mip0() {
    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(module_path!(), "wgpu adapter not found");
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    const OPC_CREATE_BUFFER: u32 = AerogpuCmdOpcode::CreateBuffer as u32;
    const OPC_CREATE_TEXTURE2D: u32 = AerogpuCmdOpcode::CreateTexture2d as u32;
    const OPC_UPLOAD_RESOURCE: u32 = AerogpuCmdOpcode::UploadResource as u32;
    const OPC_COPY_TEXTURE2D: u32 = AerogpuCmdOpcode::CopyTexture2d as u32;
    const OPC_CREATE_SHADER_DXBC: u32 = AerogpuCmdOpcode::CreateShaderDxbc as u32;
    const OPC_BIND_SHADERS: u32 = AerogpuCmdOpcode::BindShaders as u32;
    const OPC_CREATE_INPUT_LAYOUT: u32 = AerogpuCmdOpcode::CreateInputLayout as u32;
    const OPC_SET_INPUT_LAYOUT: u32 = AerogpuCmdOpcode::SetInputLayout as u32;
    const OPC_SET_RENDER_TARGETS: u32 = AerogpuCmdOpcode::SetRenderTargets as u32;
    const OPC_SET_VIEWPORT: u32 = AerogpuCmdOpcode::SetViewport as u32;
    const OPC_SET_VERTEX_BUFFERS: u32 = AerogpuCmdOpcode::SetVertexBuffers as u32;
    const OPC_SET_PRIMITIVE_TOPOLOGY: u32 = AerogpuCmdOpcode::SetPrimitiveTopology as u32;
    const OPC_SET_TEXTURE: u32 = AerogpuCmdOpcode::SetTexture as u32;
    const OPC_SET_SAMPLER_STATE: u32 = AerogpuCmdOpcode::SetSamplerState as u32;
    const OPC_CLEAR: u32 = AerogpuCmdOpcode::Clear as u32;
    const OPC_DRAW: u32 = AerogpuCmdOpcode::Draw as u32;

    const AEROGPU_FORMAT_R8G8B8A8_UNORM: u32 = AerogpuFormat::R8G8B8A8Unorm as u32;
    const AEROGPU_TOPOLOGY_TRIANGLELIST: u32 = AerogpuPrimitiveTopology::TriangleList as u32;

    const STAGE_PIXEL: u32 = 1;

    // D3D9 sampler state IDs/enums.
    const D3DSAMP_ADDRESSU: u32 = 1;
    const D3DSAMP_ADDRESSV: u32 = 2;
    const D3DSAMP_MAGFILTER: u32 = 5;
    const D3DSAMP_MINFILTER: u32 = 6;
    const D3DSAMP_MIPFILTER: u32 = 7;
    const D3DSAMP_MAXMIPLEVEL: u32 = 9;

    const D3DTADDRESS_CLAMP: u32 = 3;
    const D3DTEXF_POINT: u32 = 1;

    const RT_HANDLE: u32 = 1;
    const SAMPLE_TEX_HANDLE: u32 = 2;
    const MIP1_SRC_TEX_HANDLE: u32 = 3;
    const VB_HANDLE: u32 = 4;
    const VS_HANDLE: u32 = 5;
    const PS_HANDLE: u32 = 6;
    const IL_HANDLE: u32 = 7;

    let width = 64u32;
    let height = 64u32;

    // Fullscreen triangle, with UVs spanning [0,1] across the viewport.
    // Each vertex is: float4 position, float4 texcoord.
    let verts: [[f32; 8]; 3] = [
        // pos.xyzw, tex.xyzw
        [-1.0f32, -1.0f32, 0.0f32, 1.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32],
        [-1.0f32, 3.0f32, 0.0f32, 1.0f32, 0.0f32, 2.0f32, 0.0f32, 0.0f32],
        [3.0f32, -1.0f32, 0.0f32, 1.0f32, 2.0f32, 0.0f32, 0.0f32, 0.0f32],
    ];
    let mut vb_data = Vec::new();
    for v in verts {
        for f in v {
            push_f32(&mut vb_data, f);
        }
    }
    assert_eq!(vb_data.len(), 3 * 32);

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

    let red_px = [255u8, 0, 0, 255];
    let green_px = [0u8, 255, 0, 255];
    let mip0 = red_px.repeat((width * height) as usize);
    let mip1 = green_px.repeat(((width / 2) * (height / 2)) as usize);

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

        // Sampled texture with 2 mip levels.
        emit_packet(out, OPC_CREATE_TEXTURE2D, |out| {
            push_u32(out, SAMPLE_TEX_HANDLE);
            push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE);
            push_u32(out, AEROGPU_FORMAT_R8G8B8A8_UNORM);
            push_u32(out, width);
            push_u32(out, height);
            push_u32(out, 2); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, width * 4); // row_pitch_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        // Source texture used to populate mip level 1 via CopyTexture2d.
        emit_packet(out, OPC_CREATE_TEXTURE2D, |out| {
            push_u32(out, MIP1_SRC_TEX_HANDLE);
            push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE);
            push_u32(out, AEROGPU_FORMAT_R8G8B8A8_UNORM);
            push_u32(out, width / 2);
            push_u32(out, height / 2);
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, (width / 2) * 4); // row_pitch_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        // Upload mip0 (red) into the sample texture.
        emit_packet(out, OPC_UPLOAD_RESOURCE, |out| {
            push_u32(out, SAMPLE_TEX_HANDLE);
            push_u32(out, 0); // reserved0
            push_u64(out, 0); // offset_bytes
            push_u64(out, mip0.len() as u64);
            out.extend_from_slice(&mip0);
        });

        // Upload mip1 source (green).
        emit_packet(out, OPC_UPLOAD_RESOURCE, |out| {
            push_u32(out, MIP1_SRC_TEX_HANDLE);
            push_u32(out, 0); // reserved0
            push_u64(out, 0); // offset_bytes
            push_u64(out, mip1.len() as u64);
            out.extend_from_slice(&mip1);
        });

        // Copy mip1 source into sample texture mip level 1.
        emit_packet(out, OPC_COPY_TEXTURE2D, |out| {
            push_u32(out, SAMPLE_TEX_HANDLE); // dst_texture
            push_u32(out, MIP1_SRC_TEX_HANDLE); // src_texture
            push_u32(out, 1); // dst_mip_level
            push_u32(out, 0); // dst_array_layer
            push_u32(out, 0); // src_mip_level
            push_u32(out, 0); // src_array_layer
            push_u32(out, 0); // dst_x
            push_u32(out, 0); // dst_y
            push_u32(out, 0); // src_x
            push_u32(out, 0); // src_y
            push_u32(out, width / 2); // width
            push_u32(out, height / 2); // height
            push_u32(out, 0); // flags
            push_u32(out, 0); // reserved0
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

        emit_packet(out, OPC_CLEAR, |out| {
            push_u32(out, AEROGPU_CLEAR_COLOR);
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 1.0); // depth
            push_u32(out, 0); // stencil
        });

        emit_packet(out, OPC_SET_TEXTURE, |out| {
            push_u32(out, STAGE_PIXEL);
            push_u32(out, 0); // slot
            push_u32(out, SAMPLE_TEX_HANDLE);
            push_u32(out, 0); // reserved0
        });

        for (state, value) in [
            (D3DSAMP_ADDRESSU, D3DTADDRESS_CLAMP),
            (D3DSAMP_ADDRESSV, D3DTADDRESS_CLAMP),
            (D3DSAMP_MINFILTER, D3DTEXF_POINT),
            (D3DSAMP_MAGFILTER, D3DTEXF_POINT),
            (D3DSAMP_MIPFILTER, D3DTEXF_POINT),
            (D3DSAMP_MAXMIPLEVEL, 1),
        ] {
            emit_packet(out, OPC_SET_SAMPLER_STATE, |out| {
                push_u32(out, STAGE_PIXEL);
                push_u32(out, 0); // slot
                push_u32(out, state);
                push_u32(out, value);
            });
        }

        emit_packet(out, OPC_DRAW, |out| {
            push_u32(out, 3); // vertex_count
            push_u32(out, 1); // instance_count
            push_u32(out, 0); // first_vertex
            push_u32(out, 0); // first_instance
        });
    });

    exec.execute_cmd_stream(&stream)
        .expect("execute should succeed");

    let (out_w, out_h, rgba) = pollster::block_on(exec.readback_texture_rgba8(RT_HANDLE))
        .expect("readback should succeed");
    assert_eq!((out_w, out_h), (width, height));

    let idx = ((height / 2 * width + width / 2) * 4) as usize;
    let px: [u8; 4] = rgba[idx..idx + 4].try_into().unwrap();
    assert_eq!(px, [0, 255, 0, 255], "expected mip1 texel");
}
