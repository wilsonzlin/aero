mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_d3d11::sm4::opcode::*;
use aero_d3d11::{FourCC, Swizzle, WriteMask};
use aero_dxbc::test_utils as dxbc_test_utils;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCullMode, AerogpuFillMode, AerogpuPrimitiveTopology, AerogpuShaderStage,
    AerogpuVertexBufferBinding, AEROGPU_CLEAR_COLOR, AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
    AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

// Validates that the translated GS compute prepass supports D3D11 LINESTRIP_ADJ (GS lineadj input)
// with the normative adjacency vertex ordering.

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

fn imm_f32x4(v: [f32; 4]) -> Vec<u32> {
    let mut out = Vec::new();
    out.push(operand_token(
        OPERAND_TYPE_IMMEDIATE32,
        2,
        OPERAND_SEL_SWIZZLE,
        swizzle_bits(Swizzle::XYZW.0),
        0,
    ));
    out.extend_from_slice(&[v[0].to_bits(), v[1].to_bits(), v[2].to_bits(), v[3].to_bits()]);
    out
}

fn build_gs_lineadj_emit_triangle_dxbc() -> Vec<u8> {
    // gs_4_0:
    //   dcl_inputprimitive lineadj
    //   dcl_outputtopology triangle_strip
    //   dcl_maxvertexcount 3
    //
    //   base = v0[3] (adjacent vertex after the line end)
    //   color = v1[1] (line start vertex color)
    //   emit 3 verts forming a small triangle around `base`
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

    // Values from `d3d10tokenizedprogramformat.h`:
    // - primitive: lineadj = 6
    // - output topology: triangle_strip = 5
    const PRIM_LINEADJ: u32 = 6;
    const TOPO_TRIANGLE_STRIP: u32 = 5;
    const MAX_VERTS: u32 = 3;

    const DCL_DUMMY: u32 = 0x300;

    let mut body = Vec::<u32>::new();
    body.extend_from_slice(&[
        opcode_token(OPCODE_DCL_GS_INPUT_PRIMITIVE, 2),
        PRIM_LINEADJ,
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

    // mov r0, v0[3] (base position from adjacency vertex)
    let mut inst = vec![0u32];
    inst.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    inst.extend_from_slice(&reg_src(OPERAND_TYPE_INPUT, &[0, 3], Swizzle::XYZW));
    inst[0] = opcode_token(OPCODE_MOV, inst.len() as u32);
    body.extend_from_slice(&inst);

    // mov r1, v1[1] (color from the line-start vertex)
    let mut inst = vec![0u32];
    inst.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 1, WriteMask::XYZW));
    inst.extend_from_slice(&reg_src(OPERAND_TYPE_INPUT, &[1, 1], Swizzle::XYZW));
    inst[0] = opcode_token(OPCODE_MOV, inst.len() as u32);
    body.extend_from_slice(&inst);

    let offsets = [
        [-0.2f32, -0.2f32, 0.0, 0.0],
        [0.2f32, -0.2f32, 0.0, 0.0],
        [0.0f32, 0.2f32, 0.0, 0.0],
    ];
    for off in offsets {
        // add o0, r0, imm
        let mut inst = vec![0u32];
        inst.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
        inst.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, &[0], Swizzle::XYZW));
        inst.extend_from_slice(&imm_f32x4(off));
        inst[0] = opcode_token(OPCODE_ADD, inst.len() as u32);
        body.extend_from_slice(&inst);

        // mov o1, r1
        let mut inst = vec![0u32];
        inst.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 1, WriteMask::XYZW));
        inst.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, &[1], Swizzle::XYZW));
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
fn aerogpu_cmd_geometry_shader_linestripadj_emits_triangle() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::aerogpu_cmd_geometry_shader_linestripadj_emits_triangle"
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

        // Allocate more vertices than the draw consumes. A broken assembly scheme that treats
        // LINESTRIP_ADJ like LINELIST_ADJ (4p + ...) will accidentally sample these.
        let vertices: [VertexPos3Color4; 8] = [
            VertexPos3Color4 {
                pos: [0.0, 0.0, 0.0],
                color: [1.0, 1.0, 1.0, 1.0],
            },
            // v1: line start for primitive 0 (expected: red triangle on the left).
            VertexPos3Color4 {
                pos: [0.0, 0.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            // v2: line start for primitive 1 (expected: blue triangle on the right).
            VertexPos3Color4 {
                pos: [0.0, 0.0, 0.0],
                color: [0.0, 0.0, 1.0, 1.0],
            },
            // v3: adjacency-after for primitive 0 (base position).
            VertexPos3Color4 {
                pos: [-0.6, 0.0, 0.0],
                color: [1.0, 1.0, 0.0, 1.0],
            },
            // v4: adjacency-after for primitive 1 (base position).
            VertexPos3Color4 {
                pos: [0.6, 0.0, 0.0],
                color: [0.0, 1.0, 1.0, 1.0],
            },
            // v5: if primitive 1 is incorrectly assembled as (4p + ...), this becomes the "line
            // start" and will turn the right triangle green.
            VertexPos3Color4 {
                pos: [0.0, 0.0, 0.0],
                color: [0.0, 1.0, 0.0, 1.0],
            },
            VertexPos3Color4 {
                pos: [0.0, 0.0, 0.0],
                color: [1.0, 0.0, 1.0, 1.0],
            },
            // v7: if primitive 1 is incorrectly assembled as (4p + ...), this becomes the
            // adjacency-after vertex and will move the second triangle away from the right side.
            VertexPos3Color4 {
                pos: [-0.6, 0.0, 0.0],
                color: [0.25, 0.25, 0.25, 1.0],
            },
        ];

        let mut writer = AerogpuCmdWriter::new();
        writer.create_buffer(
            VB,
            AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
            core::mem::size_of_val(&vertices) as u64,
            0,
            0,
        );
        writer.upload_resource(VB, 0, bytemuck::cast_slice(&vertices));

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

        // Disable culling so winding differences don't affect the result.
        writer.set_rasterizer_state(
            AerogpuFillMode::Solid,
            AerogpuCullMode::None,
            false,
            false,
            0,
            0,
        );

        let gs_dxbc = build_gs_lineadj_emit_triangle_dxbc();
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

        writer.set_primitive_topology(AerogpuPrimitiveTopology::LineStripAdj);
        writer.bind_shaders_ex(VS, PS, 0, GS, 0, 0);

        writer.clear(AEROGPU_CLEAR_COLOR, [0.0, 0.0, 0.0, 1.0], 1.0, 0);
        writer.draw(5, 1, 0, 0);

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
            px(10, y_mid),
            [255, 0, 0, 255],
            "left triangle pixel mismatch"
        );
        assert_eq!(
            px(w - 12, y_mid),
            [0, 0, 255, 255],
            "right triangle pixel mismatch (LINESTRIP_ADJ primitive assembly or vertex ordering broken?)"
        );
        assert_eq!(
            px(w / 2, y_mid),
            [0, 0, 0, 255],
            "center pixel should remain clear"
        );
    });
}
