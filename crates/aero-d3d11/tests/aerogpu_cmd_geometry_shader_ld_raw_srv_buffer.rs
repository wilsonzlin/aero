mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_d3d11::sm4::opcode::*;
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
) -> u32 {
    let mut token = 0u32;
    token |= num_components & OPERAND_NUM_COMPONENTS_MASK;
    token |= (selection_mode & OPERAND_SELECTION_MODE_MASK) << OPERAND_SELECTION_MODE_SHIFT;
    token |= (ty & OPERAND_TYPE_MASK) << OPERAND_TYPE_SHIFT;
    token |=
        (component_sel & OPERAND_COMPONENT_SELECTION_MASK) << OPERAND_COMPONENT_SELECTION_SHIFT;
    token |= (index_dim & OPERAND_INDEX_DIMENSION_MASK) << OPERAND_INDEX_DIMENSION_SHIFT;
    // Explicitly set index representations even though IMMEDIATE32 encodes as 0; this keeps the
    // encoding self-documenting and resilient if the opcode constants ever change.
    token |= OPERAND_INDEX_REP_IMMEDIATE32 << OPERAND_INDEX0_REP_SHIFT;
    token |= OPERAND_INDEX_REP_IMMEDIATE32 << OPERAND_INDEX1_REP_SHIFT;
    token |= OPERAND_INDEX_REP_IMMEDIATE32 << OPERAND_INDEX2_REP_SHIFT;
    token
}

fn swizzle_bits(swz: [u8; 4]) -> u32 {
    (swz[0] as u32) | ((swz[1] as u32) << 2) | ((swz[2] as u32) << 4) | ((swz[3] as u32) << 6)
}

fn reg_dst(ty: u32, idx: u32, mask: u32) -> Vec<u32> {
    vec![operand_token(ty, 2, OPERAND_SEL_MASK, mask, 1), idx]
}

fn reg_src(ty: u32, idx: u32) -> Vec<u32> {
    vec![
        operand_token(
            ty,
            2,
            OPERAND_SEL_SWIZZLE,
            swizzle_bits([0, 1, 2, 3]), // XYZW
            1,
        ),
        idx,
    ]
}

fn imm32_scalar_x(v: u32) -> Vec<u32> {
    vec![
        operand_token(
            OPERAND_TYPE_IMMEDIATE32,
            1,
            OPERAND_SEL_SWIZZLE,
            swizzle_bits([0, 0, 0, 0]), // XXXX
            0,
        ),
        v,
    ]
}

fn imm32_vec4(values: [u32; 4]) -> Vec<u32> {
    let mut out = Vec::with_capacity(1 + 4);
    // Use the same immediate-vec4 token encoding as the existing SM4 fixtures/tests.
    out.push(operand_token(
        OPERAND_TYPE_IMMEDIATE32,
        2,
        OPERAND_SEL_MASK,
        0x0f, // xyzw
        0,
    ));
    out.extend_from_slice(&values);
    out
}

