mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_dxbc::{test_utils as dxbc_test_utils, FourCC};
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuConstantBufferBinding, AerogpuCullMode, AerogpuFillMode, AerogpuPrimitiveTopology,
    AerogpuShaderStage, AerogpuShaderStageEx, AEROGPU_CLEAR_COLOR,
    AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER, AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

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

fn build_signature_chunk(params: &[SigParam]) -> Vec<u8> {
    // Mirrors `aero_d3d11::signature::parse_signature_chunk` expectations.
    let mut out = Vec::new();
    out.extend_from_slice(&(params.len() as u32).to_le_bytes()); // param_count
    out.extend_from_slice(&8u32.to_le_bytes()); // param_offset

    let entry_size = 24usize;
    let table_start = out.len();
    out.resize(table_start + params.len() * entry_size, 0);

    for (i, p) in params.iter().enumerate() {
        let semantic_name_offset = out.len() as u32;
        out.extend_from_slice(p.semantic_name.as_bytes());
        out.push(0);
        while out.len() % 4 != 0 {
            out.push(0);
        }

        let base = table_start + i * entry_size;
        out[base..base + 4].copy_from_slice(&semantic_name_offset.to_le_bytes());
        out[base + 4..base + 8].copy_from_slice(&p.semantic_index.to_le_bytes());
        out[base + 8..base + 12].copy_from_slice(&0u32.to_le_bytes()); // system_value_type
        out[base + 12..base + 16].copy_from_slice(&0u32.to_le_bytes()); // component_type
        out[base + 16..base + 20].copy_from_slice(&p.register.to_le_bytes());
        out[base + 20] = p.mask;
        out[base + 21] = p.mask; // read_write_mask
        out[base + 22] = 0; // stream
        out[base + 23] = 0; // min_precision
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

fn build_vs_pos_only_dxbc() -> Vec<u8> {
    // vs_4_0: mov o0, v0; ret
    let isgn = build_signature_chunk(&[SigParam {
        semantic_name: "POSITION",
        semantic_index: 0,
        register: 0,
        mask: 0x07,
    }]);
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

fn build_gs_reads_cb0_dxbc() -> Vec<u8> {
    use aero_d3d11::sm4::opcode::*;

    fn opcode_token(opcode: u32, len: u32) -> u32 {
        opcode | (len << OPCODE_LEN_SHIFT)
    }

    // gs_4_0:
    //   dcl_inputprimitive point
    //   dcl_outputtopology triangle_strip
    //   dcl_maxvertexcount 3
    //   ret
    //
    // Note: The shader does not need to reference `cb0`; the executor's placeholder compute prepass
    // reads geometry-stage cbuffer b0 directly. We still provide a valid GS token stream so the GS
    // object is accepted by the executor's GS translation gate.
    const PRIM_POINT: u32 = 1;
    const TOPO_TRIANGLE_STRIP: u32 = 5;
    const MAX_VERTS: u32 = 3;

    let mut tokens = vec![
        0x0002_0040u32, // gs_4_0
        0,              // length patched below
        opcode_token(OPCODE_DCL_GS_INPUT_PRIMITIVE, 2),
        PRIM_POINT,
        opcode_token(OPCODE_DCL_GS_OUTPUT_TOPOLOGY, 2),
        TOPO_TRIANGLE_STRIP,
        opcode_token(OPCODE_DCL_GS_MAX_OUTPUT_VERTEX_COUNT, 2),
        MAX_VERTS,
        opcode_token(OPCODE_RET, 1),
    ];
    tokens[1] = tokens.len() as u32;

    let shdr = tokens_to_bytes(&tokens);
    build_dxbc(&[(FourCC(*b"SHDR"), shdr)])
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct GsCb0 {
    offset: [f32; 4],
}

#[test]
fn aerogpu_cmd_geometry_shader_cb0_offsets_placeholder_prepass() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const CB0: u32 = 1;
        const CB1: u32 = 2;
        const RT0: u32 = 10;
        const RT1: u32 = 11;
        const VS: u32 = 20;
        const GS: u32 = 21;
        const PS: u32 = 22;

        let w = 64u32;
        let h = 64u32;

        let cb0_bytes = bytemuck::bytes_of(&GsCb0 {
            offset: [0.0, 0.0, 0.0, 0.0],
        });
        let cb1_bytes = bytemuck::bytes_of(&GsCb0 {
            // Shift the generated triangle far enough in clip space to no longer cover the center.
            offset: [2.0, 0.0, 0.0, 0.0],
        });

        let mut writer = AerogpuCmdWriter::new();
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
            &build_gs_reads_cb0_dxbc(),
        );

        writer.set_viewport(0.0, 0.0, w as f32, h as f32, 0.0, 1.0);
        writer.set_primitive_topology(AerogpuPrimitiveTopology::TriangleList);
        // Disable face culling to avoid depending on backend-specific winding conventions.
        writer.set_rasterizer_state_ext(
            AerogpuFillMode::Solid,
            AerogpuCullMode::None,
            false,
            false,
            0,
            false,
        );

        // Pass 1: offset = (0,0). Triangle should cover the center pixel.
        writer.bind_shaders_with_gs(VS, GS, PS, 0);
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
        writer.draw(3, 1, 0, 0);

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
        writer.draw(3, 1, 0, 0);

        writer.present(0, 0);
        let stream = writer.finish();

        let mut guest_mem = VecGuestMemory::new(0);
        if let Err(err) = exec.execute_cmd_stream(&stream, None, &mut guest_mem) {
            if common::skip_if_compute_or_indirect_unsupported(module_path!(), &err) {
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

        // With offset (0,0), the generated triangle should cover the center pixel.
        assert_eq!(px(&pixels0, w / 2, h / 2), [0, 255, 0, 255]);
        // With offset (+2,0), the triangle shifts away, leaving the center as the clear color.
        assert_eq!(px(&pixels1, w / 2, h / 2), [255, 0, 0, 255]);
    });
}
