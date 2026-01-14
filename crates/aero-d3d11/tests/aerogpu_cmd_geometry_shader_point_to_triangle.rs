mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_d3d11::sm4::opcode::{
    OPCODE_CUT, OPCODE_EMIT, OPCODE_LEN_MASK, OPCODE_LEN_SHIFT, OPCODE_MASK, OPCODE_MOV, OPCODE_RET,
};
use aero_d3d11::{
    DxbcFile, GsInputPrimitive, GsOutputTopology, ShaderStage as Sm4ShaderStage, Sm4Decl, Sm4Inst,
    Sm4Program,
};
use aero_dxbc::{test_utils as dxbc_test_utils, FourCC};
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCullMode, AerogpuFillMode, AerogpuIndexFormat, AerogpuPrimitiveTopology,
    AerogpuShaderStage, AerogpuShaderStageEx, AerogpuVertexBufferBinding, AEROGPU_CLEAR_COLOR,
    AEROGPU_RESOURCE_USAGE_INDEX_BUFFER, AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
    AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

const ILAY_POS3_COLOR: &[u8] = include_bytes!("fixtures/ilay_pos3_color.bin");
const DXBC_GS_POINT_TO_TRIANGLE: &[u8] = include_bytes!("fixtures/gs_point_to_triangle.dxbc");