fn build_gs_ld_raw_t0_point_to_triangle_dxbc() -> Vec<u8> {
    // Minimal gs_4_0 that:
    // - Declares point input + triangle strip output + maxvertexcount=3.
    // - Declares a raw SRV buffer `t0` (ByteAddressBuffer).
    // - `ld_raw` loads 16 bytes from `t0` at byte offset 0 (u32[4]).
    // - Converts u32 -> f32 via `utof` and writes to output varying `o1`.
    // - Emits a centered triangle using constant `o0` positions.
    //
    // The uploaded buffer pattern is `[0,1,0,1]` so `o1` becomes `(0,1,0,1)` (green).
    const PRIM_POINT: u32 = 1;
    const TOPO_TRIANGLE_STRIP: u32 = 5;
    const MAX_VERTS: u32 = 3;

    let version_token = 0x0002_0040u32; // gs_4_0

    let mut tokens = vec![
        version_token,
        0, // length patched below
        opcode_token(OPCODE_DCL_GS_INPUT_PRIMITIVE, 2),
        PRIM_POINT,
        opcode_token(OPCODE_DCL_GS_OUTPUT_TOPOLOGY, 2),
        TOPO_TRIANGLE_STRIP,
        opcode_token(OPCODE_DCL_GS_MAX_OUTPUT_VERTEX_COUNT, 2),
        MAX_VERTS,
        // dcl_output o0.xyzw
        opcode_token(OPCODE_DCL_OUTPUT, 3),
        operand_token(OPERAND_TYPE_OUTPUT, 2, OPERAND_SEL_MASK, 0x0f, 1),
        0,
        // dcl_output o1.xyzw
        opcode_token(OPCODE_DCL_OUTPUT, 3),
        operand_token(OPERAND_TYPE_OUTPUT, 2, OPERAND_SEL_MASK, 0x0f, 1),
        1,
        // dcl_resource_raw t0
        opcode_token(OPCODE_DCL_RESOURCE_RAW, 3),
        operand_token(
            OPERAND_TYPE_RESOURCE,
            2,
            OPERAND_SEL_SWIZZLE,
            swizzle_bits([0, 1, 2, 3]), // XYZW
            1,
        ),
        0,
    ];

    // ld_raw r0.xyzw, l(0), t0
    let mut inst = Vec::<u32>::new();
    inst.push(0);
    inst.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, 0x0f));
    inst.extend_from_slice(&imm32_scalar_x(0));
    inst.extend_from_slice(&reg_src(OPERAND_TYPE_RESOURCE, 0));
    inst[0] = opcode_token(OPCODE_LD_RAW, inst.len() as u32);
    tokens.extend_from_slice(&inst);

    // utof o1.xyzw, r0.xyzw
    let mut inst = Vec::<u32>::new();
    inst.push(0);
    inst.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 1, 0x0f));
    inst.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 0));
    inst[0] = opcode_token(OPCODE_UTOF, inst.len() as u32);
    tokens.extend_from_slice(&inst);

    let emit_tri_vertex = |tokens: &mut Vec<u32>, x: f32, y: f32, z: f32, w: f32| {
        // mov o0.xyzw, l(x,y,z,w)
        let mut inst = Vec::<u32>::new();
        inst.push(0);
        inst.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, 0x0f));
        inst.extend_from_slice(&imm32_vec4([
            x.to_bits(),
            y.to_bits(),
            z.to_bits(),
            w.to_bits(),
        ]));
        inst[0] = opcode_token(OPCODE_MOV, inst.len() as u32);
        tokens.extend_from_slice(&inst);
        // emit
        tokens.push(opcode_token(OPCODE_EMIT, 1));
    };

    emit_tri_vertex(&mut tokens, -0.5, -0.5, 0.0, 1.0);
    emit_tri_vertex(&mut tokens, 0.0, 0.5, 0.0, 1.0);
    emit_tri_vertex(&mut tokens, 0.5, -0.5, 0.0, 1.0);

    tokens.push(opcode_token(OPCODE_CUT, 1));
    tokens.push(opcode_token(OPCODE_RET, 1));

    tokens[1] = tokens.len() as u32;
    let shdr = tokens_to_bytes(&tokens);
    build_dxbc(&[(FourCC(*b"SHDR"), shdr)])
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct VertexPos3Color4 {
    pos: [f32; 3],
    color: [f32; 4],
}

#[test]
fn aerogpu_cmd_geometry_shader_ld_raw_srv_buffer_translated_prepass() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::aerogpu_cmd_geometry_shader_ld_raw_srv_buffer_translated_prepass"
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

        // The translated GS prepass already binds 4 storage buffers in `@group(0)`
        // (expanded verts/indices/state + gs_inputs). Adding an SRV buffer (t0) via `ld_raw`
        // requires one more storage buffer binding in `@group(3)`.
        let max_storage = exec.device().limits().max_storage_buffers_per_shader_stage;
        if max_storage < 5 {
            common::skip_or_panic(
                test_name,
                &format!(
                    "requires >=5 storage buffers per shader stage for GS prepass + SRV buffer (got {max_storage})"
                ),
            );
            return;
        }

        const VB: u32 = 1;
        const SRV: u32 = 2;
        const RT: u32 = 3;
        const VS: u32 = 4;
        const GS: u32 = 5;
        const PS: u32 = 6;
        const IL: u32 = 7;

        let vertex = VertexPos3Color4 {
            // Place the input point near the top-right so the non-GS path would not cover the
            // center pixel.
            pos: [0.75, 0.75, 0.0],
            // Non-green color to avoid false positives if GS prepass is bypassed.
            color: [1.0, 0.0, 0.0, 1.0],
        };
        let vb_bytes = bytemuck::bytes_of(&vertex);

        // Raw SRV contents: 16 bytes => u32[4].
        let srv_words: [u32; 4] = [0, 1, 0, 1];
        let srv_bytes: &[u8] = bytemuck::cast_slice(&srv_words);

        let w = 64u32;
        let h = 64u32;

        let gs_dxbc = build_gs_ld_raw_t0_point_to_triangle_dxbc();

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
        writer.set_render_targets(&[RT], 0);
        writer.set_viewport(0.0, 0.0, w as f32, h as f32, 0.0, 1.0);

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

        // Bind the SRV buffer at GS-stage slot t0.
        writer.set_shader_resource_buffers(
            AerogpuShaderStage::Geometry,
            0,
            &[AerogpuShaderResourceBufferBinding {
                buffer: SRV,
                offset_bytes: 0,
                size_bytes: 0, // whole buffer
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

        let px = |x: u32, y: u32| -> [u8; 4] {
            let idx = ((y * w + x) * 4) as usize;
            pixels[idx..idx + 4].try_into().unwrap()
        };

        // The triangle is centered and does not cover the top-left corner.
        assert_eq!(px(0, 0), [255, 0, 0, 255]);
        // Center pixel should be the `ld_raw` buffer contents: [0,1,0,1] => green.
        assert_eq!(px(w / 2, h / 2), [0, 255, 0, 255]);
    });
}
