mod common;

use aero_protocol::aerogpu::{
    aerogpu_cmd::{
        AerogpuPrimitiveTopology, AerogpuShaderStage, AerogpuVertexBufferBinding,
        AEROGPU_CLEAR_COLOR, AEROGPU_RESOURCE_USAGE_RENDER_TARGET, AEROGPU_RESOURCE_USAGE_TEXTURE,
        AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
    },
    aerogpu_pci::AerogpuFormat,
    cmd_writer::AerogpuCmdWriter,
};

// D3D9 render state IDs (subset).
const D3DRS_SCISSORTESTENABLE: u32 = 174;

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

fn enc_inst_sm3(opcode: u16, params: &[u32]) -> Vec<u32> {
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
    words.extend(enc_inst_sm3(
        0x0001,
        &[enc_dst(4, 0, 0xF), enc_src(1, 0, 0xE4)],
    ));
    words.push(0x0000_FFFF);
    to_bytes(&words)
}

fn assemble_ps3_runtime_ib_constants() -> Vec<u8> {
    // ps_3_0 (uses i0 and b0 without defi/defb so runtime SetShaderConstI/B values are required).
    let mut out = vec![0xFFFF_0300];

    // mov r0, c0
    out.extend(enc_inst_sm3(
        0x0001,
        &[enc_dst(0, 0, 0xF), enc_src(2, 0, 0xE4)],
    ));

    // loop aL, i0
    out.extend(enc_inst_sm3(
        0x001B,
        &[
            enc_src(15, 0, 0xE4), // aL
            enc_src(7, 0, 0xE4),  // i0
        ],
    ));

    // add r0, r0, c0
    out.extend(enc_inst_sm3(
        0x0002,
        &[
            enc_dst(0, 0, 0xF),  // r0
            enc_src(0, 0, 0xE4), // r0
            enc_src(2, 0, 0xE4), // c0
        ],
    ));

    // endloop
    out.extend(enc_inst_sm3(0x001D, &[]));

    // if b0
    out.extend(enc_inst_sm3(0x0028, &[enc_src(14, 0, 0x00)]));
    // mov oC0, r0
    out.extend(enc_inst_sm3(
        0x0001,
        &[enc_dst(8, 0, 0xF), enc_src(0, 0, 0xE4)],
    ));
    // else
    out.extend(enc_inst_sm3(0x002A, &[]));
    // mov oC0, c1
    out.extend(enc_inst_sm3(
        0x0001,
        &[enc_dst(8, 0, 0xF), enc_src(2, 1, 0xE4)],
    ));
    // endif
    out.extend(enc_inst_sm3(0x002B, &[]));

    out.push(0x0000_FFFF);
    to_bytes(&out)
}

fn vertex_decl_pos4() -> Vec<u8> {
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
    vertex_decl
}

fn pixel_at(pixels: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let idx = ((y * width + x) * 4) as usize;
    [
        pixels[idx],
        pixels[idx + 1],
        pixels[idx + 2],
        pixels[idx + 3],
    ]
}

