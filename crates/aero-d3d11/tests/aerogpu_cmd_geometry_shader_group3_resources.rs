mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_d3d11::sm4::opcode::*;
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
const DXBC_VS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/vs_passthrough.dxbc");
const DXBC_PS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/ps_passthrough.dxbc");

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
    token |= (component_sel & OPERAND_COMPONENT_SELECTION_MASK) << OPERAND_COMPONENT_SELECTION_SHIFT;
    token |= (index_dim & OPERAND_INDEX_DIMENSION_MASK) << OPERAND_INDEX_DIMENSION_SHIFT;
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

fn cbuffer_src(slot: u32, reg: u32) -> Vec<u32> {
    vec![
        operand_token(
            OPERAND_TYPE_CONSTANT_BUFFER,
            2,
            OPERAND_SEL_SWIZZLE,
            swizzle_bits([0, 1, 2, 3]),
            2,
        ),
        slot,
        reg,
    ]
}

fn imm32_vec4(values: [u32; 4]) -> Vec<u32> {
    let mut out = Vec::with_capacity(1 + 4);
    out.push(operand_token(
        OPERAND_TYPE_IMMEDIATE32,
        2,
        OPERAND_SEL_SWIZZLE,
        swizzle_bits([0, 1, 2, 3]),
        0,
    ));
    out.extend_from_slice(&values);
    out
}

fn build_gs_reads_cb0_and_writes_color_dxbc() -> Vec<u8> {
    // Minimal gs_4_0 shader that:
    // - declares cb0[1]
    // - writes cb0[0] to output register o1 (varying 1 / COLOR0)
    // - emits a centered triangle so the center pixel is shaded.
    //
    // Pseudocode:
    //   dcl_inputprimitive point
    //   dcl_outputtopology triangle_strip
    //   dcl_maxvertexcount 3
    //   dcl_constantbuffer cb0[1]
    //   mov o1, cb0[0]
    //   mov o0, l(-0.5, -0.5, 0, 1); emit
    //   mov o0, l( 0.0,  0.5, 0, 1); emit
    //   mov o0, l( 0.5, -0.5, 0, 1); emit
    //   ret
    const PRIM_POINT: u32 = 1;
    const TOPO_TRIANGLE_STRIP: u32 = 5;

    let mut tokens = vec![0x0002_0040u32 /* gs_4_0 */, 0 /* length patched below */];
    tokens.push(opcode_token(OPCODE_DCL_GS_INPUT_PRIMITIVE, 2));
    tokens.push(PRIM_POINT);
    tokens.push(opcode_token(OPCODE_DCL_GS_OUTPUT_TOPOLOGY, 2));
    tokens.push(TOPO_TRIANGLE_STRIP);
    tokens.push(opcode_token(OPCODE_DCL_GS_MAX_OUTPUT_VERTEX_COUNT, 2));
    tokens.push(3);

    // Declarations for outputs (not strictly required, but keeps the token stream realistic).
    // dcl_output o0.xyzw
    tokens.push(opcode_token(0x100, 3));
    tokens.push(operand_token(OPERAND_TYPE_OUTPUT, 2, OPERAND_SEL_MASK, 0x0f, 1));
    tokens.push(0);
    // dcl_output o1.xyzw
    tokens.push(opcode_token(0x100, 3));
    tokens.push(operand_token(OPERAND_TYPE_OUTPUT, 2, OPERAND_SEL_MASK, 0x0f, 1));
    tokens.push(1);

    // dcl_constantbuffer cb0[1]
    // NOTE: The decoder only keys on OPERAND_TYPE_CONSTANT_BUFFER + indices; the exact declaration
    // opcode value is not important as long as it is in the declaration range (>= 0x100).
    tokens.push(opcode_token(0x101, 4));
    tokens.push(operand_token(
        OPERAND_TYPE_CONSTANT_BUFFER,
        0,
        OPERAND_SEL_MASK,
        0,
        2,
    ));
    tokens.push(0); // slot
    tokens.push(1); // reg_count

    // mov o1.xyzw, cb0[0].xyzw
    tokens.push(opcode_token(OPCODE_MOV, 6));
    tokens.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 1, 0x0f));
    tokens.extend_from_slice(&cbuffer_src(0, 0));

    // mov o0.xyzw, l(-0.5, -0.5, 0, 1)
    // emit
    tokens.push(opcode_token(OPCODE_MOV, 8));
    tokens.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, 0x0f));
    tokens.extend_from_slice(&imm32_vec4([
        (-0.5f32).to_bits(),
        (-0.5f32).to_bits(),
        0.0f32.to_bits(),
        1.0f32.to_bits(),
    ]));
    tokens.push(opcode_token(OPCODE_EMIT, 1));

    // mov o0.xyzw, l(0, 0.5, 0, 1)
    // emit
    tokens.push(opcode_token(OPCODE_MOV, 8));
    tokens.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, 0x0f));
    tokens.extend_from_slice(&imm32_vec4([
        0.0f32.to_bits(),
        0.5f32.to_bits(),
        0.0f32.to_bits(),
        1.0f32.to_bits(),
    ]));
    tokens.push(opcode_token(OPCODE_EMIT, 1));

    // mov o0.xyzw, l(0.5, -0.5, 0, 1)
    // emit
    tokens.push(opcode_token(OPCODE_MOV, 8));
    tokens.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, 0x0f));
    tokens.extend_from_slice(&imm32_vec4([
        0.5f32.to_bits(),
        (-0.5f32).to_bits(),
        0.0f32.to_bits(),
        1.0f32.to_bits(),
    ]));
    tokens.push(opcode_token(OPCODE_EMIT, 1));

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

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Cb0 {
    color: [f32; 4],
}

