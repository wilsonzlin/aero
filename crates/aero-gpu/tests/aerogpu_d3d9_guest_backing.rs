mod common;

use aero_gpu::aerogpu_executor::{AllocEntry, AllocTable};
use aero_gpu::{AerogpuD3d9Error, AerogpuD3d9Executor, GuestMemory, VecGuestMemory};
use aero_protocol::aerogpu::{
    aerogpu_cmd::{
        AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode,
        AerogpuCmdStreamHeader as ProtocolCmdStreamHeader, AerogpuPrimitiveTopology,
        AerogpuShaderStage, AEROGPU_CLEAR_COLOR, AEROGPU_CMD_STREAM_MAGIC,
        AEROGPU_COPY_FLAG_WRITEBACK_DST, AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
        AEROGPU_RESOURCE_USAGE_TEXTURE, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
    },
    aerogpu_pci::{AerogpuFormat, AEROGPU_ABI_MAJOR},
    aerogpu_ring as ring,
};

const CMD_STREAM_SIZE_BYTES_OFFSET: usize =
    core::mem::offset_of!(ProtocolCmdStreamHeader, size_bytes);
const CMD_HDR_SIZE_BYTES_OFFSET: usize = core::mem::offset_of!(ProtocolCmdHdr, size_bytes);
const AEROGPU_ABI_VERSION_U32_COMPAT: u32 = AEROGPU_ABI_MAJOR << 16; // minor=0

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
    push_u32(&mut out, AEROGPU_ABI_VERSION_U32_COMPAT);
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

