mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_d3d11::sm4::opcode::{
    OPCODE_CUT, OPCODE_DCL_GS_INPUT_PRIMITIVE, OPCODE_DCL_GS_MAX_OUTPUT_VERTEX_COUNT,
    OPCODE_DCL_GS_OUTPUT_TOPOLOGY, OPCODE_DCL_RESOURCE, OPCODE_DCL_SAMPLER, OPCODE_EMIT,
    OPCODE_LEN_SHIFT, OPCODE_MOV, OPCODE_RET, OPCODE_SAMPLE,
};
use aero_dxbc::{test_utils as dxbc_test_utils, FourCC};
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCullMode, AerogpuFillMode, AerogpuPrimitiveTopology, AerogpuSamplerAddressMode,
    AerogpuSamplerFilter, AerogpuShaderStage, AerogpuShaderStageEx, AerogpuVertexBufferBinding,
    AEROGPU_CLEAR_COLOR, AEROGPU_RESOURCE_USAGE_RENDER_TARGET, AEROGPU_RESOURCE_USAGE_TEXTURE,
    AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

const VS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/vs_passthrough.dxbc");
const PS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/ps_passthrough.dxbc");
const ILAY_POS3_COLOR: &[u8] = include_bytes!("fixtures/ilay_pos3_color.bin");

fn build_dxbc(chunks: &[(FourCC, Vec<u8>)]) -> Vec<u8> {
    dxbc_test_utils::build_container_owned(chunks)
}

fn tokens_to_bytes(tokens: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(tokens.len() * 4);
    for &t in tokens {
        out.extend_from_slice(&t.to_le_bytes());
    }
    out
}

fn opcode_token(opcode: u32, len: u32) -> u32 {
    opcode | (len << OPCODE_LEN_SHIFT)
}

fn build_gs_point_to_triangle_sample_t0_s0() -> Vec<u8> {
    // Minimal gs_4_0 that:
    // - Declares point input + triangle strip output + maxvertexcount=3.
    // - Declares `Texture2D t0` and `Sampler s0`.
    // - Samples `t0` at a constant UV and writes the result to `o1` (COLOR0).
    // - Emits a small centered triangle with that color.

    const PRIM_POINT: u32 = 1;
    const TOPO_TRIANGLE_STRIP: u32 = 5;
    const MAX_VERTS: u32 = 3;

    // Operand encodings (Aero's internal SM4 token format; shared across fixtures).
    let dst_o = 0x0010_f022u32; // o#.xyzw
    let imm_vec4 = 0x0000_f042u32; // immediate32 vec4
    let t = 0x0010_0072u32; // t#
    let s = 0x0010_0062u32; // s#

    let mut tokens = vec![
        0x0002_0040u32, // gs_4_0
        0,              // length patched below
        opcode_token(OPCODE_DCL_GS_INPUT_PRIMITIVE, 2),
        PRIM_POINT,
        opcode_token(OPCODE_DCL_GS_OUTPUT_TOPOLOGY, 2),
        TOPO_TRIANGLE_STRIP,
        opcode_token(OPCODE_DCL_GS_MAX_OUTPUT_VERTEX_COUNT, 2),
        MAX_VERTS,
        // dcl_resource t0, Texture2D
        opcode_token(OPCODE_DCL_RESOURCE, 4),
        t,
        0, // slot
        2, // dimension token (Texture2D)
        // dcl_sampler s0
        opcode_token(OPCODE_DCL_SAMPLER, 3),
        s,
        0, // slot
        // sample o1, l(0.5,0.5,0,0), t0, s0
        opcode_token(OPCODE_SAMPLE, 12),
        dst_o,
        1, // o1 index (COLOR0)
        imm_vec4,
        0.5f32.to_bits(),
        0.5f32.to_bits(),
        0,
        0,
        t,
        0,
        s,
        0,
    ];

    let mov_token = opcode_token(OPCODE_MOV, 8);
    let emit_token = opcode_token(OPCODE_EMIT, 1);
    let cut_token = opcode_token(OPCODE_CUT, 1);
    let ret_token = opcode_token(OPCODE_RET, 1);

    let zero = 0.0f32.to_bits();
    let one = 1.0f32.to_bits();

    let emit_vertex = |tokens: &mut Vec<u32>, x: f32, y: f32| {
        tokens.extend_from_slice(&[
            // mov o0, l(x,y,0,1)
            mov_token,
            dst_o,
            0, // o0 index (SV_Position)
            imm_vec4,
            x.to_bits(),
            y.to_bits(),
            zero,
            one,
            // emit
            emit_token,
        ]);
    };

    // Small centered triangle (clockwise). NDC (0,0) is covered.
    emit_vertex(&mut tokens, -0.25, -0.25);
    emit_vertex(&mut tokens, 0.0, 0.25);
    emit_vertex(&mut tokens, 0.25, -0.25);

    tokens.push(cut_token);
    tokens.push(ret_token);

    tokens[1] = tokens.len() as u32;

    let shdr = tokens_to_bytes(&tokens);
    build_dxbc(&[(FourCC(*b"SHDR"), shdr)])
}

