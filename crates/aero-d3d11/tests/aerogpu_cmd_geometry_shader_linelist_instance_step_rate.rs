mod common;

use aero_d3d11::input_layout::{
    fnv1a_32, AEROGPU_INPUT_LAYOUT_BLOB_MAGIC, AEROGPU_INPUT_LAYOUT_BLOB_VERSION,
};
use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_d3d11::sm4::opcode as sm4_opcode;
use aero_dxbc::{test_utils as dxbc_test_utils, FourCC};
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCullMode, AerogpuFillMode, AerogpuPrimitiveTopology, AerogpuShaderStage,
    AerogpuShaderStageEx, AerogpuVertexBufferBinding, AEROGPU_CLEAR_COLOR,
    AEROGPU_RESOURCE_USAGE_RENDER_TARGET, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

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
    opcode | (len_dwords << sm4_opcode::OPCODE_LEN_SHIFT)
}

fn build_gs_linelist_to_triangle_color_dxbc() -> Vec<u8> {
    // gs_4_0:
    //   dcl_inputprimitive line
    //   dcl_outputtopology triangle_strip
    //   dcl_maxvertexcount 3
    //   mov o0, v0[0]; mov o1, v1[0]; emit
    //   mov o0, v0[1]; mov o1, v1[1]; emit
    //   mov o0, l(0,0.5,0,1); mov o1, v1[0]; emit
    //   ret
    let version_token = 0x0002_0040u32; // gs_4_0
    let mut tokens = vec![version_token, 0];

    tokens.push(opcode_token(sm4_opcode::OPCODE_DCL_GS_INPUT_PRIMITIVE, 2));
    tokens.push(2); // line
    tokens.push(opcode_token(sm4_opcode::OPCODE_DCL_GS_OUTPUT_TOPOLOGY, 2));
    tokens.push(3); // triangle_strip (tokenized shader format)
    tokens.push(opcode_token(
        sm4_opcode::OPCODE_DCL_GS_MAX_OUTPUT_VERTEX_COUNT,
        2,
    ));
    tokens.push(3); // maxvertexcount

    // Minimal I/O decls (opcode value is irrelevant as long as it's treated as a declaration by the decoder).
    const DCL_DUMMY: u32 = 0x300;
    // v0.xyzw
    tokens.push(opcode_token(DCL_DUMMY, 3));
    tokens.push(0x0010_F012);
    tokens.push(0);
    // v1.xyzw
    tokens.push(opcode_token(DCL_DUMMY, 3));
    tokens.push(0x0010_F012);
    tokens.push(1);
    // o0.xyzw
    tokens.push(opcode_token(DCL_DUMMY + 1, 3));
    tokens.push(0x0010_F022);
    tokens.push(0);
    // o1.xyzw
    tokens.push(opcode_token(DCL_DUMMY + 1, 3));
    tokens.push(0x0010_F022);
    tokens.push(1);

    // mov o0.xyzw, v0[0].xyzw
    tokens.push(opcode_token(sm4_opcode::OPCODE_MOV, 6));
    tokens.push(0x0010_F022); // o0.xyzw
    tokens.push(0);
    tokens.push(0x0020_F012); // v0[0].xyzw (2D indexing)
    tokens.push(0); // reg
    tokens.push(0); // vertex
    // mov o1.xyzw, v1[0].xyzw
    tokens.push(opcode_token(sm4_opcode::OPCODE_MOV, 6));
    tokens.push(0x0010_F022); // o1.xyzw
    tokens.push(1);
    tokens.push(0x0020_F012); // v1[0].xyzw (2D indexing)
    tokens.push(1); // reg
    tokens.push(0); // vertex
    tokens.push(opcode_token(sm4_opcode::OPCODE_EMIT, 1));

    // mov o0.xyzw, v0[1].xyzw
    tokens.push(opcode_token(sm4_opcode::OPCODE_MOV, 6));
    tokens.push(0x0010_F022); // o0.xyzw
    tokens.push(0);
    tokens.push(0x0020_F012); // v0[1].xyzw (2D indexing)
    tokens.push(0); // reg
    tokens.push(1); // vertex
    // mov o1.xyzw, v1[1].xyzw
    tokens.push(opcode_token(sm4_opcode::OPCODE_MOV, 6));
    tokens.push(0x0010_F022); // o1.xyzw
    tokens.push(1);
    tokens.push(0x0020_F012); // v1[1].xyzw (2D indexing)
    tokens.push(1); // reg
    tokens.push(1); // vertex
    tokens.push(opcode_token(sm4_opcode::OPCODE_EMIT, 1));

    // mov o0.xyzw, l(0, 0.5, 0, 1)
    tokens.push(opcode_token(sm4_opcode::OPCODE_MOV, 8));
    tokens.push(0x0010_F022); // o0.xyzw
    tokens.push(0);
    tokens.push(0x0000_F042); // immediate vec4
    tokens.push(0.0f32.to_bits());
    tokens.push(0.5f32.to_bits());
    tokens.push(0.0f32.to_bits());
    tokens.push(1.0f32.to_bits());
    // mov o1.xyzw, v1[0].xyzw
    tokens.push(opcode_token(sm4_opcode::OPCODE_MOV, 6));
    tokens.push(0x0010_F022); // o1.xyzw
    tokens.push(1);
    tokens.push(0x0020_F012); // v1[0].xyzw (2D indexing)
    tokens.push(1); // reg
    tokens.push(0); // vertex
    tokens.push(opcode_token(sm4_opcode::OPCODE_EMIT, 1));

    tokens.push(opcode_token(sm4_opcode::OPCODE_RET, 1));
    tokens[1] = tokens.len() as u32;

    build_dxbc(&[(FourCC(*b"SHDR"), tokens_to_bytes(&tokens))])
}

