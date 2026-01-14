mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_d3d11::sm4::opcode::{
    OPCODE_DCL_GS_INPUT_PRIMITIVE, OPCODE_DCL_GS_MAX_OUTPUT_VERTEX_COUNT, OPCODE_DCL_OUTPUT,
    OPCODE_DCL_GS_OUTPUT_TOPOLOGY, OPCODE_EMIT, OPCODE_LEN_SHIFT, OPCODE_MOV, OPCODE_RET,
};
use aero_dxbc::{test_utils as dxbc_test_utils, FourCC};
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCullMode, AerogpuFillMode, AerogpuPrimitiveTopology, AerogpuShaderStage,
    AerogpuShaderStageEx, AerogpuVertexBufferBinding, AEROGPU_CLEAR_COLOR,
    AEROGPU_RESOURCE_USAGE_RENDER_TARGET, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

const ILAY_POS3_COLOR: &[u8] = include_bytes!("fixtures/ilay_pos3_color.bin");
const DXBC_VS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/vs_passthrough.dxbc");

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

fn build_gs_output_points_dxbc(max_vertex_count: u32, points: &[[f32; 4]]) -> Vec<u8> {
    // Minimal gs_4_0 token stream with point-list output:
    // - dcl_inputprimitive point
    // - dcl_outputtopology pointlist
    // - dcl_maxvertexcount {max_vertex_count}
    // - emit N points
    //
    // Tokenized-program header version token:
    // - stage: Geometry (2)
    // - model: 4.0
    let version_token = 0x0002_0040u32; // gs_4_0
    let mut tokens = vec![version_token, 0];

    // GS declarations required by the translator.
    tokens.push(opcode_token(OPCODE_DCL_GS_INPUT_PRIMITIVE, 2)); // dcl_inputprimitive
    tokens.push(1); // point
    tokens.push(opcode_token(OPCODE_DCL_GS_OUTPUT_TOPOLOGY, 2)); // dcl_outputtopology
    tokens.push(1); // pointlist
    tokens.push(opcode_token(OPCODE_DCL_GS_MAX_OUTPUT_VERTEX_COUNT, 2)); // dcl_maxvertexcount
    tokens.push(max_vertex_count);

    // dcl_output o0.xyzw
    tokens.push(opcode_token(OPCODE_DCL_OUTPUT, 3));
    tokens.push(0x0010_F022);
    tokens.push(0);
    // dcl_output o1.xyzw
    tokens.push(opcode_token(OPCODE_DCL_OUTPUT, 3));
    tokens.push(0x0010_F022);
    tokens.push(1);

    fn mov_o0_imm(tokens: &mut Vec<u32>, x: f32, y: f32, z: f32, w: f32) {
        tokens.push(opcode_token(OPCODE_MOV, 8));
        tokens.push(0x0010_F022); // o0.xyzw
        tokens.push(0);
        tokens.push(0x0000_F042); // immediate vec4
        tokens.push(x.to_bits());
        tokens.push(y.to_bits());
        tokens.push(z.to_bits());
        tokens.push(w.to_bits());
    }

    for &[x, y, z, w] in points {
        mov_o0_imm(&mut tokens, x, y, z, w);
        tokens.push(opcode_token(OPCODE_EMIT, 1));
    }

    tokens.push(opcode_token(OPCODE_RET, 1));

    tokens[1] = tokens.len() as u32;

    let shdr = tokens_to_bytes(&tokens);
    build_dxbc(&[(FourCC(*b"SHDR"), shdr)])
}

fn build_pointlist_cmd_stream(gs_dxbc: &[u8], w: u32, h: u32) -> Vec<u8> {
    const VB: u32 = 1;
    const RT: u32 = 2;
    const VS: u32 = 3;
    const GS: u32 = 4;
    const PS: u32 = 5;
    const IL: u32 = 6;

    // Single input point. The GS output is driven entirely by immediates.
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

    writer.create_shader_dxbc(VS, AerogpuShaderStage::Vertex, DXBC_VS_PASSTHROUGH);
    writer.create_shader_dxbc_ex(GS, AerogpuShaderStageEx::Geometry, gs_dxbc);
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
    // Disable face culling so misrendering as TriangleList doesn't get culled away by winding.
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
    writer.finish()
}