#[test]
fn d3d9_cmd_stream_flushes_guest_backed_resources_from_dirty_ranges() {
    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(module_path!(), "wgpu adapter not found");
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    // Protocol constants from `drivers/aerogpu/protocol/aerogpu_cmd.h`.
    const OPC_CREATE_BUFFER: u32 = AerogpuCmdOpcode::CreateBuffer as u32;
    const OPC_CREATE_TEXTURE2D: u32 = AerogpuCmdOpcode::CreateTexture2d as u32;
    const OPC_RESOURCE_DIRTY_RANGE: u32 = AerogpuCmdOpcode::ResourceDirtyRange as u32;
    const OPC_CREATE_SHADER_DXBC: u32 = AerogpuCmdOpcode::CreateShaderDxbc as u32;
    const OPC_BIND_SHADERS: u32 = AerogpuCmdOpcode::BindShaders as u32;
    const OPC_CREATE_INPUT_LAYOUT: u32 = AerogpuCmdOpcode::CreateInputLayout as u32;
    const OPC_SET_INPUT_LAYOUT: u32 = AerogpuCmdOpcode::SetInputLayout as u32;
    const OPC_SET_RENDER_TARGETS: u32 = AerogpuCmdOpcode::SetRenderTargets as u32;
    const OPC_SET_VIEWPORT: u32 = AerogpuCmdOpcode::SetViewport as u32;
    const OPC_SET_SCISSOR: u32 = AerogpuCmdOpcode::SetScissor as u32;
    const OPC_SET_VERTEX_BUFFERS: u32 = AerogpuCmdOpcode::SetVertexBuffers as u32;
    const OPC_SET_PRIMITIVE_TOPOLOGY: u32 = AerogpuCmdOpcode::SetPrimitiveTopology as u32;
    const OPC_SET_TEXTURE: u32 = AerogpuCmdOpcode::SetTexture as u32;
    const OPC_CLEAR: u32 = AerogpuCmdOpcode::Clear as u32;
    const OPC_DRAW: u32 = AerogpuCmdOpcode::Draw as u32;
    const OPC_PRESENT: u32 = AerogpuCmdOpcode::Present as u32;

    const AEROGPU_FORMAT_R8G8B8A8_UNORM: u32 = AerogpuFormat::R8G8B8A8Unorm as u32;
    const AEROGPU_TOPOLOGY_TRIANGLELIST: u32 = AerogpuPrimitiveTopology::TriangleList as u32;

    const RT_HANDLE: u32 = 1;
    const VB_HANDLE: u32 = 2;
    const TEX_HANDLE: u32 = 3;
    const VS_HANDLE: u32 = 4;
    const PS_HANDLE: u32 = 5;
    const IL_HANDLE: u32 = 6;

    const VB_ALLOC_ID: u32 = 1;
    const TEX_ALLOC_ID: u32 = 2;
    const VB_GPA: u64 = 0x1000;
    const TEX_GPA: u64 = 0x2000;

    let width = 1u32;
    let height = 1u32;

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

    let tex_data = [255u8, 0, 0, 255];

    let mut guest_memory = VecGuestMemory::new(0x4000);
    guest_memory.write(VB_GPA, &vb_data).unwrap();
    guest_memory.write(TEX_GPA, &tex_data).unwrap();

    let alloc_table = AllocTable::new([
        (
            VB_ALLOC_ID,
            AllocEntry {
                flags: 0,
                gpa: VB_GPA,
                size_bytes: 4096,
            },
        ),
        (
            TEX_ALLOC_ID,
            AllocEntry {
                flags: 0,
                gpa: TEX_GPA,
                size_bytes: 4096,
            },
        ),
    ])
    .expect("alloc table");

    let vs_bytes = assemble_vs_fullscreen_pos_tex();
    let ps_bytes = assemble_ps_sample_tex0();

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
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, 4); // row_pitch_bytes
            push_u32(out, TEX_ALLOC_ID);
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, OPC_CREATE_BUFFER, |out| {
            push_u32(out, VB_HANDLE);
            push_u32(out, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER);
            push_u64(out, vb_data.len() as u64);
            push_u32(out, VB_ALLOC_ID);
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, OPC_RESOURCE_DIRTY_RANGE, |out| {
            push_u32(out, VB_HANDLE);
            push_u32(out, 0); // reserved0
            push_u64(out, 0);
            push_u64(out, vb_data.len() as u64);
        });

        emit_packet(out, OPC_RESOURCE_DIRTY_RANGE, |out| {
            push_u32(out, TEX_HANDLE);
            push_u32(out, 0); // reserved0
            push_u64(out, 0);
            push_u64(out, tex_data.len() as u64);
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
            push_u32(out, TEX_HANDLE);
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

    exec.execute_cmd_stream_with_guest_memory(&stream, &mut guest_memory, Some(&alloc_table))
        .expect("execute should succeed");

    let (_out_w, _out_h, rgba) = pollster::block_on(exec.readback_texture_rgba8(RT_HANDLE))
        .expect("readback should succeed");
    assert_eq!(&rgba[0..4], &[255, 0, 0, 255]);
}

#[test]
fn d3d9_cmd_stream_uses_current_alloc_table_for_dirty_range_uploads() {
    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(module_path!(), "wgpu adapter not found");
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    // Protocol constants from `drivers/aerogpu/protocol/aerogpu_cmd.h`.
    const OPC_CREATE_BUFFER: u32 = AerogpuCmdOpcode::CreateBuffer as u32;
    const OPC_CREATE_TEXTURE2D: u32 = AerogpuCmdOpcode::CreateTexture2d as u32;
    const OPC_RESOURCE_DIRTY_RANGE: u32 = AerogpuCmdOpcode::ResourceDirtyRange as u32;
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
    const OPC_SET_TEXTURE: u32 = AerogpuCmdOpcode::SetTexture as u32;
    const OPC_CLEAR: u32 = AerogpuCmdOpcode::Clear as u32;
    const OPC_DRAW: u32 = AerogpuCmdOpcode::Draw as u32;
    const OPC_PRESENT: u32 = AerogpuCmdOpcode::Present as u32;

    const AEROGPU_FORMAT_R8G8B8A8_UNORM: u32 = AerogpuFormat::R8G8B8A8Unorm as u32;
    const AEROGPU_TOPOLOGY_TRIANGLELIST: u32 = AerogpuPrimitiveTopology::TriangleList as u32;

    const RT_HANDLE: u32 = 1;
    const TEX_HANDLE: u32 = 2;
    const VB_HANDLE: u32 = 3;
    const VS_HANDLE: u32 = 4;
    const PS_HANDLE: u32 = 5;
    const IL_HANDLE: u32 = 6;

    const TEX_ALLOC_ID: u32 = 1;
    const TEX_GPA_A: u64 = 0x1000;
    const TEX_GPA_B: u64 = 0x2000;

    let width = 1u32;
    let height = 1u32;

    // Populate two possible base GPAs. The executor must consult the current submission's
    // allocation table when flushing dirty ranges; it must not bake the base GPA at create time.
    let mut guest_memory = VecGuestMemory::new(0x4000);
    guest_memory.write(TEX_GPA_A, &[0, 255, 0, 255]).unwrap(); // green
    guest_memory.write(TEX_GPA_B, &[255, 0, 0, 255]).unwrap(); // red

    let alloc_table_create = AllocTable::new([(
        TEX_ALLOC_ID,
        AllocEntry {
            flags: 0,
            gpa: TEX_GPA_A,
            size_bytes: 4096,
        },
    )])
    .expect("alloc table");
    let alloc_table_draw = AllocTable::new([(
        TEX_ALLOC_ID,
        AllocEntry {
            flags: 0,
            gpa: TEX_GPA_B,
            size_bytes: 4096,
        },
    )])
    .expect("alloc table");

    let stream_create = build_stream(|out| {
        emit_packet(out, OPC_CREATE_TEXTURE2D, |out| {
            push_u32(out, TEX_HANDLE);
            push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE);
            push_u32(out, AEROGPU_FORMAT_R8G8B8A8_UNORM);
            push_u32(out, width);
            push_u32(out, height);
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, 4); // row_pitch_bytes
            push_u32(out, TEX_ALLOC_ID);
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });
    });

    exec.execute_cmd_stream_with_guest_memory(
        &stream_create,
        &mut guest_memory,
        Some(&alloc_table_create),
    )
    .expect("create submission should succeed");

    // Full-screen triangle (pos float4 + texcoord float4), same as the main guest-backing test.
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

    let vs_bytes = assemble_vs_fullscreen_pos_tex();
    let ps_bytes = assemble_ps_sample_tex0();

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

    let stream_draw = build_stream(|out| {
        // Render target.
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

        // Host-owned vertex buffer.
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

        // Mark the guest-backed texture dirty: the executor must consult alloc_table_draw,
        // not the base GPA from alloc_table_create.
        emit_packet(out, OPC_RESOURCE_DIRTY_RANGE, |out| {
            push_u32(out, TEX_HANDLE);
            push_u32(out, 0); // reserved0
            push_u64(out, 0);
            push_u64(out, 4);
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
            push_u32(out, TEX_HANDLE);
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

    exec.execute_cmd_stream_with_guest_memory(
        &stream_draw,
        &mut guest_memory,
        Some(&alloc_table_draw),
    )
    .expect("draw submission should succeed");

    let (_out_w, _out_h, rgba) = pollster::block_on(exec.readback_texture_rgba8(RT_HANDLE))
        .expect("readback should succeed");
    assert_eq!(&rgba[0..4], &[255, 0, 0, 255]);
}

#[test]
fn d3d9_copy_texture2d_flushes_dst_dirty_ranges_before_sampling() {
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
    const OPC_RESOURCE_DIRTY_RANGE: u32 = AerogpuCmdOpcode::ResourceDirtyRange as u32;
    const OPC_UPLOAD_RESOURCE: u32 = AerogpuCmdOpcode::UploadResource as u32;
    const OPC_COPY_TEXTURE2D: u32 = AerogpuCmdOpcode::CopyTexture2d as u32;
    const OPC_CREATE_SHADER_DXBC: u32 = AerogpuCmdOpcode::CreateShaderDxbc as u32;
    const OPC_BIND_SHADERS: u32 = AerogpuCmdOpcode::BindShaders as u32;
    const OPC_CREATE_INPUT_LAYOUT: u32 = AerogpuCmdOpcode::CreateInputLayout as u32;
    const OPC_SET_INPUT_LAYOUT: u32 = AerogpuCmdOpcode::SetInputLayout as u32;
    const OPC_SET_RENDER_TARGETS: u32 = AerogpuCmdOpcode::SetRenderTargets as u32;
    const OPC_SET_VIEWPORT: u32 = AerogpuCmdOpcode::SetViewport as u32;
    const OPC_SET_SCISSOR: u32 = AerogpuCmdOpcode::SetScissor as u32;
    const OPC_SET_VERTEX_BUFFERS: u32 = AerogpuCmdOpcode::SetVertexBuffers as u32;
    const OPC_SET_PRIMITIVE_TOPOLOGY: u32 = AerogpuCmdOpcode::SetPrimitiveTopology as u32;
    const OPC_SET_TEXTURE: u32 = AerogpuCmdOpcode::SetTexture as u32;
    const OPC_DRAW: u32 = AerogpuCmdOpcode::Draw as u32;

    const AEROGPU_FORMAT_R8G8B8A8_UNORM: u32 = AerogpuFormat::R8G8B8A8Unorm as u32;
    const AEROGPU_TOPOLOGY_TRIANGLELIST: u32 = AerogpuPrimitiveTopology::TriangleList as u32;

    const RT_HANDLE: u32 = 1;
    const SRC_TEX_HANDLE: u32 = 2;
    const DST_TEX_HANDLE: u32 = 3;
    const VB_HANDLE: u32 = 4;
    const VS_HANDLE: u32 = 5;
    const PS_HANDLE: u32 = 6;
    const IL_HANDLE: u32 = 7;

    const DST_ALLOC_ID: u32 = 1;
    const DST_GPA: u64 = 0x1000;

    let width = 1u32;
    let height = 1u32;

    let mut vb_data = Vec::new();
    // D3D9 defaults to back-face culling with clockwise front faces.
    // Use a clockwise full-screen triangle so the test does not depend on cull state.
    let verts = [
        (-1.0f32, -1.0f32, 0.0f32, 1.0f32),
        (-1.0f32, 3.0f32, 0.0f32, 1.0f32),
        (3.0f32, -1.0f32, 0.0f32, 1.0f32),
    ];
    for (x, y, z, w) in verts {
        // position
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

    let src_tex_data = [255u8, 0, 0, 255];
    let dst_tex_data = [0u8, 255, 0, 255];

    let mut guest_memory = VecGuestMemory::new(0x4000);
    guest_memory.write(DST_GPA, &dst_tex_data).unwrap();

    let alloc_table = AllocTable::new([(
        DST_ALLOC_ID,
        AllocEntry {
            flags: 0,
            gpa: DST_GPA,
            size_bytes: 4096,
        },
    )])
    .expect("alloc table");

    let vs_bytes = assemble_vs_fullscreen_pos_tex();
    let ps_bytes = assemble_ps_sample_tex0();

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

    let stream = build_stream(|out| {
        // Render target.
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

        // Source texture (host-owned).
        emit_packet(out, OPC_CREATE_TEXTURE2D, |out| {
            push_u32(out, SRC_TEX_HANDLE);
            push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE);
            push_u32(out, AEROGPU_FORMAT_R8G8B8A8_UNORM);
            push_u32(out, 1);
            push_u32(out, 1);
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, 4); // row_pitch_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, OPC_UPLOAD_RESOURCE, |out| {
            push_u32(out, SRC_TEX_HANDLE);
            push_u32(out, 0); // reserved0
            push_u64(out, 0); // offset_bytes
            push_u64(out, src_tex_data.len() as u64); // size_bytes
            out.extend_from_slice(&src_tex_data);
        });

        // Destination texture (guest-backed), pre-filled with green in guest memory and marked dirty.
        emit_packet(out, OPC_CREATE_TEXTURE2D, |out| {
            push_u32(out, DST_TEX_HANDLE);
            push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE);
            push_u32(out, AEROGPU_FORMAT_R8G8B8A8_UNORM);
            push_u32(out, 1);
            push_u32(out, 1);
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, 4); // row_pitch_bytes
            push_u32(out, DST_ALLOC_ID);
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, OPC_RESOURCE_DIRTY_RANGE, |out| {
            push_u32(out, DST_TEX_HANDLE);
            push_u32(out, 0); // reserved0
            push_u64(out, 0);
            push_u64(out, dst_tex_data.len() as u64);
        });

        emit_packet(out, OPC_COPY_TEXTURE2D, |out| {
            push_u32(out, DST_TEX_HANDLE);
            push_u32(out, SRC_TEX_HANDLE);
            push_u32(out, 0); // dst_mip_level
            push_u32(out, 0); // dst_array_layer
            push_u32(out, 0); // src_mip_level
            push_u32(out, 0); // src_array_layer
            push_u32(out, 0); // dst_x
            push_u32(out, 0); // dst_y
            push_u32(out, 0); // src_x
            push_u32(out, 0); // src_y
            push_u32(out, 1); // width
            push_u32(out, 1); // height
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

        emit_packet(out, OPC_SET_TEXTURE, |out| {
            push_u32(out, AerogpuShaderStage::Pixel as u32);
            push_u32(out, 0); // slot
            push_u32(out, DST_TEX_HANDLE);
            push_u32(out, 0); // reserved0
        });

        emit_packet(out, OPC_DRAW, |out| {
            push_u32(out, 3);
            push_u32(out, 1);
            push_u32(out, 0);
            push_u32(out, 0);
        });
    });

    exec.execute_cmd_stream_with_guest_memory(&stream, &mut guest_memory, Some(&alloc_table))
        .expect("execute should succeed");

    let (_out_w, _out_h, rgba) = pollster::block_on(exec.readback_texture_rgba8(RT_HANDLE))
        .expect("readback should succeed");
    assert_eq!(
        &rgba[0..4],
        &src_tex_data,
        "copy should win over stale dirty-range upload"
    );
}

#[test]
fn d3d9_copy_buffer_writeback_writes_guest_backing() {
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
    const OPC_UPLOAD_RESOURCE: u32 = AerogpuCmdOpcode::UploadResource as u32;
    const OPC_COPY_BUFFER: u32 = AerogpuCmdOpcode::CopyBuffer as u32;

    const SRC_HANDLE: u32 = 1;
    const DST_HANDLE: u32 = 2;

    const DST_ALLOC_ID: u32 = 1;
    const DST_GPA: u64 = 0x1000;

    let mut guest_memory = VecGuestMemory::new(0x4000);
    let alloc_table = AllocTable::new([(
        DST_ALLOC_ID,
        AllocEntry {
            flags: 0,
            gpa: DST_GPA,
            size_bytes: 0x1000,
        },
    )])
    .expect("alloc table");

    let pattern = [
        0xDEu8, 0xAD, 0xBE, 0xEF, 0xAA, 0xBB, 0xCC, 0xDD, 0x10, 0x20, 0x30, 0x40, 0x55, 0x66, 0x77,
        0x88,
    ];
    let stream = build_stream(|out| {
        emit_packet(out, OPC_CREATE_BUFFER, |out| {
            push_u32(out, SRC_HANDLE);
            push_u32(out, 0); // usage_flags
            push_u64(out, 16); // size_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, OPC_CREATE_BUFFER, |out| {
            push_u32(out, DST_HANDLE);
            push_u32(out, 0); // usage_flags
            push_u64(out, 16); // size_bytes
            push_u32(out, DST_ALLOC_ID); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, OPC_UPLOAD_RESOURCE, |out| {
            push_u32(out, SRC_HANDLE);
            push_u32(out, 0); // reserved0
            push_u64(out, 0); // offset_bytes
            push_u64(out, pattern.len() as u64); // size_bytes
            out.extend_from_slice(&pattern);
        });

        emit_packet(out, OPC_COPY_BUFFER, |out| {
            push_u32(out, DST_HANDLE);
            push_u32(out, SRC_HANDLE);
            push_u64(out, 0); // dst_offset_bytes
            push_u64(out, 0); // src_offset_bytes
            push_u64(out, 16); // size_bytes
            push_u32(out, AEROGPU_COPY_FLAG_WRITEBACK_DST);
            push_u32(out, 0); // reserved0
        });
    });

    exec.execute_cmd_stream_with_guest_memory(&stream, &mut guest_memory, Some(&alloc_table))
        .expect("execute should succeed");

    let mut out = vec![0u8; pattern.len()];
    guest_memory
        .read(DST_GPA, &mut out)
        .expect("read guest backing");
    assert_eq!(&out, &pattern);
}

#[test]
fn d3d9_copy_buffer_writeback_rejects_readonly_alloc() {
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
    const OPC_UPLOAD_RESOURCE: u32 = AerogpuCmdOpcode::UploadResource as u32;
    const OPC_COPY_BUFFER: u32 = AerogpuCmdOpcode::CopyBuffer as u32;

    const SRC_HANDLE: u32 = 1;
    const DST_HANDLE: u32 = 2;

    const DST_ALLOC_ID: u32 = 1;
    const DST_GPA: u64 = 0x1000;

    let mut guest_memory = VecGuestMemory::new(0x4000);
    guest_memory
        .write(DST_GPA, &[0xEEu8; 16])
        .expect("write dst sentinel");
    let alloc_table = AllocTable::new([(
        DST_ALLOC_ID,
        AllocEntry {
            flags: ring::AEROGPU_ALLOC_FLAG_READONLY,
            gpa: DST_GPA,
            size_bytes: 0x1000,
        },
    )])
    .expect("alloc table");

    let pattern = [
        0xDEu8, 0xAD, 0xBE, 0xEF, 0xAA, 0xBB, 0xCC, 0xDD, 0x10, 0x20, 0x30, 0x40, 0x55, 0x66, 0x77,
        0x88,
    ];
    let stream = build_stream(|out| {
        emit_packet(out, OPC_CREATE_BUFFER, |out| {
            push_u32(out, SRC_HANDLE);
            push_u32(out, 0); // usage_flags
            push_u64(out, 16); // size_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, OPC_CREATE_BUFFER, |out| {
            push_u32(out, DST_HANDLE);
            push_u32(out, 0); // usage_flags
            push_u64(out, 16); // size_bytes
            push_u32(out, DST_ALLOC_ID); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, OPC_UPLOAD_RESOURCE, |out| {
            push_u32(out, SRC_HANDLE);
            push_u32(out, 0); // reserved0
            push_u64(out, 0); // offset_bytes
            push_u64(out, pattern.len() as u64); // size_bytes
            out.extend_from_slice(&pattern);
        });

        emit_packet(out, OPC_COPY_BUFFER, |out| {
            push_u32(out, DST_HANDLE);
            push_u32(out, SRC_HANDLE);
            push_u64(out, 0); // dst_offset_bytes
            push_u64(out, 0); // src_offset_bytes
            push_u64(out, 16); // size_bytes
            push_u32(out, AEROGPU_COPY_FLAG_WRITEBACK_DST);
            push_u32(out, 0); // reserved0
        });
    });

    let err = exec
        .execute_cmd_stream_with_guest_memory(&stream, &mut guest_memory, Some(&alloc_table))
        .expect_err("execute should fail");
    match err {
        AerogpuD3d9Error::Validation(msg) => {
            let msg_lc = msg.to_ascii_lowercase();
            assert!(
                msg_lc.contains("readonly") || msg_lc.contains("read-only"),
                "expected READONLY validation error, got: {msg}"
            );
        }
        other => panic!("unexpected error: {other}"),
    }

    let mut out = vec![0u8; pattern.len()];
    guest_memory
        .read(DST_GPA, &mut out)
        .expect("read guest backing");
    assert_eq!(&out, &[0xEEu8; 16]);
}

#[test]
fn d3d9_copy_buffer_writeback_requires_alloc_table_each_submit() {
    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(module_path!(), "wgpu adapter not found");
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    const OPC_CREATE_BUFFER: u32 = AerogpuCmdOpcode::CreateBuffer as u32;
    const OPC_UPLOAD_RESOURCE: u32 = AerogpuCmdOpcode::UploadResource as u32;
    const OPC_COPY_BUFFER: u32 = AerogpuCmdOpcode::CopyBuffer as u32;

    const SRC_HANDLE: u32 = 1;
    const DST_HANDLE: u32 = 2;

    const DST_ALLOC_ID: u32 = 1;
    const DST_GPA: u64 = 0x1000;

    let mut guest_memory = VecGuestMemory::new(0x4000);
    guest_memory
        .write(DST_GPA, &[0xEEu8; 16])
        .expect("write dst sentinel");

    let alloc_table = AllocTable::new([(
        DST_ALLOC_ID,
        AllocEntry {
            flags: 0,
            gpa: DST_GPA,
            size_bytes: 0x1000,
        },
    )])
    .expect("alloc table");

    let pattern = [
        0xDEu8, 0xAD, 0xBE, 0xEF, 0xAA, 0xBB, 0xCC, 0xDD, 0x10, 0x20, 0x30, 0x40, 0x55, 0x66, 0x77,
        0x88,
    ];

    // Submit 1: create SRC and DST, upload SRC payload.
    let stream_create = build_stream(|out| {
        emit_packet(out, OPC_CREATE_BUFFER, |out| {
            push_u32(out, SRC_HANDLE);
            push_u32(out, 0); // usage_flags
            push_u64(out, pattern.len() as u64); // size_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, OPC_CREATE_BUFFER, |out| {
            push_u32(out, DST_HANDLE);
            push_u32(out, 0); // usage_flags
            push_u64(out, pattern.len() as u64); // size_bytes
            push_u32(out, DST_ALLOC_ID); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, OPC_UPLOAD_RESOURCE, |out| {
            push_u32(out, SRC_HANDLE);
            push_u32(out, 0); // reserved0
            push_u64(out, 0); // offset_bytes
            push_u64(out, pattern.len() as u64); // size_bytes
            out.extend_from_slice(&pattern);
        });
    });

    exec.execute_cmd_stream_with_guest_memory(
        &stream_create,
        &mut guest_memory,
        Some(&alloc_table),
    )
    .expect("create should succeed");

    let stream_copy = build_stream(|out| {
        emit_packet(out, OPC_COPY_BUFFER, |out| {
            push_u32(out, DST_HANDLE);
            push_u32(out, SRC_HANDLE);
            push_u64(out, 0); // dst_offset_bytes
            push_u64(out, 0); // src_offset_bytes
            push_u64(out, pattern.len() as u64); // size_bytes
            push_u32(out, AEROGPU_COPY_FLAG_WRITEBACK_DST);
            push_u32(out, 0); // reserved0
        });
    });

    // Submit 2: COPY_BUFFER (WRITEBACK_DST) but no alloc table.
    let err = exec
        .execute_cmd_stream_with_guest_memory(&stream_copy, &mut guest_memory, None)
        .expect_err("expected missing alloc table error");
    match err {
        AerogpuD3d9Error::MissingAllocationTable(alloc_id) => assert_eq!(alloc_id, DST_ALLOC_ID),
        other => panic!("unexpected error: {other}"),
    }
    let mut out = [0u8; 16];
    guest_memory.read(DST_GPA, &mut out).unwrap();
    assert_eq!(out, [0xEEu8; 16]);

    // Submit 3: alloc table present but missing the dst alloc_id.
    let bad_alloc_table = AllocTable::new([(
        DST_ALLOC_ID + 1,
        AllocEntry {
            flags: 0,
            gpa: 0x2000,
            size_bytes: 0x1000,
        },
    )])
    .expect("bad alloc table");
    let err = exec
        .execute_cmd_stream_with_guest_memory(
            &stream_copy,
            &mut guest_memory,
            Some(&bad_alloc_table),
        )
        .expect_err("expected missing alloc_id error");
    match err {
        AerogpuD3d9Error::MissingAllocTable(alloc_id) => assert_eq!(alloc_id, DST_ALLOC_ID),
        other => panic!("unexpected error: {other}"),
    }
    guest_memory.read(DST_GPA, &mut out).unwrap();
    assert_eq!(out, [0xEEu8; 16]);

    // Submit 4: correct alloc table.
    exec.execute_cmd_stream_with_guest_memory(&stream_copy, &mut guest_memory, Some(&alloc_table))
        .expect("writeback should succeed");
    guest_memory.read(DST_GPA, &mut out).unwrap();
    assert_eq!(out, pattern);
}

#[test]
fn d3d9_copy_buffer_writeback_does_not_consume_dirty_ranges_on_alloc_table_error() {
    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(module_path!(), "wgpu adapter not found");
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    const OPC_CREATE_BUFFER: u32 = AerogpuCmdOpcode::CreateBuffer as u32;
    const OPC_RESOURCE_DIRTY_RANGE: u32 = AerogpuCmdOpcode::ResourceDirtyRange as u32;
    const OPC_COPY_BUFFER: u32 = AerogpuCmdOpcode::CopyBuffer as u32;

    const SRC_HANDLE: u32 = 1;
    const DST_HANDLE: u32 = 2;
    const OUT_HANDLE: u32 = 3;

    const SRC_ALLOC_ID: u32 = 1;
    const DST_ALLOC_ID: u32 = 2;
    const OUT_ALLOC_ID: u32 = 3;

    const SRC_GPA: u64 = 0x1000;
    const DST_GPA: u64 = 0x2000;
    const OUT_GPA: u64 = 0x3000;

    let mut guest_memory = VecGuestMemory::new(0x4000);

    let pattern_a = [
        0x00u8, 0x01, 0x02, 0x03, 0x10, 0x11, 0x12, 0x13, 0x20, 0x21, 0x22, 0x23, 0x30, 0x31, 0x32,
        0x33,
    ];
    let pattern_b = [
        0x99u8, 0x98, 0x97, 0x96, 0x89, 0x88, 0x87, 0x86, 0x79, 0x78, 0x77, 0x76, 0x69, 0x68, 0x67,
        0x66,
    ];

    guest_memory.write(SRC_GPA, &pattern_a).unwrap();
    guest_memory.write(DST_GPA, &[0xEEu8; 16]).unwrap();
    guest_memory.write(OUT_GPA, &[0xEEu8; 16]).unwrap();

    let alloc_table_all = AllocTable::new([
        (
            SRC_ALLOC_ID,
            AllocEntry {
                flags: 0,
                gpa: SRC_GPA,
                size_bytes: 0x1000,
            },
        ),
        (
            DST_ALLOC_ID,
            AllocEntry {
                flags: 0,
                gpa: DST_GPA,
                size_bytes: 0x1000,
            },
        ),
        (
            OUT_ALLOC_ID,
            AllocEntry {
                flags: 0,
                gpa: OUT_GPA,
                size_bytes: 0x1000,
            },
        ),
    ])
    .expect("alloc table");

    let alloc_table_src_only = AllocTable::new([(
        SRC_ALLOC_ID,
        AllocEntry {
            flags: 0,
            gpa: SRC_GPA,
            size_bytes: 0x1000,
        },
    )])
    .expect("alloc table");

    // First submission: create buffers + mark the src dirty (but do not flush yet).
    let stream_create = build_stream(|out| {
        emit_packet(out, OPC_CREATE_BUFFER, |out| {
            push_u32(out, SRC_HANDLE);
            push_u32(out, 0); // usage_flags
            push_u64(out, 16); // size_bytes
            push_u32(out, SRC_ALLOC_ID);
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, OPC_CREATE_BUFFER, |out| {
            push_u32(out, DST_HANDLE);
            push_u32(out, 0); // usage_flags
            push_u64(out, 16); // size_bytes
            push_u32(out, DST_ALLOC_ID);
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, OPC_CREATE_BUFFER, |out| {
            push_u32(out, OUT_HANDLE);
            push_u32(out, 0); // usage_flags
            push_u64(out, 16); // size_bytes
            push_u32(out, OUT_ALLOC_ID);
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, OPC_RESOURCE_DIRTY_RANGE, |out| {
            push_u32(out, SRC_HANDLE);
            push_u32(out, 0); // reserved0
            push_u64(out, 0); // offset_bytes
            push_u64(out, 16); // size_bytes
        });
    });

    exec.execute_cmd_stream_with_guest_memory(
        &stream_create,
        &mut guest_memory,
        Some(&alloc_table_all),
    )
    .expect("create should succeed");

    // Second submission: attempt WRITEBACK_DST without including the destination alloc in the alloc
    // table. This must fail *without* consuming the pending dirty range for the source buffer.
    let stream_invalid = build_stream(|out| {
        emit_packet(out, OPC_COPY_BUFFER, |out| {
            push_u32(out, DST_HANDLE);
            push_u32(out, SRC_HANDLE);
            push_u64(out, 0); // dst_offset_bytes
            push_u64(out, 0); // src_offset_bytes
            push_u64(out, 16); // size_bytes
            push_u32(out, AEROGPU_COPY_FLAG_WRITEBACK_DST);
            push_u32(out, 0); // reserved0
        });
    });

    exec.execute_cmd_stream_with_guest_memory(
        &stream_invalid,
        &mut guest_memory,
        Some(&alloc_table_src_only),
    )
    .expect_err("expected alloc table validation error");

    // Mutate the guest backing for the source buffer without emitting a new dirty range. If the
    // executor incorrectly flushed + cleared the dirty range during the failing submission above,
    // the next copy would observe stale GPU data instead of the updated guest memory.
    guest_memory.write(SRC_GPA, &pattern_b).unwrap();

    // Third submission: valid writeback into OUT_HANDLE should observe the updated bytes.
    let stream_valid = build_stream(|out| {
        emit_packet(out, OPC_COPY_BUFFER, |out| {
            push_u32(out, OUT_HANDLE);
            push_u32(out, SRC_HANDLE);
            push_u64(out, 0); // dst_offset_bytes
            push_u64(out, 0); // src_offset_bytes
            push_u64(out, 16); // size_bytes
            push_u32(out, AEROGPU_COPY_FLAG_WRITEBACK_DST);
            push_u32(out, 0); // reserved0
        });
    });

    exec.execute_cmd_stream_with_guest_memory(
        &stream_valid,
        &mut guest_memory,
        Some(&alloc_table_all),
    )
    .expect("writeback should succeed");

    let mut out = [0u8; 16];
    guest_memory.read(OUT_GPA, &mut out).unwrap();
    assert_eq!(&out, &pattern_b);
}

#[test]
fn d3d9_copy_texture2d_writeback_does_not_consume_dirty_ranges_on_alloc_table_error() {
    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(module_path!(), "wgpu adapter not found");
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    const OPC_CREATE_TEXTURE2D: u32 = AerogpuCmdOpcode::CreateTexture2d as u32;
    const OPC_RESOURCE_DIRTY_RANGE: u32 = AerogpuCmdOpcode::ResourceDirtyRange as u32;
    const OPC_COPY_TEXTURE2D: u32 = AerogpuCmdOpcode::CopyTexture2d as u32;

    const SRC_HANDLE: u32 = 1;
    const DST_HANDLE: u32 = 2;
    const OUT_HANDLE: u32 = 3;

    const SRC_ALLOC_ID: u32 = 1;
    const DST_ALLOC_ID: u32 = 2;
    const OUT_ALLOC_ID: u32 = 3;

    const SRC_GPA: u64 = 0x1000;
    const DST_GPA: u64 = 0x2000;
    const OUT_GPA: u64 = 0x3000;

    let mut guest_memory = VecGuestMemory::new(0x4000);

    let pixel_a = [1u8, 2, 3, 4];
    let pixel_b = [5u8, 6, 7, 8];
    guest_memory.write(SRC_GPA, &pixel_a).unwrap();
    guest_memory.write(DST_GPA, &[0xEEu8; 4]).unwrap();
    guest_memory.write(OUT_GPA, &[0xEEu8; 4]).unwrap();

    let alloc_table_all = AllocTable::new([
        (
            SRC_ALLOC_ID,
            AllocEntry {
                flags: 0,
                gpa: SRC_GPA,
                size_bytes: 0x1000,
            },
        ),
        (
            DST_ALLOC_ID,
            AllocEntry {
                flags: 0,
                gpa: DST_GPA,
                size_bytes: 0x1000,
            },
        ),
        (
            OUT_ALLOC_ID,
            AllocEntry {
                flags: 0,
                gpa: OUT_GPA,
                size_bytes: 0x1000,
            },
        ),
    ])
    .expect("alloc table");

    let alloc_table_src_only = AllocTable::new([(
        SRC_ALLOC_ID,
        AllocEntry {
            flags: 0,
            gpa: SRC_GPA,
            size_bytes: 0x1000,
        },
    )])
    .expect("alloc table");

    // First submission: create textures + mark the src dirty (but do not flush yet).
    let stream_create = build_stream(|out| {
        for (handle, alloc_id) in [
            (SRC_HANDLE, SRC_ALLOC_ID),
            (DST_HANDLE, DST_ALLOC_ID),
            (OUT_HANDLE, OUT_ALLOC_ID),
        ] {
            emit_packet(out, OPC_CREATE_TEXTURE2D, |out| {
                push_u32(out, handle);
                push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE); // usage_flags
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32);
                push_u32(out, 1); // width
                push_u32(out, 1); // height
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 4); // row_pitch_bytes
                push_u32(out, alloc_id);
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });
        }

        emit_packet(out, OPC_RESOURCE_DIRTY_RANGE, |out| {
            push_u32(out, SRC_HANDLE);
            push_u32(out, 0); // reserved0
            push_u64(out, 0); // offset_bytes
            push_u64(out, 4); // size_bytes
        });
    });

    exec.execute_cmd_stream_with_guest_memory(
        &stream_create,
        &mut guest_memory,
        Some(&alloc_table_all),
    )
    .expect("create should succeed");

    // Second submission: attempt WRITEBACK_DST without including the destination alloc in the alloc
    // table. This must fail *without* consuming the pending dirty range for the source texture.
    let stream_invalid = build_stream(|out| {
        emit_packet(out, OPC_COPY_TEXTURE2D, |out| {
            push_u32(out, DST_HANDLE);
            push_u32(out, SRC_HANDLE);
            push_u32(out, 0); // dst_mip_level
            push_u32(out, 0); // dst_array_layer
            push_u32(out, 0); // src_mip_level
            push_u32(out, 0); // src_array_layer
            push_u32(out, 0); // dst_x
            push_u32(out, 0); // dst_y
            push_u32(out, 0); // src_x
            push_u32(out, 0); // src_y
            push_u32(out, 1); // width
            push_u32(out, 1); // height
            push_u32(out, AEROGPU_COPY_FLAG_WRITEBACK_DST);
            push_u32(out, 0); // reserved0
        });
    });

    exec.execute_cmd_stream_with_guest_memory(
        &stream_invalid,
        &mut guest_memory,
        Some(&alloc_table_src_only),
    )
    .expect_err("expected alloc table validation error");

    guest_memory.write(SRC_GPA, &pixel_b).unwrap();

    // Third submission: valid writeback into OUT_HANDLE should observe the updated bytes.
    let stream_valid = build_stream(|out| {
        emit_packet(out, OPC_COPY_TEXTURE2D, |out| {
            push_u32(out, OUT_HANDLE);
            push_u32(out, SRC_HANDLE);
            push_u32(out, 0); // dst_mip_level
            push_u32(out, 0); // dst_array_layer
            push_u32(out, 0); // src_mip_level
            push_u32(out, 0); // src_array_layer
            push_u32(out, 0); // dst_x
            push_u32(out, 0); // dst_y
            push_u32(out, 0); // src_x
            push_u32(out, 0); // src_y
            push_u32(out, 1); // width
            push_u32(out, 1); // height
            push_u32(out, AEROGPU_COPY_FLAG_WRITEBACK_DST);
            push_u32(out, 0); // reserved0
        });
    });

    exec.execute_cmd_stream_with_guest_memory(
        &stream_valid,
        &mut guest_memory,
        Some(&alloc_table_all),
    )
    .expect("writeback should succeed");

    let mut out = [0u8; 4];
    guest_memory.read(OUT_GPA, &mut out).unwrap();
    assert_eq!(&out, &pixel_b);
}

#[test]
fn d3d9_create_buffer_rebind_updates_guest_backing() {
    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(module_path!(), "wgpu adapter not found");
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    const OPC_CREATE_BUFFER: u32 = AerogpuCmdOpcode::CreateBuffer as u32;
    const OPC_UPLOAD_RESOURCE: u32 = AerogpuCmdOpcode::UploadResource as u32;
    const OPC_COPY_BUFFER: u32 = AerogpuCmdOpcode::CopyBuffer as u32;

    const SRC_HANDLE: u32 = 1;
    const DST_HANDLE: u32 = 2;

    const ALLOC_A: u32 = 1;
    const ALLOC_B: u32 = 2;
    const GPA_A: u64 = 0x1000;
    const GPA_B: u64 = 0x2000;

    let mut guest_memory = VecGuestMemory::new(0x8000);
    let alloc_table = AllocTable::new([
        (
            ALLOC_A,
            AllocEntry {
                flags: 0,
                gpa: GPA_A,
                size_bytes: 0x1000,
            },
        ),
        (
            ALLOC_B,
            AllocEntry {
                flags: 0,
                gpa: GPA_B,
                size_bytes: 0x1000,
            },
        ),
    ])
    .expect("alloc table");

    let pattern_a = [0x01u8; 16];
    let pattern_b = [0xBBu8; 16];

    let stream_a = build_stream(|out| {
        emit_packet(out, OPC_CREATE_BUFFER, |out| {
            push_u32(out, SRC_HANDLE);
            push_u32(out, 0); // usage_flags
            push_u64(out, 16); // size_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, OPC_CREATE_BUFFER, |out| {
            push_u32(out, DST_HANDLE);
            push_u32(out, 0); // usage_flags
            push_u64(out, 16); // size_bytes
            push_u32(out, ALLOC_A);
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, OPC_UPLOAD_RESOURCE, |out| {
            push_u32(out, SRC_HANDLE);
            push_u32(out, 0); // reserved0
            push_u64(out, 0); // offset_bytes
            push_u64(out, pattern_a.len() as u64); // size_bytes
            out.extend_from_slice(&pattern_a);
        });

        emit_packet(out, OPC_COPY_BUFFER, |out| {
            push_u32(out, DST_HANDLE);
            push_u32(out, SRC_HANDLE);
            push_u64(out, 0); // dst_offset_bytes
            push_u64(out, 0); // src_offset_bytes
            push_u64(out, 16); // size_bytes
            push_u32(out, AEROGPU_COPY_FLAG_WRITEBACK_DST);
            push_u32(out, 0); // reserved0
        });
    });

    exec.execute_cmd_stream_with_guest_memory(&stream_a, &mut guest_memory, Some(&alloc_table))
        .expect("first submit should succeed");

    let mut out_a = vec![0u8; pattern_a.len()];
    guest_memory.read(GPA_A, &mut out_a).unwrap();
    assert_eq!(&out_a, &pattern_a);

    let stream_b = build_stream(|out| {
        // Rebind dst to a different allocation ID.
        emit_packet(out, OPC_CREATE_BUFFER, |out| {
            push_u32(out, DST_HANDLE);
            push_u32(out, 0); // usage_flags
            push_u64(out, 16); // size_bytes
            push_u32(out, ALLOC_B);
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, OPC_UPLOAD_RESOURCE, |out| {
            push_u32(out, SRC_HANDLE);
            push_u32(out, 0); // reserved0
            push_u64(out, 0); // offset_bytes
            push_u64(out, pattern_b.len() as u64); // size_bytes
            out.extend_from_slice(&pattern_b);
        });

        emit_packet(out, OPC_COPY_BUFFER, |out| {
            push_u32(out, DST_HANDLE);
            push_u32(out, SRC_HANDLE);
            push_u64(out, 0); // dst_offset_bytes
            push_u64(out, 0); // src_offset_bytes
            push_u64(out, 16); // size_bytes
            push_u32(out, AEROGPU_COPY_FLAG_WRITEBACK_DST);
            push_u32(out, 0); // reserved0
        });
    });

    exec.execute_cmd_stream_with_guest_memory(&stream_b, &mut guest_memory, Some(&alloc_table))
        .expect("second submit should succeed");

    let mut out_a_after = vec![0u8; pattern_a.len()];
    guest_memory.read(GPA_A, &mut out_a_after).unwrap();
    assert_eq!(
        &out_a_after, &pattern_a,
        "rebind should not change previously written backing"
    );

    let mut out_b = vec![0u8; pattern_b.len()];
    guest_memory.read(GPA_B, &mut out_b).unwrap();
    assert_eq!(&out_b, &pattern_b);
}

#[test]
fn d3d9_create_buffer_rebind_rejects_mismatched_size() {
    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(module_path!(), "wgpu adapter not found");
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    let stream = build_stream(|out| {
        emit_packet(out, AerogpuCmdOpcode::CreateBuffer as u32, |out| {
            push_u32(out, 1); // buffer_handle
            push_u32(out, 0); // usage_flags
            push_u64(out, 16); // size_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, AerogpuCmdOpcode::CreateBuffer as u32, |out| {
            push_u32(out, 1); // buffer_handle
            push_u32(out, 0); // usage_flags
            push_u64(out, 32); // size_bytes (mismatch)
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });
    });

    match exec.execute_cmd_stream(&stream) {
        Ok(_) => panic!("expected mismatched CREATE_BUFFER rebind to fail"),
        Err(AerogpuD3d9Error::Validation(msg)) => assert!(msg.contains("mismatched immutable")),
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn d3d9_copy_texture2d_writeback_writes_guest_backing() {
    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(module_path!(), "wgpu adapter not found");
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    const OPC_CREATE_TEXTURE2D: u32 = AerogpuCmdOpcode::CreateTexture2d as u32;
    const OPC_UPLOAD_RESOURCE: u32 = AerogpuCmdOpcode::UploadResource as u32;
    const OPC_COPY_TEXTURE2D: u32 = AerogpuCmdOpcode::CopyTexture2d as u32;

    const AEROGPU_FORMAT_R8G8B8A8_UNORM: u32 = AerogpuFormat::R8G8B8A8Unorm as u32;

    const SRC_TEX: u32 = 1;
    const DST_TEX: u32 = 2;

    const DST_ALLOC_ID: u32 = 1;
    const DST_GPA: u64 = 0x1000;

    let width = 2u32;
    let height = 2u32;
    let row_pitch = 12u32;
    let bpr = width * 4;

    let src_tex_data: Vec<u8> = vec![
        // row 0: red, green
        255, 0, 0, 255, 0, 255, 0, 255, // row 1: blue, white
        0, 0, 255, 255, 255, 255, 255, 255,
    ];
    assert_eq!(src_tex_data.len(), (bpr * height) as usize);

    let mut guest_memory = VecGuestMemory::new(0x4000);
    let dst_init = vec![0xEEu8; (row_pitch * height) as usize];
    guest_memory.write(DST_GPA, &dst_init).unwrap();
    let alloc_table = AllocTable::new([(
        DST_ALLOC_ID,
        AllocEntry {
            flags: 0,
            gpa: DST_GPA,
            size_bytes: 0x1000,
        },
    )])
    .expect("alloc table");

    let stream = build_stream(|out| {
        // Host-owned source texture.
        emit_packet(out, OPC_CREATE_TEXTURE2D, |out| {
            push_u32(out, SRC_TEX);
            push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE);
            push_u32(out, AEROGPU_FORMAT_R8G8B8A8_UNORM);
            push_u32(out, width);
            push_u32(out, height);
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, 0); // row_pitch_bytes (use tight packing)
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, OPC_UPLOAD_RESOURCE, |out| {
            push_u32(out, SRC_TEX);
            push_u32(out, 0); // reserved0
            push_u64(out, 0); // offset_bytes
            push_u64(out, src_tex_data.len() as u64); // size_bytes
            out.extend_from_slice(&src_tex_data);
        });

        // Guest-backed destination texture with padded row pitch.
        emit_packet(out, OPC_CREATE_TEXTURE2D, |out| {
            push_u32(out, DST_TEX);
            push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE);
            push_u32(out, AEROGPU_FORMAT_R8G8B8A8_UNORM);
            push_u32(out, width);
            push_u32(out, height);
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, row_pitch);
            push_u32(out, DST_ALLOC_ID);
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, OPC_COPY_TEXTURE2D, |out| {
            push_u32(out, DST_TEX);
            push_u32(out, SRC_TEX);
            push_u32(out, 0); // dst_mip_level
            push_u32(out, 0); // dst_array_layer
            push_u32(out, 0); // src_mip_level
            push_u32(out, 0); // src_array_layer
            push_u32(out, 0); // dst_x
            push_u32(out, 0); // dst_y
            push_u32(out, 0); // src_x
            push_u32(out, 0); // src_y
            push_u32(out, width);
            push_u32(out, height);
            push_u32(out, AEROGPU_COPY_FLAG_WRITEBACK_DST);
            push_u32(out, 0); // reserved0
        });
    });

    exec.execute_cmd_stream_with_guest_memory(&stream, &mut guest_memory, Some(&alloc_table))
        .expect("execute should succeed");

    let mut out = vec![0u8; (row_pitch * height) as usize];
    guest_memory
        .read(DST_GPA, &mut out)
        .expect("read dst backing");
    let mut expected = vec![0xEEu8; (row_pitch * height) as usize];
    expected[0..bpr as usize].copy_from_slice(&src_tex_data[0..bpr as usize]);
    expected[row_pitch as usize..row_pitch as usize + bpr as usize]
        .copy_from_slice(&src_tex_data[bpr as usize..bpr as usize * 2]);
    assert_eq!(out, expected);
}

#[test]
fn d3d9_copy_texture2d_writeback_requires_alloc_table_each_submit() {
    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(module_path!(), "wgpu adapter not found");
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    const OPC_CREATE_TEXTURE2D: u32 = AerogpuCmdOpcode::CreateTexture2d as u32;
    const OPC_UPLOAD_RESOURCE: u32 = AerogpuCmdOpcode::UploadResource as u32;
    const OPC_COPY_TEXTURE2D: u32 = AerogpuCmdOpcode::CopyTexture2d as u32;

    const AEROGPU_FORMAT_R8G8B8A8_UNORM: u32 = AerogpuFormat::R8G8B8A8Unorm as u32;

    const SRC_TEX: u32 = 1;
    const DST_TEX: u32 = 2;

    const DST_ALLOC_ID: u32 = 1;
    const DST_GPA: u64 = 0x1000;

    let width = 1u32;
    let height = 1u32;
    let row_pitch = 4u32;
    let backing_len = (row_pitch * height) as usize;

    let mut guest_memory = VecGuestMemory::new(0x4000);
    guest_memory
        .write(DST_GPA, &[0xEEu8; 4])
        .expect("write dst sentinel");

    let alloc_table = AllocTable::new([(
        DST_ALLOC_ID,
        AllocEntry {
            flags: 0,
            gpa: DST_GPA,
            size_bytes: 0x1000,
        },
    )])
    .expect("alloc table");

    let src_tex_data = [1u8, 2, 3, 4];

    // Submit 1: create SRC/DST and upload SRC bytes.
    let stream_create = build_stream(|out| {
        emit_packet(out, OPC_CREATE_TEXTURE2D, |out| {
            push_u32(out, SRC_TEX);
            push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE);
            push_u32(out, AEROGPU_FORMAT_R8G8B8A8_UNORM);
            push_u32(out, width);
            push_u32(out, height);
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, 0); // row_pitch_bytes (tight packing)
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, OPC_UPLOAD_RESOURCE, |out| {
            push_u32(out, SRC_TEX);
            push_u32(out, 0); // reserved0
            push_u64(out, 0); // offset_bytes
            push_u64(out, src_tex_data.len() as u64); // size_bytes
            out.extend_from_slice(&src_tex_data);
        });

        emit_packet(out, OPC_CREATE_TEXTURE2D, |out| {
            push_u32(out, DST_TEX);
            push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE);
            push_u32(out, AEROGPU_FORMAT_R8G8B8A8_UNORM);
            push_u32(out, width);
            push_u32(out, height);
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, row_pitch);
            push_u32(out, DST_ALLOC_ID);
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });
    });

    exec.execute_cmd_stream_with_guest_memory(
        &stream_create,
        &mut guest_memory,
        Some(&alloc_table),
    )
    .expect("create should succeed");

    let stream_copy = build_stream(|out| {
        emit_packet(out, OPC_COPY_TEXTURE2D, |out| {
            push_u32(out, DST_TEX);
            push_u32(out, SRC_TEX);
            push_u32(out, 0); // dst_mip_level
            push_u32(out, 0); // dst_array_layer
            push_u32(out, 0); // src_mip_level
            push_u32(out, 0); // src_array_layer
            push_u32(out, 0); // dst_x
            push_u32(out, 0); // dst_y
            push_u32(out, 0); // src_x
            push_u32(out, 0); // src_y
            push_u32(out, width);
            push_u32(out, height);
            push_u32(out, AEROGPU_COPY_FLAG_WRITEBACK_DST);
            push_u32(out, 0); // reserved0
        });
    });

    // Submit 2: COPY_TEXTURE2D (WRITEBACK_DST) but no alloc table.
    let err = exec
        .execute_cmd_stream_with_guest_memory(&stream_copy, &mut guest_memory, None)
        .expect_err("expected missing alloc table error");
    match err {
        AerogpuD3d9Error::MissingAllocationTable(alloc_id) => assert_eq!(alloc_id, DST_ALLOC_ID),
        other => panic!("unexpected error: {other}"),
    }
    let mut out = vec![0u8; backing_len];
    guest_memory.read(DST_GPA, &mut out).unwrap();
    assert_eq!(out, vec![0xEEu8; 4]);

    // Submit 3: alloc table present but missing dst alloc_id.
    let bad_alloc_table = AllocTable::new([(
        DST_ALLOC_ID + 1,
        AllocEntry {
            flags: 0,
            gpa: 0x2000,
            size_bytes: 0x1000,
        },
    )])
    .expect("bad alloc table");
    let err = exec
        .execute_cmd_stream_with_guest_memory(
            &stream_copy,
            &mut guest_memory,
            Some(&bad_alloc_table),
        )
        .expect_err("expected missing alloc_id error");
    match err {
        AerogpuD3d9Error::MissingAllocTable(alloc_id) => assert_eq!(alloc_id, DST_ALLOC_ID),
        other => panic!("unexpected error: {other}"),
    }
    guest_memory.read(DST_GPA, &mut out).unwrap();
    assert_eq!(out, vec![0xEEu8; 4]);

    // Submit 4: correct alloc table.
    exec.execute_cmd_stream_with_guest_memory(&stream_copy, &mut guest_memory, Some(&alloc_table))
        .expect("writeback should succeed");
    guest_memory.read(DST_GPA, &mut out).unwrap();
    assert_eq!(&out[..4], &src_tex_data);
}

#[test]
fn d3d9_copy_texture2d_writeback_rejects_readonly_alloc() {
    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(module_path!(), "wgpu adapter not found");
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    const OPC_CREATE_TEXTURE2D: u32 = AerogpuCmdOpcode::CreateTexture2d as u32;
    const OPC_UPLOAD_RESOURCE: u32 = AerogpuCmdOpcode::UploadResource as u32;
    const OPC_COPY_TEXTURE2D: u32 = AerogpuCmdOpcode::CopyTexture2d as u32;

    const AEROGPU_FORMAT_R8G8B8A8_UNORM: u32 = AerogpuFormat::R8G8B8A8Unorm as u32;

    const SRC_TEX: u32 = 1;
    const DST_TEX: u32 = 2;

    const DST_ALLOC_ID: u32 = 1;
    const DST_GPA: u64 = 0x1000;

    let width = 1u32;
    let height = 1u32;
    let row_pitch = 4u32;
    let backing_len = (row_pitch * height) as usize;

    let mut guest_memory = VecGuestMemory::new(0x4000);
    let sentinel = vec![0xEEu8; backing_len];
    guest_memory.write(DST_GPA, &sentinel).unwrap();
    let alloc_table = AllocTable::new([(
        DST_ALLOC_ID,
        AllocEntry {
            flags: ring::AEROGPU_ALLOC_FLAG_READONLY,
            gpa: DST_GPA,
            size_bytes: 0x1000,
        },
    )])
    .expect("alloc table");

    let src_tex_data = [255u8, 0, 0, 255];
    let stream = build_stream(|out| {
        // Host-owned source texture.
        emit_packet(out, OPC_CREATE_TEXTURE2D, |out| {
            push_u32(out, SRC_TEX);
            push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE);
            push_u32(out, AEROGPU_FORMAT_R8G8B8A8_UNORM);
            push_u32(out, width);
            push_u32(out, height);
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, 0); // row_pitch_bytes (use tight packing)
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, OPC_UPLOAD_RESOURCE, |out| {
            push_u32(out, SRC_TEX);
            push_u32(out, 0); // reserved0
            push_u64(out, 0); // offset_bytes
            push_u64(out, src_tex_data.len() as u64); // size_bytes
            out.extend_from_slice(&src_tex_data);
        });

        // Guest-backed destination texture (READONLY alloc).
        emit_packet(out, OPC_CREATE_TEXTURE2D, |out| {
            push_u32(out, DST_TEX);
            push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE);
            push_u32(out, AEROGPU_FORMAT_R8G8B8A8_UNORM);
            push_u32(out, width);
            push_u32(out, height);
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, row_pitch);
            push_u32(out, DST_ALLOC_ID);
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, OPC_COPY_TEXTURE2D, |out| {
            push_u32(out, DST_TEX);
            push_u32(out, SRC_TEX);
            push_u32(out, 0); // dst_mip_level
            push_u32(out, 0); // dst_array_layer
            push_u32(out, 0); // src_mip_level
            push_u32(out, 0); // src_array_layer
            push_u32(out, 0); // dst_x
            push_u32(out, 0); // dst_y
            push_u32(out, 0); // src_x
            push_u32(out, 0); // src_y
            push_u32(out, width);
            push_u32(out, height);
            push_u32(out, AEROGPU_COPY_FLAG_WRITEBACK_DST);
            push_u32(out, 0); // reserved0
        });
    });

    let err = exec
        .execute_cmd_stream_with_guest_memory(&stream, &mut guest_memory, Some(&alloc_table))
        .expect_err("execute should fail");
    match err {
        AerogpuD3d9Error::Validation(msg) => {
            let msg_lc = msg.to_ascii_lowercase();
            assert!(
                msg_lc.contains("readonly") || msg_lc.contains("read-only"),
                "expected READONLY validation error, got: {msg}"
            );
        }
        other => panic!("unexpected error: {other}"),
    }

    let mut out = vec![0u8; backing_len];
    guest_memory
        .read(DST_GPA, &mut out)
        .expect("read guest backing");
    assert_eq!(&out, &sentinel);
}

#[test]
fn d3d9_create_texture2d_rebind_updates_guest_backing() {
    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(module_path!(), "wgpu adapter not found");
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    const OPC_CREATE_TEXTURE2D: u32 = AerogpuCmdOpcode::CreateTexture2d as u32;
    const OPC_UPLOAD_RESOURCE: u32 = AerogpuCmdOpcode::UploadResource as u32;
    const OPC_COPY_TEXTURE2D: u32 = AerogpuCmdOpcode::CopyTexture2d as u32;

    const FORMAT_RGBA8: u32 = AerogpuFormat::R8G8B8A8Unorm as u32;

    const SRC_TEX: u32 = 1;
    const DST_TEX: u32 = 2;

    const ALLOC_A: u32 = 1;
    const ALLOC_B: u32 = 2;
    const GPA_A: u64 = 0x1000;
    const GPA_B: u64 = 0x2000;

    let width = 1u32;
    let height = 1u32;
    let row_pitch = 8u32;
    let backing_len = (row_pitch * height) as usize;
    let bpr = (width * 4) as usize;

    let mut guest_memory = VecGuestMemory::new(0x8000);
    guest_memory
        .write(GPA_A, &vec![0xEEu8; backing_len])
        .unwrap();
    guest_memory
        .write(GPA_B, &vec![0xEEu8; backing_len])
        .unwrap();

    let alloc_table = AllocTable::new([
        (
            ALLOC_A,
            AllocEntry {
                flags: 0,
                gpa: GPA_A,
                size_bytes: 0x1000,
            },
        ),
        (
            ALLOC_B,
            AllocEntry {
                flags: 0,
                gpa: GPA_B,
                size_bytes: 0x1000,
            },
        ),
    ])
    .expect("alloc table");

    let pixel_a = [0x10u8, 0x20, 0x30, 0x40];
    let pixel_b = [0xABu8, 0xCD, 0xEF, 0x01];

    let stream_a = build_stream(|out| {
        // Host-owned source texture.
        emit_packet(out, OPC_CREATE_TEXTURE2D, |out| {
            push_u32(out, SRC_TEX);
            push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE);
            push_u32(out, FORMAT_RGBA8);
            push_u32(out, width);
            push_u32(out, height);
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, 0); // row_pitch_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, OPC_UPLOAD_RESOURCE, |out| {
            push_u32(out, SRC_TEX);
            push_u32(out, 0); // reserved0
            push_u64(out, 0); // offset_bytes
            push_u64(out, pixel_a.len() as u64); // size_bytes
            out.extend_from_slice(&pixel_a);
        });

        // Guest-backed destination texture with padded row pitch.
        emit_packet(out, OPC_CREATE_TEXTURE2D, |out| {
            push_u32(out, DST_TEX);
            push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE);
            push_u32(out, FORMAT_RGBA8);
            push_u32(out, width);
            push_u32(out, height);
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, row_pitch);
            push_u32(out, ALLOC_A);
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, OPC_COPY_TEXTURE2D, |out| {
            push_u32(out, DST_TEX);
            push_u32(out, SRC_TEX);
            push_u32(out, 0); // dst_mip_level
            push_u32(out, 0); // dst_array_layer
            push_u32(out, 0); // src_mip_level
            push_u32(out, 0); // src_array_layer
            push_u32(out, 0); // dst_x
            push_u32(out, 0); // dst_y
            push_u32(out, 0); // src_x
            push_u32(out, 0); // src_y
            push_u32(out, width);
            push_u32(out, height);
            push_u32(out, AEROGPU_COPY_FLAG_WRITEBACK_DST);
            push_u32(out, 0); // reserved0
        });
    });

    exec.execute_cmd_stream_with_guest_memory(&stream_a, &mut guest_memory, Some(&alloc_table))
        .expect("first submit should succeed");

    let mut out_a = vec![0u8; backing_len];
    guest_memory.read(GPA_A, &mut out_a).unwrap();
    let mut expected_a = vec![0xEEu8; backing_len];
    expected_a[0..bpr].copy_from_slice(&pixel_a);
    assert_eq!(out_a, expected_a);

    let stream_b = build_stream(|out| {
        // Rebind destination to a different allocation ID.
        emit_packet(out, OPC_CREATE_TEXTURE2D, |out| {
            push_u32(out, DST_TEX);
            push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE);
            push_u32(out, FORMAT_RGBA8);
            push_u32(out, width);
            push_u32(out, height);
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, row_pitch);
            push_u32(out, ALLOC_B);
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, OPC_UPLOAD_RESOURCE, |out| {
            push_u32(out, SRC_TEX);
            push_u32(out, 0); // reserved0
            push_u64(out, 0); // offset_bytes
            push_u64(out, pixel_b.len() as u64); // size_bytes
            out.extend_from_slice(&pixel_b);
        });

        emit_packet(out, OPC_COPY_TEXTURE2D, |out| {
            push_u32(out, DST_TEX);
            push_u32(out, SRC_TEX);
            push_u32(out, 0); // dst_mip_level
            push_u32(out, 0); // dst_array_layer
            push_u32(out, 0); // src_mip_level
            push_u32(out, 0); // src_array_layer
            push_u32(out, 0); // dst_x
            push_u32(out, 0); // dst_y
            push_u32(out, 0); // src_x
            push_u32(out, 0); // src_y
            push_u32(out, width);
            push_u32(out, height);
            push_u32(out, AEROGPU_COPY_FLAG_WRITEBACK_DST);
            push_u32(out, 0); // reserved0
        });
    });

    exec.execute_cmd_stream_with_guest_memory(&stream_b, &mut guest_memory, Some(&alloc_table))
        .expect("second submit should succeed");

    let mut out_a_after = vec![0u8; backing_len];
    guest_memory.read(GPA_A, &mut out_a_after).unwrap();
    assert_eq!(out_a_after, expected_a);

    let mut out_b = vec![0u8; backing_len];
    guest_memory.read(GPA_B, &mut out_b).unwrap();
    let mut expected_b = vec![0xEEu8; backing_len];
    expected_b[0..bpr].copy_from_slice(&pixel_b);
    assert_eq!(out_b, expected_b);
}

