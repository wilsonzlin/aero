mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_d3d11::sm4::opcode::{
    OPCODE_DCL_GS_MAX_OUTPUT_VERTEX_COUNT, OPCODE_DCL_GS_OUTPUT_TOPOLOGY, OPCODE_LEN_MASK,
    OPCODE_LEN_SHIFT, OPCODE_MASK, OPCODE_MOV, OPCODE_RET,
};
use aero_d3d11::{
    DxbcFile, FourCC, GsInputPrimitive, GsOutputTopology, ShaderStage as Sm4ShaderStage, Sm4Decl,
    Sm4Inst, Sm4Program,
};
use aero_dxbc::{test_utils as dxbc_test_utils, FourCC as DxbcFourCC};
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCullMode, AerogpuFillMode, AerogpuPrimitiveTopology, AerogpuShaderStage,
    AerogpuShaderStageEx, AerogpuVertexBufferBinding, AEROGPU_CLEAR_COLOR,
    AEROGPU_RESOURCE_USAGE_RENDER_TARGET, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

const ILAY_POS3_COLOR: &[u8] = include_bytes!("fixtures/ilay_pos3_color.bin");
const DXBC_GS_POINT_TO_TRIANGLE: &[u8] = include_bytes!("fixtures/gs_point_to_triangle.dxbc");
const VS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/vs_passthrough.dxbc");

