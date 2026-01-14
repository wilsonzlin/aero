mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_d3d11::sm4::opcode::*;
use aero_d3d11::{FourCC, Swizzle, WriteMask};
use aero_dxbc::test_utils as dxbc_test_utils;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCullMode, AerogpuFillMode, AerogpuIndexFormat, AerogpuPrimitiveTopology,
    AerogpuShaderStage, AerogpuVertexBufferBinding, AEROGPU_CLEAR_COLOR,
    AEROGPU_RESOURCE_USAGE_INDEX_BUFFER, AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
    AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

// Validates TRIANGLESTRIP_ADJ DrawIndexed primitive-restart handling:
// - restart index splits the stream into disjoint strips (no bridging primitives),
// - triangle-strip-adj parity resets after a restart.

const VS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/vs_passthrough.dxbc");
const PS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/ps_passthrough.dxbc");
const ILAY_POS3_COLOR: &[u8] = include_bytes!("fixtures/ilay_pos3_color.bin");

const FOURCC_ISGN: FourCC = FourCC(*b"ISGN");
const FOURCC_OSGN: FourCC = FourCC(*b"OSGN");
const FOURCC_SHDR: FourCC = FourCC(*b"SHDR");

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

fn reg_dst(ty: u32, idx: u32, mask: WriteMask) -> Vec<u32> {
    vec![
        operand_token(ty, 2, OPERAND_SEL_MASK, mask.0 as u32, 1),
        idx,
    ]
}

fn reg_src(ty: u32, indices: &[u32], swizzle: Swizzle) -> Vec<u32> {
    let num_components = match ty {
        OPERAND_TYPE_SAMPLER | OPERAND_TYPE_RESOURCE => 0,
        _ => 2,
    };
    let selection_mode = if num_components == 0 {
        OPERAND_SEL_MASK
    } else {
        OPERAND_SEL_SWIZZLE
    };
    let token = operand_token(
        ty,
        num_components,
        selection_mode,
        swizzle_bits(swizzle.0),
        indices.len() as u32,
    );
    let mut out = Vec::new();
    out.push(token);
    out.extend_from_slice(indices);
    out
}