fn build_ps_solid_green_dxbc() -> Vec<u8> {
    // ps_4_0: mov o0, l(0,1,0,1); ret
    let isgn = dxbc_test_utils::build_signature_chunk_v0(&[]);
    let osgn = dxbc_test_utils::build_signature_chunk_v0(&[dxbc_test_utils::SignatureEntryDesc {
        semantic_name: "SV_Target",
        semantic_index: 0,
        system_value_type: 0,
        component_type: 0,
        register: 0,
        mask: 0x0f,
        read_write_mask: 0x0f,
        stream: 0,
        min_precision: 0,
    }]);

    let version_token = 0x40u32; // ps_4_0

    let mov_token = OPCODE_MOV | (8u32 << OPCODE_LEN_SHIFT);
    let ret_token = OPCODE_RET | (1u32 << OPCODE_LEN_SHIFT);

    let dst_o0 = 0x0010_F022u32;
    let imm_vec4 = 0x0000_F042u32;

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
fn aerogpu_cmd_geometry_shader_output_topology_pointlist_renders_points_not_triangles() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::aerogpu_cmd_geometry_shader_output_topology_pointlist_renders_points_not_triangles"
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
        // The translator-backed GS prepass uses 4 storage buffers in the compute shader bind group
        // (expanded vertices, expanded indices, indirect args+counters, gs_inputs). Some
        // downlevel/embedded backends only support fewer.
        if exec.device().limits().max_storage_buffers_per_shader_stage < 4 {
            common::skip_or_panic(
                test_name,
                "backend limit max_storage_buffers_per_shader_stage < 4 (GS translate prepass requires 4 storage buffers)",
            );
            return;
        }

        // Use an odd render target size so NDC (0,0) maps exactly to the center pixel.
        let w = 65u32;
        let h = 65u32;
        let gs_dxbc = build_gs_output_points_dxbc(
            4,
            &[
                // 3 points that would form a full-screen triangle if misrendered as TriangleList.
                [-1.0, -1.0, 0.0, 1.0],
                [-1.0, 3.0, 0.0, 1.0],
                [3.0, -1.0, 0.0, 1.0],
                // One point at the exact screen center.
                [0.0, 0.0, 0.0, 1.0],
            ],
        );
        let stream = build_pointlist_cmd_stream(&gs_dxbc, w, h);

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
        assert_eq!(render_target, 2);

        let pixels = exec
            .read_texture_rgba8(render_target)
            .await
            .expect("readback should succeed");
        assert_eq!(pixels.len(), (w * h * 4) as usize);

        let px = |x: u32, y: u32| -> [u8; 4] {
            let idx = ((y * w + x) * 4) as usize;
            pixels[idx..idx + 4].try_into().unwrap()
        };

        // The GS emits a point exactly at the center.
        assert_eq!(px(w / 2, h / 2), [0, 255, 0, 255]);

        // Pick a pixel well away from the center point, but comfortably inside the large triangle
        // formed by the first three emitted points. If the executor incorrectly renders the
        // expanded draw as TriangleList, this pixel will be shaded green.
        assert_eq!(px(w / 4, h / 4), [255, 0, 0, 255]);
    });
}

#[test]
fn aerogpu_cmd_geometry_shader_output_topology_pointlist_maxvertexcount2_renders_points() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::aerogpu_cmd_geometry_shader_output_topology_pointlist_maxvertexcount2_renders_points"
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
        if exec.device().limits().max_storage_buffers_per_shader_stage < 4 {
            common::skip_or_panic(
                test_name,
                "backend limit max_storage_buffers_per_shader_stage < 4 (GS translate prepass requires 4 storage buffers)",
            );
            return;
        }

        let w = 65u32;
        let h = 65u32;
        let gs_dxbc = build_gs_output_points_dxbc(2, &[[0.0, 0.0, 0.0, 1.0], [3.0, 3.0, 0.0, 1.0]]);
        let stream = build_pointlist_cmd_stream(&gs_dxbc, w, h);

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
        assert_eq!(render_target, 2);

        let pixels = exec
            .read_texture_rgba8(render_target)
            .await
            .expect("readback should succeed");
        assert_eq!(pixels.len(), (w * h * 4) as usize);

        let idx = (((h / 2) * w + (w / 2)) * 4) as usize;
        let center: [u8; 4] = pixels[idx..idx + 4].try_into().unwrap();
        assert_eq!(
            center,
            [0, 255, 0, 255],
            "expected center point to render with maxvertexcount=2"
        );
    });
}
