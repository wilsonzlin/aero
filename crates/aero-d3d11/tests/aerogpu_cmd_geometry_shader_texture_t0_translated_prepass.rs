mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_d3d11::sm4::opcode::{
    OPCODE_CUT, OPCODE_DCL_GS_INPUT_PRIMITIVE, OPCODE_DCL_GS_MAX_OUTPUT_VERTEX_COUNT,
    OPCODE_DCL_GS_OUTPUT_TOPOLOGY, OPCODE_DCL_OUTPUT, OPCODE_DCL_RESOURCE, OPCODE_DCL_SAMPLER,
    OPCODE_EMIT, OPCODE_LEN_SHIFT, OPCODE_MOV, OPCODE_RET, OPCODE_SAMPLE,
    OPERAND_COMPONENT_SELECTION_MASK, OPERAND_COMPONENT_SELECTION_SHIFT, OPERAND_INDEX0_REP_SHIFT,
    OPERAND_INDEX1_REP_SHIFT, OPERAND_INDEX2_REP_SHIFT, OPERAND_INDEX_DIMENSION_MASK,
    OPERAND_INDEX_DIMENSION_SHIFT, OPERAND_INDEX_REP_IMMEDIATE32, OPERAND_NUM_COMPONENTS_MASK,
    OPERAND_SELECTION_MODE_MASK, OPERAND_SELECTION_MODE_SHIFT, OPERAND_SEL_MASK,
    OPERAND_SEL_SWIZZLE, OPERAND_TYPE_IMMEDIATE32, OPERAND_TYPE_MASK, OPERAND_TYPE_OUTPUT,
    OPERAND_TYPE_RESOURCE, OPERAND_TYPE_SAMPLER, OPERAND_TYPE_SHIFT, OPERAND_TYPE_TEMP,
};
use aero_d3d11::{Swizzle, WriteMask};
use aero_dxbc::{test_utils as dxbc_test_utils, FourCC};
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCullMode, AerogpuFillMode, AerogpuPrimitiveTopology, AerogpuShaderStage,
    AerogpuShaderStageEx, AerogpuVertexBufferBinding, AEROGPU_CLEAR_COLOR,
    AEROGPU_RESOURCE_USAGE_RENDER_TARGET, AEROGPU_RESOURCE_USAGE_TEXTURE,
    AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

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

fn opcode_token(opcode: u32, len_dwords: u32) -> u32 {
    opcode | (len_dwords << OPCODE_LEN_SHIFT)
}