fn build_gs_triadj_passthrough_triangle_dxbc() -> Vec<u8> {
    // gs_4_0:
    //   dcl_inputprimitive triadj
    //   dcl_outputtopology triangle_strip
    //   dcl_maxvertexcount 3
    //
    //   color = v1[0] (triangle vertex 0 color)
    //   emit triangle using positions v0[0], v0[2], v0[4] (tri vertices)
    //   cut
    //   ret

    let isgn = build_signature_chunk(&[
        SigParam {
            semantic_name: "SV_Position",
            semantic_index: 0,
            register: 0,
            mask: 0x0f,
        },
        SigParam {
            semantic_name: "COLOR",
            semantic_index: 0,
            register: 1,
            mask: 0x0f,
        },
    ]);
    let osgn = build_signature_chunk(&[
        SigParam {
            semantic_name: "SV_Position",
            semantic_index: 0,
            register: 0,
            mask: 0x0f,
        },
        SigParam {
            semantic_name: "COLOR",
            semantic_index: 0,
            register: 1,
            mask: 0x0f,
        },
    ]);

    const PRIM_TRIADJ: u32 = 7;
    const TOPO_TRIANGLE_STRIP: u32 = 5;
    const MAX_VERTS: u32 = 3;

    const DCL_DUMMY: u32 = 0x300;

    let mut body = Vec::<u32>::new();
    body.extend_from_slice(&[
        opcode_token(OPCODE_DCL_GS_INPUT_PRIMITIVE, 2),
        PRIM_TRIADJ,
        opcode_token(OPCODE_DCL_GS_OUTPUT_TOPOLOGY, 2),
        TOPO_TRIANGLE_STRIP,
        opcode_token(OPCODE_DCL_GS_MAX_OUTPUT_VERTEX_COUNT, 2),
        MAX_VERTS,
    ]);

    // Basic IO decls.
    body.extend_from_slice(&[opcode_token(DCL_DUMMY, 3)]);
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_INPUT, 0, WriteMask::XYZW));
    body.extend_from_slice(&[opcode_token(DCL_DUMMY, 3)]);
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_INPUT, 1, WriteMask::XYZW));
    body.extend_from_slice(&[opcode_token(DCL_DUMMY + 1, 3)]);
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    body.extend_from_slice(&[opcode_token(DCL_DUMMY + 1, 3)]);
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 1, WriteMask::XYZW));

    // mov r0, v1[0] (color)
    let mut inst = vec![0u32];
    inst.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    inst.extend_from_slice(&reg_src(OPERAND_TYPE_INPUT, &[1, 0], Swizzle::XYZW));
    inst[0] = opcode_token(OPCODE_MOV, inst.len() as u32);
    body.extend_from_slice(&inst);

    for tri_v in [0u32, 2u32, 4u32] {
        // mov o0, v0[tri_v]
        let mut inst = vec![0u32];
        inst.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
        inst.extend_from_slice(&reg_src(OPERAND_TYPE_INPUT, &[0, tri_v], Swizzle::XYZW));
        inst[0] = opcode_token(OPCODE_MOV, inst.len() as u32);
        body.extend_from_slice(&inst);

        // mov o1, r0
        let mut inst = vec![0u32];
        inst.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 1, WriteMask::XYZW));
        inst.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, &[0], Swizzle::XYZW));
        inst[0] = opcode_token(OPCODE_MOV, inst.len() as u32);
        body.extend_from_slice(&inst);

        body.push(opcode_token(OPCODE_EMIT, 1));
    }
    body.push(opcode_token(OPCODE_CUT, 1));
    body.push(opcode_token(OPCODE_RET, 1));

    let version = 0x0002_0040u32; // gs_4_0
    let mut tokens = Vec::with_capacity(2 + body.len());
    tokens.push(version);
    tokens.push(0); // length patched below
    tokens.extend_from_slice(&body);
    tokens[1] = tokens.len() as u32;

    let shdr = tokens_to_bytes(&tokens);
    build_dxbc(&[(FOURCC_ISGN, isgn), (FOURCC_OSGN, osgn), (FOURCC_SHDR, shdr)])
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct VertexPos3Color4 {
    pos: [f32; 3],
    color: [f32; 4],
}

