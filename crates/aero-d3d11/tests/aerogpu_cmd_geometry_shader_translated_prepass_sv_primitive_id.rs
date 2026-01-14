mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_d3d11::sm4::opcode as sm4_opcode;
use aero_d3d11::{FourCC, WriteMask};
use aero_dxbc::test_utils as dxbc_test_utils;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCullMode, AerogpuFillMode, AerogpuPrimitiveTopology, AerogpuShaderStage,
    AerogpuShaderStageEx, AerogpuVertexBufferBinding, AEROGPU_CLEAR_COLOR,
    AEROGPU_RESOURCE_USAGE_RENDER_TARGET, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

const FOURCC_SHEX: FourCC = FourCC(*b"SHEX");

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

fn reg_dst(ty: u32, idx: u32, mask: WriteMask) -> Vec<u32> {
    vec![
        operand_token(ty, 2, sm4_opcode::OPERAND_SEL_MASK, mask.0 as u32, 1),
        idx,
    ]
}

fn reg_src(ty: u32, idx: u32) -> Vec<u32> {
    vec![
        operand_token(
            ty,
            2,
            sm4_opcode::OPERAND_SEL_SWIZZLE,
            swizzle_bits([0, 1, 2, 3]),
            1,
        ),
        idx,
    ]
}

fn imm32_vec4(values: [u32; 4]) -> Vec<u32> {
    let mut out = Vec::with_capacity(1 + 4);
    out.push(operand_token(
        sm4_opcode::OPERAND_TYPE_IMMEDIATE32,
        2,
        sm4_opcode::OPERAND_SEL_SWIZZLE,
        swizzle_bits([0, 1, 2, 3]),
        0,
    ));
    out.extend_from_slice(&values);
    out
}