fn push_u32(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn build_ilay_pos3_color4_instance_step2() -> Vec<u8> {
    let mut blob = Vec::new();
    push_u32(&mut blob, AEROGPU_INPUT_LAYOUT_BLOB_MAGIC);
    push_u32(&mut blob, AEROGPU_INPUT_LAYOUT_BLOB_VERSION);
    push_u32(&mut blob, 2); // element_count
    push_u32(&mut blob, 0); // flags/reserved

    let pos_hash = fnv1a_32(b"POSITION");
    let color_hash = fnv1a_32(b"COLOR");

    // POSITION0: R32G32B32_FLOAT, slot 0, per-vertex.
    push_u32(&mut blob, pos_hash);
    push_u32(&mut blob, 0); // semantic_index
    push_u32(&mut blob, 6); // DXGI_FORMAT_R32G32B32_FLOAT
    push_u32(&mut blob, 0); // input_slot
    push_u32(&mut blob, 0); // aligned_byte_offset
    push_u32(&mut blob, 0); // input_slot_class (per-vertex)
    push_u32(&mut blob, 0); // instance_data_step_rate

    // COLOR0: R32G32B32A32_FLOAT, slot 1, per-instance with step rate 2.
    push_u32(&mut blob, color_hash);
    push_u32(&mut blob, 0); // semantic_index
    push_u32(&mut blob, 2); // DXGI_FORMAT_R32G32B32A32_FLOAT
    push_u32(&mut blob, 1); // input_slot
    push_u32(&mut blob, 0); // aligned_byte_offset
    push_u32(&mut blob, 1); // input_slot_class (per-instance)
    push_u32(&mut blob, 2); // instance_data_step_rate

    blob
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct VertexPos3 {
    pos: [f32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Color4 {
    rgba: [f32; 4],
}

#[test]
fn aerogpu_cmd_geometry_shader_linelist_instance_step_rate_respected() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::aerogpu_cmd_geometry_shader_linelist_instance_step_rate_respected"
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

        const VB_POS: u32 = 1;
        const VB_INSTANCE: u32 = 2;
        const RT: u32 = 3;
        const VS: u32 = 4;
        const GS: u32 = 5;
        const PS: u32 = 6;
        const IL: u32 = 7;

        // Line vertices: base of a triangle at y=-0.5.
        let positions = [
            VertexPos3 {
                pos: [-0.5, -0.5, 0.0],
            },
            VertexPos3 {
                pos: [0.5, -0.5, 0.0],
            },
        ];

        // Instance colors: pick element 1 (green) when first_instance=3 and step_rate=2.
        // Incorrect behavior (no divide) would select element 3 (red).
        let instance_colors = [
            Color4 {
                rgba: [0.0, 0.0, 0.0, 1.0],
            },
            Color4 {
                rgba: [0.0, 1.0, 0.0, 1.0],
            },
            Color4 {
                rgba: [0.0, 0.0, 1.0, 1.0],
            },
            Color4 {
                rgba: [1.0, 0.0, 0.0, 1.0],
            },
        ];

        let gs_dxbc = build_gs_linelist_to_triangle_color_dxbc();
        let ilay = build_ilay_pos3_color4_instance_step2();

        let mut writer = AerogpuCmdWriter::new();
        writer.create_buffer(
            VB_POS,
            AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
            bytemuck::bytes_of(&positions).len() as u64,
            0,
            0,
        );
        writer.upload_resource(VB_POS, 0, bytemuck::bytes_of(&positions));

        writer.create_buffer(
            VB_INSTANCE,
            AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
            bytemuck::bytes_of(&instance_colors).len() as u64,
            0,
            0,
        );
        writer.upload_resource(VB_INSTANCE, 0, bytemuck::bytes_of(&instance_colors));

        // Use an odd-sized render target so NDC (0,0) maps exactly to the center pixel.
        let w = 65u32;
        let h = 65u32;
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

        // Cull CW triangles (front faces) so placeholder prepass output is culled; the GS emits a
        // CCW triangle using both input line vertices.
        writer.set_rasterizer_state(
            AerogpuFillMode::Solid,
            AerogpuCullMode::Front,
            false,
            false,
            0,
            0,
        );

        writer.create_shader_dxbc(VS, AerogpuShaderStage::Vertex, VS_PASSTHROUGH);
        writer.create_shader_dxbc_ex(GS, AerogpuShaderStageEx::Geometry, &gs_dxbc);
        writer.create_shader_dxbc(PS, AerogpuShaderStage::Pixel, PS_PASSTHROUGH);

        writer.create_input_layout(IL, &ilay);
        writer.set_input_layout(IL);
        writer.set_vertex_buffers(
            0,
            &[
                AerogpuVertexBufferBinding {
                    buffer: VB_POS,
                    stride_bytes: core::mem::size_of::<VertexPos3>() as u32,
                    offset_bytes: 0,
                    reserved0: 0,
                },
                AerogpuVertexBufferBinding {
                    buffer: VB_INSTANCE,
                    stride_bytes: core::mem::size_of::<Color4>() as u32,
                    offset_bytes: 0,
                    reserved0: 0,
                },
            ],
        );

        writer.set_primitive_topology(AerogpuPrimitiveTopology::LineList);
        writer.bind_shaders_ex(VS, PS, 0, GS, 0, 0);

        writer.clear(AEROGPU_CLEAR_COLOR, [0.0, 0.0, 0.0, 1.0], 1.0, 0);
        writer.draw(2, 1, 0, 3);

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
        assert_eq!(pixels.len(), (w * h * 4) as usize);

        let x = w / 2;
        let y = h / 2;
        let idx = ((y * w + x) * 4) as usize;
        let center: [u8; 4] = pixels[idx..idx + 4].try_into().unwrap();

        assert_eq!(
            center,
            [0, 255, 0, 255],
            "expected instance-step-rate color (green) at center pixel; got {center:?}"
        );
    });
}

