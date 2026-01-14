mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_d3d11::sm4::opcode::{
    OPCODE_ADD, OPCODE_DCL_GS_INPUT_PRIMITIVE, OPCODE_DCL_GS_MAX_OUTPUT_VERTEX_COUNT,
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
    let mov_token = 0x01u32 | (5u32 << 11);
    let dst_o0 = 0x0010_f022u32;
    let src_v0 = 0x001e_4016u32;
    let ret_token = 0x3eu32 | (1u32 << 11);

    let mut tokens = vec![
        version_token,
        0, // length patched below
        mov_token,
        dst_o0,
        0, // o0 index
        src_v0,
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

fn opcode_token(opcode: u32, len: u32) -> u32 {
    opcode | (len << OPCODE_LEN_SHIFT)
}

fn build_gs_triangle_strip_adds_cb1_offset_dxbc() -> Vec<u8> {
    // gs_4_0 token stream that emits a single triangle strip (3 vertices) and offsets the
    // positions by cb1[0].xyzw.
    //
    // Pseudocode:
    //   dcl_inputprimitive point
    //   dcl_outputtopology triangle_strip
    //   dcl_maxvertexcount 3
    //   dcl_constantbuffer cb1[1]
    //   dcl_output o0.xyzw
    //   add o0, l(-0.5,-0.5,0,1), cb1[0]
    //   emit
    //   add o0, l(0,0.5,0,1), cb1[0]
    //   emit
    //   add o0, l(0.5,-0.5,0,1), cb1[0]
    //   emit
    //   ret
    const PRIM_POINT: u32 = 1;
    const TOPO_TRIANGLE_STRIP: u32 = 5;

    // Output register operand (o0.xyzw).
    let dst_o0 = 0x0010_f022u32;
    // Constant-buffer operand token (slot + reg immediate indices).
    let cb_operand = 0x002e_4086u32;
    // Immediate vec4 operand token.
    let imm_vec4 = 0x0000_f042u32;

    let add_len = 1 + 2 + 5 + 3;

    let mut tokens = vec![0x0002_0040u32, 0]; // gs_4_0, length patched below
    tokens.push(opcode_token(OPCODE_DCL_GS_INPUT_PRIMITIVE, 2));
    tokens.push(PRIM_POINT);
    tokens.push(opcode_token(OPCODE_DCL_GS_OUTPUT_TOPOLOGY, 2));
    tokens.push(TOPO_TRIANGLE_STRIP);
    tokens.push(opcode_token(OPCODE_DCL_GS_MAX_OUTPUT_VERTEX_COUNT, 2));
    tokens.push(3);

    // dcl_constantbuffer cb1[1]
    tokens.push(opcode_token(0x100, 1 + 1 + 2 + 1));
    tokens.push(cb_operand);
    tokens.push(1); // slot
    tokens.push(1); // reg_count
    tokens.push(0); // access pattern (ignored)

    // dcl_output o0.xyzw
    tokens.push(opcode_token(0x100, 3));
    tokens.push(dst_o0);
    tokens.push(0); // o0

    let emit_triangle = |tokens: &mut Vec<u32>, x0: f32, y0: f32| {
        tokens.push(opcode_token(OPCODE_ADD, add_len));
        // dst o0.xyzw
        tokens.push(dst_o0);
        tokens.push(0);
        // src0 immediate vec4
        tokens.push(imm_vec4);
        tokens.push(x0.to_bits());
        tokens.push(y0.to_bits());
        tokens.push(0.0f32.to_bits());
        tokens.push(1.0f32.to_bits());
        // src1 cb1[0]
        tokens.push(cb_operand);
        tokens.push(1); // slot
        tokens.push(0); // reg
                        // emit
        tokens.push(opcode_token(OPCODE_EMIT, 1));
    };

    emit_triangle(&mut tokens, -0.5, -0.5);
    emit_triangle(&mut tokens, 0.0, 0.5);
    emit_triangle(&mut tokens, 0.5, -0.5);
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
struct OffsetVec4 {
    v: [f32; 4],
}

#[test]
fn aerogpu_cmd_geometry_shader_translate_binds_cb1_in_prepass() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::aerogpu_cmd_geometry_shader_translate_binds_cb1_in_prepass"
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
        const CB_ZERO: u32 = 2;
        const CB_SHIFT: u32 = 3;
        const RT0: u32 = 10;
        const RT1: u32 = 11;
        const VS: u32 = 20;
        const GS: u32 = 21;
        const PS: u32 = 22;
        const IL: u32 = 23;

        let vertex = VertexPos3Color4 {
            pos: [0.0, 0.0, 0.0],
            color: [0.0, 0.0, 0.0, 1.0],
        };
        let vb_bytes = bytemuck::bytes_of(&vertex);

        let cb_zero = bytemuck::bytes_of(&OffsetVec4 {
            v: [0.0, 0.0, 0.0, 0.0],
        });
        let cb_shift = bytemuck::bytes_of(&OffsetVec4 {
            v: [2.0, 0.0, 0.0, 0.0],
        });

        let w = 64u32;
        let h = 64u32;

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
            CB_ZERO,
            AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER,
            cb_zero.len() as u64,
            0,
            0,
        );
        writer.upload_resource(CB_ZERO, 0, cb_zero);
        writer.create_buffer(
            CB_SHIFT,
            AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER,
            cb_shift.len() as u64,
            0,
            0,
        );
        writer.upload_resource(CB_SHIFT, 0, cb_shift);

        writer.create_texture2d(
            RT0,
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
        writer.create_texture2d(
            RT1,
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

        writer.create_shader_dxbc(VS, AerogpuShaderStage::Vertex, &build_vs_pos_only_dxbc());
        writer.create_shader_dxbc_ex(
            GS,
            AerogpuShaderStageEx::Geometry,
            &build_gs_triangle_strip_adds_cb1_offset_dxbc(),
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

        // First draw: cb1 offset is 0 -> triangle centered -> center pixel turns green.
        writer.set_render_targets(&[RT0], 0);
        writer.set_viewport(0.0, 0.0, w as f32, h as f32, 0.0, 1.0);
        writer.clear(AEROGPU_CLEAR_COLOR, [1.0, 0.0, 0.0, 1.0], 1.0, 0);
        writer.set_constant_buffers(
            AerogpuShaderStage::Geometry,
            1,
            &[AerogpuConstantBufferBinding {
                buffer: CB_ZERO,
                offset_bytes: 0,
                size_bytes: cb_zero.len() as u32,
                reserved0: 0,
            }],
        );
        writer.draw(1, 1, 0, 0);
        writer.present(0, 0);

        // Second draw: cb1 offset shifts triangle offscreen -> center stays red.
        writer.set_render_targets(&[RT1], 0);
        writer.set_viewport(0.0, 0.0, w as f32, h as f32, 0.0, 1.0);
        writer.clear(AEROGPU_CLEAR_COLOR, [1.0, 0.0, 0.0, 1.0], 1.0, 0);
        writer.set_constant_buffers(
            AerogpuShaderStage::Geometry,
            1,
            &[AerogpuConstantBufferBinding {
                buffer: CB_SHIFT,
                offset_bytes: 0,
                size_bytes: cb_shift.len() as u32,
                reserved0: 0,
            }],
        );
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

        let px = |pixels: &[u8], x: u32, y: u32| -> [u8; 4] {
            let idx = ((y * w + x) * 4) as usize;
            pixels[idx..idx + 4].try_into().unwrap()
        };

        let pixels0 = exec.read_texture_rgba8(RT0).await.expect("readback RT0");
        let pixels1 = exec.read_texture_rgba8(RT1).await.expect("readback RT1");
        assert_eq!(px(&pixels0, w / 2, h / 2), [0, 255, 0, 255]);
        assert_eq!(px(&pixels1, w / 2, h / 2), [255, 0, 0, 255]);
    });
}