fn build_gs_point_to_triangle_sample_t3_s2() -> Vec<u8> {
    // Same as `build_gs_point_to_triangle_sample_t0_s0`, but uses non-zero D3D register slots:
    // - `Texture2D t3`
    // - `Sampler s2`
    //
    // This exercises binding-number offsets in the translated GS prepass and the stage_ex binding
    // table updates.

    const PRIM_POINT: u32 = 1;
    const TOPO_TRIANGLE_STRIP: u32 = 5;
    const MAX_VERTS: u32 = 3;

    // Operand encodings (Aero's internal SM4 token format; shared across fixtures).
    let dst_o = 0x0010_f022u32; // o#.xyzw
    let imm_vec4 = 0x0000_f042u32; // immediate32 vec4
    let t = 0x0010_0072u32; // t#
    let s = 0x0010_0062u32; // s#

    let mut tokens = vec![
        0x0002_0040u32, // gs_4_0
        0,              // length patched below
        opcode_token(OPCODE_DCL_GS_INPUT_PRIMITIVE, 2),
        PRIM_POINT,
        opcode_token(OPCODE_DCL_GS_OUTPUT_TOPOLOGY, 2),
        TOPO_TRIANGLE_STRIP,
        opcode_token(OPCODE_DCL_GS_MAX_OUTPUT_VERTEX_COUNT, 2),
        MAX_VERTS,
        // dcl_resource t3, Texture2D
        opcode_token(OPCODE_DCL_RESOURCE, 4),
        t,
        3, // slot
        2, // dimension token (Texture2D)
        // dcl_sampler s2
        opcode_token(OPCODE_DCL_SAMPLER, 3),
        s,
        2, // slot
        // sample o1, l(0.5,0.5,0,0), t3, s2
        opcode_token(OPCODE_SAMPLE, 12),
        dst_o,
        1, // o1 index (COLOR0)
        imm_vec4,
        0.5f32.to_bits(),
        0.5f32.to_bits(),
        0,
        0,
        t,
        3,
        s,
        2,
    ];

    let mov_token = opcode_token(OPCODE_MOV, 8);
    let emit_token = opcode_token(OPCODE_EMIT, 1);
    let cut_token = opcode_token(OPCODE_CUT, 1);
    let ret_token = opcode_token(OPCODE_RET, 1);

    let zero = 0.0f32.to_bits();
    let one = 1.0f32.to_bits();

    let emit_vertex = |tokens: &mut Vec<u32>, x: f32, y: f32| {
        tokens.extend_from_slice(&[
            // mov o0, l(x,y,0,1)
            mov_token,
            dst_o,
            0, // o0 index (SV_Position)
            imm_vec4,
            x.to_bits(),
            y.to_bits(),
            zero,
            one,
            // emit
            emit_token,
        ]);
    };

    // Small centered triangle (clockwise). NDC (0,0) is covered.
    emit_vertex(&mut tokens, -0.25, -0.25);
    emit_vertex(&mut tokens, 0.0, 0.25);
    emit_vertex(&mut tokens, 0.25, -0.25);

    tokens.push(cut_token);
    tokens.push(ret_token);

    tokens[1] = tokens.len() as u32;

    let shdr = tokens_to_bytes(&tokens);
    build_dxbc(&[(FourCC(*b"SHDR"), shdr)])
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct VertexPos3Color4 {
    pos: [f32; 3],
    color: [f32; 4],
}

#[test]
fn aerogpu_cmd_geometry_shader_samples_texture_translated_prepass() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::aerogpu_cmd_geometry_shader_samples_texture_translated_prepass"
        );

        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(test_name, &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        if !common::require_gs_prepass_or_skip(&exec, test_name) {
            return;
        }

        const VB: u32 = 1;
        const TEX: u32 = 2;
        const SAMP: u32 = 3;
        const RT: u32 = 4;
        const VS: u32 = 5;
        const GS: u32 = 6;
        const PS: u32 = 7;
        const IL: u32 = 8;

        let vertex = VertexPos3Color4 {
            pos: [0.0, 0.0, 0.0],
            color: [0.0, 0.0, 0.0, 1.0],
        };

        // 1x1 texture, opaque green.
        let tex_bytes: [u8; 4] = [0, 255, 0, 255];

        // Use an odd render target size so NDC (0,0) maps exactly to the center pixel.
        let w = 65u32;
        let h = 65u32;

        let mut writer = AerogpuCmdWriter::new();

        writer.create_buffer(
            VB,
            AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
            core::mem::size_of::<VertexPos3Color4>() as u64,
            0,
            0,
        );
        writer.upload_resource(VB, 0, bytemuck::bytes_of(&vertex));

        writer.create_texture2d(
            TEX,
            AEROGPU_RESOURCE_USAGE_TEXTURE,
            AerogpuFormat::R8G8B8A8Unorm as u32,
            1,
            1,
            1,
            1,
            0,
            0,
            0,
        );
        writer.upload_resource(TEX, 0, &tex_bytes);

        // Create a sampler and bind it to the emulated geometry stage (`stage_ex=GEOMETRY`).
        //
        // The translated GS prepass lowers `sample` to `textureSampleLevel`, which requires a
        // filterable sampler, so use `Linear`.
        writer.create_sampler(
            SAMP,
            AerogpuSamplerFilter::Linear,
            AerogpuSamplerAddressMode::ClampToEdge,
            AerogpuSamplerAddressMode::ClampToEdge,
            AerogpuSamplerAddressMode::ClampToEdge,
        );

        writer.create_texture2d(
            RT,
            AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
            AerogpuFormat::R8G8B8A8Unorm as u32,
            w,
            h,
            1,
            1,
            0,
            0,
            0,
        );

        writer.create_shader_dxbc(VS, AerogpuShaderStage::Vertex, VS_PASSTHROUGH);
        writer.create_shader_dxbc(PS, AerogpuShaderStage::Pixel, PS_PASSTHROUGH);
        writer.create_shader_dxbc_ex(
            GS,
            AerogpuShaderStageEx::Geometry,
            &build_gs_point_to_triangle_sample_t0_s0(),
        );

        writer.create_input_layout(IL, ILAY_POS3_COLOR);
        writer.set_input_layout(IL);

        writer.set_vertex_buffers(
            0,
            &[AerogpuVertexBufferBinding {
                buffer: VB,
                stride_bytes: core::mem::size_of::<VertexPos3Color4>() as u32,
                offset_bytes: 0,
                reserved0: 0,
            }],
        );
        writer.set_viewport(0.0, 0.0, w as f32, h as f32, 0.0, 1.0);
        writer.set_primitive_topology(AerogpuPrimitiveTopology::PointList);

        writer.bind_shaders_ex(VS, PS, 0, GS, 0, 0);
        // Disable face culling so the test does not depend on backend-specific winding conventions.
        writer.set_rasterizer_state_ext(
            AerogpuFillMode::Solid,
            AerogpuCullMode::None,
            false,
            false,
            0,
            false,
        );

        // Bind `t0`/`s0` to the emulated geometry stage (translated GS prepass uses @group(3)).
        writer.set_texture_ex(AerogpuShaderStageEx::Geometry, 0, TEX);
        writer.set_samplers_ex(AerogpuShaderStageEx::Geometry, 0, &[SAMP]);

        writer.set_render_targets(&[RT], 0);
        writer.clear(AEROGPU_CLEAR_COLOR, [1.0, 0.0, 0.0, 1.0], 1.0, 0);
        writer.draw(1, 1, 0, 0);
        writer.present(0, 0);

        let stream = writer.finish();

        let mut guest_mem = VecGuestMemory::new(0);
        if let Err(err) = exec.execute_cmd_stream(&stream, None, &mut guest_mem) {
            if common::skip_if_compute_or_indirect_unsupported(test_name, &err) {
                return;
            }
            panic!("execute_cmd_stream failed: {err:#}");
        }
        exec.poll_wait();

        let pixels = exec
            .read_texture_rgba8(RT)
            .await
            .expect("readback should succeed");
        let idx = ((h / 2) * w + (w / 2)) as usize * 4;
        let px: [u8; 4] = pixels[idx..idx + 4].try_into().unwrap();

        // Center pixel should be inside the emitted triangle and should reflect the sampled texture.
        let [r, g, b, _a] = px;
        assert!(
            g > r && g > b && g >= 200,
            "expected green-dominant pixel at center, got rgba={px:?}"
        );
    });
}

