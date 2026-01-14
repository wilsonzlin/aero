mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_d3d11::sm4::opcode as sm4_opcode;
use aero_dxbc::{test_utils as dxbc_test_utils, FourCC};
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCullMode, AerogpuFillMode, AerogpuPrimitiveTopology, AerogpuShaderStage,
    AerogpuVertexBufferBinding, AEROGPU_CLEAR_COLOR, AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
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

fn opcode_token(opcode: u32, len_dwords: u32) -> u32 {
    opcode | (len_dwords << sm4_opcode::OPCODE_LEN_SHIFT)
}

fn operand_token(
    ty: u32,
    num_components: u32,
    selection_mode: u32,
    component_sel: u32,
    index_dim: u32,
) -> u32 {
    let mut token = 0u32;
    token |= num_components & sm4_opcode::OPERAND_NUM_COMPONENTS_MASK;
    token |= (selection_mode & sm4_opcode::OPERAND_SELECTION_MODE_MASK)
        << sm4_opcode::OPERAND_SELECTION_MODE_SHIFT;
    token |= (ty & sm4_opcode::OPERAND_TYPE_MASK) << sm4_opcode::OPERAND_TYPE_SHIFT;
    token |= (component_sel & sm4_opcode::OPERAND_COMPONENT_SELECTION_MASK)
        << sm4_opcode::OPERAND_COMPONENT_SELECTION_SHIFT;
    token |= (index_dim & sm4_opcode::OPERAND_INDEX_DIMENSION_MASK)
        << sm4_opcode::OPERAND_INDEX_DIMENSION_SHIFT;
    token |= sm4_opcode::OPERAND_INDEX_REP_IMMEDIATE32 << sm4_opcode::OPERAND_INDEX0_REP_SHIFT;
    token |= sm4_opcode::OPERAND_INDEX_REP_IMMEDIATE32 << sm4_opcode::OPERAND_INDEX1_REP_SHIFT;
    token |= sm4_opcode::OPERAND_INDEX_REP_IMMEDIATE32 << sm4_opcode::OPERAND_INDEX2_REP_SHIFT;
    token
}

fn swizzle_bits(swz: [u8; 4]) -> u32 {
    (swz[0] as u32) | ((swz[1] as u32) << 2) | ((swz[2] as u32) << 4) | ((swz[3] as u32) << 6)
}

fn reg_dst(ty: u32, idx: u32, mask: u32) -> Vec<u32> {
    vec![
        operand_token(ty, 2, sm4_opcode::OPERAND_SEL_MASK, mask, 1),
        idx,
    ]
}

fn gs_input_src(reg: u32, vertex: u32) -> Vec<u32> {
    vec![
        operand_token(
            sm4_opcode::OPERAND_TYPE_INPUT,
            2,
            sm4_opcode::OPERAND_SEL_SWIZZLE,
            swizzle_bits([0, 1, 2, 3]),
            sm4_opcode::OPERAND_INDEX_DIMENSION_2D,
        ),
        reg,
        vertex,
    ]
}

fn imm_f32x4(v: [f32; 4]) -> Vec<u32> {
    vec![
        operand_token(
            sm4_opcode::OPERAND_TYPE_IMMEDIATE32,
            2,
            sm4_opcode::OPERAND_SEL_SWIZZLE,
            swizzle_bits([0, 1, 2, 3]),
            0,
        ),
        v[0].to_bits(),
        v[1].to_bits(),
        v[2].to_bits(),
        v[3].to_bits(),
    ]
}