fn swizzle_bits(swz: [u8; 4]) -> u32 {
    (swz[0] as u32) | ((swz[1] as u32) << 2) | ((swz[2] as u32) << 4) | ((swz[3] as u32) << 6)
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

fn reg_dst(ty: u32, idx: u32, mask: WriteMask) -> Vec<u32> {
    vec![
        operand_token(ty, 2, OPERAND_SEL_MASK, mask.0 as u32, 1),
        idx,
    ]
}

fn reg_src(ty: u32, indices: &[u32], swizzle: Swizzle) -> Vec<u32> {
    let num_components = match ty {
        OPERAND_TYPE_SAMPLER | OPERAND_TYPE_RESOURCE => 0,
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
    );
    let mut out = Vec::new();
    out.push(token);
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
    ));
    out.extend_from_slice(&values);
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

    let version_token = 0x0001_0040u32; // vs_4_0
    let mov_token = OPCODE_MOV | (5u32 << OPCODE_LEN_SHIFT);
    let ret_token = OPCODE_RET | (1u32 << OPCODE_LEN_SHIFT);

    // Operand encodings are shared across the SM4 fixtures.
    let dst_o0 = 0x0010_f022u32;
    let src_v = 0x001e_4016u32;

    let mut tokens = vec![
        version_token,
        0, // length patched below
        mov_token,
        dst_o0,
        0, // o0 index
        src_v,
        0, // v0 index
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

fn build_ps_passthrough_color_in_v1_dxbc() -> Vec<u8> {
    // ps_4_0:
    //   mov o0, v1
    //   ret
    //
    // This ensures the expanded draw path keeps @location(1) varyings.
    let isgn = build_signature_chunk(&[SigParam {
        semantic_name: "COLOR",
        semantic_index: 0,
        register: 1,
        mask: 0x0f,
    }]);
    let osgn = build_signature_chunk(&[SigParam {
        semantic_name: "SV_Target",
        semantic_index: 0,
        register: 0,
        mask: 0x0f,
    }]);

    let version_token = 0x40u32; // ps_4_0
    let mov_token = OPCODE_MOV | (5u32 << OPCODE_LEN_SHIFT);
    let ret_token = OPCODE_RET | (1u32 << OPCODE_LEN_SHIFT);

    let dst_o0 = 0x0010_f022u32;
    let src_v = 0x001e_4016u32;

    let mut tokens = vec![
        version_token,
        0, // length patched below
        mov_token,
        dst_o0,
        0, // o0 index
        src_v,
        1, // v1 index
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

fn build_gs_point_to_triangle_sample_t0_write_o1() -> Vec<u8> {
    const PRIM_POINT: u32 = 1;
    const TOPO_TRIANGLE_STRIP: u32 = 5;
    const MAX_VERTS: u32 = 3;

    let mut body = vec![
        opcode_token(OPCODE_DCL_GS_INPUT_PRIMITIVE, 2),
        PRIM_POINT,
        opcode_token(OPCODE_DCL_GS_OUTPUT_TOPOLOGY, 2),
        TOPO_TRIANGLE_STRIP,
        opcode_token(OPCODE_DCL_GS_MAX_OUTPUT_VERTEX_COUNT, 2),
        MAX_VERTS,
    ];

    // dcl_output o0.xyzw (position)
    body.push(opcode_token(OPCODE_DCL_OUTPUT, 3));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    // dcl_output o1.xyzw (varying/color)
    body.push(opcode_token(OPCODE_DCL_OUTPUT, 3));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 1, WriteMask::XYZW));

    // dcl_resource_texture2d t0
    let tex_decl = reg_src(OPERAND_TYPE_RESOURCE, &[0], Swizzle::XYZW);
    body.push(opcode_token(
        OPCODE_DCL_RESOURCE,
        1 + tex_decl.len() as u32 + 1, // + dimension token
    ));
    body.extend_from_slice(&tex_decl);
    body.push(2); // Texture2D dimension token

    // dcl_sampler s0
    let samp_decl = reg_src(OPERAND_TYPE_SAMPLER, &[0], Swizzle::XYZW);
    body.push(opcode_token(OPCODE_DCL_SAMPLER, 1 + samp_decl.len() as u32));
    body.extend_from_slice(&samp_decl);

    // sample r0, l(0.5, 0.5, 0, 0), t0, s0
    let uv = imm32_vec4([0.5f32.to_bits(), 0.5f32.to_bits(), 0, 0]);
    body.push(opcode_token(
        OPCODE_SAMPLE,
        1 + 2 + uv.len() as u32 + 2 + 2, // opcode + dst + coord + tex + samp
    ));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    body.extend_from_slice(&uv);
    body.extend_from_slice(&reg_src(OPERAND_TYPE_RESOURCE, &[0], Swizzle::XYZW));
    body.extend_from_slice(&reg_src(OPERAND_TYPE_SAMPLER, &[0], Swizzle::XYZW));

    let emit_vertex = |body: &mut Vec<u32>, x: f32, y: f32| {
        // mov o0, l(x,y,0,1)
        let pos = imm32_vec4([x.to_bits(), y.to_bits(), 0.0f32.to_bits(), 1.0f32.to_bits()]);
        body.push(opcode_token(OPCODE_MOV, 1 + 2 + pos.len() as u32));
        body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
        body.extend_from_slice(&pos);

        // mov o1, r0
        body.push(opcode_token(OPCODE_MOV, 1 + 2 + 2));
        body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 1, WriteMask::XYZW));
        body.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, &[0], Swizzle::XYZW));

        body.push(opcode_token(OPCODE_EMIT, 1));
    };

    // Small centered triangle.
    emit_vertex(&mut body, -0.25, -0.25);
    emit_vertex(&mut body, 0.0, 0.25);
    emit_vertex(&mut body, 0.25, -0.25);

    body.push(opcode_token(OPCODE_CUT, 1));
    body.push(opcode_token(OPCODE_RET, 1));

    let mut tokens = vec![
        0x0002_0040u32, // gs_4_0
        0,              // length patched below
    ];
    tokens.extend_from_slice(&body);
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
fn aerogpu_cmd_geometry_shader_texture_t0_translated_prepass() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::aerogpu_cmd_geometry_shader_texture_t0_translated_prepass"
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
        const SRC0: u32 = 2;
        const SRC1: u32 = 3;
        const RT0: u32 = 4;
        const RT1: u32 = 5;
        const VS: u32 = 6;
        const GS: u32 = 7;
        const PS: u32 = 8;
        const IL: u32 = 9;
        const SAMP0: u32 = 10;

        // Use an odd render target size so NDC (0,0) maps exactly to the center pixel.
        let w = 65u32;
        let h = 65u32;

        let vertex = VertexPos3Color4 {
            pos: [0.0, 0.0, 0.0],
            color: [0.0, 0.0, 0.0, 1.0],
        };
        let vb_bytes = bytemuck::bytes_of(&vertex);

        let mut writer = AerogpuCmdWriter::new();
        writer.create_buffer(
            VB,
            AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
            vb_bytes.len() as u64,
            0,
            0,
        );
        writer.upload_resource(VB, 0, vb_bytes);

        // Source textures: renderable + sampleable.
        let src_usage = AEROGPU_RESOURCE_USAGE_RENDER_TARGET | AEROGPU_RESOURCE_USAGE_TEXTURE;
        for &tex in &[SRC0, SRC1] {
            writer.create_texture2d(
                tex,
                src_usage,
                AerogpuFormat::R8G8B8A8Unorm as u32,
                1,
                1,
                1,
                1,
                0,
                0,
                0,
            );
        }

        // Initialize source texture contents via clears (avoids needing a texture upload path).
        writer.set_render_targets(&[SRC0], 0);
        writer.clear(AEROGPU_CLEAR_COLOR, [0.0, 1.0, 0.0, 1.0], 1.0, 0); // green
        writer.set_render_targets(&[SRC1], 0);
        writer.clear(AEROGPU_CLEAR_COLOR, [0.0, 0.0, 1.0, 1.0], 1.0, 0); // blue

        // Output render targets.
        for &rt in &[RT0, RT1] {
            writer.create_texture2d(
                rt,
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
        }

        writer.create_sampler(
            SAMP0,
            aero_protocol::aerogpu::aerogpu_cmd::AerogpuSamplerFilter::Nearest,
            aero_protocol::aerogpu::aerogpu_cmd::AerogpuSamplerAddressMode::ClampToEdge,
            aero_protocol::aerogpu::aerogpu_cmd::AerogpuSamplerAddressMode::ClampToEdge,
            aero_protocol::aerogpu::aerogpu_cmd::AerogpuSamplerAddressMode::ClampToEdge,
        );

        writer.create_shader_dxbc(VS, AerogpuShaderStage::Vertex, &build_vs_pos_only_dxbc());
        writer.create_shader_dxbc(
            PS,
            AerogpuShaderStage::Pixel,
            &build_ps_passthrough_color_in_v1_dxbc(),
        );
        writer.create_shader_dxbc_ex(
            GS,
            AerogpuShaderStageEx::Geometry,
            &build_gs_point_to_triangle_sample_t0_write_o1(),
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

        // Pass 1: bind green source texture.
        writer.set_texture_ex(AerogpuShaderStageEx::Geometry, 0, SRC0);
        writer.set_samplers_ex(AerogpuShaderStageEx::Geometry, 0, &[SAMP0]);
        writer.set_render_targets(&[RT0], 0);
        writer.clear(AEROGPU_CLEAR_COLOR, [0.0, 0.0, 0.0, 1.0], 1.0, 0);
        writer.draw(1, 1, 0, 0);

        // Pass 2: bind blue source texture.
        writer.set_texture_ex(AerogpuShaderStageEx::Geometry, 0, SRC1);
        writer.set_samplers_ex(AerogpuShaderStageEx::Geometry, 0, &[SAMP0]);
        writer.set_render_targets(&[RT1], 0);
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

        let pixels0 = exec
            .read_texture_rgba8(RT0)
            .await
            .expect("readback should succeed");
        let pixels1 = exec
            .read_texture_rgba8(RT1)
            .await
            .expect("readback should succeed");

        let px = |pixels: &[u8], x: u32, y: u32| -> [u8; 4] {
            let idx = ((y * w + x) * 4) as usize;
            pixels[idx..idx + 4].try_into().unwrap()
        };

        assert_eq!(px(&pixels0, w / 2, h / 2), [0, 255, 0, 255]);
        assert_eq!(px(&pixels1, w / 2, h / 2), [0, 0, 255, 255]);
    });
}
