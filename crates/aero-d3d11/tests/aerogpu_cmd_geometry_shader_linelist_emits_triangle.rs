mod common;

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

const ILAY_POS3_COLOR: &[u8] = include_bytes!("fixtures/ilay_pos3_color.bin");
const VS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/vs_passthrough.dxbc");

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

fn build_gs_linelist_to_triangle_dxbc() -> Vec<u8> {
    // gs_4_0:
    //   dcl_inputprimitive line
    //   dcl_outputtopology triangle_strip
    //   dcl_maxvertexcount 3
    //   mov o0, v0[0]; emit
    //   mov o0, v0[1]; emit
    //   mov o0, l(0, 0.5, 0, 1); emit
    //   ret
    //
    // With front_face=CW, the emitted vertices (left->right base + top) form a CCW triangle. We
    // set cull=Front in the test so placeholder prepass output (CW fullscreen triangle) is culled,
    // making the assertion sensitive to the translated GS prepass and correct primitive assembly.
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
    tokens.push(opcode_token(DCL_DUMMY, 3));
    tokens.push(0x0010_F012); // v0.xyzw (1D indexing)
    tokens.push(0);
    tokens.push(opcode_token(DCL_DUMMY + 1, 3));
    tokens.push(0x0010_F022); // o0.xyzw
    tokens.push(0);

    // mov o0.xyzw, v0[0].xyzw
    tokens.push(opcode_token(sm4_opcode::OPCODE_MOV, 6));
    tokens.push(0x0010_F022); // o0.xyzw
    tokens.push(0);
    tokens.push(0x0020_F012); // v0[0].xyzw (2D indexing)
    tokens.push(0); // reg
    tokens.push(0); // vertex
    tokens.push(opcode_token(sm4_opcode::OPCODE_EMIT, 1));

    // mov o0.xyzw, v0[1].xyzw
    tokens.push(opcode_token(sm4_opcode::OPCODE_MOV, 6));
    tokens.push(0x0010_F022); // o0.xyzw
    tokens.push(0);
    tokens.push(0x0020_F012); // v0[1].xyzw (2D indexing)
    tokens.push(0); // reg
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
    tokens.push(opcode_token(sm4_opcode::OPCODE_EMIT, 1));

    tokens.push(opcode_token(sm4_opcode::OPCODE_RET, 1));
    tokens[1] = tokens.len() as u32;

    build_dxbc(&[(FourCC(*b"SHDR"), tokens_to_bytes(&tokens))])
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
    let mov_token = sm4_opcode::OPCODE_MOV | (8u32 << sm4_opcode::OPCODE_LEN_SHIFT);
    let ret_token = sm4_opcode::OPCODE_RET | (1u32 << sm4_opcode::OPCODE_LEN_SHIFT);

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
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct VertexPos3Color4 {
    pos: [f32; 3],
    color: [f32; 4],
}

#[test]
fn aerogpu_cmd_geometry_shader_linelist_emits_triangle() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::aerogpu_cmd_geometry_shader_linelist_emits_triangle"
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

        let vertices = [
            VertexPos3Color4 {
                pos: [-0.5, -0.5, 0.0],
                color: [0.0, 0.0, 0.0, 1.0],
            },
            VertexPos3Color4 {
                pos: [0.5, -0.5, 0.0],
                color: [0.0, 0.0, 0.0, 1.0],
            },
        ];
        let vb_bytes = bytemuck::bytes_of(&vertices);

        let gs_dxbc = build_gs_linelist_to_triangle_dxbc();
        let ps_dxbc = build_ps_solid_green_dxbc();

        let mut writer = AerogpuCmdWriter::new();
        writer.create_buffer(
            VB,
            AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
            vb_bytes.len() as u64,
            0,
            0,
        );
        writer.upload_resource(VB, 0, vb_bytes);

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
        // CCW triangle using both input line vertices, so only the translated GS prepass should
        // touch the center pixel.
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
        writer.create_shader_dxbc(PS, AerogpuShaderStage::Pixel, &ps_dxbc);

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
        writer.set_primitive_topology(AerogpuPrimitiveTopology::LineList);
        writer.bind_shaders_ex(VS, PS, 0, GS, 0, 0);

        // Clear to solid red so we can detect whether the draw actually touched the center pixel.
        writer.clear(AEROGPU_CLEAR_COLOR, [1.0, 0.0, 0.0, 1.0], 1.0, 0);
        writer.draw(2, 1, 0, 0);

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

        assert_ne!(
            center,
            [255, 0, 0, 255],
            "center pixel should not match the clear color; translated line-list GS prepass may not have executed"
        );

        // Solid-green PS: center should be green-dominant.
        let [r, g, b, a] = center;
        assert_eq!(a, 255, "expected alpha=255 at center pixel");
        assert!(
            g > r && g > b,
            "expected center pixel to be green-dominant (g > r && g > b), got {center:?}"
        );
    });
}
