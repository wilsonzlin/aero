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

fn assemble_vs_texld_s0_to_od0() -> Vec<u8> {
    // vs_3_0:
    //   mov oPos, v0
    //   texld r0, c0, s0
    //   mov oD0, r0
    //   end
    let mut words = vec![0xFFFE_0300];
    words.extend(enc_inst(0x0001, &[enc_dst(4, 0, 0xF), enc_src(1, 0, 0xE4)]));
    words.extend(enc_inst(
        0x0042,
        &[
            enc_dst(0, 0, 0xF),   // r0
            enc_src(2, 0, 0xE4),  // c0
            enc_src(10, 0, 0xE4), // s0
        ],
    ));
    words.extend(enc_inst(0x0001, &[enc_dst(5, 0, 0xF), enc_src(0, 0, 0xE4)]));
    words.push(0x0000_FFFF);
    to_bytes(&words)
}

fn assemble_ps_mov_oc0_from_v0() -> Vec<u8> {
    // ps_2_0:
    //   mov oC0, v0
    //   end
    let mut words = vec![0xFFFF_0200];
    words.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(1, 0, 0xE4)]));
    words.push(0x0000_FFFF);
    to_bytes(&words)
}

#[test]
fn d3d9_cmd_stream_vertex_shader_bc1_texture_sampling() {
    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(module_path!(), "wgpu adapter not found");
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    // D3DSAMPLERSTATETYPE / D3DTEXTUREADDRESS subset.
    const D3DSAMP_ADDRESSU: u32 = 1;
    const D3DSAMP_ADDRESSV: u32 = 2;
    const D3DTADDRESS_CLAMP: u32 = 3;

    const RT_HANDLE: u32 = 1;
    const VB_HANDLE: u32 = 2;
    const SAMPLE_TEX_HANDLE: u32 = 3;
    const VS_HANDLE: u32 = 4;
    const PS_HANDLE: u32 = 5;
    const IL_HANDLE: u32 = 6;

    const TEX_ALLOC_ID: u32 = 1;
    const TEX_GPA: u64 = 0x1000;

    let width = 64u32;
    let height = 64u32;

    let mut vb_data = Vec::new();
    // D3D9 defaults to back-face culling with clockwise front faces.
    let verts = [
        (-0.8f32, -0.2f32, 0.0f32, 1.0f32),
        (0.0f32, 0.8f32, 0.0f32, 1.0f32),
        (0.8f32, -0.2f32, 0.0f32, 1.0f32),
    ];
    for (x, y, z, w) in verts {
        push_f32(&mut vb_data, x);
        push_f32(&mut vb_data, y);
        push_f32(&mut vb_data, z);
        push_f32(&mut vb_data, w);
    }
    assert_eq!(vb_data.len(), 3 * 16);

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

    // Single BC1 block for a 1x1 texture. Make it solid red by setting
    // color0=color1=RGB565(255,0,0)=0xF800 and all indices to 0.
    let bc1_solid_red = [0x00, 0xF8, 0x00, 0xF8, 0x00, 0x00, 0x00, 0x00];

    let alloc_table = AllocTable::new([(
        TEX_ALLOC_ID,
        AllocEntry {
            flags: 0,
            gpa: TEX_GPA,
            size_bytes: 0x1000,
        },
    )])
    .expect("alloc table");

    let mut guest_memory = VecGuestMemory::new(0x2000);
    guest_memory
        .write(TEX_GPA, &bc1_solid_red)
        .expect("write guest BC data");

    let vs_bytes = assemble_vs_texld_s0_to_od0();
    let ps_bytes = assemble_ps_mov_oc0_from_v0();

    let mut stream = AerogpuCmdWriter::new();

    stream.create_texture2d(
        RT_HANDLE,
        AEROGPU_RESOURCE_USAGE_TEXTURE | AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
        AerogpuFormat::R8G8B8A8Unorm as u32,
        width,
        height,
        1,
        1,
        width * 4,
        0,
        0,
    );

    stream.create_texture2d(
        SAMPLE_TEX_HANDLE,
        AEROGPU_RESOURCE_USAGE_TEXTURE,
        AerogpuFormat::BC1RgbaUnorm as u32,
        1,
        1,
        1,
        1,
        8, // row_pitch_bytes (1 block row of BC1)
        TEX_ALLOC_ID,
        0,
    );
    stream.resource_dirty_range(SAMPLE_TEX_HANDLE, 0, bc1_solid_red.len() as u64);

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
    stream.set_viewport(0.0, 0.0, width as f32, height as f32, 0.0, 1.0);

    stream.clear(AEROGPU_CLEAR_COLOR, [0.0, 0.0, 0.0, 1.0], 1.0, 0);

    // c0.xy controls the sample coordinate; sample at center.
    stream.set_shader_constants_f(AerogpuShaderStage::Vertex, 0, &[0.5, 0.5, 0.0, 1.0]);

    // Critical: bind s0 for the *vertex* shader stage.
    stream.set_texture(AerogpuShaderStage::Vertex, 0, SAMPLE_TEX_HANDLE);
    stream.set_sampler_state(
        AerogpuShaderStage::Vertex,
        0,
        D3DSAMP_ADDRESSU,
        D3DTADDRESS_CLAMP,
    );
    stream.set_sampler_state(
        AerogpuShaderStage::Vertex,
        0,
        D3DSAMP_ADDRESSV,
        D3DTADDRESS_CLAMP,
    );

    stream.draw(3, 1, 0, 0);

    exec.execute_cmd_stream_with_guest_memory(
        &stream.finish(),
        &mut guest_memory,
        Some(&alloc_table),
    )
    .expect("execute should succeed");

    let (out_w, out_h, rgba) = pollster::block_on(exec.readback_texture_rgba8(RT_HANDLE))
        .expect("readback should succeed");
    assert_eq!((out_w, out_h), (width, height));

    let idx = ((16 * width + 32) * 4) as usize;
    let px: [u8; 4] = rgba[idx..idx + 4].try_into().unwrap();
    assert_eq!(px, [255, 0, 0, 255], "triangle should sample BC1 texture");
}