fn build_gs_triadj_emits_triangle_dxbc() -> Vec<u8> {
    // gs_4_0:
    //   dcl_inputprimitive triangleadj
    //   dcl_outputtopology triangle_strip
    //   dcl_maxvertexcount 3
    //
    // TriangleListAdj vertex ordering (D3D11 / HLSL `triangleadj`) uses alternating main/adjacent
    // vertices:
    //   [0] main 0
    //   [1] adjacent (edge 0-2)
    //   [2] main 1
    //   [3] adjacent (edge 2-4)
    //   [4] main 2
    //   [5] adjacent (edge 4-0)
    //
    // This shader shifts the main triangle vertices by v0[5] (adjacent vertex), so the output
    // triangle depends on both the main vertices (0/2/4) and the adjacency vertex (5). We
    // intentionally read v0[5] so the test fails if the executor only populates 3 vertices per
    // primitive.
    let version_token = 0x0002_0040u32; // gs_4_0
    let mut tokens = vec![version_token, 0];

    tokens.push(opcode_token(sm4_opcode::OPCODE_DCL_GS_INPUT_PRIMITIVE, 2));
    tokens.push(7); // D3D10_SB_PRIMITIVE_TRIANGLE_ADJ
    tokens.push(opcode_token(sm4_opcode::OPCODE_DCL_GS_OUTPUT_TOPOLOGY, 2));
    tokens.push(3); // trianglestrip
    tokens.push(opcode_token(
        sm4_opcode::OPCODE_DCL_GS_MAX_OUTPUT_VERTEX_COUNT,
        2,
    ));
    tokens.push(3);

    // dcl_output o0.xyzw
    tokens.push(opcode_token(0x100, 3));
    tokens.extend_from_slice(&reg_dst(sm4_opcode::OPERAND_TYPE_OUTPUT, 0, 0xF));
    // dcl_output o1.xyzw
    tokens.push(opcode_token(0x100, 3));
    tokens.extend_from_slice(&reg_dst(sm4_opcode::OPERAND_TYPE_OUTPUT, 1, 0xF));

    for &main_vertex in &[0u32, 2u32, 4u32] {
        // add o0, v0[main], v0[5]
        let mut inst = vec![0u32];
        inst.extend_from_slice(&reg_dst(sm4_opcode::OPERAND_TYPE_OUTPUT, 0, 0xF));
        inst.extend_from_slice(&gs_input_src(0, main_vertex));
        inst.extend_from_slice(&gs_input_src(0, 5));
        inst[0] = opcode_token(sm4_opcode::OPCODE_ADD, inst.len() as u32);
        tokens.extend_from_slice(&inst);

        // mov o0.w, l(0,0,0,1)
        let mut inst = vec![0u32];
        inst.extend_from_slice(&reg_dst(sm4_opcode::OPERAND_TYPE_OUTPUT, 0, 0x8));
        inst.extend_from_slice(&imm_f32x4([0.0, 0.0, 0.0, 1.0]));
        inst[0] = opcode_token(sm4_opcode::OPCODE_MOV, inst.len() as u32);
        tokens.extend_from_slice(&inst);

        // mov o1, l(0,1,0,1)
        let mut inst = vec![0u32];
        inst.extend_from_slice(&reg_dst(sm4_opcode::OPERAND_TYPE_OUTPUT, 1, 0xF));
        inst.extend_from_slice(&imm_f32x4([0.0, 1.0, 0.0, 1.0]));
        inst[0] = opcode_token(sm4_opcode::OPCODE_MOV, inst.len() as u32);
        tokens.extend_from_slice(&inst);

        tokens.push(opcode_token(sm4_opcode::OPCODE_EMIT, 1));
    }

    tokens.push(opcode_token(sm4_opcode::OPCODE_CUT, 1));
    tokens.push(opcode_token(sm4_opcode::OPCODE_RET, 1));
    tokens[1] = tokens.len() as u32;

    build_dxbc(&[(FourCC(*b"SHDR"), tokens_to_bytes(&tokens))])
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct VertexPos3Color4 {
    pos: [f32; 3],
    color: [f32; 4],
}

#[test]
fn aerogpu_cmd_geometry_shader_trianglelistadj_emits_triangle() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::aerogpu_cmd_geometry_shader_trianglelistadj_emits_triangle"
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
        const RT: u32 = 2;
        const VS: u32 = 3;
        const GS: u32 = 4;
        const PS: u32 = 5;
        const IL: u32 = 6;

        // TriangleListAdj consumes 6 vertices per primitive. The GS expects `triangleadj` ordering
        // where indices 0/2/4 are the main triangle vertices and 1/3/5 are adjacency vertices.
        //
        // We use v0[5] as an X shift (+2) applied to the main vertices, moving an otherwise
        // off-screen triangle into view near the center of the render target.
        let vertices = [
            // v0 (main0)
            VertexPos3Color4 {
                pos: [-2.5, -0.5, 0.0],
                color: [0.0, 0.0, 0.0, 1.0],
            },
            // v1 (adj0, unused)
            VertexPos3Color4 {
                pos: [0.0, 0.0, 0.0],
                color: [0.0, 0.0, 0.0, 1.0],
            },
            // v2 (main1)
            VertexPos3Color4 {
                pos: [-1.5, -0.5, 0.0],
                color: [0.0, 0.0, 0.0, 1.0],
            },
            // v3 (adj1, unused)
            VertexPos3Color4 {
                pos: [0.0, 0.0, 0.0],
                color: [0.0, 0.0, 0.0, 1.0],
            },
            // v4 (main2)
            VertexPos3Color4 {
                pos: [-2.0, 0.5, 0.0],
                color: [0.0, 0.0, 0.0, 1.0],
            },
            // v5 (adj2 / shift)
            VertexPos3Color4 {
                pos: [2.0, 0.0, 0.0],
                color: [0.0, 0.0, 0.0, 1.0],
            },
        ];
        let vb_bytes = bytemuck::bytes_of(&vertices);

        let mut writer = AerogpuCmdWriter::new();
        writer.create_buffer(
            VB,
            AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
            vb_bytes.len() as u64,
            0,
            0,
        );
        writer.upload_resource(VB, 0, vb_bytes);

        // Use an odd-sized render target so NDC (0,0) maps exactly to the center pixel.
        let w = 65u32;
        let h = 65u32;
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

        // Disable culling so the emitted triangle is visible regardless of winding.
        writer.set_rasterizer_state(
            AerogpuFillMode::Solid,
            AerogpuCullMode::None,
            false,
            false,
            0,
            0,
        );

        let gs_dxbc = build_gs_triadj_emits_triangle_dxbc();
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
        writer.set_primitive_topology(AerogpuPrimitiveTopology::TriangleListAdj);
        writer.bind_shaders_ex(VS, PS, 0, GS, 0, 0);

        // Clear to red; the translated GS prepass should draw a green triangle near the center.
        writer.clear(AEROGPU_CLEAR_COLOR, [1.0, 0.0, 0.0, 1.0], 1.0, 0);
        writer.draw(6, 1, 0, 0);

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

        let x = w / 2;
        let y = h / 2;
        let idx = ((y * w + x) * 4) as usize;
        let center: [u8; 4] = pixels[idx..idx + 4].try_into().unwrap();

        assert_eq!(
            center,
            [0, 255, 0, 255],
            "expected a green pixel at the center (translated triadj GS prepass); got {center:?}"
        );
    });
}