#[test]
fn aerogpu_cmd_geometry_shader_trianglestripadj_draw_indexed_restart() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::aerogpu_cmd_geometry_shader_trianglestripadj_draw_indexed_restart"
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

        // Two disjoint tri-adj primitives: one on the left, one on the right.
        let vertices: [VertexPos3Color4; 12] = [
            // --- left strip (primitive A) ---
            // tri vertex 0
            VertexPos3Color4 {
                pos: [-0.8, -0.2, 0.0],
                color: [1.0, 0.0, 0.0, 1.0], // red
            },
            // adj (0,2)
            VertexPos3Color4 {
                pos: [-0.9, 0.0, 0.0],
                color: [1.0, 0.5, 0.0, 1.0],
            },
            // tri vertex 1
            VertexPos3Color4 {
                pos: [-0.4, -0.2, 0.0],
                color: [0.0, 1.0, 0.0, 1.0],
            },
            // adj (2,4)
            VertexPos3Color4 {
                pos: [-0.5, 0.0, 0.0],
                color: [0.0, 1.0, 1.0, 1.0],
            },
            // tri vertex 2
            VertexPos3Color4 {
                pos: [-0.6, 0.2, 0.0],
                color: [1.0, 0.0, 1.0, 1.0],
            },
            // adj (4,0)
            VertexPos3Color4 {
                pos: [-0.7, 0.0, 0.0],
                color: [0.5, 0.5, 0.5, 1.0],
            },
            // --- right strip (primitive B) ---
            // tri vertex 0 (expected color when parity resets correctly)
            VertexPos3Color4 {
                pos: [0.4, -0.2, 0.0],
                color: [0.0, 0.0, 1.0, 1.0], // blue
            },
            // adj (0,2)
            VertexPos3Color4 {
                pos: [0.3, 0.0, 0.0],
                color: [0.5, 0.0, 0.5, 1.0],
            },
            // tri vertex 1 (used as tri vertex 0 if parity is *not* reset after restart)
            VertexPos3Color4 {
                pos: [0.8, -0.2, 0.0],
                color: [0.0, 1.0, 0.0, 1.0], // green
            },
            // adj (2,4)
            VertexPos3Color4 {
                pos: [0.9, 0.0, 0.0],
                color: [1.0, 1.0, 0.0, 1.0],
            },
            // tri vertex 2
            VertexPos3Color4 {
                pos: [0.6, 0.2, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            // adj (4,0)
            VertexPos3Color4 {
                pos: [0.7, 0.0, 0.0],
                color: [0.25, 0.25, 0.25, 1.0],
            },
        ];

        // Two 6-index TRIANGLESTRIP_ADJ strips separated by a restart index.
        let indices: [u16; 13] = [0, 1, 2, 3, 4, 5, u16::MAX, 6, 7, 8, 9, 10, 11];
        // UPLOAD_RESOURCE requires 4-byte alignment; pad the upload with one extra u16.
        let mut indices_upload: Vec<u8> = Vec::with_capacity(core::mem::size_of_val(&indices) + 2);
        indices_upload.extend_from_slice(bytemuck::cast_slice(&indices));
        indices_upload.extend_from_slice(&0u16.to_le_bytes());

        let mut writer = AerogpuCmdWriter::new();
        writer.create_buffer(
            VB,
            AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
            core::mem::size_of_val(&vertices) as u64,
            0,
            0,
        );
        writer.upload_resource(VB, 0, bytemuck::cast_slice(&vertices));

        writer.create_buffer(
            IB,
            AEROGPU_RESOURCE_USAGE_INDEX_BUFFER,
            indices_upload.len() as u64,
            0,
            0,
        );
        writer.upload_resource(IB, 0, &indices_upload);

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

        writer.set_rasterizer_state(
            AerogpuFillMode::Solid,
            AerogpuCullMode::None,
            false,
            false,
            0,
            0,
        );

        let gs_dxbc = build_gs_triadj_passthrough_triangle_dxbc();
        writer.create_shader_dxbc(VS, AerogpuShaderStage::Vertex, VS_PASSTHROUGH);
        writer.create_shader_dxbc(GS, AerogpuShaderStage::Geometry, &gs_dxbc);
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
        writer.set_index_buffer(IB, AerogpuIndexFormat::Uint16, 0);

        writer.set_primitive_topology(AerogpuPrimitiveTopology::TriangleStripAdj);
        writer.bind_shaders_ex(VS, PS, 0, GS, 0, 0);

        writer.clear(AEROGPU_CLEAR_COLOR, [0.0, 0.0, 0.0, 1.0], 1.0, 0);
        writer.draw_indexed(indices.len() as u32, 1, 0, 0, 0);

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

        let px = |x: u32, y: u32| -> [u8; 4] {
            let idx = ((y * w + x) * 4) as usize;
            pixels[idx..idx + 4].try_into().unwrap()
        };

        let y_mid = h / 2;
        assert_eq!(
            px(w / 2, y_mid),
            [0, 0, 0, 255],
            "gap pixel should remain clear (primitive restart/cut semantics broken?)"
        );
        assert_eq!(
            px(w - 12, y_mid),
            [0, 0, 255, 255],
            "right triangle pixel mismatch (parity did not reset after restart?)"
        );
    });
}
