mod common;

use aero_protocol::aerogpu::aerogpu_cmd as cmd;
use aero_protocol::aerogpu::aerogpu_pci as pci;

const CMD_STREAM_SIZE_BYTES_OFFSET: usize =
    core::mem::offset_of!(cmd::AerogpuCmdStreamHeader, size_bytes);
const CMD_HDR_SIZE_BYTES_OFFSET: usize = core::mem::offset_of!(cmd::AerogpuCmdHdr, size_bytes);

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
    push_u32(&mut out, cmd::AEROGPU_CMD_STREAM_MAGIC);
    push_u32(&mut out, pci::AEROGPU_ABI_VERSION_U32);
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
    // D3D9 SM2/SM3 encodes the *total* instruction length in DWORD tokens (including the opcode
    // token) in bits 24..27.
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
    // vs_2_0:
    //   mov oPos, v0
    //   mov oT0, v1
    //   end
    let mut words = vec![0xFFFE_0200];
    words.extend(enc_inst(0x0001, &[enc_dst(4, 0, 0xF), enc_src(1, 0, 0xE4)]));
    words.extend(enc_inst(0x0001, &[enc_dst(6, 0, 0xF), enc_src(1, 1, 0xE4)]));
    words.push(0x0000_FFFF);
    to_bytes(&words)
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

fn assemble_ps_solid_color_c0_c1() -> Vec<u8> {
    // ps_3_0:
    //   mov oC0, c0
    //   mov oC1, c0
    //   end
    let mut words = vec![0xFFFF_0300];
    words.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(2, 0, 0xE4)]));
    words.extend(enc_inst(0x0001, &[enc_dst(8, 1, 0xF), enc_src(2, 0, 0xE4)]));
    words.push(0x0000_FFFF);
    to_bytes(&words)
}

fn assemble_ps_tex() -> Vec<u8> {
    // ps_2_0:
    //   texld r0, t0, s0
    //   mov oC0, r0
    //   end
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

fn fullscreen_triangle_pos_tex(uv: [[f32; 2]; 3]) -> Vec<u8> {
    let mut vb = Vec::new();
    // D3D9 defaults to back-face culling with clockwise front faces, so keep the fullscreen
    // triangle winding clockwise.
    let verts = [
        ([-1.0f32, -1.0, 0.0, 1.0], [uv[0][0], uv[0][1], 0.0, 0.0]),
        ([-1.0f32, 3.0, 0.0, 1.0], [uv[2][0], uv[2][1], 0.0, 0.0]),
        ([3.0f32, -1.0, 0.0, 1.0], [uv[1][0], uv[1][1], 0.0, 0.0]),
    ];
    for (pos, tex) in verts {
        for f in pos {
            push_f32(&mut vb, f);
        }
        for f in tex {
            push_f32(&mut vb, f);
        }
    }
    vb
}

fn fullscreen_triangle_pos() -> Vec<u8> {
    let mut vb = Vec::new();
    // D3D9 defaults to back-face culling with clockwise front faces, so keep the fullscreen
    // triangle winding clockwise.
    let verts = [
        [-1.0f32, -1.0, 0.0, 1.0],
        [-1.0f32, 3.0, 0.0, 1.0],
        [3.0f32, -1.0, 0.0, 1.0],
    ];
    for pos in verts {
        for f in pos {
            push_f32(&mut vb, f);
        }
    }
    vb
}

fn vertex_decl_pos() -> Vec<u8> {
    // D3DVERTEXELEMENT9 stream (little-endian).
    // Element 0: POSITION0 float4 at stream 0 offset 0.
    // End marker: stream 0xFF, type UNUSED.
    let mut decl = Vec::new();
    // POSITION0
    push_u16(&mut decl, 0); // stream
    push_u16(&mut decl, 0); // offset
    push_u8(&mut decl, 3); // type = FLOAT4
    push_u8(&mut decl, 0); // method
    push_u8(&mut decl, 0); // usage = POSITION
    push_u8(&mut decl, 0); // usage_index
                           // End marker
    push_u16(&mut decl, 0x00FF); // stream = 0xFF
    push_u16(&mut decl, 0); // offset
    push_u8(&mut decl, 17); // type = UNUSED
    push_u8(&mut decl, 0); // method
    push_u8(&mut decl, 0); // usage
    push_u8(&mut decl, 0); // usage_index
    decl
}

fn vertex_decl_pos_tex() -> Vec<u8> {
    // D3DVERTEXELEMENT9 stream (little-endian).
    // Element 0: POSITION0 float4 at stream 0 offset 0.
    // Element 1: TEXCOORD0 float4 at stream 0 offset 16.
    // End marker: stream 0xFF, type UNUSED.
    let mut decl = Vec::new();
    // POSITION0
    push_u16(&mut decl, 0); // stream
    push_u16(&mut decl, 0); // offset
    push_u8(&mut decl, 3); // type = FLOAT4
    push_u8(&mut decl, 0); // method
    push_u8(&mut decl, 0); // usage = POSITION
    push_u8(&mut decl, 0); // usage_index
                           // TEXCOORD0
    push_u16(&mut decl, 0); // stream
    push_u16(&mut decl, 16); // offset
    push_u8(&mut decl, 3); // type = FLOAT4
    push_u8(&mut decl, 0); // method
    push_u8(&mut decl, 5); // usage = TEXCOORD
    push_u8(&mut decl, 0); // usage_index
                           // End marker
    push_u16(&mut decl, 0x00FF); // stream = 0xFF
    push_u16(&mut decl, 0); // offset
    push_u8(&mut decl, 17); // type = UNUSED
    push_u8(&mut decl, 0); // method
    push_u8(&mut decl, 0); // usage
    push_u8(&mut decl, 0); // usage_index
    decl
}

#[test]
fn d3d9_cmd_stream_alpha_blend_srcalpha_invsrcalpha() {
    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    // Protocol constants from `aero-protocol`.
    const OPC_CREATE_BUFFER: u32 = cmd::AerogpuCmdOpcode::CreateBuffer as u32;
    const OPC_CREATE_TEXTURE2D: u32 = cmd::AerogpuCmdOpcode::CreateTexture2d as u32;
    const OPC_UPLOAD_RESOURCE: u32 = cmd::AerogpuCmdOpcode::UploadResource as u32;
    const OPC_CREATE_SHADER_DXBC: u32 = cmd::AerogpuCmdOpcode::CreateShaderDxbc as u32;
    const OPC_BIND_SHADERS: u32 = cmd::AerogpuCmdOpcode::BindShaders as u32;
    const OPC_CREATE_INPUT_LAYOUT: u32 = cmd::AerogpuCmdOpcode::CreateInputLayout as u32;
    const OPC_SET_INPUT_LAYOUT: u32 = cmd::AerogpuCmdOpcode::SetInputLayout as u32;
    const OPC_SET_RENDER_TARGETS: u32 = cmd::AerogpuCmdOpcode::SetRenderTargets as u32;
    const OPC_SET_VIEWPORT: u32 = cmd::AerogpuCmdOpcode::SetViewport as u32;
    const OPC_SET_VERTEX_BUFFERS: u32 = cmd::AerogpuCmdOpcode::SetVertexBuffers as u32;
    const OPC_SET_PRIMITIVE_TOPOLOGY: u32 = cmd::AerogpuCmdOpcode::SetPrimitiveTopology as u32;
    const OPC_SET_TEXTURE: u32 = cmd::AerogpuCmdOpcode::SetTexture as u32;
    const OPC_SET_SAMPLER_STATE: u32 = cmd::AerogpuCmdOpcode::SetSamplerState as u32;
    const OPC_SET_RENDER_STATE: u32 = cmd::AerogpuCmdOpcode::SetRenderState as u32;
    const OPC_CLEAR: u32 = cmd::AerogpuCmdOpcode::Clear as u32;
    const OPC_DRAW: u32 = cmd::AerogpuCmdOpcode::Draw as u32;
    const OPC_PRESENT: u32 = cmd::AerogpuCmdOpcode::Present as u32;

    // D3D9 render/sampler state IDs (subset).
    const D3DRS_ALPHABLENDENABLE: u32 = 27;
    const D3DRS_SRCBLEND: u32 = 19;
    const D3DRS_DESTBLEND: u32 = 20;
    const D3DRS_BLENDOP: u32 = 171;

    const D3DSAMP_ADDRESSU: u32 = 1;
    const D3DSAMP_ADDRESSV: u32 = 2;
    const D3DSAMP_MAGFILTER: u32 = 5;
    const D3DSAMP_MINFILTER: u32 = 6;
    const D3DSAMP_MIPFILTER: u32 = 7;

    // D3D9 blend/sampler enums.
    const D3DBLEND_SRCALPHA: u32 = 5;
    const D3DBLEND_INVSRCALPHA: u32 = 6;
    const D3DBLENDOP_ADD: u32 = 1;

    const D3DTADDRESS_CLAMP: u32 = 3;
    const D3DTEXF_NONE: u32 = 0;
    const D3DTEXF_POINT: u32 = 1;

    const AEROGPU_FORMAT_R8G8B8A8_UNORM: u32 = pci::AerogpuFormat::R8G8B8A8Unorm as u32;
    const AEROGPU_RESOURCE_USAGE_TEXTURE: u32 = cmd::AEROGPU_RESOURCE_USAGE_TEXTURE;
    const AEROGPU_RESOURCE_USAGE_RENDER_TARGET: u32 = cmd::AEROGPU_RESOURCE_USAGE_RENDER_TARGET;
    const AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER: u32 = cmd::AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER;
    const AEROGPU_TOPOLOGY_TRIANGLELIST: u32 = cmd::AerogpuPrimitiveTopology::TriangleList as u32;
    const AEROGPU_CLEAR_COLOR: u32 = cmd::AEROGPU_CLEAR_COLOR;

    const RT_HANDLE: u32 = 1;
    const VB_HANDLE: u32 = 2;
    const VS_HANDLE: u32 = 3;
    const PS_HANDLE: u32 = 4;
    const IL_HANDLE: u32 = 5;
    const TEX_HANDLE: u32 = 6;

    let width = 64u32;
    let height = 64u32;

    let vertex_decl = vertex_decl_pos_tex();
    assert_eq!(vertex_decl.len(), 24);

    let vb_data = fullscreen_triangle_pos_tex([[0.0, 0.5], [2.0, 0.5], [0.0, 0.5]]);
    assert_eq!(vb_data.len(), 3 * 32);

    // Texture: two texels, both green but left alpha=0 and right alpha=255.
    let tex_data: [u8; 8] = [0, 255, 0, 0, 0, 255, 0, 255];

    let vs_bytes = assemble_vs_passthrough_pos_tex();
    let ps_bytes = assemble_ps_tex();

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

        emit_packet(out, OPC_CREATE_TEXTURE2D, |out| {
            push_u32(out, TEX_HANDLE);
            push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE);
            push_u32(out, AEROGPU_FORMAT_R8G8B8A8_UNORM);
            push_u32(out, 2); // width
            push_u32(out, 1); // height
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, 2 * 4); // row_pitch_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, OPC_UPLOAD_RESOURCE, |out| {
            push_u32(out, TEX_HANDLE);
            push_u32(out, 0); // reserved0
            push_u64(out, 0); // offset_bytes
            push_u64(out, tex_data.len() as u64);
            out.extend_from_slice(&tex_data);
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
            push_u32(out, cmd::AerogpuShaderStage::Vertex as u32);
            push_u32(out, vs_bytes.len() as u32);
            push_u32(out, 0); // reserved0
            out.extend_from_slice(&vs_bytes);
        });

        emit_packet(out, OPC_CREATE_SHADER_DXBC, |out| {
            push_u32(out, PS_HANDLE);
            push_u32(out, cmd::AerogpuShaderStage::Pixel as u32);
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
            // Clear red.
            push_u32(out, AEROGPU_CLEAR_COLOR);
            push_f32(out, 1.0);
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 1.0); // depth
            push_u32(out, 0); // stencil
        });

        emit_packet(out, OPC_SET_TEXTURE, |out| {
            push_u32(out, cmd::AerogpuShaderStage::Pixel as u32);
            push_u32(out, 0); // slot
            push_u32(out, TEX_HANDLE);
            push_u32(out, 0); // reserved0
        });

        emit_packet(out, OPC_SET_SAMPLER_STATE, |out| {
            push_u32(out, cmd::AerogpuShaderStage::Pixel as u32);
            push_u32(out, 0); // slot
            push_u32(out, D3DSAMP_MINFILTER);
            push_u32(out, D3DTEXF_POINT);
        });
        emit_packet(out, OPC_SET_SAMPLER_STATE, |out| {
            push_u32(out, cmd::AerogpuShaderStage::Pixel as u32);
            push_u32(out, 0); // slot
            push_u32(out, D3DSAMP_MAGFILTER);
            push_u32(out, D3DTEXF_POINT);
        });
        emit_packet(out, OPC_SET_SAMPLER_STATE, |out| {
            push_u32(out, cmd::AerogpuShaderStage::Pixel as u32);
            push_u32(out, 0); // slot
            push_u32(out, D3DSAMP_MIPFILTER);
            push_u32(out, D3DTEXF_NONE);
        });
        emit_packet(out, OPC_SET_SAMPLER_STATE, |out| {
            push_u32(out, cmd::AerogpuShaderStage::Pixel as u32);
            push_u32(out, 0); // slot
            push_u32(out, D3DSAMP_ADDRESSU);
            push_u32(out, D3DTADDRESS_CLAMP);
        });
        emit_packet(out, OPC_SET_SAMPLER_STATE, |out| {
            push_u32(out, cmd::AerogpuShaderStage::Pixel as u32);
            push_u32(out, 0); // slot
            push_u32(out, D3DSAMP_ADDRESSV);
            push_u32(out, D3DTADDRESS_CLAMP);
        });

        // Enable blending.
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
            push_u32(out, 3); // vertex_count
            push_u32(out, 1); // instance_count
            push_u32(out, 0); // first_vertex
            push_u32(out, 0); // first_instance
        });

        emit_packet(out, OPC_PRESENT, |out| {
            push_u32(out, 0); // scanout_id
            push_u32(out, 0); // flags
        });
    });

    exec.execute_cmd_stream(&stream)
        .expect("execute should succeed");

    let (out_w, out_h, rgba) = pollster::block_on(exec.readback_texture_rgba8(RT_HANDLE))
        .expect("readback should succeed");
    assert_eq!((out_w, out_h), (width, height));

    // Left side samples alpha=0 texel, should remain red after blending.
    assert_eq!(pixel_at(&rgba, width, 8, 32), [255, 0, 0, 255]);
    // Right side samples alpha=255 texel, should be green.
    assert_eq!(pixel_at(&rgba, width, 56, 32), [0, 255, 0, 255]);
}

