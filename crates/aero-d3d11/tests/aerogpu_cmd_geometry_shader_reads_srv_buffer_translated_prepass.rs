mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_d3d11::sm4::opcode::*;
use aero_d3d11::{OperandModifier, Swizzle, WriteMask};
use aero_dxbc::{test_utils as dxbc_test_utils, FourCC};
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCullMode, AerogpuFillMode, AerogpuPrimitiveTopology, AerogpuShaderResourceBufferBinding,
    AerogpuShaderStage, AerogpuShaderStageEx, AerogpuVertexBufferBinding, AEROGPU_CLEAR_COLOR,
    AEROGPU_RESOURCE_USAGE_RENDER_TARGET, AEROGPU_RESOURCE_USAGE_STORAGE,
    AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

const ILAY_POS3_COLOR: &[u8] = include_bytes!("fixtures/ilay_pos3_color.bin");
const VS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/vs_passthrough.dxbc");
const PS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/ps_passthrough.dxbc");

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

fn operand_token(
    ty: u32,
    num_components: u32,
    selection_mode: u32,
    component_sel: u32,
    index_dim: u32,
    extended: bool,
) -> u32 {
    let mut token = 0u32;
    token |= num_components & OPERAND_NUM_COMPONENTS_MASK;
    token |= (selection_mode & OPERAND_SELECTION_MODE_MASK) << OPERAND_SELECTION_MODE_SHIFT;
    token |= (ty & OPERAND_TYPE_MASK) << OPERAND_TYPE_SHIFT;
    token |=
        (component_sel & OPERAND_COMPONENT_SELECTION_MASK) << OPERAND_COMPONENT_SELECTION_SHIFT;
    token |= (index_dim & OPERAND_INDEX_DIMENSION_MASK) << OPERAND_INDEX_DIMENSION_SHIFT;
    token |= OPERAND_INDEX_REP_IMMEDIATE32 << OPERAND_INDEX0_REP_SHIFT;
    token |= OPERAND_INDEX_REP_IMMEDIATE32 << OPERAND_INDEX1_REP_SHIFT;
    token |= OPERAND_INDEX_REP_IMMEDIATE32 << OPERAND_INDEX2_REP_SHIFT;
    if extended {
        token |= OPERAND_EXTENDED_BIT;
    }
    token
}

fn swizzle_bits(swz: [u8; 4]) -> u32 {
    (swz[0] as u32) | ((swz[1] as u32) << 2) | ((swz[2] as u32) << 4) | ((swz[3] as u32) << 6)
}

fn reg_dst(ty: u32, idx: u32, mask: WriteMask) -> Vec<u32> {
    vec![
        operand_token(ty, 2, OPERAND_SEL_MASK, mask.0 as u32, 1, false),
        idx,
    ]
}

fn reg_src(ty: u32, indices: &[u32], swizzle: Swizzle, modifier: OperandModifier) -> Vec<u32> {
    let needs_ext = !matches!(modifier, OperandModifier::None);
    let num_components = match ty {
        OPERAND_TYPE_SAMPLER | OPERAND_TYPE_RESOURCE | OPERAND_TYPE_UNORDERED_ACCESS_VIEW => 0,
        _ => 2,
    };
    let selection_mode = if num_components == 0 {
        OPERAND_SEL_MASK
    } else {
        OPERAND_SEL_SWIZZLE
    };
    let token = operand_token(
        ty,
        num_components,
        selection_mode,
        swizzle_bits(swizzle.0),
        indices.len() as u32,
        needs_ext,
    );
    let mut out = Vec::new();
    out.push(token);
    if needs_ext {
        let mod_bits: u32 = match modifier {
            OperandModifier::None => 0,
            OperandModifier::Neg => 1,
            OperandModifier::Abs => 2,
            OperandModifier::AbsNeg => 3,
        };
        out.push(mod_bits << 6);
    }
    out.extend_from_slice(indices);
    out
}

fn imm32_vec4(values: [u32; 4]) -> Vec<u32> {
    let mut out = Vec::with_capacity(1 + 4);
    out.push(operand_token(
        OPERAND_TYPE_IMMEDIATE32,
        2,
        OPERAND_SEL_SWIZZLE,
        swizzle_bits(Swizzle::XYZW.0),
        0,
        false,
    ));
    out.extend_from_slice(&values);
    out
}

fn imm32_scalar(value: u32) -> Vec<u32> {
    vec![
        operand_token(OPERAND_TYPE_IMMEDIATE32, 1, OPERAND_SEL_SELECT1, 0, 0, false),
        value,
    ]
}