fn rgba_at(pixels: &[u8], width: usize, x: usize, y: usize) -> &[u8] {
    let idx = (y * width + x) * 4;
    &pixels[idx..idx + 4]
}

#[test]
fn aerogpu_cmd_geometry_shader_group3_constant_buffer_is_visible_to_prepass() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::aerogpu_cmd_geometry_shader_group3_constant_buffer_is_visible_to_prepass"
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

        // One point; the GS emits a centered triangle.
        let vertex = VertexPos3Color4 {
            pos: [0.0, 0.0, 0.0],
            color: [0.0, 0.0, 0.0, 1.0],
        };
        let vb_bytes = bytemuck::bytes_of(&vertex);
        assert_eq!(vb_bytes.len(), 28);

        let cb0 = Cb0 {
            color: [0.0, 1.0, 0.0, 1.0],
        };
        let cb_bytes = bytemuck::bytes_of(&cb0);

        const VB: u32 = 1;
        const CB: u32 = 2;
        const RT: u32 = 3;
        const VS: u32 = 10;
        const GS: u32 = 11;
        const PS: u32 = 12;
        const IL: u32 = 20;

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

        writer.create_buffer(
            CB,
            AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER,
            cb_bytes.len() as u64,
            0,
            0,
        );
        writer.upload_resource(CB, 0, cb_bytes);

        writer.create_texture2d(
            RT,
            AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
            AerogpuFormat::B8G8R8A8Unorm as u32,
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

        writer.create_shader_dxbc(VS, AerogpuShaderStage::Vertex, DXBC_VS_PASSTHROUGH);
        writer.create_shader_dxbc_ex(
            GS,
            AerogpuShaderStageEx::Geometry,
            &build_gs_reads_cb0_and_writes_color_dxbc(),
        );
        writer.create_shader_dxbc(PS, AerogpuShaderStage::Pixel, DXBC_PS_PASSTHROUGH);

        writer.create_input_layout(IL, ILAY_POS3_COLOR);
        writer.set_input_layout(IL);

        writer.set_primitive_topology(AerogpuPrimitiveTopology::PointList);
        writer.set_vertex_buffers(
            0,
            &[AerogpuVertexBufferBinding {
                buffer: VB,
                stride_bytes: 28,
                offset_bytes: 0,
                reserved0: 0,
            }],
        );

        writer.bind_shaders_ex(VS, PS, 0, GS, 0, 0);
        // Disable face culling so the test does not depend on winding.
        writer.set_rasterizer_state_ext(
            AerogpuFillMode::Solid,
            AerogpuCullMode::None,
            false,
            false,
            0,
            false,
        );

        writer.set_constant_buffers_ex(
            AerogpuShaderStageEx::Geometry,
            0,
            &[AerogpuConstantBufferBinding {
                buffer: CB,
                offset_bytes: 0,
                size_bytes: cb_bytes.len() as u32,
                reserved0: 0,
            }],
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

        let presented_rt = report
            .presents
            .last()
            .and_then(|p| p.presented_render_target)
            .expect("expected a present event");

        let pixels = exec.read_texture_rgba8(presented_rt).await.unwrap();
        assert_eq!(pixels.len(), (width * height * 4) as usize);

        let w = width as usize;
        assert_eq!(
            rgba_at(&pixels, w, (width / 2) as usize, (height / 2) as usize),
            &[0, 255, 0, 255],
            "center pixel should match GS cb0[0] color"
        );
        assert_eq!(
            rgba_at(&pixels, w, 0, 0),
            &[255, 0, 0, 255],
            "corner should remain at clear color"
        );
    });
}