#[test]
fn d3d9_cmd_stream_blend_factor_constant() {
    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    // Protocol constants from `aero-protocol`.
    const OPC_CREATE_BUFFER: u32 = cmd::AerogpuCmdOpcode::CreateBuffer as u32;
    const OPC_CREATE_TEXTURE2D: u32 = cmd::AerogpuCmdOpcode::CreateTexture2d as u32;
    const OPC_UPLOAD_RESOURCE: u32 = cmd::AerogpuCmdOpcode::UploadResource as u32;
    const OPC_CREATE_SHADER_DXBC: u32 = cmd::AerogpuCmdOpcode::CreateShaderDxbc as u32;
    const OPC_BIND_SHADERS: u32 = cmd::AerogpuCmdOpcode::BindShaders as u32;
    const OPC_SET_SHADER_CONSTANTS_F: u32 = cmd::AerogpuCmdOpcode::SetShaderConstantsF as u32;
    const OPC_CREATE_INPUT_LAYOUT: u32 = cmd::AerogpuCmdOpcode::CreateInputLayout as u32;
    const OPC_SET_INPUT_LAYOUT: u32 = cmd::AerogpuCmdOpcode::SetInputLayout as u32;
    const OPC_SET_RENDER_TARGETS: u32 = cmd::AerogpuCmdOpcode::SetRenderTargets as u32;
    const OPC_SET_VIEWPORT: u32 = cmd::AerogpuCmdOpcode::SetViewport as u32;
    const OPC_SET_VERTEX_BUFFERS: u32 = cmd::AerogpuCmdOpcode::SetVertexBuffers as u32;
    const OPC_SET_PRIMITIVE_TOPOLOGY: u32 = cmd::AerogpuCmdOpcode::SetPrimitiveTopology as u32;
    const OPC_SET_RENDER_STATE: u32 = cmd::AerogpuCmdOpcode::SetRenderState as u32;
    const OPC_CLEAR: u32 = cmd::AerogpuCmdOpcode::Clear as u32;
    const OPC_DRAW: u32 = cmd::AerogpuCmdOpcode::Draw as u32;

    // D3D9 render state IDs (subset).
    const D3DRS_ALPHABLENDENABLE: u32 = 27;
    const D3DRS_SRCBLEND: u32 = 19;
    const D3DRS_DESTBLEND: u32 = 20;
    const D3DRS_BLENDOP: u32 = 171;
    const D3DRS_BLENDFACTOR: u32 = 193;

    // D3D9 blend enums.
    const D3DBLEND_BLENDFACTOR: u32 = 14;
    const D3DBLEND_INVBLENDFACTOR: u32 = 15;
    const D3DBLENDOP_ADD: u32 = 1;

    const AEROGPU_FORMAT_R8G8B8A8_UNORM: u32 = pci::AerogpuFormat::R8G8B8A8Unorm as u32;
    const AEROGPU_RESOURCE_USAGE_TEXTURE: u32 = cmd::AEROGPU_RESOURCE_USAGE_TEXTURE;
    const AEROGPU_RESOURCE_USAGE_RENDER_TARGET: u32 = cmd::AEROGPU_RESOURCE_USAGE_RENDER_TARGET;
    const AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER: u32 = cmd::AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER;
    const AEROGPU_TOPOLOGY_TRIANGLELIST: u32 = cmd::AerogpuPrimitiveTopology::TriangleList as u32;
    const AEROGPU_CLEAR_COLOR: u32 = cmd::AEROGPU_CLEAR_COLOR;

    const RT_HANDLE: u32 = 1;
    const VB_HANDLE: u32 = 2;
    const VS_HANDLE: u32 = 3;
    const PS_HANDLE: u32 = 4;
    const IL_HANDLE: u32 = 5;

    let width = 64u32;
    let height = 64u32;

    let vertex_decl = vertex_decl_pos();
    let vb_data = fullscreen_triangle_pos();

    let vs_bytes = assemble_vs_passthrough_pos();
    let ps_bytes = assemble_ps_solid_color_c0();

    // D3DCOLOR = 0xAARRGGBB.
    let blend_factor_argb: u32 = 0x40_80_20_E0;

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
            push_u32(out, cmd::AerogpuShaderStage::Vertex as u32);
            push_u32(out, vs_bytes.len() as u32);
            push_u32(out, 0); // reserved0
            out.extend_from_slice(&vs_bytes);
        });

        emit_packet(out, OPC_CREATE_SHADER_DXBC, |out| {
            push_u32(out, PS_HANDLE);
            push_u32(out, cmd::AerogpuShaderStage::Pixel as u32);
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
            push_u32(out, 16); // stride_bytes
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
            // Clear black with alpha=0 so the blend-factor alpha is observable.
            push_u32(out, AEROGPU_CLEAR_COLOR);
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0); // depth
            push_u32(out, 0); // stencil
        });

        for (state, value) in [
            (D3DRS_ALPHABLENDENABLE, 1),
            (D3DRS_SRCBLEND, D3DBLEND_BLENDFACTOR),
            (D3DRS_DESTBLEND, D3DBLEND_INVBLENDFACTOR),
            (D3DRS_BLENDOP, D3DBLENDOP_ADD),
            (D3DRS_BLENDFACTOR, blend_factor_argb),
        ] {
            emit_packet(out, OPC_SET_RENDER_STATE, |out| {
                push_u32(out, state);
                push_u32(out, value);
            });
        }

        // Set c0 to white.
        emit_packet(out, OPC_SET_SHADER_CONSTANTS_F, |out| {
            push_u32(out, cmd::AerogpuShaderStage::Pixel as u32);
            push_u32(out, 0); // start_register
            push_u32(out, 1); // vec4_count
            push_u32(out, 0); // reserved0
            push_f32(out, 1.0);
            push_f32(out, 1.0);
            push_f32(out, 1.0);
            push_f32(out, 1.0);
        });

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

    let px = pixel_at(&rgba, width, 32, 32);
    assert!(
        (127..=129).contains(&px[0])
            && (31..=33).contains(&px[1])
            && (223..=225).contains(&px[2])
            && (63..=65).contains(&px[3]),
        "expected output ~= [128, 32, 224, 64], got {px:?}"
    );
}

