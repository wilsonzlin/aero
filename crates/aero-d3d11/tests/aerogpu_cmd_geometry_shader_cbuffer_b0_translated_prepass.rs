mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_d3d11::sm4::opcode::{
    OPCODE_ADD, OPCODE_CUT, OPCODE_DCL_GS_INPUT_PRIMITIVE, OPCODE_DCL_GS_MAX_OUTPUT_VERTEX_COUNT,
    OPCODE_DCL_GS_OUTPUT_TOPOLOGY, OPCODE_EMIT, OPCODE_LEN_SHIFT, OPCODE_RET,
};
use aero_dxbc::{test_utils as dxbc_test_utils, FourCC};
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuConstantBufferBinding, AerogpuCullMode, AerogpuFillMode, AerogpuPrimitiveTopology,
    AerogpuShaderStage, AerogpuShaderStageEx, AerogpuVertexBufferBinding, AEROGPU_CLEAR_COLOR,
    AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER, AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
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

fn opcode_token(opcode: u32, len: u32) -> u32 {
    opcode | (len << OPCODE_LEN_SHIFT)
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
    let mov_token = 0x01u32 | (5u32 << 11);
    let dst_o0 = 0x0010_f022u32;
    let src_v0 = 0x001e_4016u32;
    let ret_token = 0x3eu32 | (1u32 << 11);

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

fn build_ps_solid_green_dxbc() -> Vec<u8> {
    // ps_4_0: mov o0, l(0,1,0,1); ret
    let isgn = build_signature_chunk(&[]);
    let osgn = build_signature_chunk(&[SigParam {
        semantic_name: "SV_Target",
        semantic_index: 0,
        register: 0,
        mask: 0x0f,
    }]);

    let version_token = 0x40u32; // ps_4_0
    let mov_token = 0x01u32 | (8u32 << 11);
    let ret_token = 0x3eu32 | (1u32 << 11);

    let dst_o0 = 0x0010_f022u32;
    let imm_vec4 = 0x0000_f042u32;

    let zero = 0.0f32.to_bits();
    let one = 1.0f32.to_bits();

    let mut tokens = vec![
        version_token,
        0, // length patched below
        mov_token,
        dst_o0,
        0, // o0 index
        imm_vec4,
        zero,
        one,
        zero,
        one,
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

fn build_gs_point_to_triangle_offset_by_cb0_xy() -> Vec<u8> {
    // Minimal gs_4_0 that:
    // - Declares point input + triangle strip output + maxvertexcount=3.
    // - Declares + uses cb0[0] to offset output positions.
    // - Emits a centered triangle when cb0.xy=(0,0), shifting it out of view when cb0.x=2.

    const PRIM_POINT: u32 = 1;
    const TOPO_TRIANGLE_STRIP: u32 = 5;
    const MAX_VERTS: u32 = 3;

    // Any opcode >= 0x100 is treated as a declaration by the SM4 decoder; we use 0x100 here for
    // `dcl_constantbuffer cb0[1]` since the executor/translator only cares about the decoded decl.
    const OPCODE_DCL_GENERIC: u32 = 0x100;

    // Output register operand (o0.xyzw).
    let dst_o0 = 0x0010_f022u32;
    // Immediate vec4 operand token.
    let imm_vec4 = 0x0000_f042u32;
    // Constant-buffer operand token (cb#[reg], swizzle XYZW, 2D immediate indices).
    let cb_operand = 0x002e_4086u32;

    let add_len = 11u32;
    let add_token = opcode_token(OPCODE_ADD, add_len);

    let mut tokens = vec![
        0x0002_0040u32, // gs_4_0
        0,              // length patched below
        opcode_token(OPCODE_DCL_GS_INPUT_PRIMITIVE, 2),
        PRIM_POINT,
        opcode_token(OPCODE_DCL_GS_OUTPUT_TOPOLOGY, 2),
        TOPO_TRIANGLE_STRIP,
        opcode_token(OPCODE_DCL_GS_MAX_OUTPUT_VERTEX_COUNT, 2),
        MAX_VERTS,
        // dcl_constantbuffer cb0[1], immediateIndexed
        opcode_token(OPCODE_DCL_GENERIC, 5),
        cb_operand,
        0, // slot
        1, // reg_count
        0, // access pattern (ignored by the decoder/translator)
    ];

    let emit_token = opcode_token(OPCODE_EMIT, 1);
    let cut_token = opcode_token(OPCODE_CUT, 1);
    let ret_token = opcode_token(OPCODE_RET, 1);

    let emit_tri_vertex = |tokens: &mut Vec<u32>, x: f32, y: f32, z: f32, w: f32| {
        tokens.extend_from_slice(&[
            // add o0, l(x,y,z,w), cb0[0]
            add_token,
            dst_o0,
            0, // o0 index
            imm_vec4,
            x.to_bits(),
            y.to_bits(),
            z.to_bits(),
            w.to_bits(),
            cb_operand,
            0, // cb slot
            0, // cb reg
            // emit
            emit_token,
        ]);
    };

    // Small centered clockwise triangle.
    //
    // This is intentionally smaller than the executor's placeholder prepass triangle so this test
    // can detect if we accidentally fell back to the placeholder prepass (which also reads GS cb0).
    emit_tri_vertex(&mut tokens, -0.25, -0.25, 0.0, 1.0);
    emit_tri_vertex(&mut tokens, 0.0, 0.25, 0.0, 1.0);
    emit_tri_vertex(&mut tokens, 0.25, -0.25, 0.0, 1.0);

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

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct GsCb0 {
    offset: [f32; 4],
}

#[test]
fn aerogpu_cmd_geometry_shader_cbuffer_b0_translated_prepass() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::aerogpu_cmd_geometry_shader_cbuffer_b0_translated_prepass"
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
        const CB0: u32 = 2;
        const CB1: u32 = 3;
        const RT0: u32 = 4;
        const RT1: u32 = 5;
        const VS: u32 = 6;
        const GS: u32 = 7;
        const PS: u32 = 8;
        const IL: u32 = 9;

        let vertex = VertexPos3Color4 {
            pos: [0.0, 0.0, 0.0],
            color: [0.0, 0.0, 0.0, 1.0],
        };
        let vb_bytes = bytemuck::bytes_of(&vertex);

        let cb0_bytes = bytemuck::bytes_of(&GsCb0 {
            offset: [0.0, 0.0, 0.0, 0.0],
        });
        let cb1_bytes = bytemuck::bytes_of(&GsCb0 {
            // Shift the triangle far enough in clip space to no longer cover the center.
            offset: [2.0, 0.0, 0.0, 0.0],
        });

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
            CB0,
            AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER,
            cb0_bytes.len() as u64,
            0,
            0,
        );
        writer.upload_resource(CB0, 0, cb0_bytes);
        writer.create_buffer(
            CB1,
            AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER,
            cb1_bytes.len() as u64,
            0,
            0,
        );
        writer.upload_resource(CB1, 0, cb1_bytes);

        writer.create_texture2d(
            RT0,
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
            RT1,
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

        writer.create_shader_dxbc(VS, AerogpuShaderStage::Vertex, &build_vs_pos_only_dxbc());
        writer.create_shader_dxbc(PS, AerogpuShaderStage::Pixel, &build_ps_solid_green_dxbc());
        writer.create_shader_dxbc_ex(
            GS,
            AerogpuShaderStageEx::Geometry,
            &build_gs_point_to_triangle_offset_by_cb0_xy(),
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

        // Pass 1: offset = (0,0). Triangle should cover the center pixel.
        writer.set_constant_buffers_ex(
            AerogpuShaderStageEx::Geometry,
            0,
            &[AerogpuConstantBufferBinding {
                buffer: CB0,
                offset_bytes: 0,
                size_bytes: cb0_bytes.len() as u32,
                reserved0: 0,
            }],
        );
        writer.set_render_targets(&[RT0], 0);
        writer.clear(AEROGPU_CLEAR_COLOR, [1.0, 0.0, 0.0, 1.0], 1.0, 0);
        writer.draw(1, 1, 0, 0);

        // Pass 2: offset = (+2,0). Triangle shifts right, leaving center pixel uncovered.
        writer.set_constant_buffers_ex(
            AerogpuShaderStageEx::Geometry,
            0,
            &[AerogpuConstantBufferBinding {
                buffer: CB1,
                offset_bytes: 0,
                size_bytes: cb1_bytes.len() as u32,
                reserved0: 0,
            }],
        );
        writer.set_render_targets(&[RT1], 0);
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

        // With offset (0,0), the triangle should cover the center pixel.
        assert_eq!(px(&pixels0, w / 2, h / 2), [0, 255, 0, 255]);
        // The translated GS emits a small triangle, so a pixel above the center should remain the
        // clear color. The placeholder prepass triangle would cover this pixel.
        assert_eq!(px(&pixels0, w / 2, h / 2 - 10), [255, 0, 0, 255]);
        // With offset (+2,0), the triangle shifts away, leaving the center as the clear color.
        assert_eq!(px(&pixels1, w / 2, h / 2), [255, 0, 0, 255]);
    });
}
