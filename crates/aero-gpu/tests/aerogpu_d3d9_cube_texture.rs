mod common;

use aero_gpu::aerogpu_executor::{AllocEntry, AllocTable};
use aero_gpu::{AerogpuD3d9Error, AerogpuD3d9Executor, VecGuestMemory};
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuPrimitiveTopology, AerogpuShaderStage, AerogpuVertexBufferBinding, AEROGPU_CLEAR_COLOR,
    AEROGPU_RESOURCE_USAGE_RENDER_TARGET, AEROGPU_RESOURCE_USAGE_TEXTURE,
    AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

fn push_f32(out: &mut Vec<u8>, v: f32) {
    out.extend_from_slice(&v.to_le_bytes());
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

fn assemble_vs_passthrough_pos() -> Vec<u8> {
    // vs_2_0: mov oPos, v0; end
    let mut words = vec![0xFFFE_0200];
    words.extend(enc_inst(0x0001, &[enc_dst(4, 0, 0xF), enc_src(1, 0, 0xE4)]));
    words.push(0x0000_FFFF);
    to_bytes(&words)
}

fn assemble_ps3_texld_cube_from_c0() -> Vec<u8> {
    // ps_3_0:
    //   dcl_cube s0
    //   texld r0, c0, s0
    //   mov oC0, r0
    //   end
    let mut words = vec![0xFFFF_0300];
    // dcl_cube s0 (DCL opcode=0x001F, texture type in bits 16..19; 3=cube)
    let dcl_token = 0x001Fu32 | (2u32 << 24) | (3u32 << 16);
    words.extend([dcl_token, enc_dst(10, 0, 0xF)]);
    // texld r0, c0, s0
    words.extend(enc_inst(
        0x0042,
        &[
            enc_dst(0, 0, 0xF),   // r0
            enc_src(2, 0, 0xE4),  // c0
            enc_src(10, 0, 0xE4), // s0
        ],
    ));
    // mov oC0, r0
    words.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 0, 0xE4)]));
    words.push(0x0000_FFFF);
    to_bytes(&words)
}