fn build_dxbc(chunks: &[(DxbcFourCC, Vec<u8>)]) -> Vec<u8> {
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

fn build_gs_pointlist_maxvertexcount2_emit2_dxbc(pos: [f32; 2]) -> Vec<u8> {
    // Take the checked-in `gs_point_to_triangle.dxbc` fixture and patch:
    // - dcl_outputtopology: triangle strip -> point list
    // - dcl_maxvertexcount: 3 -> 2
    // - overwrite the first two emitted positions so the emitted points land on `pos`
    //
    // This keeps the DXBC token length stable while exercising point-list index output with a
    // small `maxvertexcount` (the original bug case).
    let dxbc = DxbcFile::parse(DXBC_GS_POINT_TO_TRIANGLE).expect("base GS fixture should parse");
    let program = Sm4Program::parse_from_dxbc(&dxbc).expect("base GS fixture should contain SM4");
    assert_eq!(
        program.stage,
        Sm4ShaderStage::Geometry,
        "base fixture should be a geometry shader"
    );

    let mut tokens = program.tokens.clone();
    let mut patched_topology = false;
    let mut patched_max = false;
    let mut patched_positions = 0u32;

    let pos_x_bits = pos[0].to_bits();
    let pos_y_bits = pos[1].to_bits();
    let pos_z_bits = 0.0f32.to_bits();
    let pos_w_bits = 1.0f32.to_bits();

    let mut i = 2usize;
    while i < tokens.len() {
        let opcode_token = tokens[i];
        let opcode = opcode_token & OPCODE_MASK;
        let len = ((opcode_token >> OPCODE_LEN_SHIFT) & OPCODE_LEN_MASK) as usize;
        assert!(len != 0, "invalid opcode length 0 at dword {i}");
        assert!(
            i + len <= tokens.len(),
            "opcode at dword {i} with len={len} overruns token stream (len={})",
            tokens.len()
        );

        match opcode {
            OPCODE_DCL_GS_OUTPUT_TOPOLOGY => {
                // Tokenized output topology values:
                // - point = 1
                // - line_strip = 2
                // - triangle_strip = 3
                assert!(
                    len >= 2,
                    "dcl_outputtopology should include an immediate payload"
                );
                tokens[i + 1] = 1;
                patched_topology = true;
            }
            OPCODE_DCL_GS_MAX_OUTPUT_VERTEX_COUNT => {
                assert!(
                    len >= 2,
                    "dcl_maxvertexcount should include an immediate payload"
                );
                tokens[i + 1] = 2;
                patched_max = true;
            }
            OPCODE_MOV => {
                // Patch the first two `mov o0, l(...)` instructions that set the emitted position.
                // Token layout for this fixture's `mov`:
                //   opcode_token, dst_token, dst_index, imm_token, imm0, imm1, imm2, imm3
                if len == 8 {
                    let dst_token = tokens.get(i + 1).copied().unwrap_or(0);
                    let dst_index = tokens.get(i + 2).copied().unwrap_or(u32::MAX);
                    let src_token = tokens.get(i + 3).copied().unwrap_or(0);
                    let is_output_reg = dst_token == 0x0010_f022;
                    let is_o0 = dst_index == 0;
                    let is_imm_vec4 = src_token == 0x000e_4046;
                    if is_output_reg && is_o0 && is_imm_vec4 && patched_positions < 2 {
                        tokens[i + 4] = pos_x_bits;
                        tokens[i + 5] = pos_y_bits;
                        tokens[i + 6] = pos_z_bits;
                        tokens[i + 7] = pos_w_bits;
                        patched_positions += 1;
                    }
                }
            }
            _ => {}
        }

        i += len;
    }

    assert!(patched_topology, "failed to patch dcl_outputtopology");
    assert!(patched_max, "failed to patch dcl_maxvertexcount");
    assert_eq!(
        patched_positions, 2,
        "failed to patch first two emitted positions"
    );

    tokens[1] = tokens.len() as u32;

    // Sanity-check that the patched token stream decodes as intended.
    let patched_program = Sm4Program {
        stage: program.stage,
        model: program.model,
        tokens: tokens.clone(),
    };
    let module =
        aero_d3d11::sm4::decode_program(&patched_program).expect("patched GS should decode");
    assert!(
        module.decls.iter().any(|d| matches!(
            d,
            Sm4Decl::GsInputPrimitive {
                primitive: GsInputPrimitive::Point(_)
            }
        )),
        "patched GS should still declare point input primitive"
    );
    assert!(
        module.decls.iter().any(|d| matches!(
            d,
            Sm4Decl::GsOutputTopology {
                topology: GsOutputTopology::Point(_)
            }
        )),
        "patched GS should declare point output topology"
    );
    assert!(
        module
            .decls
            .iter()
            .any(|d| matches!(d, Sm4Decl::GsMaxOutputVertexCount { max: 2 })),
        "patched GS should declare maxvertexcount=2"
    );
    let emit_count = module
        .instructions
        .iter()
        .filter(|inst| matches!(inst, Sm4Inst::Emit { .. } | Sm4Inst::EmitThenCut { .. }))
        .count();
    assert!(
        emit_count >= 2,
        "patched GS should contain at least 2 emit-like instructions (got {emit_count})"
    );

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
fn aerogpu_cmd_geometry_shader_point_output_pointlist() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::aerogpu_cmd_geometry_shader_point_output_pointlist"
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
        // The GS compute-prepass path currently binds 5 storage buffers (expanded vertices, indices,
        // indirect args, counters, gs_inputs). Some downlevel adapters (notably wgpu-GL/WebGL2) cap
        // storage buffers per stage at 4, so skip rather than panic on validation errors.
        if exec.device().limits().max_storage_buffers_per_shader_stage < 5 {
            common::skip_or_panic(
                test_name,
                "max_storage_buffers_per_shader_stage < 5 (GS prepass requires 5 storage buffers)",
            );
            return;
        }

        const VB: u32 = 1;
        const RT: u32 = 2;
        const VS: u32 = 3;
        const GS: u32 = 4;
        const PS: u32 = 5;
        const IL: u32 = 6;

        // Select a pixel in the top-right quadrant (outside the placeholder centered triangle) and
        // compute its corresponding clip-space coordinates.
        let w = 64u32;
        let h = 64u32;
        let target_x = w * 7 / 8;
        let target_y = h / 8;
        let x_ndc = ((target_x as f32 + 0.5) / w as f32) * 2.0 - 1.0;
        let y_ndc = 1.0 - ((target_y as f32 + 0.5) / h as f32) * 2.0;

        // Draw a single point; the GS emits 2 points at the same position (`maxvertexcount=2`).
        // Older executor code sized the expanded index buffer assuming triangle output, which would
        // under-allocate for point output and cause the prepass to trip overflow and emit a no-op
        // indirect draw.
        let vertex = VertexPos3Color4 {
            // The patched GS fixture ignores inputs, but the runtime still requires a bound input
            // layout + vertex buffer for GS prepass execution.
            pos: [0.0, 0.0, 0.0],
            color: [0.0, 0.0, 0.0, 1.0],
        };
        let vb_bytes = bytemuck::bytes_of(&vertex);

        let gs_dxbc = build_gs_pointlist_maxvertexcount2_emit2_dxbc([x_ndc, y_ndc]);

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

        writer.create_shader_dxbc(VS, AerogpuShaderStage::Vertex, VS_PASSTHROUGH);
        writer.create_shader_dxbc_ex(GS, AerogpuShaderStageEx::Geometry, &gs_dxbc);
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
        // Disable culling to avoid backend-specific state differences (even though points are not
        // culled, keep this consistent with other GS tests).
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

        // Top-left corner should remain red.
        assert_eq!(px(0, 0), [255, 0, 0, 255]);
        // The selected target pixel should be shaded green by the point.
        assert_eq!(px(target_x, target_y), [0, 255, 0, 255]);
    });
}
