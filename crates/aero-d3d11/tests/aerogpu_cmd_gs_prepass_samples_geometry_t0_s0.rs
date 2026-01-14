mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_d3d11::sm4::opcode::{
    OPCODE_DCL_GS_INPUT_PRIMITIVE, OPCODE_DCL_GS_MAX_OUTPUT_VERTEX_COUNT, OPCODE_DCL_GS_OUTPUT_TOPOLOGY,
    OPCODE_DCL_OUTPUT, OPCODE_DCL_RESOURCE, OPCODE_DCL_SAMPLER, OPCODE_EMIT, OPCODE_LEN_SHIFT,
    OPCODE_MOV, OPCODE_RET, OPCODE_SAMPLE,
};
use aero_dxbc::{test_utils as dxbc_test_utils, FourCC};
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCullMode, AerogpuFillMode, AerogpuPrimitiveTopology, AerogpuSamplerAddressMode,
    AerogpuSamplerFilter, AerogpuShaderStage, AerogpuVertexBufferBinding, AEROGPU_CLEAR_COLOR,
    AEROGPU_RESOURCE_USAGE_RENDER_TARGET, AEROGPU_RESOURCE_USAGE_TEXTURE,
    AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

// End-to-end regression test: translated GS compute prepass can sample Texture2D+Sampler bound to
// the Geometry stage (t0/s0) via the shared @group(3) binding model.

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

fn opcode_token(opcode: u32, len_dwords: u32) -> u32 {
    opcode | (len_dwords << OPCODE_LEN_SHIFT)
}