fn build_gs_primitive_id_colored_split() -> Vec<u8> {
    // D3D name token for `SV_PrimitiveID`.
    const D3D_NAME_PRIMITIVE_ID: u32 = 7;
    // The SM4 decoder treats any opcode >= 0x100 as a declaration. For input/output declarations we
    // only care that it is decoded as `Sm4Decl::{InputSiv,Output}`. Use dummy declaration opcodes.
    const DCL_DUMMY: u32 = 0x100;

    // gs_5_0 (program type = geometry).
    let version_token = 0x0002_0050u32;
    let mut tokens = vec![version_token, 0];

    // Geometry metadata declarations.
    tokens.push(opcode_token(sm4_opcode::OPCODE_DCL_GS_INPUT_PRIMITIVE, 2));
    tokens.push(4); // triangle (tokenized format / D3D topology constant)
    tokens.push(opcode_token(sm4_opcode::OPCODE_DCL_GS_OUTPUT_TOPOLOGY, 2));
    tokens.push(5); // triangle_strip (D3D primitive topology constant)
    tokens.push(opcode_token(
        sm4_opcode::OPCODE_DCL_GS_MAX_OUTPUT_VERTEX_COUNT,
        2,
    ));
    tokens.push(3); // 3 verts per triangle

    // dcl_input_siv v0.xyzw, SV_PrimitiveID
    //
    // Use an XYZW write mask so the `movc` condition is uniform across lanes.
    tokens.push(opcode_token(DCL_DUMMY, 4));
    tokens.extend_from_slice(&reg_dst(
        sm4_opcode::OPERAND_TYPE_INPUT,
        0,
        WriteMask::XYZW,
    ));
    tokens.push(D3D_NAME_PRIMITIVE_ID);

    // dcl_output o0.xyzw (position)
    tokens.push(opcode_token(DCL_DUMMY + 1, 3));
    tokens.extend_from_slice(&reg_dst(
        sm4_opcode::OPERAND_TYPE_OUTPUT,
        0,
        WriteMask::XYZW,
    ));
    // dcl_output o1.xyzw (color)
    tokens.push(opcode_token(DCL_DUMMY + 2, 3));
    tokens.extend_from_slice(&reg_dst(
        sm4_opcode::OPERAND_TYPE_OUTPUT,
        1,
        WriteMask::XYZW,
    ));

    // utof r0.xyzw, v0.xyzw
    tokens.push(opcode_token(sm4_opcode::OPCODE_UTOF, 5));
    tokens.extend_from_slice(&reg_dst(
        sm4_opcode::OPERAND_TYPE_TEMP,
        0,
        WriteMask::XYZW,
    ));
    tokens.extend_from_slice(&reg_src(sm4_opcode::OPERAND_TYPE_INPUT, 0));

    // mul r1.xyzw, r0.xyzw, l(1,0,0,0)  (x offset per primitive)
    tokens.push(opcode_token(sm4_opcode::OPCODE_MUL, 10));
    tokens.extend_from_slice(&reg_dst(
        sm4_opcode::OPERAND_TYPE_TEMP,
        1,
        WriteMask::XYZW,
    ));
    tokens.extend_from_slice(&reg_src(sm4_opcode::OPERAND_TYPE_TEMP, 0));
    tokens.extend_from_slice(&imm32_vec4([
        1.0f32.to_bits(),
        0.0f32.to_bits(),
        0.0f32.to_bits(),
        0.0f32.to_bits(),
    ]));

    // movc o1.xyzw, v0.xyzw, l(1,0,1,1), l(0,1,1,1)
    // primitive 0 => cyan, primitive 1 => magenta.
    //
    // Use colors that the placeholder compute prepass cannot emit (placeholder is red/green).
    tokens.push(opcode_token(sm4_opcode::OPCODE_MOVC, 15));
    tokens.extend_from_slice(&reg_dst(
        sm4_opcode::OPERAND_TYPE_OUTPUT,
        1,
        WriteMask::XYZW,
    ));
    tokens.extend_from_slice(&reg_src(sm4_opcode::OPERAND_TYPE_INPUT, 0));
    // a = magenta (primitive_id != 0)
    tokens.extend_from_slice(&imm32_vec4([
        1.0f32.to_bits(),
        0.0f32.to_bits(),
        1.0f32.to_bits(),
        1.0f32.to_bits(),
    ]));
    // b = cyan (primitive_id == 0)
    tokens.extend_from_slice(&imm32_vec4([
        0.0f32.to_bits(),
        1.0f32.to_bits(),
        1.0f32.to_bits(),
        1.0f32.to_bits(),
    ]));

    // Emit a triangle per primitive. Primitive ID shifts the triangle horizontally so both are
    // visible in one draw.
    //
    // add o0.xyzw, l(-0.9, -0.5, 0, 1), r1; emit
    tokens.push(opcode_token(sm4_opcode::OPCODE_ADD, 10));
    tokens.extend_from_slice(&reg_dst(
        sm4_opcode::OPERAND_TYPE_OUTPUT,
        0,
        WriteMask::XYZW,
    ));
    tokens.extend_from_slice(&imm32_vec4([
        (-0.9f32).to_bits(),
        (-0.5f32).to_bits(),
        0.0f32.to_bits(),
        1.0f32.to_bits(),
    ]));
    tokens.extend_from_slice(&reg_src(sm4_opcode::OPERAND_TYPE_TEMP, 1));
    tokens.push(opcode_token(sm4_opcode::OPCODE_EMIT, 1));

    // add o0.xyzw, l(-0.1, -0.5, 0, 1), r1; emit
    tokens.push(opcode_token(sm4_opcode::OPCODE_ADD, 10));
    tokens.extend_from_slice(&reg_dst(
        sm4_opcode::OPERAND_TYPE_OUTPUT,
        0,
        WriteMask::XYZW,
    ));
    tokens.extend_from_slice(&imm32_vec4([
        (-0.1f32).to_bits(),
        (-0.5f32).to_bits(),
        0.0f32.to_bits(),
        1.0f32.to_bits(),
    ]));
    tokens.extend_from_slice(&reg_src(sm4_opcode::OPERAND_TYPE_TEMP, 1));
    tokens.push(opcode_token(sm4_opcode::OPCODE_EMIT, 1));

    // add o0.xyzw, l(-0.5, 0.5, 0, 1), r1; emitthen_cut
    tokens.push(opcode_token(sm4_opcode::OPCODE_ADD, 10));
    tokens.extend_from_slice(&reg_dst(
        sm4_opcode::OPERAND_TYPE_OUTPUT,
        0,
        WriteMask::XYZW,
    ));
    tokens.extend_from_slice(&imm32_vec4([
        (-0.5f32).to_bits(),
        0.5f32.to_bits(),
        0.0f32.to_bits(),
        1.0f32.to_bits(),
    ]));
    tokens.extend_from_slice(&reg_src(sm4_opcode::OPERAND_TYPE_TEMP, 1));
    tokens.push(opcode_token(sm4_opcode::OPCODE_EMITTHENCUT, 1));

    // ret
    tokens.push(opcode_token(sm4_opcode::OPCODE_RET, 1));

    tokens[1] = tokens.len() as u32;

    build_dxbc(&[(FOURCC_SHEX, tokens_to_bytes(&tokens))])
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct VertexPos3Color4 {
    pos: [f32; 3],
    color: [f32; 4],
}

#[test]
fn aerogpu_cmd_geometry_shader_translated_prepass_sv_primitive_id() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::aerogpu_cmd_geometry_shader_translated_prepass_sv_primitive_id"
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
        const PS: u32 = 4;
        const GS: u32 = 5;
        const IL: u32 = 6;

        // Two input triangles (2 primitives) in one draw call.
        //
        // The translated GS ignores the input vertex data and instead uses `SV_PrimitiveID` to:
        // - offset emitted triangles left vs right
        // - pick different colors (cyan vs magenta)
        //
        // This structure ensures the placeholder prepass (which emits huge red/green triangles) cannot
        // accidentally satisfy the pixel assertions.
        let vertices: [VertexPos3Color4; 6] = [
            // Triangle 0
            VertexPos3Color4 {
                pos: [-1.0, -1.0, 0.0],
                color: [0.0, 0.0, 0.0, 1.0],
            },
            VertexPos3Color4 {
                pos: [-1.0, 1.0, 0.0],
                color: [0.0, 0.0, 0.0, 1.0],
            },
            VertexPos3Color4 {
                pos: [0.0, -1.0, 0.0],
                color: [0.0, 0.0, 0.0, 1.0],
            },
            // Triangle 1
            VertexPos3Color4 {
                pos: [0.0, -1.0, 0.0],
                color: [0.0, 0.0, 0.0, 1.0],
            },
            VertexPos3Color4 {
                pos: [1.0, 1.0, 0.0],
                color: [0.0, 0.0, 0.0, 1.0],
            },
            VertexPos3Color4 {
                pos: [1.0, -1.0, 0.0],
                color: [0.0, 0.0, 0.0, 1.0],
            },
        ];
        let vb_bytes = bytemuck::bytes_of(&vertices);

        let (width, height) = (64u32, 64u32);

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
            RT,
            AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
            AerogpuFormat::R8G8B8A8Unorm as u32,
            width,
            height,
            1,
            1,
            0,
            0,
            0,
        );
        writer.set_render_targets(&[RT], 0);
        writer.set_viewport(0.0, 0.0, width as f32, height as f32, 0.0, 1.0);

        writer.create_shader_dxbc(VS, AerogpuShaderStage::Vertex, VS_PASSTHROUGH);
        writer.create_shader_dxbc(PS, AerogpuShaderStage::Pixel, PS_PASSTHROUGH);
        writer.create_shader_dxbc_ex(
            GS,
            AerogpuShaderStageEx::Geometry,
            &build_gs_primitive_id_colored_split(),
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
        writer.set_primitive_topology(AerogpuPrimitiveTopology::TriangleList);
        writer.bind_shaders_ex(VS, PS, 0, GS, 0, 0);

        // Disable face culling so the test does not depend on triangle winding conventions.
        writer.set_rasterizer_state_ext(
            AerogpuFillMode::Solid,
            AerogpuCullMode::None,
            false,
            false,
            0,
            false,
        );

        writer.clear(AEROGPU_CLEAR_COLOR, [0.0, 0.0, 0.0, 1.0], 1.0, 0);
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
        assert_eq!(pixels.len(), (width * height * 4) as usize);

        let px = |x: u32, y: u32| -> [u8; 4] {
            let idx = ((y * width + x) * 4) as usize;
            pixels[idx..idx + 4].try_into().unwrap()
        };

        // Left triangle should be primitive 0 (cyan).
        assert_eq!(px(width / 4, height / 2), [0, 255, 255, 255]);
        // Right triangle should be primitive 1 (magenta).
        assert_eq!(px(width * 3 / 4, height / 2), [255, 0, 255, 255]);

        // Corners should remain clear (the placeholder prepass would fill them with red/green).
        assert_eq!(px(0, 0), [0, 0, 0, 255]);
    });
}