#[test]
fn d3d9_create_texture2d_rebind_rejects_mismatched_format() {
    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(module_path!(), "wgpu adapter not found");
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    let stream = build_stream(|out| {
        emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
            push_u32(out, 1); // texture_handle
            push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE);
            push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32);
            push_u32(out, 1); // width
            push_u32(out, 1); // height
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, 0); // row_pitch_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
            push_u32(out, 1); // texture_handle
            push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE);
            push_u32(out, AerogpuFormat::B8G8R8A8Unorm as u32); // format (mismatch)
            push_u32(out, 1); // width
            push_u32(out, 1); // height
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, 0); // row_pitch_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });
    });

    match exec.execute_cmd_stream(&stream) {
        Ok(_) => panic!("expected mismatched CREATE_TEXTURE2D rebind to fail"),
        Err(AerogpuD3d9Error::Validation(msg)) => assert!(msg.contains("mismatched immutable")),
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn d3d9_copy_texture2d_writeback_respects_copy_region() {
    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(module_path!(), "wgpu adapter not found");
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    const OPC_CREATE_TEXTURE2D: u32 = AerogpuCmdOpcode::CreateTexture2d as u32;
    const OPC_UPLOAD_RESOURCE: u32 = AerogpuCmdOpcode::UploadResource as u32;
    const OPC_COPY_TEXTURE2D: u32 = AerogpuCmdOpcode::CopyTexture2d as u32;

    const AEROGPU_FORMAT_R8G8B8A8_UNORM: u32 = AerogpuFormat::R8G8B8A8Unorm as u32;

    const SRC_TEX: u32 = 1;
    const DST_TEX: u32 = 2;

    const DST_ALLOC_ID: u32 = 1;
    const DST_GPA: u64 = 0x1000;

    let width = 2u32;
    let height = 2u32;
    let row_pitch = 12u32;
    let backing_len = (row_pitch as usize) * (height as usize);

    let src_tex_data: Vec<u8> = vec![
        // row 0: red, green
        255, 0, 0, 255, 0, 255, 0, 255, // row 1: blue, white
        0, 0, 255, 255, 255, 255, 255, 255,
    ];
    let pixel = [255u8, 0, 0, 255];

    let mut guest_memory = VecGuestMemory::new(0x4000);
    guest_memory
        .write(DST_GPA, &vec![0xEEu8; backing_len])
        .unwrap();
    let alloc_table = AllocTable::new([(
        DST_ALLOC_ID,
        AllocEntry {
            flags: 0,
            gpa: DST_GPA,
            size_bytes: 0x1000,
        },
    )])
    .expect("alloc table");

    let dst_x = 1u32;
    let dst_y = 1u32;
    let copy_width = 1u32;
    let copy_height = 1u32;

    let stream = build_stream(|out| {
        emit_packet(out, OPC_CREATE_TEXTURE2D, |out| {
            push_u32(out, SRC_TEX);
            push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE);
            push_u32(out, AEROGPU_FORMAT_R8G8B8A8_UNORM);
            push_u32(out, width);
            push_u32(out, height);
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, 0); // row_pitch_bytes (use tight packing)
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, OPC_UPLOAD_RESOURCE, |out| {
            push_u32(out, SRC_TEX);
            push_u32(out, 0); // reserved0
            push_u64(out, 0); // offset_bytes
            push_u64(out, src_tex_data.len() as u64); // size_bytes
            out.extend_from_slice(&src_tex_data);
        });

        emit_packet(out, OPC_CREATE_TEXTURE2D, |out| {
            push_u32(out, DST_TEX);
            push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE);
            push_u32(out, AEROGPU_FORMAT_R8G8B8A8_UNORM);
            push_u32(out, width);
            push_u32(out, height);
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, row_pitch);
            push_u32(out, DST_ALLOC_ID);
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, OPC_COPY_TEXTURE2D, |out| {
            push_u32(out, DST_TEX);
            push_u32(out, SRC_TEX);
            push_u32(out, 0); // dst_mip_level
            push_u32(out, 0); // dst_array_layer
            push_u32(out, 0); // src_mip_level
            push_u32(out, 0); // src_array_layer
            push_u32(out, dst_x);
            push_u32(out, dst_y);
            push_u32(out, 0); // src_x
            push_u32(out, 0); // src_y
            push_u32(out, copy_width);
            push_u32(out, copy_height);
            push_u32(out, AEROGPU_COPY_FLAG_WRITEBACK_DST);
            push_u32(out, 0); // reserved0
        });
    });

    exec.execute_cmd_stream_with_guest_memory(&stream, &mut guest_memory, Some(&alloc_table))
        .expect("execute should succeed");

    let mut out = vec![0u8; backing_len];
    guest_memory
        .read(DST_GPA, &mut out)
        .expect("read dst backing");
    let mut expected = vec![0xEEu8; backing_len];
    let dst_off = (dst_y as usize) * (row_pitch as usize) + (dst_x as usize) * pixel.len();
    expected[dst_off..dst_off + pixel.len()].copy_from_slice(&pixel);
    assert_eq!(out, expected);
}