#[test]
fn d3d9_cmd_stream_sample_mask_discards_draw() {
    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    // Protocol constants from `aero-protocol`.
    const OPC_CREATE_BUFFER: u32 = cmd::AerogpuCmdOpcode::CreateBuffer as u32;
    const OPC_CREATE_TEXTURE2D: u32 = cmd::AerogpuCmdOpcode::CreateTexture2d as u32;
    const OPC_UPLOAD_RESOURCE: u32 = cmd::AerogpuCmdOpcode::UploadResource as u32;
    const OPC_CREATE_SHADER_DXBC: u32 = cmd::AerogpuCmdOpcode::CreateShaderDxbc as u32;
    const OPC_BIND_SHADERS: u32 = cmd::AerogpuCmdOpcode::BindShaders as u32;
    const OPC_SET_SHADER_CONSTANTS_F: u32 = cmd::AerogpuCmdOpcode::SetShaderConstantsF as u32;
    const OPC_CREATE_INPUT_LAYOUT: u32 = cmd::AerogpuCmdOpcode::CreateInputLayout as u32;
    const OPC_SET_INPUT_LAYOUT: u32 = cmd::AerogpuCmdOpcode::SetInputLayout as u32;
    const OPC_SET_RENDER_TARGETS: u32 = cmd::AerogpuCmdOpcode::SetRenderTargets as u32;
    const OPC_SET_VIEWPORT: u32 = cmd::AerogpuCmdOpcode::SetViewport as u32;
    const OPC_SET_VERTEX_BUFFERS: u32 = cmd::AerogpuCmdOpcode::SetVertexBuffers as u32;
    const OPC_SET_PRIMITIVE_TOPOLOGY: u32 = cmd::AerogpuCmdOpcode::SetPrimitiveTopology as u32;
    const OPC_SET_BLEND_STATE: u32 = cmd::AerogpuCmdOpcode::SetBlendState as u32;
    const OPC_CLEAR: u32 = cmd::AerogpuCmdOpcode::Clear as u32;
    const OPC_DRAW: u32 = cmd::AerogpuCmdOpcode::Draw as u32;

    const AEROGPU_FORMAT_R8G8B8A8_UNORM: u32 = pci::AerogpuFormat::R8G8B8A8Unorm as u32;
    const AEROGPU_RESOURCE_USAGE_TEXTURE: u32 = cmd::AEROGPU_RESOURCE_USAGE_TEXTURE;
    const AEROGPU_RESOURCE_USAGE_RENDER_TARGET: u32 = cmd::AEROGPU_RESOURCE_USAGE_RENDER_TARGET;
    const AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER: u32 = cmd::AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER;
    const AEROGPU_TOPOLOGY_TRIANGLELIST: u32 = cmd::AerogpuPrimitiveTopology::TriangleList as u32;
    const AEROGPU_CLEAR_COLOR: u32 = cmd::AEROGPU_CLEAR_COLOR;

    const RT_HANDLE: u32 = 1;
    const VB_HANDLE: u32 = 2;
    const VS_HANDLE: u32 = 3;
    const PS_HANDLE: u32 = 4;
    const IL_HANDLE: u32 = 5;

    let width = 64u32;
    let height = 64u32;

    let vertex_decl = vertex_decl_pos();
    let vb_data = fullscreen_triangle_pos();

    let vs_bytes = assemble_vs_passthrough_pos();
    let ps_bytes = assemble_ps_solid_color_c0();

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
            push_u32(out, cmd::AerogpuShaderStage::Vertex as u32);
            push_u32(out, vs_bytes.len() as u32);
            push_u32(out, 0); // reserved0
            out.extend_from_slice(&vs_bytes);
        });
        emit_packet(out, OPC_CREATE_SHADER_DXBC, |out| {
            push_u32(out, PS_HANDLE);
            push_u32(out, cmd::AerogpuShaderStage::Pixel as u32);
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
            push_u32(out, 16); // stride_bytes
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
            for _ in 1..cmd::AEROGPU_MAX_RENDER_TARGETS {
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

        // Disable drawing via the D3D11-style sample mask field.
        emit_packet(out, OPC_SET_BLEND_STATE, |out| {
            push_u32(out, 0); // enable
            push_u32(out, cmd::AerogpuBlendFactor::One as u32);
            push_u32(out, cmd::AerogpuBlendFactor::Zero as u32);
            push_u32(out, cmd::AerogpuBlendOp::Add as u32);
            push_u32(out, 0xF); // color_write_mask (u8) + padding
            push_u32(out, cmd::AerogpuBlendFactor::One as u32); // src_factor_alpha
            push_u32(out, cmd::AerogpuBlendFactor::Zero as u32); // dst_factor_alpha
            push_u32(out, cmd::AerogpuBlendOp::Add as u32); // blend_op_alpha
            for _ in 0..4 {
                push_f32(out, 1.0); // blend_constant_rgba_f32
            }
            push_u32(out, 0); // sample_mask (bit0 cleared => discard draw for sample_count=1)
        });

        emit_packet(out, OPC_CLEAR, |out| {
            push_u32(out, AEROGPU_CLEAR_COLOR);
            push_f32(out, 1.0);
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 1.0); // depth
            push_u32(out, 0); // stencil
        });

        // Pixel shader emits c0; set it to green.
        emit_packet(out, OPC_SET_SHADER_CONSTANTS_F, |out| {
            push_u32(out, cmd::AerogpuShaderStage::Pixel as u32);
            push_u32(out, 0); // start_register
            push_u32(out, 1); // vec4_count
            push_u32(out, 0); // reserved0
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
        });

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

    assert_eq!(pixel_at(&rgba, width, 32, 32), [255, 0, 0, 255]);
}

#[test]
fn d3d9_cmd_stream_color_write_enable1_masks_mrt() {
    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    // Protocol constants from `aero-protocol`.
    const OPC_CREATE_BUFFER: u32 = cmd::AerogpuCmdOpcode::CreateBuffer as u32;
    const OPC_CREATE_TEXTURE2D: u32 = cmd::AerogpuCmdOpcode::CreateTexture2d as u32;
    const OPC_UPLOAD_RESOURCE: u32 = cmd::AerogpuCmdOpcode::UploadResource as u32;
    const OPC_CREATE_SHADER_DXBC: u32 = cmd::AerogpuCmdOpcode::CreateShaderDxbc as u32;
    const OPC_BIND_SHADERS: u32 = cmd::AerogpuCmdOpcode::BindShaders as u32;
    const OPC_SET_SHADER_CONSTANTS_F: u32 = cmd::AerogpuCmdOpcode::SetShaderConstantsF as u32;
    const OPC_CREATE_INPUT_LAYOUT: u32 = cmd::AerogpuCmdOpcode::CreateInputLayout as u32;
    const OPC_SET_INPUT_LAYOUT: u32 = cmd::AerogpuCmdOpcode::SetInputLayout as u32;
    const OPC_SET_RENDER_TARGETS: u32 = cmd::AerogpuCmdOpcode::SetRenderTargets as u32;
    const OPC_SET_VIEWPORT: u32 = cmd::AerogpuCmdOpcode::SetViewport as u32;
    const OPC_SET_VERTEX_BUFFERS: u32 = cmd::AerogpuCmdOpcode::SetVertexBuffers as u32;
    const OPC_SET_PRIMITIVE_TOPOLOGY: u32 = cmd::AerogpuCmdOpcode::SetPrimitiveTopology as u32;
    const OPC_SET_RENDER_STATE: u32 = cmd::AerogpuCmdOpcode::SetRenderState as u32;
    const OPC_CLEAR: u32 = cmd::AerogpuCmdOpcode::Clear as u32;
    const OPC_DRAW: u32 = cmd::AerogpuCmdOpcode::Draw as u32;

    // D3D9 render state IDs (subset).
    const D3DRS_COLORWRITEENABLE1: u32 = 190;

    const AEROGPU_FORMAT_R8G8B8A8_UNORM: u32 = pci::AerogpuFormat::R8G8B8A8Unorm as u32;
    const AEROGPU_RESOURCE_USAGE_TEXTURE: u32 = cmd::AEROGPU_RESOURCE_USAGE_TEXTURE;
    const AEROGPU_RESOURCE_USAGE_RENDER_TARGET: u32 = cmd::AEROGPU_RESOURCE_USAGE_RENDER_TARGET;
    const AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER: u32 = cmd::AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER;
    const AEROGPU_TOPOLOGY_TRIANGLELIST: u32 = cmd::AerogpuPrimitiveTopology::TriangleList as u32;
    const AEROGPU_CLEAR_COLOR: u32 = cmd::AEROGPU_CLEAR_COLOR;

    const RT0_HANDLE: u32 = 1;
    const RT1_HANDLE: u32 = 2;
    const VB_HANDLE: u32 = 3;
    const VS_HANDLE: u32 = 4;
    const PS_HANDLE: u32 = 5;
    const IL_HANDLE: u32 = 6;

    let width = 64u32;
    let height = 64u32;

    let vertex_decl = vertex_decl_pos();
    let vb_data = fullscreen_triangle_pos();

    let vs_bytes = assemble_vs_passthrough_pos();
    let ps_bytes = assemble_ps_solid_color_c0_c1();

    let stream = build_stream(|out| {
        for handle in [RT0_HANDLE, RT1_HANDLE] {
            emit_packet(out, OPC_CREATE_TEXTURE2D, |out| {
                push_u32(out, handle);
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
        }

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
            push_u32(out, cmd::AerogpuShaderStage::Vertex as u32);
            push_u32(out, vs_bytes.len() as u32);
            push_u32(out, 0); // reserved0
            out.extend_from_slice(&vs_bytes);
        });

        emit_packet(out, OPC_CREATE_SHADER_DXBC, |out| {
            push_u32(out, PS_HANDLE);
            push_u32(out, cmd::AerogpuShaderStage::Pixel as u32);
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
            push_u32(out, 16); // stride_bytes
            push_u32(out, 0); // offset_bytes
            push_u32(out, 0); // reserved0
        });

        emit_packet(out, OPC_SET_PRIMITIVE_TOPOLOGY, |out| {
            push_u32(out, AEROGPU_TOPOLOGY_TRIANGLELIST);
            push_u32(out, 0); // reserved0
        });

        emit_packet(out, OPC_SET_RENDER_TARGETS, |out| {
            push_u32(out, 2); // color_count
            push_u32(out, 0); // depth_stencil
            push_u32(out, RT0_HANDLE);
            push_u32(out, RT1_HANDLE);
            for _ in 0..6 {
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
            // Clear both render targets to red.
            push_u32(out, AEROGPU_CLEAR_COLOR);
            push_f32(out, 1.0);
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 1.0); // depth
            push_u32(out, 0); // stencil
        });

        // Disable color writes to render target 1.
        emit_packet(out, OPC_SET_RENDER_STATE, |out| {
            push_u32(out, D3DRS_COLORWRITEENABLE1);
            push_u32(out, 0);
        });

        // c0 = green.
        emit_packet(out, OPC_SET_SHADER_CONSTANTS_F, |out| {
            push_u32(out, cmd::AerogpuShaderStage::Pixel as u32);
            push_u32(out, 0); // start_register
            push_u32(out, 1); // vec4_count
            push_u32(out, 0); // reserved0
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
        });

        emit_packet(out, OPC_DRAW, |out| {
            push_u32(out, 3); // vertex_count
            push_u32(out, 1); // instance_count
            push_u32(out, 0); // first_vertex
            push_u32(out, 0); // first_instance
        });
    });

    exec.execute_cmd_stream(&stream)
        .expect("execute should succeed");

    let (_, _, rt0) = pollster::block_on(exec.readback_texture_rgba8(RT0_HANDLE))
        .expect("readback rt0 should succeed");
    let (_, _, rt1) = pollster::block_on(exec.readback_texture_rgba8(RT1_HANDLE))
        .expect("readback rt1 should succeed");

    assert_eq!(pixel_at(&rt0, width, 32, 32), [0, 255, 0, 255]);
    assert_eq!(pixel_at(&rt1, width, 32, 32), [255, 0, 0, 255]);
}

#[test]
fn d3d9_cmd_stream_scissor_test_enable_clips_draw() {
    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    // Protocol constants from `aero-protocol`.
    const OPC_CREATE_BUFFER: u32 = cmd::AerogpuCmdOpcode::CreateBuffer as u32;
    const OPC_CREATE_TEXTURE2D: u32 = cmd::AerogpuCmdOpcode::CreateTexture2d as u32;
    const OPC_UPLOAD_RESOURCE: u32 = cmd::AerogpuCmdOpcode::UploadResource as u32;
    const OPC_CREATE_SHADER_DXBC: u32 = cmd::AerogpuCmdOpcode::CreateShaderDxbc as u32;
    const OPC_BIND_SHADERS: u32 = cmd::AerogpuCmdOpcode::BindShaders as u32;
    const OPC_SET_SHADER_CONSTANTS_F: u32 = cmd::AerogpuCmdOpcode::SetShaderConstantsF as u32;
    const OPC_CREATE_INPUT_LAYOUT: u32 = cmd::AerogpuCmdOpcode::CreateInputLayout as u32;
    const OPC_SET_INPUT_LAYOUT: u32 = cmd::AerogpuCmdOpcode::SetInputLayout as u32;
    const OPC_SET_RENDER_TARGETS: u32 = cmd::AerogpuCmdOpcode::SetRenderTargets as u32;
    const OPC_SET_VIEWPORT: u32 = cmd::AerogpuCmdOpcode::SetViewport as u32;
    const OPC_SET_SCISSOR: u32 = cmd::AerogpuCmdOpcode::SetScissor as u32;
    const OPC_SET_VERTEX_BUFFERS: u32 = cmd::AerogpuCmdOpcode::SetVertexBuffers as u32;
    const OPC_SET_PRIMITIVE_TOPOLOGY: u32 = cmd::AerogpuCmdOpcode::SetPrimitiveTopology as u32;
    const OPC_SET_RENDER_STATE: u32 = cmd::AerogpuCmdOpcode::SetRenderState as u32;
    const OPC_CLEAR: u32 = cmd::AerogpuCmdOpcode::Clear as u32;
    const OPC_DRAW: u32 = cmd::AerogpuCmdOpcode::Draw as u32;
    const OPC_PRESENT: u32 = cmd::AerogpuCmdOpcode::Present as u32;

    // D3D9 render state IDs (subset).
    const D3DRS_SCISSORTESTENABLE: u32 = 174;

    const AEROGPU_FORMAT_R8G8B8A8_UNORM: u32 = pci::AerogpuFormat::R8G8B8A8Unorm as u32;
    const AEROGPU_RESOURCE_USAGE_TEXTURE: u32 = cmd::AEROGPU_RESOURCE_USAGE_TEXTURE;
    const AEROGPU_RESOURCE_USAGE_RENDER_TARGET: u32 = cmd::AEROGPU_RESOURCE_USAGE_RENDER_TARGET;
    const AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER: u32 = cmd::AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER;
    const AEROGPU_TOPOLOGY_TRIANGLELIST: u32 = cmd::AerogpuPrimitiveTopology::TriangleList as u32;
    const AEROGPU_CLEAR_COLOR: u32 = cmd::AEROGPU_CLEAR_COLOR;

    const RT_HANDLE: u32 = 1;
    const VB_HANDLE: u32 = 2;
    const VS_HANDLE: u32 = 3;
    const PS_HANDLE: u32 = 4;
    const IL_HANDLE: u32 = 5;

    let width = 64u32;
    let height = 64u32;

    let vertex_decl = vertex_decl_pos();
    let vb_data = fullscreen_triangle_pos();

    let vs_bytes = assemble_vs_passthrough_pos();
    let ps_bytes = assemble_ps_solid_color_c0();

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
            push_u32(out, cmd::AerogpuShaderStage::Vertex as u32);
            push_u32(out, vs_bytes.len() as u32);
            push_u32(out, 0); // reserved0
            out.extend_from_slice(&vs_bytes);
        });

        emit_packet(out, OPC_CREATE_SHADER_DXBC, |out| {
            push_u32(out, PS_HANDLE);
            push_u32(out, cmd::AerogpuShaderStage::Pixel as u32);
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
            push_u32(out, 16); // stride_bytes
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
            // Clear red.
            push_u32(out, AEROGPU_CLEAR_COLOR);
            push_f32(out, 1.0);
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 1.0); // depth
            push_u32(out, 0); // stencil
        });

        // Enable scissor for subsequent draws.
        emit_packet(out, OPC_SET_RENDER_STATE, |out| {
            push_u32(out, D3DRS_SCISSORTESTENABLE);
            push_u32(out, 1);
        });
        emit_packet(out, OPC_SET_SCISSOR, |out| {
            push_i32(out, 0);
            push_i32(out, 0);
            push_i32(out, 32);
            push_i32(out, 64);
        });

        // Set c0 to green.
        emit_packet(out, OPC_SET_SHADER_CONSTANTS_F, |out| {
            push_u32(out, cmd::AerogpuShaderStage::Pixel as u32);
            push_u32(out, 0); // start_register
            push_u32(out, 1); // vec4_count
            push_u32(out, 0); // reserved0
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
        });

        emit_packet(out, OPC_DRAW, |out| {
            push_u32(out, 3); // vertex_count
            push_u32(out, 1); // instance_count
            push_u32(out, 0); // first_vertex
            push_u32(out, 0); // first_instance
        });

        emit_packet(out, OPC_PRESENT, |out| {
            push_u32(out, 0); // scanout_id
            push_u32(out, 0); // flags
        });
    });

    exec.execute_cmd_stream(&stream)
        .expect("execute should succeed");

    let (out_w, out_h, rgba) = pollster::block_on(exec.readback_texture_rgba8(RT_HANDLE))
        .expect("readback should succeed");
    assert_eq!((out_w, out_h), (width, height));

    assert_eq!(pixel_at(&rgba, width, 16, 32), [0, 255, 0, 255]);
    assert_eq!(pixel_at(&rgba, width, 48, 32), [255, 0, 0, 255]);
}

