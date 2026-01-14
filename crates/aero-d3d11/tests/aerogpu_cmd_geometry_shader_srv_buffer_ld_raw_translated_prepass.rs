mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_d3d11::sm4::opcode::{
    OPCODE_CUT, OPCODE_DCL_GS_INPUT_PRIMITIVE, OPCODE_DCL_GS_MAX_OUTPUT_VERTEX_COUNT,
    OPCODE_DCL_GS_OUTPUT_TOPOLOGY, OPCODE_DCL_RESOURCE_RAW, OPCODE_EMIT, OPCODE_LD_RAW,
    OPCODE_LEN_SHIFT, OPCODE_MOV, OPCODE_RET, OPERAND_COMPONENT_SELECTION_MASK,
    OPERAND_COMPONENT_SELECTION_SHIFT, OPERAND_INDEX0_REP_SHIFT, OPERAND_INDEX1_REP_SHIFT,
    OPERAND_INDEX2_REP_SHIFT, OPERAND_INDEX_DIMENSION_MASK, OPERAND_INDEX_DIMENSION_SHIFT,
    OPERAND_INDEX_REP_IMMEDIATE32, OPERAND_NUM_COMPONENTS_MASK, OPERAND_SELECTION_MODE_MASK,
    OPERAND_SELECTION_MODE_SHIFT, OPERAND_SEL_MASK, OPERAND_SEL_SELECT1, OPERAND_SEL_SWIZZLE,
    OPERAND_TYPE_IMMEDIATE32, OPERAND_TYPE_MASK, OPERAND_TYPE_OUTPUT, OPERAND_TYPE_RESOURCE,
    OPERAND_TYPE_SHIFT, OPERAND_TYPE_TEMP,
};
use aero_d3d11::{Swizzle, WriteMask};
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

#[derive(Clone, Copy)]
struct SigParam {
    semantic_name: &'static str,
    semantic_index: u32,
    register: u32,
    mask: u8,
}

fn build_signature_chunk(params: &[SigParam]) -> Vec<u8> {
    let entries: Vec<dxbc_test_utils::SignatureEntryDesc<'_>> = params
        .iter()
        .map(|p| dxbc_test_utils::SignatureEntryDesc {
            semantic_name: p.semantic_name,
            semantic_index: p.semantic_index,
            system_value_type: 0,
            component_type: 0,
            register: p.register,
            mask: p.mask,
            read_write_mask: p.mask,
            stream: 0,
            min_precision: 0,
        })
        .collect();
    dxbc_test_utils::build_signature_chunk_v0(&entries)
}

fn opcode_token(opcode: u32, len: u32) -> u32 {
    opcode | (len << OPCODE_LEN_SHIFT)
}

fn operand_token(
    ty: u32,
    num_components: u32,
    selection_mode: u32,
    component_sel: u32,
    index_dim: u32,
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
    token
}

fn swizzle_bits(swz: [u8; 4]) -> u32 {
    (swz[0] as u32) | ((swz[1] as u32) << 2) | ((swz[2] as u32) << 4) | ((swz[3] as u32) << 6)
}

fn reg_dst(ty: u32, idx: u32, mask: WriteMask) -> Vec<u32> {
    vec![
        operand_token(ty, 2, OPERAND_SEL_MASK, mask.0 as u32, 1),
        idx,
    ]
}

fn reg_src(ty: u32, indices: &[u32], swizzle: Swizzle) -> Vec<u32> {
    let num_components = match ty {
        OPERAND_TYPE_RESOURCE => 0,
        _ => 2,
    };
    let selection_mode = if num_components == 0 {
        OPERAND_SEL_MASK
    } else {
        OPERAND_SEL_SWIZZLE
    };
    let component_sel = if num_components == 0 {
        0
    } else {
        swizzle_bits(swizzle.0)
    };
    let token = operand_token(
        ty,
        num_components,
        selection_mode,
        component_sel,
        indices.len() as u32,
    );
    let mut out = Vec::new();
    out.push(token);
    out.extend_from_slice(indices);
    out
}