#[test]
fn d3d9_cmd_stream_cube_texture_upload_and_sample_faces() {
    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(module_path!(), "wgpu adapter not found");
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    const RT_HANDLE: u32 = 1;
    const VB_HANDLE: u32 = 2;
    const CUBE_TEX_HANDLE: u32 = 3;
    const VS_HANDLE: u32 = 4;
    const PS_HANDLE: u32 = 5;
    const IL_HANDLE: u32 = 6;

    const TEX_ALLOC_ID: u32 = 1;
    const TEX_GPA: u64 = 0x1000;

    let rt_width = 6u32;
    let rt_height = 1u32;

    // Full-screen triangle (clockwise) so we don't depend on cull state.
    let mut vb_data = Vec::new();
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
    }

    // D3DVERTEXELEMENT9 stream (little-endian).
    // Element 0: POSITION0 float4 at stream 0 offset 0.
    // End marker: stream 0xFF, type UNUSED.
    let mut vertex_decl = Vec::new();
    vertex_decl.extend_from_slice(&0u16.to_le_bytes()); // stream
    vertex_decl.extend_from_slice(&0u16.to_le_bytes()); // offset
    vertex_decl.push(3); // type = FLOAT4
    vertex_decl.push(0); // method
    vertex_decl.push(0); // usage = POSITION
    vertex_decl.push(0); // usage_index
    vertex_decl.extend_from_slice(&0x00FFu16.to_le_bytes()); // stream = 0xFF
    vertex_decl.extend_from_slice(&0u16.to_le_bytes()); // offset
    vertex_decl.push(17); // type = UNUSED
    vertex_decl.push(0); // method
    vertex_decl.push(0); // usage
    vertex_decl.push(0); // usage_index
    assert_eq!(vertex_decl.len(), 16);

    // Face order in both D3D9 and WebGPU cube textures is +X, -X, +Y, -Y, +Z, -Z.
    let face_rgba: [[u8; 4]; 6] = [
        [255, 0, 0, 255],   // +X red
        [0, 255, 0, 255],   // -X green
        [0, 0, 255, 255],   // +Y blue
        [255, 255, 0, 255], // -Y yellow
        [255, 0, 255, 255], // +Z magenta
        [0, 255, 255, 255], // -Z cyan
    ];

    let mut guest = VecGuestMemory::new(0x2000);
    for (i, color) in face_rgba.iter().enumerate() {
        guest
            .write(TEX_GPA + (i as u64) * 4, color)
            .expect("write cube face");
    }
    let alloc_table = AllocTable::new([(
        TEX_ALLOC_ID,
        AllocEntry {
            flags: 0,
            gpa: TEX_GPA,
            size_bytes: 0x1000,
        },
    )])
    .expect("alloc table");

    let vs_bytes = assemble_vs_passthrough_pos();
    let ps_bytes = assemble_ps3_texld_cube_from_c0();

    let mut stream = AerogpuCmdWriter::new();

    stream.create_texture2d(
        RT_HANDLE,
        AEROGPU_RESOURCE_USAGE_TEXTURE | AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
        AerogpuFormat::R8G8B8A8Unorm as u32,
        rt_width,
        rt_height,
        1,
        1,
        rt_width * 4,
        0,
        0,
    );

    stream.create_texture2d(
        CUBE_TEX_HANDLE,
        AEROGPU_RESOURCE_USAGE_TEXTURE,
        AerogpuFormat::R8G8B8A8Unorm as u32,
        1,
        1,
        1,
        6,
        4, // row_pitch_bytes
        TEX_ALLOC_ID,
        0,
    );
    stream.resource_dirty_range(CUBE_TEX_HANDLE, 0, (face_rgba.len() * 4) as u64);

    stream.create_buffer(
        VB_HANDLE,
        AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
        vb_data.len() as u64,
        0,
        0,
    );
    stream.upload_resource(VB_HANDLE, 0, &vb_data);

    stream.create_shader_dxbc(VS_HANDLE, AerogpuShaderStage::Vertex, &vs_bytes);
    stream.create_shader_dxbc(PS_HANDLE, AerogpuShaderStage::Pixel, &ps_bytes);
    stream.bind_shaders(VS_HANDLE, PS_HANDLE, 0);

    stream.create_input_layout(IL_HANDLE, &vertex_decl);
    stream.set_input_layout(IL_HANDLE);

    stream.set_vertex_buffers(
        0,
        &[AerogpuVertexBufferBinding {
            buffer: VB_HANDLE,
            stride_bytes: 16,
            offset_bytes: 0,
            reserved0: 0,
        }],
    );
    stream.set_primitive_topology(AerogpuPrimitiveTopology::TriangleList);

    stream.set_render_targets(&[RT_HANDLE], 0);
    stream.set_viewport(0.0, 0.0, rt_width as f32, rt_height as f32, 0.0, 1.0);
    stream.clear(AEROGPU_CLEAR_COLOR, [0.0, 0.0, 0.0, 1.0], 1.0, 0);

    stream.set_texture(AerogpuShaderStage::Pixel, 0, CUBE_TEX_HANDLE);

    let dirs: [[f32; 4]; 6] = [
        [1.0, 0.0, 0.0, 1.0],  // +X
        [-1.0, 0.0, 0.0, 1.0], // -X
        [0.0, 1.0, 0.0, 1.0],  // +Y
        [0.0, -1.0, 0.0, 1.0], // -Y
        [0.0, 0.0, 1.0, 1.0],  // +Z
        [0.0, 0.0, -1.0, 1.0], // -Z
    ];

    for (i, dir) in dirs.iter().enumerate() {
        stream.set_viewport(i as f32, 0.0, 1.0, 1.0, 0.0, 1.0);
        stream.set_shader_constants_f(AerogpuShaderStage::Pixel, 0, dir);
        stream.draw(3, 1, 0, 0);
    }

    exec.execute_cmd_stream_with_guest_memory(&stream.finish(), &mut guest, Some(&alloc_table))
        .expect("execute should succeed");

    let (w, h, rgba) = pollster::block_on(exec.readback_texture_rgba8(RT_HANDLE))
        .expect("readback should succeed");
    assert_eq!((w, h), (rt_width, rt_height));

    for (i, expected) in face_rgba.iter().enumerate() {
        let off = i * 4;
        let got: [u8; 4] = rgba[off..off + 4].try_into().unwrap();
        assert_eq!(got, *expected, "face {i} mismatch");
    }
}