fn build_gs_point_to_fullscreen_triangle_sample_t0_s0_dxbc() -> Vec<u8> {
    // gs_4_0 token stream (Aero's legacy SM4 encoding) that:
    // - Declares Texture2D t0 and Sampler s0.
    // - Samples t0/s0 once and stores it in output register o1 (COLOR0 varying).
    // - Emits a fullscreen triangle as a triangle strip, writing position to o0.
    //
    // Pseudocode:
    //   dcl_inputprimitive point
    //   dcl_outputtopology triangle_strip
    //   dcl_maxvertexcount 3
    //   dcl_resource_texture2d t0
    //   dcl_sampler s0
    //   dcl_output o0.xyzw
    //   dcl_output o1.xyzw
    //   sample o1, l(0.5,0.5,0,0), t0, s0
    //   mov o0, l(-1,-1,0,1); emit
    //   mov o0, l(-1, 3,0,1); emit
    //   mov o0, l( 3,-1,0,1); emit
    //   ret

    // Values from `d3d10tokenizedprogramformat.h`:
    // - primitive: point = 1
    // - output topology: triangle_strip = 5
    const PRIM_POINT: u32 = 1;
    const TOPO_TRIANGLE_STRIP: u32 = 5;

    // Operand encodings borrowed from existing SM4-ish fixtures/tests.
    let dst_o = 0x0010_f022u32; // output register operand (o#) with xyzw mask
    let imm_vec4 = 0x0000_f042u32; // immediate vec4<f32>
    let t0 = 0x0010_0072u32; // resource operand (t#)
    let s0 = 0x0010_0062u32; // sampler operand (s#)

    let mut body: Vec<u32> = Vec::new();
    body.push(opcode_token(OPCODE_DCL_GS_INPUT_PRIMITIVE, 2));
    body.push(PRIM_POINT);
    body.push(opcode_token(OPCODE_DCL_GS_OUTPUT_TOPOLOGY, 2));
    body.push(TOPO_TRIANGLE_STRIP);
    body.push(opcode_token(OPCODE_DCL_GS_MAX_OUTPUT_VERTEX_COUNT, 2));
    body.push(3);

    // dcl_resource_texture2d t0
    body.push(opcode_token(OPCODE_DCL_RESOURCE, 4));
    body.push(t0);
    body.push(0); // t0
    body.push(2); // dim=2 => Texture2D

    // dcl_sampler s0
    body.push(opcode_token(OPCODE_DCL_SAMPLER, 3));
    body.push(s0);
    body.push(0); // s0

    // dcl_output o0.xyzw (position)
    body.push(opcode_token(OPCODE_DCL_OUTPUT, 3));
    body.push(dst_o);
    body.push(0); // o0

    // dcl_output o1.xyzw (varying used by ps_passthrough)
    body.push(opcode_token(OPCODE_DCL_OUTPUT, 3));
    body.push(dst_o);
    body.push(1); // o1

    // sample o1, l(0.5,0.5,0,0), t0, s0
    body.push(opcode_token(OPCODE_SAMPLE, 12));
    body.push(dst_o);
    body.push(1); // o1
    body.push(imm_vec4);
    body.push(0.5f32.to_bits());
    body.push(0.5f32.to_bits());
    body.push(0.0f32.to_bits());
    body.push(0.0f32.to_bits());
    body.push(t0);
    body.push(0); // t0
    body.push(s0);
    body.push(0); // s0

    let emit_pos = |body: &mut Vec<u32>, x: f32, y: f32| {
        // mov o0, l(x,y,0,1)
        body.push(opcode_token(OPCODE_MOV, 8));
        body.push(dst_o);
        body.push(0); // o0
        body.push(imm_vec4);
        body.push(x.to_bits());
        body.push(y.to_bits());
        body.push(0.0f32.to_bits());
        body.push(1.0f32.to_bits());
        // emit
        body.push(opcode_token(OPCODE_EMIT, 1));
    };

    emit_pos(&mut body, -1.0, -1.0);
    emit_pos(&mut body, -1.0, 3.0);
    emit_pos(&mut body, 3.0, -1.0);
    body.push(opcode_token(OPCODE_RET, 1));

    let version = 0x0002_0040u32; // gs_4_0
    let mut tokens = Vec::with_capacity(2 + body.len());
    tokens.push(version);
    tokens.push(0); // length patched below
    tokens.extend_from_slice(&body);
    tokens[1] = tokens.len() as u32;

    let shdr = tokens_to_bytes(&tokens);
    build_dxbc(&[(FourCC(*b"SHDR"), shdr)])
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct VertexPos3Color4 {
    pos: [f32; 3],
    color: [f32; 4],
}

#[test]
fn aerogpu_cmd_gs_prepass_samples_geometry_t0_s0() {
    pollster::block_on(async {
        let test_name =
            concat!(module_path!(), "::aerogpu_cmd_gs_prepass_samples_geometry_t0_s0");

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
        const RT: u32 = 2;
        const TEX_GS: u32 = 3;
        const TEX_PS: u32 = 4;
        const SAMP: u32 = 5;
        const VS: u32 = 6;
        const GS: u32 = 7;
        const PS: u32 = 8;
        const IL: u32 = 9;

        let vertex = VertexPos3Color4 {
            pos: [0.0, 0.0, 0.0],
            color: [0.0, 0.0, 0.0, 1.0],
        };

        let w = 8u32;
        let h = 8u32;

        // t0 bound to the Geometry stage should win (green). Bind a different texture to the Pixel
        // stage (red) to catch bind-group mixups.
        let texel_gs = [0u8, 255u8, 0u8, 255u8];
        let texel_ps = [255u8, 0u8, 0u8, 255u8];

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
        writer.create_texture2d(
            TEX_GS,
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
        writer.upload_resource(TEX_GS, 0, &texel_gs);
        writer.create_texture2d(
            TEX_PS,
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
        writer.upload_resource(TEX_PS, 0, &texel_ps);

        writer.create_sampler(
            SAMP,
            AerogpuSamplerFilter::Nearest,
            AerogpuSamplerAddressMode::ClampToEdge,
            AerogpuSamplerAddressMode::ClampToEdge,
            AerogpuSamplerAddressMode::ClampToEdge,
        );

        let gs_dxbc = build_gs_point_to_fullscreen_triangle_sample_t0_s0_dxbc();
        writer.create_shader_dxbc(VS, AerogpuShaderStage::Vertex, VS_PASSTHROUGH);
        writer.create_shader_dxbc(GS, AerogpuShaderStage::Geometry, &gs_dxbc);
        writer.create_shader_dxbc(PS, AerogpuShaderStage::Pixel, PS_PASSTHROUGH);

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
        writer.set_primitive_topology(AerogpuPrimitiveTopology::PointList);
        writer.bind_shaders_ex(VS, PS, 0, GS, 0, 0);

        writer.set_render_targets(&[RT], 0);
        writer.set_viewport(0.0, 0.0, w as f32, h as f32, 0.0, 1.0);
        writer.set_rasterizer_state(
            AerogpuFillMode::Solid,
            AerogpuCullMode::None,
            false,
            false,
            0,
            0,
        );

        // Bind t0/s0 to Geometry stage (the translated GS prepass runs as compute, but should read
        // these through the shared @group(3) binding model).
        writer.set_texture(AerogpuShaderStage::Geometry, 0, TEX_GS);
        writer.set_samplers(AerogpuShaderStage::Geometry, 0, &[SAMP]);

        // Bind a different t0/s0 pair to Pixel stage to ensure we don't accidentally sample the
        // wrong stage's bind group.
        writer.set_texture(AerogpuShaderStage::Pixel, 0, TEX_PS);
        writer.set_samplers(AerogpuShaderStage::Pixel, 0, &[SAMP]);

        // Clear to opaque black; the fullscreen triangle should overwrite it with the sampled
        // texel.
        writer.clear(AEROGPU_CLEAR_COLOR, [0.0, 0.0, 0.0, 1.0], 1.0, 0);
        writer.draw(1, 1, 0, 0);

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
        let idx = ((h as usize / 2) * w as usize + (w as usize / 2)) * 4;
        let center = &pixels[idx..idx + 4];
        assert_eq!(
            center, &texel_gs,
            "center pixel should match sampled Geometry-stage t0/s0 texel"
        );
    });
}

