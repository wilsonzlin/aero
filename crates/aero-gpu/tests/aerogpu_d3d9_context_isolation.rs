mod common;

use aero_gpu::{AerogpuD3d9Error, AerogpuD3d9Executor};
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

fn assemble_ps_solid_color_c0() -> Vec<u8> {
    // ps_2_0: mov oC0, c0; end
    let mut words = vec![0xFFFF_0200];
    words.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(2, 0, 0xE4)]));
    words.push(0x0000_FFFF);
    to_bytes(&words)
}

fn assert_triangle_color(rgba: &[u8], width: u32, expected: [u8; 4]) {
    let px = |x: u32, y: u32| -> [u8; 4] {
        let idx = ((y * width + x) * 4) as usize;
        rgba[idx..idx + 4].try_into().unwrap()
    };

    assert_eq!(px(32, 2), [0, 0, 0, 255], "top row should be background");
    assert_eq!(
        px(32, 48),
        [0, 0, 0, 255],
        "bottom probe should be background"
    );
    assert_eq!(
        px(32, 16),
        expected,
        "center-top probe should be inside the triangle"
    );
}

#[test]
fn d3d9_executor_contexts_do_not_leak_constants() {
    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(
                concat!(
                    module_path!(),
                    "::d3d9_executor_contexts_do_not_leak_constants"
                ),
                "wgpu adapter not found",
            );
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    const CONTEXT_A: u32 = 1;
    const CONTEXT_B: u32 = 2;

    const RT_A: u32 = 1;
    const RT_B: u32 = 2;
    const VB: u32 = 3;
    const VS: u32 = 4;
    const PS: u32 = 5;
    const IL: u32 = 6;

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

    let vs_bytes = assemble_vs_passthrough_pos();
    let ps_bytes = assemble_ps_solid_color_c0();

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

    // Context A: create resources, set constants to red, draw.
    let mut a1 = AerogpuCmdWriter::new();
    for handle in [RT_A, RT_B] {
        a1.create_texture2d(
            handle,
            AEROGPU_RESOURCE_USAGE_TEXTURE | AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
            AerogpuFormat::R8G8B8A8Unorm as u32,
            width,
            height,
            1, // mip_levels
            1, // array_layers
            width * 4,
            0,
            0,
        );
    }
    a1.create_buffer(
        VB,
        AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
        vb_data.len() as u64,
        0,
        0,
    );
    a1.upload_resource(VB, 0, &vb_data);
    a1.create_shader_dxbc(VS, AerogpuShaderStage::Vertex, &vs_bytes);
    a1.create_shader_dxbc(PS, AerogpuShaderStage::Pixel, &ps_bytes);
    a1.bind_shaders(VS, PS, 0);
    a1.create_input_layout(IL, &vertex_decl);
    a1.set_input_layout(IL);
    a1.set_vertex_buffers(
        0,
        &[AerogpuVertexBufferBinding {
            buffer: VB,
            stride_bytes: 16,
            offset_bytes: 0,
            reserved0: 0,
        }],
    );
    a1.set_primitive_topology(AerogpuPrimitiveTopology::TriangleList);
    a1.set_render_targets(&[RT_A], 0);
    a1.set_viewport(0.0, 0.0, width as f32, height as f32, 0.0, 1.0);
    a1.clear(AEROGPU_CLEAR_COLOR, [0.0, 0.0, 0.0, 1.0], 1.0, 0);
    a1.set_shader_constants_f(AerogpuShaderStage::Pixel, 0, &[1.0, 0.0, 0.0, 1.0]);
    a1.draw(3, 1, 0, 0);
    exec.execute_cmd_stream_for_context(CONTEXT_A, &a1.finish())
        .expect("context A draw should succeed");

    // Context B: set constants to green, draw.
    let mut b = AerogpuCmdWriter::new();
    b.bind_shaders(VS, PS, 0);
    b.set_input_layout(IL);
    b.set_vertex_buffers(
        0,
        &[AerogpuVertexBufferBinding {
            buffer: VB,
            stride_bytes: 16,
            offset_bytes: 0,
            reserved0: 0,
        }],
    );
    b.set_primitive_topology(AerogpuPrimitiveTopology::TriangleList);
    b.set_render_targets(&[RT_B], 0);
    b.set_viewport(0.0, 0.0, width as f32, height as f32, 0.0, 1.0);
    b.clear(AEROGPU_CLEAR_COLOR, [0.0, 0.0, 0.0, 1.0], 1.0, 0);
    b.set_shader_constants_f(AerogpuShaderStage::Pixel, 0, &[0.0, 1.0, 0.0, 1.0]);
    b.draw(3, 1, 0, 0);
    exec.execute_cmd_stream_for_context(CONTEXT_B, &b.finish())
        .expect("context B draw should succeed");

    // Context A again: draw without re-uploading constants; should remain red.
    let mut a2 = AerogpuCmdWriter::new();
    a2.set_render_targets(&[RT_A], 0);
    a2.set_viewport(0.0, 0.0, width as f32, height as f32, 0.0, 1.0);
    a2.draw(3, 1, 0, 0);
    exec.execute_cmd_stream_for_context(CONTEXT_A, &a2.finish())
        .expect("context A second draw should succeed");

    let (_w, _h, rgba_a) = pollster::block_on(exec.readback_texture_rgba8(RT_A))
        .expect("readback of RT_A should succeed");
    let (_w, _h, rgba_b) = pollster::block_on(exec.readback_texture_rgba8(RT_B))
        .expect("readback of RT_B should succeed");

    assert_triangle_color(&rgba_a, width, [255, 0, 0, 255]);
    assert_triangle_color(&rgba_b, width, [0, 255, 0, 255]);
}