#[test]
fn d3d9_copy_texture2d_writeback_encodes_x8_alpha_as_255() {
    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(module_path!(), "wgpu adapter not found");
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    const OPC_CREATE_TEXTURE2D: u32 = AerogpuCmdOpcode::CreateTexture2d as u32;
    const OPC_UPLOAD_RESOURCE: u32 = AerogpuCmdOpcode::UploadResource as u32;
    const OPC_COPY_TEXTURE2D: u32 = AerogpuCmdOpcode::CopyTexture2d as u32;

    const SRC_TEX: u32 = 1;
    const DST_TEX: u32 = 2;

    const DST_ALLOC_ID: u32 = 1;
    const DST_GPA: u64 = 0x1000;
    const BACKING_OFFSET_BYTES: u32 = 4;

    let width = 2u32;
    let height = 2u32;
    let row_pitch = 12u32;

    let backing_total = (BACKING_OFFSET_BYTES + row_pitch * height) as usize;
    for format in [
        AerogpuFormat::B8G8R8X8Unorm,
        AerogpuFormat::B8G8R8X8UnormSrgb,
        AerogpuFormat::R8G8B8X8Unorm,
        AerogpuFormat::R8G8B8X8UnormSrgb,
    ] {
        exec.reset();

        let format_u32 = format as u32;

        let mut upload = vec![0u8; (row_pitch * height) as usize];
        // Row 0 (y=0): pixel(0,0)=[1,2,3,0] pixel(1,0)=[4,5,6,0]
        upload[0..8].copy_from_slice(&[1, 2, 3, 0, 4, 5, 6, 0]);
        // Row 1 (y=1): pixel(0,1)=[7,8,9,0] pixel(1,1)=[10,11,12,0]
        let row1 = row_pitch as usize;
        upload[row1..row1 + 8].copy_from_slice(&[7, 8, 9, 0, 10, 11, 12, 0]);

        let mut guest_memory = VecGuestMemory::new(0x4000);
        guest_memory
            .write(DST_GPA, &vec![0xEEu8; backing_total])
            .unwrap();
        let alloc_table = AllocTable::new([(
            DST_ALLOC_ID,
            AllocEntry {
                flags: 0,
                gpa: DST_GPA,
                size_bytes: 0x1000,
            },
        )])
        .expect("alloc table");

        let stream = build_stream(|out| {
            emit_packet(out, OPC_CREATE_TEXTURE2D, |out| {
                push_u32(out, SRC_TEX);
                push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE);
                push_u32(out, format_u32);
                push_u32(out, width);
                push_u32(out, height);
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, row_pitch);
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            emit_packet(out, OPC_UPLOAD_RESOURCE, |out| {
                push_u32(out, SRC_TEX);
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, upload.len() as u64); // size_bytes
                out.extend_from_slice(&upload);
            });

            emit_packet(out, OPC_CREATE_TEXTURE2D, |out| {
                push_u32(out, DST_TEX);
                push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE);
                push_u32(out, format_u32);
                push_u32(out, width);
                push_u32(out, height);
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, row_pitch);
                push_u32(out, DST_ALLOC_ID);
                push_u32(out, BACKING_OFFSET_BYTES);
                push_u64(out, 0); // reserved0
            });

            // Copy src pixel (0,1) into dst pixel (1,0).
            emit_packet(out, OPC_COPY_TEXTURE2D, |out| {
                push_u32(out, DST_TEX);
                push_u32(out, SRC_TEX);
                push_u32(out, 0); // dst_mip_level
                push_u32(out, 0); // dst_array_layer
                push_u32(out, 0); // src_mip_level
                push_u32(out, 0); // src_array_layer
                push_u32(out, 1); // dst_x
                push_u32(out, 0); // dst_y
                push_u32(out, 0); // src_x
                push_u32(out, 1); // src_y
                push_u32(out, 1); // width
                push_u32(out, 1); // height
                push_u32(out, AEROGPU_COPY_FLAG_WRITEBACK_DST);
                push_u32(out, 0); // reserved0
            });
        });

        exec.execute_cmd_stream_with_guest_memory(&stream, &mut guest_memory, Some(&alloc_table))
            .expect("execute should succeed");

        let mut out = vec![0u8; backing_total];
        guest_memory.read(DST_GPA, &mut out).unwrap();

        let mut expected = vec![0xEEu8; backing_total];
        let pixel_off = (BACKING_OFFSET_BYTES + 4) as usize;
        expected[pixel_off..pixel_off + 4].copy_from_slice(&[7, 8, 9, 255]);
        assert_eq!(out, expected, "format_u32={format_u32}");
    }
}
