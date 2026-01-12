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

fn to_bytes(words: &[u32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(words.len() * 4);
    for w in words {
        bytes.extend_from_slice(&w.to_le_bytes());
    }
    bytes
}

#[test]
fn d3d9_cmd_stream_fixedfunc_poscolor_multi_stream_renders_triangle() {
    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(module_path!(), "wgpu adapter not found");
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    // Protocol constants from `aero-protocol`.
    const OPC_CREATE_BUFFER: u32 = AerogpuCmdOpcode::CreateBuffer as u32;
    const OPC_CREATE_TEXTURE2D: u32 = AerogpuCmdOpcode::CreateTexture2d as u32;
    const OPC_UPLOAD_RESOURCE: u32 = AerogpuCmdOpcode::UploadResource as u32;
    const OPC_CREATE_SHADER_DXBC: u32 = AerogpuCmdOpcode::CreateShaderDxbc as u32;
    const OPC_BIND_SHADERS: u32 = AerogpuCmdOpcode::BindShaders as u32;
    const OPC_CREATE_INPUT_LAYOUT: u32 = AerogpuCmdOpcode::CreateInputLayout as u32;
    const OPC_SET_INPUT_LAYOUT: u32 = AerogpuCmdOpcode::SetInputLayout as u32;
    const OPC_SET_RENDER_TARGETS: u32 = AerogpuCmdOpcode::SetRenderTargets as u32;
    const OPC_SET_VIEWPORT: u32 = AerogpuCmdOpcode::SetViewport as u32;
    const OPC_SET_SCISSOR: u32 = AerogpuCmdOpcode::SetScissor as u32;
    const OPC_SET_VERTEX_BUFFERS: u32 = AerogpuCmdOpcode::SetVertexBuffers as u32;
    const OPC_SET_PRIMITIVE_TOPOLOGY: u32 = AerogpuCmdOpcode::SetPrimitiveTopology as u32;
    const OPC_CLEAR: u32 = AerogpuCmdOpcode::Clear as u32;
    const OPC_DRAW: u32 = AerogpuCmdOpcode::Draw as u32;

    const AEROGPU_FORMAT_R8G8B8A8_UNORM: u32 = AerogpuFormat::R8G8B8A8Unorm as u32;
    const AEROGPU_TOPOLOGY_TRIANGLELIST: u32 = AerogpuPrimitiveTopology::TriangleList as u32;

    const RT_HANDLE: u32 = 1;
    const VB_POS_HANDLE: u32 = 2;
    const VB_COLOR_HANDLE: u32 = 3;
    const VS_HANDLE: u32 = 4;
    const PS_HANDLE: u32 = 5;
    const IL_HANDLE: u32 = 6;

    let width = 64u32;
    let height = 64u32;

    // Built-in fixed-function fallback shader token streams (same as
    // `drivers/aerogpu/umd/d3d9/src/aerogpu_d3d9_fixedfunc_shaders.h`).
    const VS_WORDS: [u32; 8] = [
        0xFFFE_0200u32, // vs_2_0
        0x0200_0001u32, // mov oPos, v0
        0x400F_0000u32, // oPos.xyzw
        0x10E4_0000u32, // v0.xyzw
        0x0200_0001u32, // mov oD0, v1
        0x500F_0000u32, // oD0.xyzw
        0x10E4_0001u32, // v1.xyzw
        0x0000_FFFFu32, // end
    ];
    const PS_WORDS: [u32; 5] = [
        0xFFFF_0200u32, // ps_2_0
        0x0200_0001u32, // mov oC0, v0
        0x000F_0800u32, // oC0.xyzw
        0x10E4_0000u32, // v0.xyzw
        0x0000_FFFFu32, // end
    ];

    let vs_bytes = to_bytes(&VS_WORDS);
    let ps_bytes = to_bytes(&PS_WORDS);

    // Vertex decl: POSITIONT float4 in stream 0, COLOR0 d3dcolor in stream 1.
    const D3DDECLTYPE_FLOAT4: u8 = 3;
    const D3DDECLTYPE_D3DCOLOR: u8 = 4;
    const D3DDECLTYPE_UNUSED: u8 = 17;
    const D3DDECLUSAGE_POSITIONT: u8 = 9;
    const D3DDECLUSAGE_COLOR: u8 = 10;

    let mut vertex_decl = Vec::new();
    // POSITIONT at stream 0 offset 0.
    push_u16(&mut vertex_decl, 0); // stream
    push_u16(&mut vertex_decl, 0); // offset
    push_u8(&mut vertex_decl, D3DDECLTYPE_FLOAT4);
    push_u8(&mut vertex_decl, 0); // method
    push_u8(&mut vertex_decl, D3DDECLUSAGE_POSITIONT);
    push_u8(&mut vertex_decl, 0); // usage_index
                                  // COLOR0 at stream 1 offset 0.
    push_u16(&mut vertex_decl, 1); // stream
    push_u16(&mut vertex_decl, 0); // offset
    push_u8(&mut vertex_decl, D3DDECLTYPE_D3DCOLOR);
    push_u8(&mut vertex_decl, 0); // method
    push_u8(&mut vertex_decl, D3DDECLUSAGE_COLOR);
    push_u8(&mut vertex_decl, 0); // usage_index
                                  // End marker.
    push_u16(&mut vertex_decl, 0x00FF);
    push_u16(&mut vertex_decl, 0);
    push_u8(&mut vertex_decl, D3DDECLTYPE_UNUSED);
    push_u8(&mut vertex_decl, 0);
    push_u8(&mut vertex_decl, 0);
    push_u8(&mut vertex_decl, 0);
    assert_eq!(vertex_decl.len(), 24);

    // Vertex stream 0: float4 position (clip space).
    // Use clockwise winding so the test does not depend on cull state.
    let positions = [
        (-0.5f32, -0.5f32, 0.0f32, 1.0f32),
        (0.0f32, 0.5f32, 0.0f32, 1.0f32),
        (0.5f32, -0.5f32, 0.0f32, 1.0f32),
    ];
    let mut vb_pos_data = Vec::new();
    for (x, y, z, w) in positions {
        push_f32(&mut vb_pos_data, x);
        push_f32(&mut vb_pos_data, y);
        push_f32(&mut vb_pos_data, z);
        push_f32(&mut vb_pos_data, w);
    }
    assert_eq!(vb_pos_data.len(), 3 * 16);

    // Vertex stream 1: D3DCOLOR (ARGB, stored as BGRA bytes in little-endian).
    //
    // Use a non-symmetric color (red) so we catch regressions in D3DCOLOR BGRA->RGBA conversion
    // when the element is in a separate stream.
    let red_argb: u32 = 0xFFFF_0000u32;
    let mut vb_color_data = Vec::new();
    for _ in 0..3 {
        push_u32(&mut vb_color_data, red_argb);
    }
    assert_eq!(vb_color_data.len(), 3 * 4);

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
            push_u32(out, VB_POS_HANDLE);
            push_u32(out, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER);
            push_u64(out, vb_pos_data.len() as u64);
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, OPC_CREATE_BUFFER, |out| {
            push_u32(out, VB_COLOR_HANDLE);
            push_u32(out, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER);
            push_u64(out, vb_color_data.len() as u64);
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, OPC_UPLOAD_RESOURCE, |out| {
            push_u32(out, VB_POS_HANDLE);
            push_u32(out, 0); // reserved0
            push_u64(out, 0); // offset_bytes
            push_u64(out, vb_pos_data.len() as u64);
            out.extend_from_slice(&vb_pos_data);
        });

        emit_packet(out, OPC_UPLOAD_RESOURCE, |out| {
            push_u32(out, VB_COLOR_HANDLE);
            push_u32(out, 0); // reserved0
            push_u64(out, 0); // offset_bytes
            push_u64(out, vb_color_data.len() as u64);
            out.extend_from_slice(&vb_color_data);
        });

        emit_packet(out, OPC_CREATE_SHADER_DXBC, |out| {
            push_u32(out, VS_HANDLE);
            push_u32(out, 0); // vertex
            push_u32(out, vs_bytes.len() as u32);
            push_u32(out, 0); // reserved0
            out.extend_from_slice(&vs_bytes);
        });

        emit_packet(out, OPC_CREATE_SHADER_DXBC, |out| {
            push_u32(out, PS_HANDLE);
            push_u32(out, 1); // pixel
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

        // Bind stream 0 (position) and stream 1 (color).
        emit_packet(out, OPC_SET_VERTEX_BUFFERS, |out| {
            push_u32(out, 0); // start_slot
            push_u32(out, 2); // buffer_count
                              // binding[0] = stream 0
            push_u32(out, VB_POS_HANDLE);
            push_u32(out, 16); // stride_bytes
            push_u32(out, 0); // offset_bytes
            push_u32(out, 0); // reserved0
                              // binding[1] = stream 1
            push_u32(out, VB_COLOR_HANDLE);
            push_u32(out, 4); // stride_bytes
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

        emit_packet(out, OPC_SET_SCISSOR, |out| {
            push_i32(out, 0);
            push_i32(out, 0);
            push_i32(out, width as i32);
            push_i32(out, height as i32);
        });

        emit_packet(out, OPC_CLEAR, |out| {
            push_u32(out, AEROGPU_CLEAR_COLOR);
            // black clear
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 1.0); // depth
            push_u32(out, 0); // stencil
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
    assert_eq!(rgba.len(), (width * height * 4) as usize);

    let px = |x: u32, y: u32| -> [u8; 4] {
        let idx = ((y * width + x) * 4) as usize;
        rgba[idx..idx + 4].try_into().unwrap()
    };

    // Corner should remain clear color.
    assert_eq!(px(5, 5), [0, 0, 0, 255]);
    // Center pixel should be red.
    assert_eq!(px(width / 2, height / 2), [255, 0, 0, 255]);
}