fn build_gs_point_to_triangle_color_from_raw_srv_t0() -> Vec<u8> {
    // gs_4_0 that:
    // - Declares point input + triangle strip output + maxvertexcount=3.
    // - Declares + reads a raw SRV buffer at t0 via ld_raw.
    // - Uses that read data as COLOR0 output (o1), so the pixel shader can observe it.
    //
    // Note: We intentionally emit a *small* centered triangle so this test can detect if we
    // accidentally fell back to the placeholder GS prepass (which emits a much larger triangle).
    const PRIM_POINT: u32 = 1;
    const TOPO_TRIANGLE_STRIP: u32 = 5;
    const MAX_VERTS: u32 = 3;

    let mut tokens: Vec<u32> = vec![
        0x0002_0040u32, // gs_4_0
        0,              // length patched below
        opcode_token(OPCODE_DCL_GS_INPUT_PRIMITIVE, 2),
        PRIM_POINT,
        opcode_token(OPCODE_DCL_GS_OUTPUT_TOPOLOGY, 2),
        TOPO_TRIANGLE_STRIP,
        opcode_token(OPCODE_DCL_GS_MAX_OUTPUT_VERTEX_COUNT, 2),
        MAX_VERTS,
    ];

    // dcl_resource_raw t0
    let t0 = reg_src(
        OPERAND_TYPE_RESOURCE,
        &[0],
        Swizzle::XYZW,
        OperandModifier::None,
    );
    tokens.push(opcode_token(
        OPCODE_DCL_RESOURCE_RAW,
        (1 + t0.len()) as u32,
    ));
    tokens.extend_from_slice(&t0);

    // ld_raw r0.xyzw, l(0), t0
    let addr = imm32_scalar(0);
    let mut ld_raw = vec![opcode_token(
        OPCODE_LD_RAW,
        (1 + 2 + addr.len() + t0.len()) as u32,
    )];
    ld_raw.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    ld_raw.extend_from_slice(&addr);
    ld_raw.extend_from_slice(&t0);
    tokens.extend_from_slice(&ld_raw);

    // mov o1.xyzw, r0.xyzw
    let mut mov_color = vec![opcode_token(OPCODE_MOV, 1 + 2 + 2)];
    mov_color.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 1, WriteMask::XYZW));
    mov_color.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[0],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    tokens.extend_from_slice(&mov_color);

    // Helper to emit one triangle vertex at a constant position.
    let emit_vertex = |tokens: &mut Vec<u32>, x: f32, y: f32| {
        // mov o0.xyzw, l(x,y,0,1)
        let imm = imm32_vec4([x.to_bits(), y.to_bits(), 0.0f32.to_bits(), 1.0f32.to_bits()]);
        let mut mov_pos = vec![opcode_token(OPCODE_MOV, (1 + 2 + imm.len()) as u32)];
        mov_pos.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
        mov_pos.extend_from_slice(&imm);
        tokens.extend_from_slice(&mov_pos);

        // emit
        tokens.push(opcode_token(OPCODE_EMIT, 1));
    };

    // Small centered clockwise triangle.
    emit_vertex(&mut tokens, -0.25, -0.25);
    emit_vertex(&mut tokens, 0.0, 0.25);
    emit_vertex(&mut tokens, 0.25, -0.25);

    tokens.push(opcode_token(OPCODE_CUT, 1));
    tokens.push(opcode_token(OPCODE_RET, 1));

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
fn aerogpu_cmd_geometry_shader_reads_srv_buffer_translated_prepass() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::aerogpu_cmd_geometry_shader_reads_srv_buffer_translated_prepass"
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
        const SRV: u32 = 2;
        const RT: u32 = 3;
        const VS: u32 = 4;
        const GS: u32 = 5;
        const PS: u32 = 6;
        const IL: u32 = 7;

        // Use an odd render target size so NDC (0,0) maps exactly to the center pixel.
        let w = 65u32;
        let h = 65u32;

        let vertex = VertexPos3Color4 {
            pos: [0.0, 0.0, 0.0],
            // If we accidentally ignore GS and run VS->PS directly, this will be the output color.
            // Pick a value that's not the SRV-buffer color we expect.
            color: [0.0, 0.0, 1.0, 1.0],
        };
        let vb_bytes = bytemuck::bytes_of(&vertex);

        // SRV buffer payload: RGBA = green. Stored as raw u32 words (float bit patterns) so
        // `ld_raw` returns these as f32 lanes via bitcast.
        let srv_words: [u32; 4] = [
            0.0f32.to_bits(),
            1.0f32.to_bits(),
            0.0f32.to_bits(),
            1.0f32.to_bits(),
        ];
        let srv_bytes: &[u8] = bytemuck::cast_slice(&srv_words);

        let mut writer = AerogpuCmdWriter::new();
        writer.create_buffer(
            VB,
            AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
            vb_bytes.len() as u64,
            0,
            0,
        );
        writer.upload_resource(VB, 0, vb_bytes);

        writer.create_buffer(
            SRV,
            AEROGPU_RESOURCE_USAGE_STORAGE,
            srv_bytes.len() as u64,
            0,
            0,
        );
        writer.upload_resource(SRV, 0, srv_bytes);

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
            &build_gs_point_to_triangle_color_from_raw_srv_t0(),
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

        // Disable face culling to avoid depending on winding conventions.
        writer.set_rasterizer_state_ext(
            AerogpuFillMode::Solid,
            AerogpuCullMode::None,
            false,
            false,
            0,
            false,
        );

        // Bind the SRV buffer via the stage_ex path so it routes into the geometry-stage binding
        // table (group(3) bindings for the translated GS prepass).
        writer.set_shader_resource_buffers_ex(
            AerogpuShaderStageEx::Geometry,
            0,
            &[AerogpuShaderResourceBufferBinding {
                buffer: SRV,
                offset_bytes: 0,
                size_bytes: 0, // 0 = whole buffer
                reserved0: 0,
            }],
        );

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
        let px = |x: u32, y: u32| -> [u8; 4] {
            let idx = ((y * w + x) * 4) as usize;
            pixels[idx..idx + 4].try_into().unwrap()
        };

        // The SRV buffer contains green, so the triangle should render green at the center pixel.
        assert_eq!(px(w / 2, h / 2), [0, 255, 0, 255]);
        // The translated GS emits a small triangle. A pixel above the center should remain the
        // clear color. The placeholder prepass triangle would cover this pixel.
        assert_eq!(px(w / 2, h / 2 - 10), [255, 0, 0, 255]);
    });
}