#[test]
fn aerogpu_cmd_geometry_shader_samples_texture_translated_prepass_multislot() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::aerogpu_cmd_geometry_shader_samples_texture_translated_prepass_multislot"
        );

        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(test_name, &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        if !common::require_gs_prepass_or_skip(&exec, test_name) {
            return;
        }

        const VB: u32 = 1;
        const TEX: u32 = 2;
        const SAMP: u32 = 3;
        const RT: u32 = 4;
        const VS: u32 = 5;
        const GS: u32 = 6;
        const PS: u32 = 7;
        const IL: u32 = 8;

        let vertex = VertexPos3Color4 {
            pos: [0.0, 0.0, 0.0],
            color: [0.0, 0.0, 0.0, 1.0],
        };

        // 1x1 texture, opaque green.
        let tex_bytes: [u8; 4] = [0, 255, 0, 255];

        // Use an odd render target size so NDC (0,0) maps exactly to the center pixel.
        let w = 65u32;
        let h = 65u32;

        let mut writer = AerogpuCmdWriter::new();

        writer.create_buffer(
            VB,
            AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
            core::mem::size_of::<VertexPos3Color4>() as u64,
            0,
            0,
        );
        writer.upload_resource(VB, 0, bytemuck::bytes_of(&vertex));

        writer.create_texture2d(
            TEX,
            AEROGPU_RESOURCE_USAGE_TEXTURE,
            AerogpuFormat::R8G8B8A8Unorm as u32,
            1,
            1,
            1,
            1,
            0,
            0,
            0,
        );
        writer.upload_resource(TEX, 0, &tex_bytes);

        writer.create_sampler(
            SAMP,
            AerogpuSamplerFilter::Linear,
            AerogpuSamplerAddressMode::ClampToEdge,
            AerogpuSamplerAddressMode::ClampToEdge,
            AerogpuSamplerAddressMode::ClampToEdge,
        );

        writer.create_texture2d(
            RT,
            AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
            AerogpuFormat::R8G8B8A8Unorm as u32,
            w,
            h,
            1,
            1,
            0,
            0,
            0,
        );

        writer.create_shader_dxbc(VS, AerogpuShaderStage::Vertex, VS_PASSTHROUGH);
        writer.create_shader_dxbc(PS, AerogpuShaderStage::Pixel, PS_PASSTHROUGH);
        writer.create_shader_dxbc_ex(
            GS,
            AerogpuShaderStageEx::Geometry,
            &build_gs_point_to_triangle_sample_t3_s2(),
        );

        writer.create_input_layout(IL, ILAY_POS3_COLOR);
        writer.set_input_layout(IL);

        writer.set_vertex_buffers(
            0,
            &[AerogpuVertexBufferBinding {
                buffer: VB,
                stride_bytes: core::mem::size_of::<VertexPos3Color4>() as u32,
                offset_bytes: 0,
                reserved0: 0,
            }],
        );
        writer.set_viewport(0.0, 0.0, w as f32, h as f32, 0.0, 1.0);
        writer.set_primitive_topology(AerogpuPrimitiveTopology::PointList);

        writer.bind_shaders_ex(VS, PS, 0, GS, 0, 0);
        writer.set_rasterizer_state_ext(
            AerogpuFillMode::Solid,
            AerogpuCullMode::None,
            false,
            false,
            0,
            false,
        );

        // Bind `t3`/`s2` to the emulated geometry stage (translated GS prepass uses @group(3)).
        writer.set_texture_ex(AerogpuShaderStageEx::Geometry, 3, TEX);
        writer.set_samplers_ex(AerogpuShaderStageEx::Geometry, 2, &[SAMP]);

        writer.set_render_targets(&[RT], 0);
        writer.clear(AEROGPU_CLEAR_COLOR, [1.0, 0.0, 0.0, 1.0], 1.0, 0);
        writer.draw(1, 1, 0, 0);
        writer.present(0, 0);

        let stream = writer.finish();

        let mut guest_mem = VecGuestMemory::new(0);
        if let Err(err) = exec.execute_cmd_stream(&stream, None, &mut guest_mem) {
            if common::skip_if_compute_or_indirect_unsupported(test_name, &err) {
                return;
            }
            panic!("execute_cmd_stream failed: {err:#}");
        }
        exec.poll_wait();

        let pixels = exec
            .read_texture_rgba8(RT)
            .await
            .expect("readback should succeed");
        let idx = ((h / 2) * w + (w / 2)) as usize * 4;
        let px: [u8; 4] = pixels[idx..idx + 4].try_into().unwrap();

        let [r, g, b, _a] = px;
        assert!(
            g > r && g > b && g >= 200,
            "expected green-dominant pixel at center, got rgba={px:?}"
        );
    });
}