fn imm_u32_scalar(value: u32) -> Vec<u32> {
    vec![
        operand_token(OPERAND_TYPE_IMMEDIATE32, 1, OPERAND_SEL_SELECT1, 0, 0),
        value,
    ]
}

fn imm_f32x4(v: [f32; 4]) -> Vec<u32> {
    let mut out = Vec::new();
    out.push(operand_token(
        OPERAND_TYPE_IMMEDIATE32,
        2,
        OPERAND_SEL_SWIZZLE,
        swizzle_bits(Swizzle::XYZW.0),
        0,
    ));
    out.extend_from_slice(&[
        v[0].to_bits(),
        v[1].to_bits(),
        v[2].to_bits(),
        v[3].to_bits(),
    ]);
    out
}

fn build_vs_pos_only_dxbc() -> Vec<u8> {
    // Minimal vs_4_0: mov o0, v0; ret
    //
    // Include an unused COLOR0 input so this shader can be paired with the existing
    // POS3+COLOR input-layout fixture.
    let isgn = build_signature_chunk(&[
        SigParam {
            semantic_name: "POSITION",
            semantic_index: 0,
            register: 0,
            mask: 0x07,
        },
        SigParam {
            semantic_name: "COLOR",
            semantic_index: 0,
            register: 1,
            mask: 0x0f,
        },
    ]);
    let osgn = build_signature_chunk(&[SigParam {
        semantic_name: "SV_Position",
        semantic_index: 0,
        register: 0,
        mask: 0x0f,
    }]);

    // Hand-authored encoding for `mov o0, v0; ret`.
    let version_token = 0x0001_0040u32; // vs_4_0
    let mov_token = OPCODE_MOV | (5u32 << OPCODE_LEN_SHIFT);
    let dst_o0 = 0x0010_f022u32;
    let src_v0 = 0x001e_4016u32;
    let ret_token = OPCODE_RET | (1u32 << OPCODE_LEN_SHIFT);

    let mut tokens = vec![
        version_token,
        0, // length patched below
        mov_token,
        dst_o0,
        0, // o0
        src_v0,
        0, // v0
        ret_token,
    ];
    tokens[1] = tokens.len() as u32;

    let shdr = tokens_to_bytes(&tokens);
    build_dxbc(&[
        (FourCC(*b"ISGN"), isgn),
        (FourCC(*b"OSGN"), osgn),
        (FourCC(*b"SHDR"), shdr),
    ])
}

