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

fn assemble_vs_passthrough_pos_and_t0_from_c0() -> Vec<u8> {
    // vs_2_0:
    //   mov oPos, v0
    //   mov oT0, c0
    //   end
    let mut words = vec![0xFFFE_0200];
    words.extend(enc_inst(0x0001, &[enc_dst(4, 0, 0xF), enc_src(1, 0, 0xE4)]));
    words.extend(enc_inst(0x0001, &[enc_dst(6, 0, 0xF), enc_src(2, 0, 0xE4)]));
    words.push(0x0000_FFFF);
    to_bytes(&words)
}

fn assemble_ps_texld_s0() -> Vec<u8> {
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
fn d3d9_destroy_resource_invalidates_bind_groups_for_all_contexts() {
    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(module_path!(), "wgpu adapter not found");
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    const CTX_A: u32 = 100;
    const CTX_B: u32 = 200;

    const RT: u32 = 1;
    const VB: u32 = 2;
    const TEX_A: u32 = 3;
    const TEX_B: u32 = 4;
    const TEX_ALIAS: u32 = 5;
    const VS: u32 = 6;
    const PS: u32 = 7;
    const IL: u32 = 8;

    const TOKEN_A: u64 = 0xAABB_CCDD_EEFF_0001;
    const TOKEN_B: u64 = 0xAABB_CCDD_EEFF_0002;

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

    let vs_bytes = assemble_vs_passthrough_pos_and_t0_from_c0();
    let ps_bytes = assemble_ps_texld_s0();

    // Context B: create everything and draw with TEX_ALIAS -> TOKEN_A -> TEX_A (red).
    let mut stream_b1 = AerogpuCmdWriter::new();
    stream_b1.create_texture2d(
        RT,
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
    for (handle, rgba) in [(TEX_A, [255u8, 0, 0, 255]), (TEX_B, [0u8, 255, 0, 255])] {
        stream_b1.create_texture2d(
            handle,
            AEROGPU_RESOURCE_USAGE_TEXTURE,
            AerogpuFormat::R8G8B8A8Unorm as u32,
            1,
            1,
            1,
            1,
            4,
            0,
            0,
        );
        stream_b1.upload_resource(handle, 0, &rgba);
    }
    stream_b1.export_shared_surface(TEX_A, TOKEN_A);
    stream_b1.export_shared_surface(TEX_B, TOKEN_B);
    stream_b1.import_shared_surface(TEX_ALIAS, TOKEN_A);

    stream_b1.create_buffer(
        VB,
        AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
        vb_data.len() as u64,
        0,
        0,
    );
    stream_b1.upload_resource(VB, 0, &vb_data);
    stream_b1.create_shader_dxbc(VS, AerogpuShaderStage::Vertex, &vs_bytes);
    stream_b1.create_shader_dxbc(PS, AerogpuShaderStage::Pixel, &ps_bytes);
    stream_b1.bind_shaders(VS, PS, 0);
    stream_b1.create_input_layout(IL, &vertex_decl);
    stream_b1.set_input_layout(IL);
    stream_b1.set_vertex_buffers(
        0,
        &[AerogpuVertexBufferBinding {
            buffer: VB,
            stride_bytes: 16,
            offset_bytes: 0,
            reserved0: 0,
        }],
    );
    stream_b1.set_primitive_topology(AerogpuPrimitiveTopology::TriangleList);
    stream_b1.set_render_targets(&[RT], 0);
    stream_b1.set_viewport(0.0, 0.0, width as f32, height as f32, 0.0, 1.0);
    stream_b1.clear(AEROGPU_CLEAR_COLOR, [0.0, 0.0, 0.0, 1.0], 1.0, 0);
    stream_b1.set_shader_constants_f(AerogpuShaderStage::Vertex, 0, &[0.5, 0.5, 0.0, 1.0]);
    stream_b1.set_texture(AerogpuShaderStage::Pixel, 0, TEX_ALIAS);
    stream_b1.draw(3, 1, 0, 0);

    exec.execute_cmd_stream_for_context(CTX_B, &stream_b1.finish())
        .expect("context B initial draw should succeed");

    // Context A: destroy the alias and re-import it to point at TOKEN_B (green).
    let mut stream_a = AerogpuCmdWriter::new();
    stream_a.destroy_resource(TEX_ALIAS);
    stream_a.import_shared_surface(TEX_ALIAS, TOKEN_B);
    exec.execute_cmd_stream_for_context(CTX_A, &stream_a.finish())
        .expect("context A retarget should succeed");

    // Context B: draw again without re-binding the texture; should sample green now.
    let mut stream_b2 = AerogpuCmdWriter::new();
    stream_b2.clear(AEROGPU_CLEAR_COLOR, [0.0, 0.0, 0.0, 1.0], 1.0, 0);
    stream_b2.draw(3, 1, 0, 0);
    exec.execute_cmd_stream_for_context(CTX_B, &stream_b2.finish())
        .expect("context B second draw should succeed");

    let (_w, _h, rgba) =
        pollster::block_on(exec.readback_texture_rgba8(RT)).expect("readback of RT should succeed");
    assert_triangle_color(&rgba, width, [0, 255, 0, 255]);
}

#[test]
fn d3d9_import_shared_surface_invalidates_bind_groups_for_all_contexts() {
    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(module_path!(), "wgpu adapter not found");
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    const CTX_A: u32 = 100;
    const CTX_B: u32 = 200;

    const RT: u32 = 1;
    const VB: u32 = 2;
    const TEX: u32 = 3;
    const TEX_ALIAS: u32 = 5;
    const VS: u32 = 6;
    const PS: u32 = 7;
    const IL: u32 = 8;

    const TOKEN: u64 = 0xAABB_CCDD_EEFF_0001;

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

    let vs_bytes = assemble_vs_passthrough_pos_and_t0_from_c0();
    let ps_bytes = assemble_ps_texld_s0();

    // Context B draws using an alias handle that has not been imported yet; it will sample the
    // executor's dummy texture (white). Later, context A imports the alias; context B draws again
    // without calling `SetTexture` and should now sample the imported texture (red), which
    // requires the import to invalidate cached bind groups across contexts.
    let mut stream_b1 = AerogpuCmdWriter::new();
    stream_b1.create_texture2d(
        RT,
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
    stream_b1.create_texture2d(
        TEX,
        AEROGPU_RESOURCE_USAGE_TEXTURE,
        AerogpuFormat::R8G8B8A8Unorm as u32,
        1,
        1,
        1,
        1,
        4,
        0,
        0,
    );
    stream_b1.upload_resource(TEX, 0, &[255, 0, 0, 255]);
    stream_b1.export_shared_surface(TEX, TOKEN);

    stream_b1.create_buffer(
        VB,
        AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
        vb_data.len() as u64,
        0,
        0,
    );
    stream_b1.upload_resource(VB, 0, &vb_data);
    stream_b1.create_shader_dxbc(VS, AerogpuShaderStage::Vertex, &vs_bytes);
    stream_b1.create_shader_dxbc(PS, AerogpuShaderStage::Pixel, &ps_bytes);
    stream_b1.bind_shaders(VS, PS, 0);
    stream_b1.create_input_layout(IL, &vertex_decl);
    stream_b1.set_input_layout(IL);
    stream_b1.set_vertex_buffers(
        0,
        &[AerogpuVertexBufferBinding {
            buffer: VB,
            stride_bytes: 16,
            offset_bytes: 0,
            reserved0: 0,
        }],
    );
    stream_b1.set_primitive_topology(AerogpuPrimitiveTopology::TriangleList);
    stream_b1.set_render_targets(&[RT], 0);
    stream_b1.set_viewport(0.0, 0.0, width as f32, height as f32, 0.0, 1.0);
    stream_b1.clear(AEROGPU_CLEAR_COLOR, [0.0, 0.0, 0.0, 1.0], 1.0, 0);
    stream_b1.set_shader_constants_f(AerogpuShaderStage::Vertex, 0, &[0.5, 0.5, 0.0, 1.0]);
    stream_b1.set_texture(AerogpuShaderStage::Pixel, 0, TEX_ALIAS);
    stream_b1.draw(3, 1, 0, 0);
    exec.execute_cmd_stream_for_context(CTX_B, &stream_b1.finish())
        .expect("context B initial draw should succeed");

    // Before the alias is imported, context B should sample the executor's dummy texture.
    let (_w, _h, rgba_before) =
        pollster::block_on(exec.readback_texture_rgba8(RT)).expect("readback of RT should succeed");
    assert_triangle_color(&rgba_before, width, [255, 255, 255, 255]);

    let mut stream_a = AerogpuCmdWriter::new();
    stream_a.import_shared_surface(TEX_ALIAS, TOKEN);
    exec.execute_cmd_stream_for_context(CTX_A, &stream_a.finish())
        .expect("context A import should succeed");

    let mut stream_b2 = AerogpuCmdWriter::new();
    stream_b2.clear(AEROGPU_CLEAR_COLOR, [0.0, 0.0, 0.0, 1.0], 1.0, 0);
    stream_b2.draw(3, 1, 0, 0);
    exec.execute_cmd_stream_for_context(CTX_B, &stream_b2.finish())
        .expect("context B second draw should succeed");

    let (_w, _h, rgba) =
        pollster::block_on(exec.readback_texture_rgba8(RT)).expect("readback of RT should succeed");
    assert_triangle_color(&rgba, width, [255, 0, 0, 255]);
}