fn build_dxbc(chunks: &[(FourCC, Vec<u8>)]) -> Vec<u8> {
    dxbc_test_utils::build_container_owned(chunks)
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

fn tokens_to_bytes(tokens: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(tokens.len() * 4);
    for &t in tokens {
        out.extend_from_slice(&t.to_le_bytes());
    }
    out
}

fn assert_gs_dxbc_decodes_as_geometry_and_has_emit(dxbc_bytes: &[u8]) {
    let dxbc = DxbcFile::parse(dxbc_bytes).expect("GS DXBC should parse");
    let program = Sm4Program::parse_from_dxbc(&dxbc).expect("GS DXBC should contain SM4 program");
    assert_eq!(
        program.stage,
        Sm4ShaderStage::Geometry,
        "GS DXBC should decode as a geometry shader"
    );

    // Ensure the token-stream declared length matches the actual chunk payload length. This catches
    // drift where the hand-authored DXBC is edited but the `len` token isn't updated.
    let shdr = dxbc
        .get_chunk(FourCC(*b"SHDR"))
        .expect("GS DXBC should contain SHDR chunk");
    assert_eq!(
        shdr.data.len() / 4,
        program.tokens.len(),
        "GS SHDR chunk payload length (dwords) must match declared token length"
    );

    // Sanity-check that the raw token stream uses the canonical opcode IDs, to catch numeric drift
    // early even if higher-level decoding logic changes.
    let mut saw_emit_opcode = false;
    let mut saw_cut_opcode = false;
    let toks = &program.tokens;
    let mut i = 2usize;
    while i < toks.len() {
        let opcode_token = toks[i];
        let opcode = opcode_token & OPCODE_MASK;
        let len = ((opcode_token >> OPCODE_LEN_SHIFT) & OPCODE_LEN_MASK) as usize;
        assert!(len != 0, "invalid opcode length 0 at dword {i}");
        assert!(
            i + len <= toks.len(),
            "opcode at dword {i} with len={len} overruns token stream (len={})",
            toks.len()
        );
        saw_emit_opcode |= opcode == OPCODE_EMIT;
        saw_cut_opcode |= opcode == OPCODE_CUT;
        i += len;
    }
    assert!(
        saw_emit_opcode,
        "GS DXBC token stream should use OPCODE_EMIT (0x{OPCODE_EMIT:x})"
    );
    assert!(
        saw_cut_opcode,
        "GS DXBC token stream should use OPCODE_CUT (0x{OPCODE_CUT:x})"
    );

    let module = aero_d3d11::sm4::decode_program(&program).expect("GS SM4 module should decode");
    assert_eq!(module.stage, Sm4ShaderStage::Geometry);
    // The GS prepass translator requires these declarations to determine the input primitive,
    // output topology, and max output vertices. Validate them explicitly so fixture drift is caught
    // even on backends that skip the full runtime test.
    assert!(
        module.decls.iter().any(|d| matches!(
            d,
            Sm4Decl::GsInputPrimitive {
                primitive: GsInputPrimitive::Point(_)
            }
        )),
        "GS DXBC should declare point input primitive via dcl_inputprimitive, got decls={:?}",
        module.decls
    );
    assert!(
        module
            .decls
            .iter()
            .any(|d| matches!(
                d,
                Sm4Decl::GsOutputTopology {
                    topology: GsOutputTopology::TriangleStrip(_)
                }
            )),
        "GS DXBC should declare triangle strip output topology via dcl_outputtopology, got decls={:?}",
        module.decls
    );
    assert!(
        module
            .decls
            .iter()
            .any(|d| matches!(d, Sm4Decl::GsMaxOutputVertexCount { max: 3 })),
        "GS DXBC should declare max output vertices via dcl_maxvertexcount (max=3), got decls={:?}",
        module.decls
    );
    let has_emit = module
        .instructions
        .iter()
        .any(|inst| matches!(inst, Sm4Inst::Emit { .. } | Sm4Inst::EmitThenCut { .. }));
    let has_cut = module
        .instructions
        .iter()
        .any(|inst| matches!(inst, Sm4Inst::Cut { .. } | Sm4Inst::EmitThenCut { .. }));
    assert!(
        has_emit,
        "GS DXBC should contain an Emit-like instruction (Emit/EmitThenCut) (module.instructions={:?})",
        module.instructions
    );
    assert!(
        has_cut,
        "GS DXBC should contain a Cut-like instruction (Cut/EmitThenCut) (module.instructions={:?})",
        module.instructions
    );
}

fn build_vs_pos_only_dxbc() -> Vec<u8> {
    // vs_4_0: mov o0, v0; ret
    let isgn = build_signature_chunk(&[
        SigParam {
            semantic_name: "POSITION",
            semantic_index: 0,
            register: 0,
            mask: 0x07,
        },
        // Include an unused COLOR0 input so this shader can be paired with the existing
        // POS3+COLOR input-layout fixture.
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
    let mov_token = OPCODE_MOV | (8u32 << OPCODE_LEN_SHIFT);
    let ret_token = OPCODE_RET | (1u32 << OPCODE_LEN_SHIFT);

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

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct VertexPos3Color4 {
    pos: [f32; 3],
    color: [f32; 4],
}

#[test]
fn aerogpu_cmd_geometry_shader_point_list_expands_to_triangle() {
    assert_gs_dxbc_decodes_as_geometry_and_has_emit(DXBC_GS_POINT_TO_TRIANGLE);

    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::aerogpu_cmd_geometry_shader_point_list_expands_to_triangle"
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

        // Draw a single point near the top-right. Without GS emulation, the point does not cover
        // the center pixel. With GS emulation, the GS emits a centered triangle and turns the
        // center pixel green.
        let vertex = VertexPos3Color4 {
            pos: [0.75, 0.75, 0.0],
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

        let w = 64u32;
        let h = 64u32;
        writer.create_texture2d(
            RT,
            AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
            AerogpuFormat::B8G8R8A8Unorm as u32,
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
        // Create a GS via the `stage_ex` ABI extension (CREATE_SHADER_DXBC.reserved0).
        writer.create_shader_dxbc_ex(
            GS,
            AerogpuShaderStageEx::Geometry,
            DXBC_GS_POINT_TO_TRIANGLE,
        );
        writer.create_shader_dxbc(PS, AerogpuShaderStage::Pixel, &build_ps_solid_green_dxbc());

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
        // Disable face culling so the test does not depend on backend-specific winding conventions.
        writer.set_rasterizer_state_ext(
            AerogpuFillMode::Solid,
            AerogpuCullMode::None,
            false,
            false,
            0,
            false,
        );

        writer.clear(AEROGPU_CLEAR_COLOR, [1.0, 0.0, 0.0, 1.0], 1.0, 0);
        writer.draw(1, 1, 0, 0);
        writer.present(0, 0);
        let stream = writer.finish();

        let mut guest_mem = VecGuestMemory::new(0);
        let report = match exec.execute_cmd_stream(&stream, None, &mut guest_mem) {
            Ok(report) => report,
            Err(err) => {
                if common::skip_if_compute_or_indirect_unsupported(test_name, &err) {
                    return;
                }
                panic!("execute_cmd_stream failed: {err:#}");
            }
        };
        exec.poll_wait();

        let render_target = report
            .presents
            .last()
            .and_then(|p| p.presented_render_target)
            .expect("stream should present a render target");
        assert_eq!(render_target, RT);

        let pixels = exec
            .read_texture_rgba8(render_target)
            .await
            .expect("readback should succeed");
        assert_eq!(pixels.len(), (w * h * 4) as usize);

        let px = |x: u32, y: u32| -> [u8; 4] {
            let idx = ((y * w + x) * 4) as usize;
            pixels[idx..idx + 4].try_into().unwrap()
        };

        // The triangle is centered and does not cover the top-left corner.
        assert_eq!(px(0, 0), [255, 0, 0, 255]);
        // The center pixel should be covered by the triangle and shaded green.
        assert_eq!(px(w / 2, h / 2), [0, 255, 0, 255]);
    });
}

#[test]
fn aerogpu_cmd_geometry_shader_point_list_draw_indexed_expands_to_triangle() {
    assert_gs_dxbc_decodes_as_geometry_and_has_emit(DXBC_GS_POINT_TO_TRIANGLE);

    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::aerogpu_cmd_geometry_shader_point_list_draw_indexed_expands_to_triangle"
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
        const IB: u32 = 2;
        const RT: u32 = 3;
        const VS: u32 = 4;
        const GS: u32 = 5;
        const PS: u32 = 6;
        const IL: u32 = 7;

        // Draw a single point near the top-right. Without GS emulation, the point does not cover
        // the center pixel. With GS emulation, the GS emits a centered triangle and turns the
        // center pixel green.
        let vertex = VertexPos3Color4 {
            pos: [0.75, 0.75, 0.0],
            color: [0.0, 0.0, 0.0, 1.0],
        };
        let vb_bytes = bytemuck::bytes_of(&vertex);
        // `CREATE_BUFFER` sizes must be 4-byte aligned in the command writer; use a single u32
        // index for this test.
        let index_bytes = 0u32.to_le_bytes();

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
            IB,
            AEROGPU_RESOURCE_USAGE_INDEX_BUFFER,
            index_bytes.len() as u64,
            0,
            0,
        );
        writer.upload_resource(IB, 0, &index_bytes);

        let w = 64u32;
        let h = 64u32;
        writer.create_texture2d(
            RT,
            AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
            AerogpuFormat::B8G8R8A8Unorm as u32,
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
        // Create a GS via the `stage_ex` ABI extension (CREATE_SHADER_DXBC.reserved0).
        writer.create_shader_dxbc_ex(
            GS,
            AerogpuShaderStageEx::Geometry,
            DXBC_GS_POINT_TO_TRIANGLE,
        );
        writer.create_shader_dxbc(PS, AerogpuShaderStage::Pixel, &build_ps_solid_green_dxbc());

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
        writer.set_index_buffer(IB, AerogpuIndexFormat::Uint32, 0);
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

        writer.clear(AEROGPU_CLEAR_COLOR, [1.0, 0.0, 0.0, 1.0], 1.0, 0);
        writer.draw_indexed(1, 1, 0, 0, 0);
        writer.present(0, 0);
        let stream = writer.finish();

        let mut guest_mem = VecGuestMemory::new(0);
        let report = match exec.execute_cmd_stream(&stream, None, &mut guest_mem) {
            Ok(report) => report,
            Err(err) => {
                if common::skip_if_compute_or_indirect_unsupported(test_name, &err) {
                    return;
                }
                panic!("execute_cmd_stream failed: {err:#}");
            }
        };
        exec.poll_wait();

        let render_target = report
            .presents
            .last()
            .and_then(|p| p.presented_render_target)
            .expect("stream should present a render target");
        assert_eq!(render_target, RT);

        let pixels = exec
            .read_texture_rgba8(render_target)
            .await
            .expect("readback should succeed");
        assert_eq!(pixels.len(), (w * h * 4) as usize);

        let px = |x: u32, y: u32| -> [u8; 4] {
            let idx = ((y * w + x) * 4) as usize;
            pixels[idx..idx + 4].try_into().unwrap()
        };

        // The triangle is centered and does not cover the top-left corner.
        assert_eq!(px(0, 0), [255, 0, 0, 255]);
        // The center pixel should be covered by the triangle and shaded green.
        assert_eq!(px(w / 2, h / 2), [0, 255, 0, 255]);
    });
}