#[test]
fn d3d9_cmd_stream_runtime_ib_constants_affect_sm3_shaders() {
    let mut exec = match common::d3d9_executor(module_path!()) {
        Some(exec) => exec,
        None => return,
    };

    const RT_HANDLE: u32 = 1;
    const VB_HANDLE: u32 = 2;
    const VS_HANDLE: u32 = 3;
    const PS_HANDLE: u32 = 4;
    const IL_HANDLE: u32 = 5;

    let width = 96u32;
    let height = 32u32;

    // Full-screen triangle (POSITION float4) at z=0.0.
    //
    // Note: Keep clockwise winding so the draw stays visible under the default D3D9 cull mode
    // (`D3DCULL_CCW` with clockwise front faces).
    let vertices: [f32; 12] = [
        -1.0, -1.0, 0.0, 1.0, //
        -1.0, 3.0, 0.0, 1.0, //
        3.0, -1.0, 0.0, 1.0, //
    ];
    let vb_bytes: &[u8] = bytemuck::cast_slice(&vertices);

    let vs_bytes = assemble_vs_passthrough_pos();
    let ps_bytes = assemble_ps3_runtime_ib_constants();
    let vertex_decl = vertex_decl_pos4();

    let mut writer = AerogpuCmdWriter::new();
    writer.create_texture2d(
        RT_HANDLE,
        AEROGPU_RESOURCE_USAGE_TEXTURE | AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
        AerogpuFormat::R8G8B8A8Unorm as u32,
        width,
        height,
        1,         // mip_levels
        1,         // array_layers
        width * 4, // row_pitch_bytes
        0,         // backing_alloc_id
        0,         // backing_offset_bytes
    );

    writer.create_buffer(
        VB_HANDLE,
        AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
        vb_bytes.len() as u64,
        0,
        0,
    );
    writer.upload_resource(VB_HANDLE, 0, vb_bytes);

    writer.create_shader_dxbc(VS_HANDLE, AerogpuShaderStage::Vertex, &vs_bytes);
    writer.create_shader_dxbc(PS_HANDLE, AerogpuShaderStage::Pixel, &ps_bytes);
    writer.bind_shaders(VS_HANDLE, PS_HANDLE, 0);

    writer.create_input_layout(IL_HANDLE, &vertex_decl);
    writer.set_input_layout(IL_HANDLE);

    writer.set_vertex_buffers(
        0,
        &[AerogpuVertexBufferBinding {
            buffer: VB_HANDLE,
            stride_bytes: 16,
            offset_bytes: 0,
            reserved0: 0,
        }],
    );
    writer.set_primitive_topology(AerogpuPrimitiveTopology::TriangleList);

    writer.set_render_targets(&[RT_HANDLE], 0);
    writer.set_viewport(0.0, 0.0, width as f32, height as f32, 0.0, 1.0);

    // Clear to black so we can detect scissor/draw coverage issues.
    writer.clear(AEROGPU_CLEAR_COLOR, [0.0, 0.0, 0.0, 1.0], 1.0, 0);

    // Enable scissor for the three draws.
    writer.set_render_state(D3DRS_SCISSORTESTENABLE, 1);

    // Pixel shader c0/c1 constants:
    //   c0 = dark red
    //   c1 = dark green
    writer.set_shader_constants_f(
        AerogpuShaderStage::Pixel,
        0,
        &[
            0.25, 0.0, 0.0, 1.0, // c0
            0.0, 0.25, 0.0, 1.0, // c1
        ],
    );

    // Left third: b0=false, output c1 (green), regardless of i0 loop count.
    writer.set_scissor(0, 0, 32, height as i32);
    writer.set_shader_constants_i(AerogpuShaderStage::Pixel, 0, &[0, 0, 1, 0]);
    writer.set_shader_constants_b(AerogpuShaderStage::Pixel, 0, &[0]);
    writer.draw(3, 1, 0, 0);

    // Middle third: b0=true, i0 = (0,-1,1,0) => 0 iterations => r0 = c0 (dark red).
    //
    // Note: D3D9 SM3 `loop aL, i#` is inclusive on the end bound (i#.y). The SM3→WGSL lowering
    // models this by breaking when `aL > end` for positive steps, so setting end=-1 yields a
    // zero-iteration loop.
    writer.set_scissor(32, 0, 32, height as i32);
    writer.set_shader_constants_i(AerogpuShaderStage::Pixel, 0, &[0, -1, 1, 0]);
    writer.set_shader_constants_b(AerogpuShaderStage::Pixel, 0, &[1]);
    writer.draw(3, 1, 0, 0);

    // Right third: b0=true, i0 = (0,0,1,0) => 1 iteration => r0 = c0 + c0 (brighter red).
    writer.set_scissor(64, 0, 32, height as i32);
    writer.set_shader_constants_i(AerogpuShaderStage::Pixel, 0, &[0, 0, 1, 0]);
    writer.set_shader_constants_b(AerogpuShaderStage::Pixel, 0, &[1]);
    writer.draw(3, 1, 0, 0);

    exec.execute_cmd_stream(&writer.finish())
        .expect("execute should succeed");

    let (_out_w, _out_h, rgba) = pollster::block_on(exec.readback_texture_rgba8(RT_HANDLE))
        .expect("readback should succeed");

    let left = pixel_at(&rgba, width, 16, height / 2);
    let middle = pixel_at(&rgba, width, 48, height / 2);
    let right = pixel_at(&rgba, width, 80, height / 2);

    // Allow a small tolerance for float->unorm conversion rounding.
    let approx_eq = |got: u8, expected: u8| -> bool {
        let lo = expected.saturating_sub(2);
        let hi = expected.saturating_add(2);
        (lo..=hi).contains(&got)
    };

    assert_eq!(left[3], 255);
    assert!(
        approx_eq(left[1], 64) && left[0] < 4 && left[2] < 4,
        "left={left:?}"
    );

    assert_eq!(middle[3], 255);
    assert!(
        approx_eq(middle[0], 64) && middle[1] < 4 && middle[2] < 4,
        "middle={middle:?}"
    );

    assert_eq!(right[3], 255);
    assert!(
        approx_eq(right[0], 128) && right[1] < 4 && right[2] < 4,
        "right={right:?}"
    );
}
