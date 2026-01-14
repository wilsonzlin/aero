mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_d3d11::sm4::decode_program;
use aero_d3d11::sm4::opcode::{
    OPCODE_CUT, OPCODE_DCL_GS_INPUT_PRIMITIVE, OPCODE_DCL_GS_MAX_OUTPUT_VERTEX_COUNT,
    OPCODE_DCL_GS_OUTPUT_TOPOLOGY, OPCODE_DCL_OUTPUT, OPCODE_DCL_RESOURCE, OPCODE_DCL_SAMPLER,
    OPCODE_EMIT, OPCODE_LEN_SHIFT, OPCODE_MOV, OPCODE_RET, OPCODE_SAMPLE_L,
};
use aero_d3d11::{
    DxbcFile, GsInputPrimitive, GsOutputTopology, ShaderStage as Sm4ShaderStage, Sm4Decl, Sm4Inst,
    Sm4Program,
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

fn build_gs_sample_l_t0_s0_to_o1_point_to_triangle() -> Vec<u8> {
    // Minimal gs_4_0 that:
    // - Declares point input + triangle strip output + maxvertexcount=3.
    // - Declares Texture2D t0 and Sampler s0.
    // - Samples t0/s0 at a constant coordinate (LOD=0) into o1.xyzw.
    // - Emits a centered triangle with constant positions in o0.xyzw.

    const PRIM_POINT: u32 = 1;
    const TOPO_TRIANGLE_STRIP: u32 = 5;
    const MAX_VERTS: u32 = 3;

    // Operand encodings are shared across other in-tree DXBC fixtures/tests.
    let dst_o = 0x0010_f022u32; // o#.xyzw
    let imm_vec4 = 0x0000_f042u32;
    let imm_scalar = 0x0000_0049u32;
    let t = 0x0010_0072u32; // t#
    let s = 0x0010_0062u32; // s#

    // gs_4_0
    let mut tokens: Vec<u32> = vec![0x0002_0040u32, 0 /* length patched below */];

    // dcl_inputprimitive point
    tokens.extend_from_slice(&[opcode_token(OPCODE_DCL_GS_INPUT_PRIMITIVE, 2), PRIM_POINT]);
    // dcl_outputtopology triangle_strip
    tokens.extend_from_slice(&[
        opcode_token(OPCODE_DCL_GS_OUTPUT_TOPOLOGY, 2),
        TOPO_TRIANGLE_STRIP,
    ]);
    // dcl_maxvertexcount 3
    tokens.extend_from_slice(&[
        opcode_token(OPCODE_DCL_GS_MAX_OUTPUT_VERTEX_COUNT, 2),
        MAX_VERTS,
    ]);

    // dcl_output o0.xyzw
    tokens.extend_from_slice(&[opcode_token(OPCODE_DCL_OUTPUT, 3), dst_o, 0]);
    // dcl_output o1.xyzw (varying used by PS passthrough)
    tokens.extend_from_slice(&[opcode_token(OPCODE_DCL_OUTPUT, 3), dst_o, 1]);

    // dcl_resource_texture2d t0
    tokens.extend_from_slice(&[opcode_token(OPCODE_DCL_RESOURCE, 4), t, 0, 2 /*dim*/]);
    // dcl_sampler s0
    tokens.extend_from_slice(&[opcode_token(OPCODE_DCL_SAMPLER, 3), s, 0]);

    // sample_l o1, l(0.5, 0.5, 0, 0), t0, s0, l(0)
    let sample_l_len = 14u32;
    tokens.extend_from_slice(&[
        opcode_token(OPCODE_SAMPLE_L, sample_l_len),
        dst_o,
        1, // o1 index
        imm_vec4,
        0.5f32.to_bits(),
        0.5f32.to_bits(),
        0.0f32.to_bits(),
        0.0f32.to_bits(),
        t,
        0, // t0
        s,
        0, // s0
        imm_scalar,
        0, // lod=0
    ]);

    // Emit three vertices (o0 positions). o1 retains sampled color.
    let mov_imm_vec4_len = 8u32;
    let mov_imm_vec4 = |tokens: &mut Vec<u32>, x: f32, y: f32, z: f32, w: f32| {
        tokens.extend_from_slice(&[
            opcode_token(OPCODE_MOV, mov_imm_vec4_len),
            dst_o,
            0, // o0
            imm_vec4,
            x.to_bits(),
            y.to_bits(),
            z.to_bits(),
            w.to_bits(),
        ]);
    };

    let emit = opcode_token(OPCODE_EMIT, 1);

    mov_imm_vec4(&mut tokens, -0.5, -0.5, 0.0, 1.0);
    tokens.push(emit);
    mov_imm_vec4(&mut tokens, 0.0, 0.5, 0.0, 1.0);
    tokens.push(emit);
    mov_imm_vec4(&mut tokens, 0.5, -0.5, 0.0, 1.0);
    tokens.push(emit);

    tokens.push(opcode_token(OPCODE_CUT, 1));
    tokens.push(opcode_token(OPCODE_RET, 1));

    // Patch declared length.
    tokens[1] = tokens.len() as u32;

    build_dxbc(&[(FourCC(*b"SHDR"), tokens_to_bytes(&tokens))])
}

fn assert_gs_dxbc_decodes_and_uses_t0_s0(dxbc_bytes: &[u8]) {
    let dxbc = DxbcFile::parse(dxbc_bytes).expect("GS DXBC should parse");
    let program = Sm4Program::parse_from_dxbc(&dxbc).expect("GS DXBC should contain SM4 program");
    assert_eq!(
        program.stage,
        Sm4ShaderStage::Geometry,
        "GS DXBC should decode as a geometry shader"
    );
    let module = decode_program(&program).expect("GS SM4 module should decode");
    assert_eq!(module.stage, Sm4ShaderStage::Geometry);
    assert!(
        module.decls.iter().any(|d| matches!(
            d,
            Sm4Decl::GsInputPrimitive {
                primitive: GsInputPrimitive::Point(_)
            }
        )),
        "GS should declare point input primitive"
    );
    assert!(
        module.decls.iter().any(|d| matches!(
            d,
            Sm4Decl::GsOutputTopology {
                topology: GsOutputTopology::TriangleStrip(_)
            }
        )),
        "GS should declare triangle strip output topology"
    );
    assert!(
        module
            .decls
            .iter()
            .any(|d| matches!(d, Sm4Decl::GsMaxOutputVertexCount { max: 3 })),
        "GS should declare maxvertexcount=3"
    );
    assert!(
        module
            .decls
            .iter()
            .any(|d| matches!(d, Sm4Decl::ResourceTexture2D { slot: 0 })),
        "GS should declare Texture2D t0"
    );
    assert!(
        module
            .decls
            .iter()
            .any(|d| matches!(d, Sm4Decl::Sampler { slot: 0 })),
        "GS should declare Sampler s0"
    );
    assert!(
        module
            .instructions
            .iter()
            .any(|inst| matches!(inst, Sm4Inst::Sample { .. } | Sm4Inst::SampleL { .. })),
        "GS should contain a sample/sample_l instruction"
    );
    assert!(
        module
            .instructions
            .iter()
            .any(|inst| matches!(inst, Sm4Inst::Emit { .. })),
        "GS should emit vertices"
    );
    assert!(
        module
            .instructions
            .iter()
            .any(|inst| matches!(inst, Sm4Inst::Cut { .. })),
        "GS should contain a cut"
    );
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct VertexPos3Color4 {
    pos: [f32; 3],
    color: [f32; 4],
}

#[test]
fn aerogpu_cmd_geometry_shader_translated_prepass_samples_t0_s0() {
    let gs_dxbc = build_gs_sample_l_t0_s0_to_o1_point_to_triangle();
    assert_gs_dxbc_decodes_and_uses_t0_s0(&gs_dxbc);

    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::aerogpu_cmd_geometry_shader_translated_prepass_samples_t0_s0"
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
        let vb_bytes = bytemuck::bytes_of(&vertex);

        let w = 64u32;
        let h = 64u32;

        // 1x1 RGBA8 texture with deterministic texel: pure green.
        let texel: [u8; 4] = [0, 255, 0, 255];

        let mut writer = AerogpuCmdWriter::new();

        writer.create_buffer(
            VB,
            AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
            vb_bytes.len() as u64,
            0,
            0,
        );
        writer.upload_resource(VB, 0, vb_bytes);

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
        writer.upload_resource(TEX, 0, &texel);

        writer.create_sampler(
            SAMP,
            AerogpuSamplerFilter::Nearest,
            AerogpuSamplerAddressMode::ClampToEdge,
            AerogpuSamplerAddressMode::ClampToEdge,
            AerogpuSamplerAddressMode::ClampToEdge,
        );

        // Bind GS-stage Texture2D(t0) and Sampler(s0). These should be wired to the translated
        // geometry prepass compute shader via @group(3).
        writer.set_texture(AerogpuShaderStage::Geometry, 0, TEX);
        writer.set_samplers(AerogpuShaderStage::Geometry, 0, &[SAMP]);

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
        writer.set_render_targets(&[RT], 0);
        writer.set_viewport(0.0, 0.0, w as f32, h as f32, 0.0, 1.0);
        writer.clear(AEROGPU_CLEAR_COLOR, [1.0, 0.0, 0.0, 1.0], 1.0, 0);

        writer.create_shader_dxbc(VS, AerogpuShaderStage::Vertex, VS_PASSTHROUGH);
        writer.create_shader_dxbc_ex(GS, AerogpuShaderStageEx::Geometry, &gs_dxbc);
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
        // Disable face culling so the test does not depend on winding conventions.
        writer.set_rasterizer_state_ext(
            AerogpuFillMode::Solid,
            AerogpuCullMode::None,
            false,
            false,
            0,
            false,
        );

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
        assert_eq!(pixels.len(), (w * h * 4) as usize);

        let px = |x: u32, y: u32| -> [u8; 4] {
            let idx = ((y * w + x) * 4) as usize;
            pixels[idx..idx + 4].try_into().unwrap()
        };

        // The triangle is centered and should not cover the top-left corner.
        assert_eq!(px(0, 0), [255, 0, 0, 255]);
        // Center pixel should be covered by the GS-emitted triangle, and shaded by the sampled
        // texel (pure green).
        assert_eq!(px(w / 2, h / 2), [0, 255, 0, 255]);
    });
}