fn build_gs_point_to_triangle_color_from_t0_ld_raw() -> Vec<u8> {
    // Minimal gs_4_0 that:
    // - Declares point input + triangle strip output + maxvertexcount=3.
    // - Declares a raw SRV buffer t0 and loads a vec4<u32> via `ld_raw`.
    // - Writes the loaded bits to output register o1 (COLOR0 varying).
    // - Emits a small triangle covering the center pixel.

    const PRIM_POINT: u32 = 1;
    const TOPO_TRIANGLE_STRIP: u32 = 5;
    const MAX_VERTS: u32 = 3;

    let mut tokens = vec![
        0x0002_0040u32, // gs_4_0
        0,              // length patched below
        opcode_token(OPCODE_DCL_GS_INPUT_PRIMITIVE, 2),
        PRIM_POINT,
        opcode_token(OPCODE_DCL_GS_OUTPUT_TOPOLOGY, 2),
        TOPO_TRIANGLE_STRIP,
        opcode_token(OPCODE_DCL_GS_MAX_OUTPUT_VERTEX_COUNT, 2),
        MAX_VERTS,
        // dcl_resource_raw t0
        opcode_token(OPCODE_DCL_RESOURCE_RAW, 3),
        operand_token(OPERAND_TYPE_RESOURCE, 0, OPERAND_SEL_MASK, 0, 1),
        0, // t0
    ];

    // ld_raw r0.xyzw, 0, t0
    let mut inst = vec![0u32];
    inst.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    inst.extend_from_slice(&imm_u32_scalar(0));
    inst.extend_from_slice(&reg_src(OPERAND_TYPE_RESOURCE, &[0], Swizzle::XYZW));
    inst[0] = opcode_token(OPCODE_LD_RAW, inst.len() as u32);
    tokens.extend_from_slice(&inst);

    // mov o1, r0  (varying COLOR0 for ps_passthrough.dxbc)
    let mut inst = vec![0u32];
    inst.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 1, WriteMask::XYZW));
    inst.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, &[0], Swizzle::XYZW));
    inst[0] = opcode_token(OPCODE_MOV, inst.len() as u32);
    tokens.extend_from_slice(&inst);

    let emit_vertex = |tokens: &mut Vec<u32>, x: f32, y: f32| {
        // mov o0, l(x,y,0,1)
        let mut inst = vec![0u32];
        inst.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
        inst.extend_from_slice(&imm_f32x4([x, y, 0.0, 1.0]));
        inst[0] = opcode_token(OPCODE_MOV, inst.len() as u32);
        tokens.extend_from_slice(&inst);

        // emit
        tokens.push(opcode_token(OPCODE_EMIT, 1));
    };

    // Small centered triangle (matches the "translated GS prepass" tests).
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
fn aerogpu_cmd_geometry_shader_srv_buffer_ld_raw_translated_prepass() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::aerogpu_cmd_geometry_shader_srv_buffer_ld_raw_translated_prepass"
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
        const SRV_T0: u32 = 2;
        const RT: u32 = 3;
        const VS: u32 = 4;
        const GS: u32 = 5;
        const PS: u32 = 6;
        const IL: u32 = 7;

        let vertex = VertexPos3Color4 {
            pos: [0.0, 0.0, 0.0],
            color: [0.0, 0.0, 0.0, 1.0],
        };
        let vb_bytes = bytemuck::bytes_of(&vertex);

        // Store float bit patterns so the translated GS prepass can bitcast directly to vec4<f32>.
        let srv_words: [u32; 4] = [
            1.0f32.to_bits(),
            0.0f32.to_bits(),
            1.0f32.to_bits(),
            1.0f32.to_bits(),
        ];
        let srv_bytes = bytemuck::cast_slice(&srv_words);

        // Use an odd render target size so NDC (0,0) maps exactly to the center pixel.
        let w = 65u32;
        let h = 65u32;

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
            SRV_T0,
            AEROGPU_RESOURCE_USAGE_STORAGE,
            srv_bytes.len() as u64,
            0,
            0,
        );
        writer.upload_resource(SRV_T0, 0, srv_bytes);

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

        writer.create_shader_dxbc(VS, AerogpuShaderStage::Vertex, &build_vs_pos_only_dxbc());
        writer.create_shader_dxbc_ex(
            GS,
            AerogpuShaderStageEx::Geometry,
            &build_gs_point_to_triangle_color_from_t0_ld_raw(),
        );
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
        // Disable face culling to avoid depending on winding conventions.
        writer.set_rasterizer_state_ext(
            AerogpuFillMode::Solid,
            AerogpuCullMode::None,
            false,
            false,
            0,
            false,
        );

        // Bind SRV buffer t0 to the geometry stage so it is visible to the translated GS prepass.
        writer.set_shader_resource_buffers_ex(
            AerogpuShaderStageEx::Geometry,
            0,
            &[AerogpuShaderResourceBufferBinding {
                buffer: SRV_T0,
                offset_bytes: 0,
                size_bytes: 0, // 0 = whole buffer
                reserved0: 0,
            }],
        );

        writer.clear(AEROGPU_CLEAR_COLOR, [0.0, 0.0, 0.0, 1.0], 1.0, 0);
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

        // Center pixel should receive the SRV-provided color (magenta).
        assert_eq!(px(w / 2, h / 2), [255, 0, 255, 255]);
        // The triangle is intentionally small; a pixel above the center should remain the clear
        // color. This helps catch accidental fallback to a placeholder prepass.
        assert_eq!(px(w / 2, h / 2 - 10), [0, 0, 0, 255]);
    });
}
