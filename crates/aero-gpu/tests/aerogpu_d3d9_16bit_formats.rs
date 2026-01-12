mod common;

use aero_gpu::aerogpu_executor::{AllocEntry, AllocTable};
use aero_gpu::{AerogpuD3d9Error, AerogpuD3d9Executor, VecGuestMemory};
use aero_protocol::aerogpu::{
    aerogpu_cmd::{
        AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode,
        AerogpuCmdStreamHeader as ProtocolCmdStreamHeader, AerogpuPrimitiveTopology,
        AerogpuShaderStage, AEROGPU_CLEAR_COLOR, AEROGPU_CMD_STREAM_MAGIC,
        AEROGPU_RESOURCE_USAGE_RENDER_TARGET, AEROGPU_RESOURCE_USAGE_TEXTURE,
        AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
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

fn assemble_vs_fullscreen_pos_tex() -> Vec<u8> {
    // vs_2_0: mov oPos, v0; mov oT0, v1; end
    let mut words = vec![0xFFFE_0200];
    words.extend(enc_inst(0x0001, &[enc_dst(4, 0, 0xF), enc_src(1, 0, 0xE4)]));
    words.extend(enc_inst(0x0001, &[enc_dst(6, 0, 0xF), enc_src(1, 1, 0xE4)]));
    words.push(0x0000_FFFF);
    to_bytes(&words)
}

fn assemble_ps_sample_tex0() -> Vec<u8> {
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

fn build_fullscreen_vb() -> Vec<u8> {
    let mut vb_data = Vec::new();
    // D3D9 defaults to back-face culling with clockwise front faces.
    // Use a clockwise full-screen triangle so the test does not depend on cull state.
    let verts = [
        (-1.0f32, -1.0f32, 0.0f32, 1.0f32),
        (-1.0f32, 3.0f32, 0.0f32, 1.0f32),
        (3.0f32, -1.0f32, 0.0f32, 1.0f32),
    ];
    for (x, y, z, w) in verts {
        push_f32(&mut vb_data, x);
        push_f32(&mut vb_data, y);
        push_f32(&mut vb_data, z);
        push_f32(&mut vb_data, w);
        // texcoord float4 (all vertices sample the same texel).
        push_f32(&mut vb_data, 0.5);
        push_f32(&mut vb_data, 0.5);
        push_f32(&mut vb_data, 0.0);
        push_f32(&mut vb_data, 0.0);
    }
    assert_eq!(vb_data.len(), 3 * 32);
    vb_data
}

fn build_pos_tex_vertex_decl() -> Vec<u8> {
    let mut vertex_decl = Vec::new();
    // POSITION0 float4 at stream 0 offset 0.
    push_u16(&mut vertex_decl, 0);
    push_u16(&mut vertex_decl, 0);
    push_u8(&mut vertex_decl, 3); // FLOAT4
    push_u8(&mut vertex_decl, 0);
    push_u8(&mut vertex_decl, 0); // POSITION
    push_u8(&mut vertex_decl, 0);
    // TEXCOORD0 float4 at offset 16.
    push_u16(&mut vertex_decl, 0);
    push_u16(&mut vertex_decl, 16);
    push_u8(&mut vertex_decl, 3); // FLOAT4
    push_u8(&mut vertex_decl, 0);
    push_u8(&mut vertex_decl, 5); // TEXCOORD
    push_u8(&mut vertex_decl, 0);
    // End marker.
    push_u16(&mut vertex_decl, 0x00FF);
    push_u16(&mut vertex_decl, 0);
    push_u8(&mut vertex_decl, 17); // UNUSED
    push_u8(&mut vertex_decl, 0);
    push_u8(&mut vertex_decl, 0);
    push_u8(&mut vertex_decl, 0);
    vertex_decl
}

#[test]
fn d3d9_cmd_stream_supports_16bit_formats_b5g6r5_and_b5g5r5a1() {
    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(module_path!(), "wgpu adapter not found");
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    let formats = [
        (
            "B5G6R5Unorm",
            AerogpuFormat::B5G6R5Unorm as u32,
            // Pure red in B5G6R5.
            [0x00u8, 0xF8u8],
            [255u8, 0, 0, 255],
        ),
        (
            "B5G5R5A1Unorm",
            AerogpuFormat::B5G5R5A1Unorm as u32,
            // Pure red with alpha=1 in B5G5R5A1.
            [0x00u8, 0xFCu8],
            [255u8, 0, 0, 255],
        ),
    ];

    // Common setup resources.
    const VB_HANDLE: u32 = 1;
    const VS_HANDLE: u32 = 2;
    const PS_HANDLE: u32 = 3;
    const IL_HANDLE: u32 = 4;

    const OPC_CREATE_BUFFER: u32 = AerogpuCmdOpcode::CreateBuffer as u32;
    const OPC_CREATE_SHADER_DXBC: u32 = AerogpuCmdOpcode::CreateShaderDxbc as u32;
    const OPC_BIND_SHADERS: u32 = AerogpuCmdOpcode::BindShaders as u32;
    const OPC_CREATE_INPUT_LAYOUT: u32 = AerogpuCmdOpcode::CreateInputLayout as u32;
    const OPC_SET_INPUT_LAYOUT: u32 = AerogpuCmdOpcode::SetInputLayout as u32;
    const OPC_SET_VERTEX_BUFFERS: u32 = AerogpuCmdOpcode::SetVertexBuffers as u32;
    const OPC_SET_PRIMITIVE_TOPOLOGY: u32 = AerogpuCmdOpcode::SetPrimitiveTopology as u32;
    const OPC_SET_VIEWPORT: u32 = AerogpuCmdOpcode::SetViewport as u32;
    const OPC_SET_SCISSOR: u32 = AerogpuCmdOpcode::SetScissor as u32;
    const OPC_UPLOAD_RESOURCE: u32 = AerogpuCmdOpcode::UploadResource as u32;

    let vb_data = build_fullscreen_vb();
    let vs_bytes = assemble_vs_fullscreen_pos_tex();
    let ps_bytes = assemble_ps_sample_tex0();
    let vertex_decl = build_pos_tex_vertex_decl();

    let setup_stream = build_stream(|out| {
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
            push_u64(out, vb_data.len() as u64); // size_bytes
            out.extend_from_slice(&vb_data);
        });

        emit_packet(out, OPC_CREATE_SHADER_DXBC, |out| {
            push_u32(out, VS_HANDLE);
            push_u32(out, AerogpuShaderStage::Vertex as u32);
            push_u32(out, vs_bytes.len() as u32);
            push_u32(out, 0); // reserved0
            out.extend_from_slice(&vs_bytes);
        });

        emit_packet(out, OPC_CREATE_SHADER_DXBC, |out| {
            push_u32(out, PS_HANDLE);
            push_u32(out, AerogpuShaderStage::Pixel as u32);
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
            push_u32(out, AerogpuPrimitiveTopology::TriangleList as u32);
            push_u32(out, 0); // reserved0
        });

        emit_packet(out, OPC_SET_VIEWPORT, |out| {
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 1.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
        });

        emit_packet(out, OPC_SET_SCISSOR, |out| {
            push_i32(out, 0);
            push_i32(out, 0);
            push_i32(out, 1);
            push_i32(out, 1);
        });
    });

    exec.execute_cmd_stream(&setup_stream)
        .expect("setup stream should succeed");

    // Guest backing used by the dirty-range path.
    let alloc_id = 1u32;
    let alloc_gpa_base = 0x1000u64;
    let mut guest_memory = VecGuestMemory::new(0x4000);
    let alloc_table = AllocTable::new([(
        alloc_id,
        AllocEntry {
            flags: 0,
            gpa: alloc_gpa_base,
            size_bytes: 0x1000,
        },
    )])
    .expect("alloc table");

    const OPC_CREATE_TEXTURE2D: u32 = AerogpuCmdOpcode::CreateTexture2d as u32;
    const OPC_RESOURCE_DIRTY_RANGE: u32 = AerogpuCmdOpcode::ResourceDirtyRange as u32;
    const OPC_SET_RENDER_TARGETS: u32 = AerogpuCmdOpcode::SetRenderTargets as u32;
    const OPC_CLEAR: u32 = AerogpuCmdOpcode::Clear as u32;
    const OPC_SET_TEXTURE: u32 = AerogpuCmdOpcode::SetTexture as u32;
    const OPC_DRAW: u32 = AerogpuCmdOpcode::Draw as u32;
    const OPC_PRESENT: u32 = AerogpuCmdOpcode::Present as u32;

    for (idx, (name, format_raw, pixel, expected_rgba)) in formats.iter().enumerate() {
        // ---- UploadResource path (host-owned texture) ----
        let rt_handle = 100 + idx as u32 * 10 + 1;
        let tex_handle = rt_handle + 1;
        let stream_upload = build_stream(|out| {
            emit_packet(out, OPC_CREATE_TEXTURE2D, |out| {
                push_u32(out, rt_handle);
                push_u32(
                    out,
                    AEROGPU_RESOURCE_USAGE_TEXTURE | AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
                );
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32);
                push_u32(out, 1); // width
                push_u32(out, 1); // height
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 4); // row_pitch_bytes
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            emit_packet(out, OPC_CREATE_TEXTURE2D, |out| {
                push_u32(out, tex_handle);
                push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE);
                push_u32(out, *format_raw);
                push_u32(out, 1); // width
                push_u32(out, 1); // height
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 0); // row_pitch_bytes (tight packing)
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            emit_packet(out, OPC_UPLOAD_RESOURCE, |out| {
                push_u32(out, tex_handle);
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, 2); // size_bytes
                out.extend_from_slice(pixel);
            });

            emit_packet(out, OPC_SET_RENDER_TARGETS, |out| {
                push_u32(out, 1); // color_count
                push_u32(out, 0); // depth_stencil
                push_u32(out, rt_handle);
                for _ in 0..7 {
                    push_u32(out, 0);
                }
            });

            emit_packet(out, OPC_CLEAR, |out| {
                push_u32(out, AEROGPU_CLEAR_COLOR);
                push_f32(out, 0.0);
                push_f32(out, 0.0);
                push_f32(out, 0.0);
                push_f32(out, 1.0);
                push_f32(out, 1.0);
                push_u32(out, 0);
            });

            emit_packet(out, OPC_SET_TEXTURE, |out| {
                push_u32(out, AerogpuShaderStage::Pixel as u32);
                push_u32(out, 0); // slot
                push_u32(out, tex_handle);
                push_u32(out, 0); // reserved0
            });

            emit_packet(out, OPC_DRAW, |out| {
                push_u32(out, 3);
                push_u32(out, 1);
                push_u32(out, 0);
                push_u32(out, 0);
            });

            emit_packet(out, OPC_PRESENT, |out| {
                push_u32(out, 0);
                push_u32(out, 0);
            });
        });

        exec.execute_cmd_stream(&stream_upload)
            .unwrap_or_else(|e| panic!("upload path stream for {name} should succeed: {e}"));

        let (_w, _h, rgba) = pollster::block_on(exec.readback_texture_rgba8(rt_handle))
            .unwrap_or_else(|e| panic!("readback upload {name} should succeed: {e}"));
        assert_eq!(&rgba[0..4], expected_rgba, "upload path {name}");

        // ---- Dirty-range path (guest-backed texture) ----
        let rt_handle = 100 + idx as u32 * 10 + 3;
        let tex_handle = rt_handle + 1;
        let backing_offset = (idx as u32) * 0x100;
        guest_memory
            .write(alloc_gpa_base + backing_offset as u64, pixel)
            .unwrap();

        let stream_dirty = build_stream(|out| {
            emit_packet(out, OPC_CREATE_TEXTURE2D, |out| {
                push_u32(out, rt_handle);
                push_u32(
                    out,
                    AEROGPU_RESOURCE_USAGE_TEXTURE | AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
                );
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32);
                push_u32(out, 1); // width
                push_u32(out, 1); // height
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 4); // row_pitch_bytes
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            emit_packet(out, OPC_CREATE_TEXTURE2D, |out| {
                push_u32(out, tex_handle);
                push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE);
                push_u32(out, *format_raw);
                push_u32(out, 1); // width
                push_u32(out, 1); // height
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 2); // row_pitch_bytes
                push_u32(out, alloc_id);
                push_u32(out, backing_offset);
                push_u64(out, 0); // reserved0
            });

            emit_packet(out, OPC_RESOURCE_DIRTY_RANGE, |out| {
                push_u32(out, tex_handle);
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, 2); // size_bytes
            });

            emit_packet(out, OPC_SET_RENDER_TARGETS, |out| {
                push_u32(out, 1); // color_count
                push_u32(out, 0); // depth_stencil
                push_u32(out, rt_handle);
                for _ in 0..7 {
                    push_u32(out, 0);
                }
            });

            emit_packet(out, OPC_CLEAR, |out| {
                push_u32(out, AEROGPU_CLEAR_COLOR);
                push_f32(out, 0.0);
                push_f32(out, 0.0);
                push_f32(out, 0.0);
                push_f32(out, 1.0);
                push_f32(out, 1.0);
                push_u32(out, 0);
            });

            emit_packet(out, OPC_SET_TEXTURE, |out| {
                push_u32(out, AerogpuShaderStage::Pixel as u32);
                push_u32(out, 0); // slot
                push_u32(out, tex_handle);
                push_u32(out, 0); // reserved0
            });

            emit_packet(out, OPC_DRAW, |out| {
                push_u32(out, 3);
                push_u32(out, 1);
                push_u32(out, 0);
                push_u32(out, 0);
            });

            emit_packet(out, OPC_PRESENT, |out| {
                push_u32(out, 0);
                push_u32(out, 0);
            });
        });

        exec.execute_cmd_stream_with_guest_memory(&stream_dirty, &mut guest_memory, Some(&alloc_table))
            .unwrap_or_else(|e| panic!("dirty-range path stream for {name} should succeed: {e}"));

        let (_w, _h, rgba) = pollster::block_on(exec.readback_texture_rgba8(rt_handle))
            .unwrap_or_else(|e| panic!("readback dirty-range {name} should succeed: {e}"));
        assert_eq!(&rgba[0..4], expected_rgba, "dirty-range path {name}");
    }
}