#[test]
fn d3d9_cmd_stream_sampler_wrap_vs_clamp() {
    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    // Protocol constants from `aero-protocol`.
    const OPC_CREATE_BUFFER: u32 = cmd::AerogpuCmdOpcode::CreateBuffer as u32;
    const OPC_CREATE_TEXTURE2D: u32 = cmd::AerogpuCmdOpcode::CreateTexture2d as u32;
    const OPC_UPLOAD_RESOURCE: u32 = cmd::AerogpuCmdOpcode::UploadResource as u32;
    const OPC_CREATE_SHADER_DXBC: u32 = cmd::AerogpuCmdOpcode::CreateShaderDxbc as u32;
    const OPC_BIND_SHADERS: u32 = cmd::AerogpuCmdOpcode::BindShaders as u32;
    const OPC_CREATE_INPUT_LAYOUT: u32 = cmd::AerogpuCmdOpcode::CreateInputLayout as u32;
    const OPC_SET_INPUT_LAYOUT: u32 = cmd::AerogpuCmdOpcode::SetInputLayout as u32;
    const OPC_SET_RENDER_TARGETS: u32 = cmd::AerogpuCmdOpcode::SetRenderTargets as u32;
    const OPC_SET_VIEWPORT: u32 = cmd::AerogpuCmdOpcode::SetViewport as u32;
    const OPC_SET_SCISSOR: u32 = cmd::AerogpuCmdOpcode::SetScissor as u32;
    const OPC_SET_VERTEX_BUFFERS: u32 = cmd::AerogpuCmdOpcode::SetVertexBuffers as u32;
    const OPC_SET_PRIMITIVE_TOPOLOGY: u32 = cmd::AerogpuCmdOpcode::SetPrimitiveTopology as u32;
    const OPC_SET_TEXTURE: u32 = cmd::AerogpuCmdOpcode::SetTexture as u32;
    const OPC_SET_SAMPLER_STATE: u32 = cmd::AerogpuCmdOpcode::SetSamplerState as u32;
    const OPC_SET_RENDER_STATE: u32 = cmd::AerogpuCmdOpcode::SetRenderState as u32;
    const OPC_CLEAR: u32 = cmd::AerogpuCmdOpcode::Clear as u32;
    const OPC_DRAW: u32 = cmd::AerogpuCmdOpcode::Draw as u32;
    const OPC_PRESENT: u32 = cmd::AerogpuCmdOpcode::Present as u32;

    // D3D9 render/sampler state IDs (subset).
    const D3DRS_SCISSORTESTENABLE: u32 = 174;
    const D3DSAMP_ADDRESSU: u32 = 1;
    const D3DSAMP_ADDRESSV: u32 = 2;
    const D3DSAMP_MAGFILTER: u32 = 5;
    const D3DSAMP_MINFILTER: u32 = 6;
    const D3DSAMP_MIPFILTER: u32 = 7;

    const D3DTADDRESS_WRAP: u32 = 1;
    const D3DTADDRESS_CLAMP: u32 = 3;
    const D3DTEXF_NONE: u32 = 0;
    const D3DTEXF_POINT: u32 = 1;

    const AEROGPU_FORMAT_R8G8B8A8_UNORM: u32 = pci::AerogpuFormat::R8G8B8A8Unorm as u32;
    const AEROGPU_RESOURCE_USAGE_TEXTURE: u32 = cmd::AEROGPU_RESOURCE_USAGE_TEXTURE;
    const AEROGPU_RESOURCE_USAGE_RENDER_TARGET: u32 = cmd::AEROGPU_RESOURCE_USAGE_RENDER_TARGET;
    const AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER: u32 = cmd::AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER;
    const AEROGPU_TOPOLOGY_TRIANGLELIST: u32 = cmd::AerogpuPrimitiveTopology::TriangleList as u32;
    const AEROGPU_CLEAR_COLOR: u32 = cmd::AEROGPU_CLEAR_COLOR;

    const RT_HANDLE: u32 = 1;
    const VB_HANDLE: u32 = 2;
    const VS_HANDLE: u32 = 3;
    const PS_HANDLE: u32 = 4;
    const IL_HANDLE: u32 = 5;
    const TEX_HANDLE: u32 = 6;

    let width = 64u32;
    let height = 64u32;

    let vertex_decl = vertex_decl_pos_tex();
    // Constant UV outside [0, 1] to exercise address modes.
    let vb_data = fullscreen_triangle_pos_tex([[1.1, 0.5], [1.1, 0.5], [1.1, 0.5]]);

    // 4x1 texture: red, green, blue, yellow.
    let tex_data: [u8; 16] = [
        255, 0, 0, 255, // red
        0, 255, 0, 255, // green
        0, 0, 255, 255, // blue
        255, 255, 0, 255, // yellow
    ];

    let vs_bytes = assemble_vs_passthrough_pos_tex();
    let ps_bytes = assemble_ps_tex();

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

        emit_packet(out, OPC_CREATE_TEXTURE2D, |out| {
            push_u32(out, TEX_HANDLE);
            push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE);
            push_u32(out, AEROGPU_FORMAT_R8G8B8A8_UNORM);
            push_u32(out, 4); // width
            push_u32(out, 1); // height
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, 4 * 4); // row_pitch_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, OPC_UPLOAD_RESOURCE, |out| {
            push_u32(out, TEX_HANDLE);
            push_u32(out, 0); // reserved0
            push_u64(out, 0); // offset_bytes
            push_u64(out, tex_data.len() as u64);
            out.extend_from_slice(&tex_data);
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
            push_u32(out, cmd::AerogpuShaderStage::Vertex as u32);
            push_u32(out, vs_bytes.len() as u32);
            push_u32(out, 0); // reserved0
            out.extend_from_slice(&vs_bytes);
        });

        emit_packet(out, OPC_CREATE_SHADER_DXBC, |out| {
            push_u32(out, PS_HANDLE);
            push_u32(out, cmd::AerogpuShaderStage::Pixel as u32);
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
            // Clear black.
            push_u32(out, AEROGPU_CLEAR_COLOR);
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 1.0); // depth
            push_u32(out, 0); // stencil
        });

        emit_packet(out, OPC_SET_TEXTURE, |out| {
            push_u32(out, cmd::AerogpuShaderStage::Pixel as u32);
            push_u32(out, 0); // slot
            push_u32(out, TEX_HANDLE);
            push_u32(out, 0); // reserved0
        });

        // Common sampler state.
        emit_packet(out, OPC_SET_SAMPLER_STATE, |out| {
            push_u32(out, cmd::AerogpuShaderStage::Pixel as u32);
            push_u32(out, 0); // slot
            push_u32(out, D3DSAMP_MINFILTER);
            push_u32(out, D3DTEXF_POINT);
        });
        emit_packet(out, OPC_SET_SAMPLER_STATE, |out| {
            push_u32(out, cmd::AerogpuShaderStage::Pixel as u32);
            push_u32(out, 0); // slot
            push_u32(out, D3DSAMP_MAGFILTER);
            push_u32(out, D3DTEXF_POINT);
        });
        emit_packet(out, OPC_SET_SAMPLER_STATE, |out| {
            push_u32(out, cmd::AerogpuShaderStage::Pixel as u32);
            push_u32(out, 0); // slot
            push_u32(out, D3DSAMP_MIPFILTER);
            push_u32(out, D3DTEXF_NONE);
        });
        emit_packet(out, OPC_SET_SAMPLER_STATE, |out| {
            push_u32(out, cmd::AerogpuShaderStage::Pixel as u32);
            push_u32(out, 0); // slot
            push_u32(out, D3DSAMP_ADDRESSV);
            push_u32(out, D3DTADDRESS_CLAMP);
        });

        // Split draws via scissor rect.
        emit_packet(out, OPC_SET_RENDER_STATE, |out| {
            push_u32(out, D3DRS_SCISSORTESTENABLE);
            push_u32(out, 1);
        });

        // Left half: clamp, so u=1.1 clamps to the last texel (yellow).
        emit_packet(out, OPC_SET_SCISSOR, |out| {
            push_i32(out, 0);
            push_i32(out, 0);
            push_i32(out, 32);
            push_i32(out, 64);
        });
        emit_packet(out, OPC_SET_SAMPLER_STATE, |out| {
            push_u32(out, cmd::AerogpuShaderStage::Pixel as u32);
            push_u32(out, 0); // slot
            push_u32(out, D3DSAMP_ADDRESSU);
            push_u32(out, D3DTADDRESS_CLAMP);
        });
        emit_packet(out, OPC_DRAW, |out| {
            push_u32(out, 3); // vertex_count
            push_u32(out, 1); // instance_count
            push_u32(out, 0); // first_vertex
            push_u32(out, 0); // first_instance
        });

        // Right half: wrap, so u=1.1 wraps to 0.1 (red).
        emit_packet(out, OPC_SET_SCISSOR, |out| {
            push_i32(out, 32);
            push_i32(out, 0);
            push_i32(out, 32);
            push_i32(out, 64);
        });
        emit_packet(out, OPC_SET_SAMPLER_STATE, |out| {
            push_u32(out, cmd::AerogpuShaderStage::Pixel as u32);
            push_u32(out, 0); // slot
            push_u32(out, D3DSAMP_ADDRESSU);
            push_u32(out, D3DTADDRESS_WRAP);
        });
        emit_packet(out, OPC_DRAW, |out| {
            push_u32(out, 3); // vertex_count
            push_u32(out, 1); // instance_count
            push_u32(out, 0); // first_vertex
            push_u32(out, 0); // first_instance
        });

        emit_packet(out, OPC_PRESENT, |out| {
            push_u32(out, 0); // scanout_id
            push_u32(out, 0); // flags
        });
    });

    exec.execute_cmd_stream(&stream)
        .expect("execute should succeed");

    let (out_w, out_h, rgba) = pollster::block_on(exec.readback_texture_rgba8(RT_HANDLE))
        .expect("readback should succeed");
    assert_eq!((out_w, out_h), (width, height));

    assert_eq!(pixel_at(&rgba, width, 16, 32), [255, 255, 0, 255]);
    assert_eq!(pixel_at(&rgba, width, 48, 32), [255, 0, 0, 255]);
}

