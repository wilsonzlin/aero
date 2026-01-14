mod common;

use aero_gpu::AerogpuD3d9Error;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuIndexFormat, AerogpuShaderStage, AerogpuVertexBufferBinding,
    AEROGPU_RESOURCE_USAGE_RENDER_TARGET, AEROGPU_RESOURCE_USAGE_TEXTURE,
    AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

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

fn assemble_ps_solid_color_c0() -> Vec<u8> {
    // ps_2_0: mov oC0, c0; end
    let mut words = vec![0xFFFF_0200];
    words.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(2, 0, 0xE4)]));
    words.push(0x0000_FFFF);
    to_bytes(&words)
}

fn vertex_decl_position0_stream0() -> Vec<u8> {
    // D3DVERTEXELEMENT9 array (little-endian).
    // Element 0: POSITION0 float4 at stream 0 offset 0.
    // End marker: stream 0xFF, type UNUSED.
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&0u16.to_le_bytes()); // stream
    bytes.extend_from_slice(&0u16.to_le_bytes()); // offset
    bytes.push(3); // type = FLOAT4
    bytes.push(0); // method
    bytes.push(0); // usage = POSITION
    bytes.push(0); // usage_index

    bytes.extend_from_slice(&0x00FFu16.to_le_bytes()); // stream = 0xFF
    bytes.extend_from_slice(&0u16.to_le_bytes()); // offset
    bytes.push(17); // type = UNUSED
    bytes.push(0); // method
    bytes.push(0); // usage
    bytes.push(0); // usage_index
    bytes
}

#[test]
fn d3d9_set_vertex_buffers_zero_handle_unbinds() {
    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    const RT_HANDLE: u32 = 1;
    const VS_HANDLE: u32 = 2;
    const PS_HANDLE: u32 = 3;
    const IL_HANDLE: u32 = 4;

    let vs_bytes = assemble_vs_passthrough_pos();
    let ps_bytes = assemble_ps_solid_color_c0();
    let vertex_decl = vertex_decl_position0_stream0();

    let mut writer = AerogpuCmdWriter::new();
    writer.create_texture2d(
        RT_HANDLE,
        AEROGPU_RESOURCE_USAGE_TEXTURE | AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
        AerogpuFormat::R8G8B8A8Unorm as u32,
        1,
        1,
        1,
        1,
        4,
        0,
        0,
    );
    writer.create_shader_dxbc(VS_HANDLE, AerogpuShaderStage::Vertex, &vs_bytes);
    writer.create_shader_dxbc(PS_HANDLE, AerogpuShaderStage::Pixel, &ps_bytes);
    writer.bind_shaders(VS_HANDLE, PS_HANDLE, 0);
    writer.create_input_layout(IL_HANDLE, &vertex_decl);
    writer.set_input_layout(IL_HANDLE);
    writer.set_render_targets(&[RT_HANDLE], 0);
    writer.set_viewport(0.0, 0.0, 1.0, 1.0, 0.0, 1.0);
    writer.set_scissor(0, 0, 1, 1);
    // buffer=0 should unbind the slot, regardless of stride/offset values.
    writer.set_vertex_buffers(
        0,
        &[AerogpuVertexBufferBinding {
            buffer: 0,
            stride_bytes: 16,
            offset_bytes: 0,
            reserved0: 0,
        }],
    );
    writer.draw(3, 1, 0, 0);

    let stream = writer.finish();
    match exec.execute_cmd_stream(&stream) {
        Err(AerogpuD3d9Error::MissingVertexBuffer { stream }) => assert_eq!(stream, 0),
        Err(other) => panic!("unexpected error: {other:?}"),
        Ok(_) => panic!("expected draw to fail due to missing vertex buffer"),
    }
}

#[test]
fn d3d9_set_index_buffer_zero_handle_unbinds() {
    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    const RT_HANDLE: u32 = 1;
    const VB_HANDLE: u32 = 2;
    const VS_HANDLE: u32 = 3;
    const PS_HANDLE: u32 = 4;
    const IL_HANDLE: u32 = 5;

    let vs_bytes = assemble_vs_passthrough_pos();
    let ps_bytes = assemble_ps_solid_color_c0();
    let vertex_decl = vertex_decl_position0_stream0();

    let vb_data = vec![0u8; 3 * 16];

    let mut writer = AerogpuCmdWriter::new();
    writer.create_texture2d(
        RT_HANDLE,
        AEROGPU_RESOURCE_USAGE_TEXTURE | AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
        AerogpuFormat::R8G8B8A8Unorm as u32,
        1,
        1,
        1,
        1,
        4,
        0,
        0,
    );
    writer.create_buffer(
        VB_HANDLE,
        AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
        vb_data.len() as u64,
        0,
        0,
    );
    writer.upload_resource(VB_HANDLE, 0, &vb_data);

    writer.create_shader_dxbc(VS_HANDLE, AerogpuShaderStage::Vertex, &vs_bytes);
    writer.create_shader_dxbc(PS_HANDLE, AerogpuShaderStage::Pixel, &ps_bytes);
    writer.bind_shaders(VS_HANDLE, PS_HANDLE, 0);
    writer.create_input_layout(IL_HANDLE, &vertex_decl);
    writer.set_input_layout(IL_HANDLE);
    writer.set_render_targets(&[RT_HANDLE], 0);
    writer.set_viewport(0.0, 0.0, 1.0, 1.0, 0.0, 1.0);
    writer.set_scissor(0, 0, 1, 1);
    writer.set_vertex_buffers(
        0,
        &[AerogpuVertexBufferBinding {
            buffer: VB_HANDLE,
            stride_bytes: 16,
            offset_bytes: 0,
            reserved0: 0,
        }],
    );

    // buffer=0 should unbind the index buffer.
    writer.set_index_buffer(0, AerogpuIndexFormat::Uint16, 0);
    writer.draw_indexed(3, 1, 0, 0, 0);

    let stream = writer.finish();
    match exec.execute_cmd_stream(&stream) {
        Err(AerogpuD3d9Error::MissingIndexBuffer) => {}
        Err(other) => panic!("unexpected error: {other:?}"),
        Ok(_) => panic!("expected draw_indexed to fail due to missing index buffer"),
    }
}