#[test]
fn d3d9_cmd_stream_srgb_write_enable_encodes_linear_output() {
    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    if !exec.supports_view_formats() {
        common::skip_or_panic(
            concat!(
                module_path!(),
                "::d3d9_cmd_stream_srgb_write_enable_encodes_linear_output"
            ),
            "DownlevelFlags::VIEW_FORMATS not supported",
        );
        return;
    }

    // Protocol constants from `aero-protocol`.
    const OPC_CREATE_BUFFER: u32 = cmd::AerogpuCmdOpcode::CreateBuffer as u32;
    const OPC_CREATE_TEXTURE2D: u32 = cmd::AerogpuCmdOpcode::CreateTexture2d as u32;
    const OPC_UPLOAD_RESOURCE: u32 = cmd::AerogpuCmdOpcode::UploadResource as u32;
    const OPC_CREATE_SHADER_DXBC: u32 = cmd::AerogpuCmdOpcode::CreateShaderDxbc as u32;
    const OPC_BIND_SHADERS: u32 = cmd::AerogpuCmdOpcode::BindShaders as u32;
    const OPC_SET_SHADER_CONSTANTS_F: u32 = cmd::AerogpuCmdOpcode::SetShaderConstantsF as u32;
    const OPC_CREATE_INPUT_LAYOUT: u32 = cmd::AerogpuCmdOpcode::CreateInputLayout as u32;
    const OPC_SET_INPUT_LAYOUT: u32 = cmd::AerogpuCmdOpcode::SetInputLayout as u32;
    const OPC_SET_RENDER_TARGETS: u32 = cmd::AerogpuCmdOpcode::SetRenderTargets as u32;
    const OPC_SET_VIEWPORT: u32 = cmd::AerogpuCmdOpcode::SetViewport as u32;
    const OPC_SET_VERTEX_BUFFERS: u32 = cmd::AerogpuCmdOpcode::SetVertexBuffers as u32;
    const OPC_SET_PRIMITIVE_TOPOLOGY: u32 = cmd::AerogpuCmdOpcode::SetPrimitiveTopology as u32;
    const OPC_SET_RENDER_STATE: u32 = cmd::AerogpuCmdOpcode::SetRenderState as u32;
    const OPC_CLEAR: u32 = cmd::AerogpuCmdOpcode::Clear as u32;
    const OPC_DRAW: u32 = cmd::AerogpuCmdOpcode::Draw as u32;

    const D3DRS_SRGBWRITEENABLE: u32 = 194;

    const AEROGPU_FORMAT_R8G8B8A8_UNORM: u32 = pci::AerogpuFormat::R8G8B8A8Unorm as u32;
    const AEROGPU_RESOURCE_USAGE_TEXTURE: u32 = cmd::AEROGPU_RESOURCE_USAGE_TEXTURE;
    const AEROGPU_RESOURCE_USAGE_RENDER_TARGET: u32 = cmd::AEROGPU_RESOURCE_USAGE_RENDER_TARGET;
    const AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER: u32 = cmd::AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER;
    const AEROGPU_TOPOLOGY_TRIANGLELIST: u32 = cmd::AerogpuPrimitiveTopology::TriangleList as u32;
    const AEROGPU_CLEAR_COLOR: u32 = cmd::AEROGPU_CLEAR_COLOR;

    const RT_HANDLE: u32 = 1;
    const VB_HANDLE: u32 = 2;
    const VS_HANDLE: u32 = 3;
    const PS_HANDLE: u32 = 4;
    const IL_HANDLE: u32 = 5;

    let width = 16u32;
    let height = 16u32;

    let vertex_decl = vertex_decl_pos();
    let vb_data = fullscreen_triangle_pos();

    let vs_bytes = assemble_vs_passthrough_pos();
    let ps_bytes = assemble_ps_solid_color_c0();

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
            push_u32(out, cmd::AerogpuShaderStage::Vertex as u32);
            push_u32(out, vs_bytes.len() as u32);
            push_u32(out, 0); // reserved0
            out.extend_from_slice(&vs_bytes);
        });
        emit_packet(out, OPC_CREATE_SHADER_DXBC, |out| {
            push_u32(out, PS_HANDLE);
            push_u32(out, cmd::AerogpuShaderStage::Pixel as u32);
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
            push_u32(out, 16); // stride_bytes
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

        emit_packet(out, OPC_SET_RENDER_STATE, |out| {
            push_u32(out, D3DRS_SRGBWRITEENABLE);
            push_u32(out, 1);
        });

        // Set c0 to 0.5 gray. With sRGB write enabled, 0.5 (linear) becomes ~188 (sRGB).
        emit_packet(out, OPC_SET_SHADER_CONSTANTS_F, |out| {
            push_u32(out, cmd::AerogpuShaderStage::Pixel as u32);
            push_u32(out, 0); // start_register
            push_u32(out, 1); // vec4_count
            push_u32(out, 0); // reserved0
            push_f32(out, 0.5);
            push_f32(out, 0.5);
            push_f32(out, 0.5);
            push_f32(out, 1.0);
        });

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

    let px = pixel_at(&rgba, width, 8, 8);
    assert!(
        (185..=190).contains(&px[0]) && px[0] == px[1] && px[1] == px[2],
        "expected srgb-encoded ~188 gray, got {px:?}"
    );
    assert_eq!(px[3], 255);
}

#[test]
fn d3d9_cmd_stream_sampler_srgb_texture_decodes_srgb_texels() {
    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    if !exec.supports_view_formats() {
        common::skip_or_panic(
            concat!(
                module_path!(),
                "::d3d9_cmd_stream_sampler_srgb_texture_decodes_srgb_texels"
            ),
            "DownlevelFlags::VIEW_FORMATS not supported",
        );
        return;
    }

    // Protocol constants from `aero-protocol`.
    const OPC_CREATE_BUFFER: u32 = cmd::AerogpuCmdOpcode::CreateBuffer as u32;
    const OPC_CREATE_TEXTURE2D: u32 = cmd::AerogpuCmdOpcode::CreateTexture2d as u32;
    const OPC_UPLOAD_RESOURCE: u32 = cmd::AerogpuCmdOpcode::UploadResource as u32;
    const OPC_CREATE_SHADER_DXBC: u32 = cmd::AerogpuCmdOpcode::CreateShaderDxbc as u32;
    const OPC_BIND_SHADERS: u32 = cmd::AerogpuCmdOpcode::BindShaders as u32;
    const OPC_CREATE_INPUT_LAYOUT: u32 = cmd::AerogpuCmdOpcode::CreateInputLayout as u32;
    const OPC_SET_INPUT_LAYOUT: u32 = cmd::AerogpuCmdOpcode::SetInputLayout as u32;
    const OPC_SET_RENDER_TARGETS: u32 = cmd::AerogpuCmdOpcode::SetRenderTargets as u32;
    const OPC_SET_VIEWPORT: u32 = cmd::AerogpuCmdOpcode::SetViewport as u32;
    const OPC_SET_SCISSOR: u32 = cmd::AerogpuCmdOpcode::SetScissor as u32;
    const OPC_SET_VERTEX_BUFFERS: u32 = cmd::AerogpuCmdOpcode::SetVertexBuffers as u32;
    const OPC_SET_PRIMITIVE_TOPOLOGY: u32 = cmd::AerogpuCmdOpcode::SetPrimitiveTopology as u32;
    const OPC_SET_TEXTURE: u32 = cmd::AerogpuCmdOpcode::SetTexture as u32;
    const OPC_SET_SAMPLER_STATE: u32 = cmd::AerogpuCmdOpcode::SetSamplerState as u32;
    const OPC_SET_RENDER_STATE: u32 = cmd::AerogpuCmdOpcode::SetRenderState as u32;
    const OPC_CLEAR: u32 = cmd::AerogpuCmdOpcode::Clear as u32;
    const OPC_DRAW: u32 = cmd::AerogpuCmdOpcode::Draw as u32;

    // D3D9 render/sampler state IDs (subset).
    const D3DRS_SCISSORTESTENABLE: u32 = 174;
    const D3DSAMP_ADDRESSU: u32 = 1;
    const D3DSAMP_ADDRESSV: u32 = 2;
    const D3DSAMP_MAGFILTER: u32 = 5;
    const D3DSAMP_MINFILTER: u32 = 6;
    const D3DSAMP_MIPFILTER: u32 = 7;
    const D3DSAMP_SRGBTEXTURE: u32 = 11;

    const D3DTADDRESS_CLAMP: u32 = 3;
    const D3DTEXF_NONE: u32 = 0;
    const D3DTEXF_POINT: u32 = 1;

    const AEROGPU_FORMAT_R8G8B8A8_UNORM: u32 = pci::AerogpuFormat::R8G8B8A8Unorm as u32;
    const AEROGPU_RESOURCE_USAGE_TEXTURE: u32 = cmd::AEROGPU_RESOURCE_USAGE_TEXTURE;
    const AEROGPU_RESOURCE_USAGE_RENDER_TARGET: u32 = cmd::AEROGPU_RESOURCE_USAGE_RENDER_TARGET;
    const AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER: u32 = cmd::AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER;
    const AEROGPU_TOPOLOGY_TRIANGLELIST: u32 = cmd::AerogpuPrimitiveTopology::TriangleList as u32;
    const AEROGPU_CLEAR_COLOR: u32 = cmd::AEROGPU_CLEAR_COLOR;

    const STAGE_PIXEL: u32 = cmd::AerogpuShaderStage::Pixel as u32;

    const RT_HANDLE: u32 = 1;
    const TEX_HANDLE: u32 = 2;
    const VB_HANDLE: u32 = 3;
    const VS_HANDLE: u32 = 4;
    const PS_HANDLE: u32 = 5;
    const IL_HANDLE: u32 = 6;

    let width = 64u32;
    let height = 64u32;

    let vertex_decl = vertex_decl_pos_tex();
    let vb_data = fullscreen_triangle_pos_tex([[0.0, 0.0], [0.0, 0.0], [0.0, 0.0]]);

    let vs_bytes = assemble_vs_passthrough_pos_tex();
    let ps_bytes = assemble_ps_tex();

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

        emit_packet(out, OPC_CREATE_TEXTURE2D, |out| {
            push_u32(out, TEX_HANDLE);
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

        // Upload a 1x1 texel containing sRGB-coded ~0.5 gray (188).
        emit_packet(out, OPC_UPLOAD_RESOURCE, |out| {
            push_u32(out, TEX_HANDLE);
            push_u32(out, 0);
            push_u64(out, 0);
            push_u64(out, 4);
            out.extend_from_slice(&[188, 188, 188, 255]);
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
            push_u32(out, cmd::AerogpuShaderStage::Vertex as u32);
            push_u32(out, vs_bytes.len() as u32);
            push_u32(out, 0); // reserved0
            out.extend_from_slice(&vs_bytes);
        });
        emit_packet(out, OPC_CREATE_SHADER_DXBC, |out| {
            push_u32(out, PS_HANDLE);
            push_u32(out, cmd::AerogpuShaderStage::Pixel as u32);
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
            push_u32(out, TEX_HANDLE);
            push_u32(out, 0); // reserved0
        });

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

        emit_packet(out, OPC_SET_RENDER_STATE, |out| {
            push_u32(out, D3DRS_SCISSORTESTENABLE);
            push_u32(out, 1);
        });

        // Left half: SRGBTEXTURE disabled -> sample returns ~0.737 -> stores ~188.
        emit_packet(out, OPC_SET_SCISSOR, |out| {
            push_i32(out, 0);
            push_i32(out, 0);
            push_i32(out, 32);
            push_i32(out, 64);
        });
        emit_packet(out, OPC_SET_SAMPLER_STATE, |out| {
            push_u32(out, STAGE_PIXEL);
            push_u32(out, 0); // slot
            push_u32(out, D3DSAMP_SRGBTEXTURE);
            push_u32(out, 0);
        });
        emit_packet(out, OPC_DRAW, |out| {
            push_u32(out, 3); // vertex_count
            push_u32(out, 1); // instance_count
            push_u32(out, 0); // first_vertex
            push_u32(out, 0); // first_instance
        });

        // Right half: SRGBTEXTURE enabled -> sample decodes to 0.5 -> stores ~128.
        emit_packet(out, OPC_SET_SCISSOR, |out| {
            push_i32(out, 32);
            push_i32(out, 0);
            push_i32(out, 32);
            push_i32(out, 64);
        });
        emit_packet(out, OPC_SET_SAMPLER_STATE, |out| {
            push_u32(out, STAGE_PIXEL);
            push_u32(out, 0); // slot
            push_u32(out, D3DSAMP_SRGBTEXTURE);
            push_u32(out, 1);
        });
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

    let left = pixel_at(&rgba, width, 16, 32);
    let right = pixel_at(&rgba, width, 48, 32);
    assert!(
        (185..=190).contains(&left[0]) && left[0] == left[1] && left[1] == left[2],
        "expected non-srgb sample to stay ~188, got {left:?}"
    );
    assert!(
        (120..=135).contains(&right[0]) && right[0] == right[1] && right[1] == right[2],
        "expected srgb sample to decode to ~128, got {right:?}"
    );
}

#[test]
fn d3d9_cmd_stream_stencil_clip_mask() {
    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    // Protocol constants from `aero-protocol`.
    const OPC_CREATE_BUFFER: u32 = cmd::AerogpuCmdOpcode::CreateBuffer as u32;
    const OPC_CREATE_TEXTURE2D: u32 = cmd::AerogpuCmdOpcode::CreateTexture2d as u32;
    const OPC_UPLOAD_RESOURCE: u32 = cmd::AerogpuCmdOpcode::UploadResource as u32;
    const OPC_CREATE_SHADER_DXBC: u32 = cmd::AerogpuCmdOpcode::CreateShaderDxbc as u32;
    const OPC_BIND_SHADERS: u32 = cmd::AerogpuCmdOpcode::BindShaders as u32;
    const OPC_SET_SHADER_CONSTANTS_F: u32 = cmd::AerogpuCmdOpcode::SetShaderConstantsF as u32;
    const OPC_CREATE_INPUT_LAYOUT: u32 = cmd::AerogpuCmdOpcode::CreateInputLayout as u32;
    const OPC_SET_INPUT_LAYOUT: u32 = cmd::AerogpuCmdOpcode::SetInputLayout as u32;
    const OPC_SET_RENDER_TARGETS: u32 = cmd::AerogpuCmdOpcode::SetRenderTargets as u32;
    const OPC_SET_VIEWPORT: u32 = cmd::AerogpuCmdOpcode::SetViewport as u32;
    const OPC_SET_SCISSOR: u32 = cmd::AerogpuCmdOpcode::SetScissor as u32;
    const OPC_SET_VERTEX_BUFFERS: u32 = cmd::AerogpuCmdOpcode::SetVertexBuffers as u32;
    const OPC_SET_PRIMITIVE_TOPOLOGY: u32 = cmd::AerogpuCmdOpcode::SetPrimitiveTopology as u32;
    const OPC_SET_RENDER_STATE: u32 = cmd::AerogpuCmdOpcode::SetRenderState as u32;
    const OPC_CLEAR: u32 = cmd::AerogpuCmdOpcode::Clear as u32;
    const OPC_DRAW: u32 = cmd::AerogpuCmdOpcode::Draw as u32;
    const OPC_PRESENT: u32 = cmd::AerogpuCmdOpcode::Present as u32;

    // D3D9 render state IDs (subset).
    const D3DRS_SCISSORTESTENABLE: u32 = 174;
    const D3DRS_COLORWRITEENABLE: u32 = 168;

    const D3DRS_STENCILENABLE: u32 = 52;
    const D3DRS_STENCILFAIL: u32 = 53;
    const D3DRS_STENCILZFAIL: u32 = 54;
    const D3DRS_STENCILPASS: u32 = 55;
    const D3DRS_STENCILFUNC: u32 = 56;
    const D3DRS_STENCILREF: u32 = 57;
    const D3DRS_STENCILMASK: u32 = 58;
    const D3DRS_STENCILWRITEMASK: u32 = 59;

    // D3D9 enums.
    const D3DCMP_EQUAL: u32 = 3;
    const D3DCMP_ALWAYS: u32 = 8;
    const D3DSTENCILOP_KEEP: u32 = 1;
    const D3DSTENCILOP_REPLACE: u32 = 3;

    const AEROGPU_FORMAT_R8G8B8A8_UNORM: u32 = pci::AerogpuFormat::R8G8B8A8Unorm as u32;
    const AEROGPU_FORMAT_D24_UNORM_S8_UINT: u32 = pci::AerogpuFormat::D24UnormS8Uint as u32;
    const AEROGPU_RESOURCE_USAGE_TEXTURE: u32 = cmd::AEROGPU_RESOURCE_USAGE_TEXTURE;
    const AEROGPU_RESOURCE_USAGE_RENDER_TARGET: u32 = cmd::AEROGPU_RESOURCE_USAGE_RENDER_TARGET;
    const AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL: u32 = cmd::AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL;
    const AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER: u32 = cmd::AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER;
    const AEROGPU_TOPOLOGY_TRIANGLELIST: u32 = cmd::AerogpuPrimitiveTopology::TriangleList as u32;
    const AEROGPU_CLEAR_COLOR: u32 = cmd::AEROGPU_CLEAR_COLOR;
    const AEROGPU_CLEAR_DEPTH: u32 = cmd::AEROGPU_CLEAR_DEPTH;
    const AEROGPU_CLEAR_STENCIL: u32 = cmd::AEROGPU_CLEAR_STENCIL;

    const RT_HANDLE: u32 = 1;
    const DS_HANDLE: u32 = 2;
    const VB_HANDLE: u32 = 3;
    const VS_HANDLE: u32 = 4;
    const PS_HANDLE: u32 = 5;
    const IL_HANDLE: u32 = 6;

    let width = 64u32;
    let height = 64u32;

    let vertex_decl = vertex_decl_pos();
    let vb_data = fullscreen_triangle_pos();

    let vs_bytes = assemble_vs_passthrough_pos();
    let ps_bytes = assemble_ps_solid_color_c0();

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

        emit_packet(out, OPC_CREATE_TEXTURE2D, |out| {
            push_u32(out, DS_HANDLE);
            push_u32(out, AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL);
            push_u32(out, AEROGPU_FORMAT_D24_UNORM_S8_UINT);
            push_u32(out, width);
            push_u32(out, height);
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, width * 4); // row_pitch_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
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
            push_u32(out, cmd::AerogpuShaderStage::Vertex as u32);
            push_u32(out, vs_bytes.len() as u32);
            push_u32(out, 0); // reserved0
            out.extend_from_slice(&vs_bytes);
        });

        emit_packet(out, OPC_CREATE_SHADER_DXBC, |out| {
            push_u32(out, PS_HANDLE);
            push_u32(out, cmd::AerogpuShaderStage::Pixel as u32);
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
            push_u32(out, 16); // stride_bytes
            push_u32(out, 0); // offset_bytes
            push_u32(out, 0); // reserved0
        });

        emit_packet(out, OPC_SET_PRIMITIVE_TOPOLOGY, |out| {
            push_u32(out, AEROGPU_TOPOLOGY_TRIANGLELIST);
            push_u32(out, 0); // reserved0
        });

        emit_packet(out, OPC_SET_RENDER_TARGETS, |out| {
            push_u32(out, 1); // color_count
            push_u32(out, DS_HANDLE); // depth_stencil
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
            push_u32(
                out,
                AEROGPU_CLEAR_COLOR | AEROGPU_CLEAR_DEPTH | AEROGPU_CLEAR_STENCIL,
            );
            push_f32(out, 1.0);
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 1.0); // depth
            push_u32(out, 0); // stencil
        });

        emit_packet(out, OPC_SET_RENDER_STATE, |out| {
            push_u32(out, D3DRS_SCISSORTESTENABLE);
            push_u32(out, 1);
        });
        emit_packet(out, OPC_SET_SCISSOR, |out| {
            push_i32(out, 0);
            push_i32(out, 0);
            push_i32(out, 32);
            push_i32(out, 64);
        });

        for (state, value) in [
            (D3DRS_COLORWRITEENABLE, 0),
            (D3DRS_STENCILENABLE, 1),
            (D3DRS_STENCILFUNC, D3DCMP_ALWAYS),
            (D3DRS_STENCILREF, 1),
            (D3DRS_STENCILFAIL, D3DSTENCILOP_KEEP),
            (D3DRS_STENCILZFAIL, D3DSTENCILOP_KEEP),
            (D3DRS_STENCILPASS, D3DSTENCILOP_REPLACE),
            (D3DRS_STENCILMASK, 0xFF),
            (D3DRS_STENCILWRITEMASK, 0xFF),
        ] {
            emit_packet(out, OPC_SET_RENDER_STATE, |out| {
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

        emit_packet(out, OPC_SET_RENDER_STATE, |out| {
            push_u32(out, D3DRS_SCISSORTESTENABLE);
            push_u32(out, 0);
        });

        for (state, value) in [
            (D3DRS_COLORWRITEENABLE, 0xF),
            (D3DRS_STENCILFUNC, D3DCMP_EQUAL),
            (D3DRS_STENCILREF, 1),
            (D3DRS_STENCILFAIL, D3DSTENCILOP_KEEP),
            (D3DRS_STENCILZFAIL, D3DSTENCILOP_KEEP),
            (D3DRS_STENCILPASS, D3DSTENCILOP_KEEP),
        ] {
            emit_packet(out, OPC_SET_RENDER_STATE, |out| {
                push_u32(out, state);
                push_u32(out, value);
            });
        }

        emit_packet(out, OPC_SET_SHADER_CONSTANTS_F, |out| {
            push_u32(out, cmd::AerogpuShaderStage::Pixel as u32);
            push_u32(out, 0); // start_register
            push_u32(out, 1); // vec4_count
            push_u32(out, 0); // reserved0
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
        });

        emit_packet(out, OPC_DRAW, |out| {
            push_u32(out, 3); // vertex_count
            push_u32(out, 1); // instance_count
            push_u32(out, 0); // first_vertex
            push_u32(out, 0); // first_instance
        });

        emit_packet(out, OPC_PRESENT, |out| {
            push_u32(out, 0); // scanout_id
            push_u32(out, 0); // flags
        });
    });

    exec.execute_cmd_stream(&stream)
        .expect("execute should succeed");

    let (out_w, out_h, rgba) = pollster::block_on(exec.readback_texture_rgba8(RT_HANDLE))
        .expect("readback should succeed");
    assert_eq!((out_w, out_h), (width, height));

    assert_eq!(pixel_at(&rgba, width, 16, 32), [0, 255, 0, 255]);
    assert_eq!(pixel_at(&rgba, width, 48, 32), [255, 0, 0, 255]);
}
