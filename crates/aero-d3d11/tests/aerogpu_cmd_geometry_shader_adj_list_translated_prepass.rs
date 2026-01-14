mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_d3d11::sm4::opcode::*;
use aero_d3d11::{Swizzle, WriteMask};
use aero_dxbc::test_utils as dxbc_test_utils;
use aero_dxbc::FourCC;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCullMode, AerogpuFillMode, AerogpuIndexFormat, AerogpuPrimitiveTopology,
    AerogpuShaderStage, AerogpuVertexBufferBinding, AEROGPU_CLEAR_COLOR,
    AEROGPU_RESOURCE_USAGE_INDEX_BUFFER, AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
    AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

const VS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/vs_passthrough.dxbc");
const PS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/ps_passthrough.dxbc");
const ILAY_POS3_COLOR: &[u8] = include_bytes!("fixtures/ilay_pos3_color.bin");

const FOURCC_SHDR: FourCC = FourCC(*b"SHDR");

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
    token |= OPERAND_INDEX_REP_IMMEDIATE32 << OPERAND_INDEX0_REP_SHIFT;
    token |= OPERAND_INDEX_REP_IMMEDIATE32 << OPERAND_INDEX1_REP_SHIFT;
    token |= OPERAND_INDEX_REP_IMMEDIATE32 << OPERAND_INDEX2_REP_SHIFT;
    token
}

fn swizzle_bits(swz: [u8; 4]) -> u32 {
    (swz[0] as u32) | ((swz[1] as u32) << 2) | ((swz[2] as u32) << 4) | ((swz[3] as u32) << 6)
}

fn reg_dst(ty: u32, idx: u32, mask: WriteMask) -> Vec<u32> {
    vec![operand_token(ty, 2, OPERAND_SEL_MASK, mask.0 as u32, 1), idx]
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
    out.extend_from_slice(&[
        v[0].to_bits(),
        v[1].to_bits(),
        v[2].to_bits(),
        v[3].to_bits(),
    ]);
    out
}

fn build_gs_adj_to_triangle(input_prim: u32, adj_vertex: u32) -> Vec<u8> {
    // gs_4_0:
    // - input primitive: lineadj or triadj
    // - output topology: triangle strip
    // - maxvertexcount: 3
    //
    // Uses the adjacency vertex (last vertex in the input primitive) to compute an X offset:
    //   offset_x = v0[adj_vertex].x - 0.5
    //
    // Emits a small green triangle centered at x=0 when offset_x==0. If the adjacency input is
    // missing/incorrect, the triangle shifts and the center pixel remains the clear color.

    const TOPO_TRIANGLE_STRIP: u32 = 5;
    const MAX_VERTS: u32 = 3;

    let mut tokens = vec![
        0x0002_0040u32, // gs_4_0
        0,              // length patched below
        opcode_token(OPCODE_DCL_GS_INPUT_PRIMITIVE, 2),
        input_prim,
        opcode_token(OPCODE_DCL_GS_OUTPUT_TOPOLOGY, 2),
        TOPO_TRIANGLE_STRIP,
        opcode_token(OPCODE_DCL_GS_MAX_OUTPUT_VERTEX_COUNT, 2),
        MAX_VERTS,
    ];

    // r0.x = v0[adj_vertex].x - 0.5
    {
        let mut inst = vec![0u32];
        inst.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::X));
        inst.extend_from_slice(&reg_src(
            OPERAND_TYPE_INPUT,
            &[0, adj_vertex],
            Swizzle::XXXX,
        ));
        inst.extend_from_slice(&imm_f32x4([-0.5, 0.0, 0.0, 0.0]));
        inst[0] = opcode_token(OPCODE_ADD, inst.len() as u32);
        tokens.extend_from_slice(&inst);
    }

    let emit_vertex = |tokens: &mut Vec<u32>, base: [f32; 4]| {
        // mov r1, base
        let mut inst = vec![0u32];
        inst.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 1, WriteMask::XYZW));
        inst.extend_from_slice(&imm_f32x4(base));
        inst[0] = opcode_token(OPCODE_MOV, inst.len() as u32);
        tokens.extend_from_slice(&inst);

        // add r1.x, r1.x, r0.x
        let mut inst = vec![0u32];
        inst.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 1, WriteMask::X));
        inst.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, &[1], Swizzle::XXXX));
        inst.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, &[0], Swizzle::XXXX));
        inst[0] = opcode_token(OPCODE_ADD, inst.len() as u32);
        tokens.extend_from_slice(&inst);

        // mov o0, r1
        let mut inst = vec![0u32];
        inst.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
        inst.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, &[1], Swizzle::XYZW));
        inst[0] = opcode_token(OPCODE_MOV, inst.len() as u32);
        tokens.extend_from_slice(&inst);

        // mov o1, green
        let mut inst = vec![0u32];
        inst.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 1, WriteMask::XYZW));
        inst.extend_from_slice(&imm_f32x4([0.0, 1.0, 0.0, 1.0]));
        inst[0] = opcode_token(OPCODE_MOV, inst.len() as u32);
        tokens.extend_from_slice(&inst);

        // emit
        tokens.push(opcode_token(OPCODE_EMIT, 1));
    };

    // Small centered triangle; matches winding-agnostic tests (culling disabled).
    emit_vertex(&mut tokens, [-0.25, -0.25, 0.0, 1.0]);
    emit_vertex(&mut tokens, [0.0, 0.25, 0.0, 1.0]);
    emit_vertex(&mut tokens, [0.25, -0.25, 0.0, 1.0]);

    tokens.push(opcode_token(OPCODE_CUT, 1));
    tokens.push(opcode_token(OPCODE_RET, 1));

    tokens[1] = tokens.len() as u32;
    let shdr = tokens_to_bytes(&tokens);
    build_dxbc(&[(FOURCC_SHDR, shdr)])
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct VertexPos3Color4 {
    pos: [f32; 3],
    color: [f32; 4],
}

async fn run_adj_test(
    test_name: &str,
    topology: AerogpuPrimitiveTopology,
    gs_input_prim_token: u32,
    verts_per_prim: usize,
    adj_vertex_index: u32,
) {
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
    const RT0: u32 = 3;
    const RT1: u32 = 4;
    const VS: u32 = 5;
    const GS: u32 = 6;
    const PS: u32 = 7;
    const IL: u32 = 8;

    // Vertex buffer contains two input primitives back-to-back:
    // - primitive 0 (non-indexed draw): last vertex has x=0.5
    // - primitive 1 (indexed draw with base_vertex): vertex 0 has x=0.5, but index buffer places it
    //   in the last adjacency slot.
    let mut vertices: Vec<VertexPos3Color4> = vec![
        VertexPos3Color4 {
            pos: [0.0, 0.0, 0.0],
            color: [0.0, 0.0, 0.0, 1.0],
        };
        verts_per_prim * 2
    ];
    // Non-indexed primitive: special vertex in the last slot.
    vertices[verts_per_prim - 1].pos[0] = 0.5;
    // Indexed primitive: special vertex in the first slot of the second primitive.
    vertices[verts_per_prim].pos[0] = 0.5;

    let indices: Vec<u16> = (1..verts_per_prim as u16)
        .chain(std::iter::once(0u16))
        .collect();
    assert_eq!(indices.len(), verts_per_prim);
    assert_eq!(adj_vertex_index as usize, verts_per_prim - 1);

    let w = 65u32;
    let h = 65u32;

    let gs_bytes = build_gs_adj_to_triangle(gs_input_prim_token, adj_vertex_index);

    let mut writer = AerogpuCmdWriter::new();
    writer.create_buffer(
        VB,
        AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
        core::mem::size_of_val(vertices.as_slice()) as u64,
        0,
        0,
    );
    writer.upload_resource(VB, 0, bytemuck::cast_slice(vertices.as_slice()));

    writer.create_buffer(
        IB,
        AEROGPU_RESOURCE_USAGE_INDEX_BUFFER,
        core::mem::size_of_val(indices.as_slice()) as u64,
        0,
        0,
    );
    writer.upload_resource(IB, 0, bytemuck::cast_slice(indices.as_slice()));

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

    writer.create_shader_dxbc(VS, AerogpuShaderStage::Vertex, VS_PASSTHROUGH);
    writer.create_shader_dxbc(GS, AerogpuShaderStage::Geometry, &gs_bytes);
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

    writer.set_viewport(0.0, 0.0, w as f32, h as f32, 0.0, 1.0);
    writer.set_primitive_topology(topology);
    writer.bind_shaders_ex(VS, PS, 0, GS, 0, 0);
    writer.set_rasterizer_state_ext(
        AerogpuFillMode::Solid,
        AerogpuCullMode::None,
        false,
        false,
        0,
        false,
    );

    // Pass 1: non-indexed.
    writer.set_render_targets(&[RT0], 0);
    writer.clear(AEROGPU_CLEAR_COLOR, [1.0, 0.0, 0.0, 1.0], 1.0, 0);
    writer.draw(verts_per_prim as u32, 1, 0, 0);

    // Pass 2: indexed, base_vertex selects the second primitive in the vertex buffer.
    writer.set_render_targets(&[RT1], 0);
    writer.clear(AEROGPU_CLEAR_COLOR, [1.0, 0.0, 0.0, 1.0], 1.0, 0);
    writer.set_index_buffer(IB, AerogpuIndexFormat::Uint16, 0);
    writer.draw_indexed(
        verts_per_prim as u32,
        1,
        0,
        verts_per_prim as i32,
        0,
    );

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

    let read_center = |pixels: &[u8]| -> [u8; 4] {
        let x = w / 2;
        let y = h / 2;
        let idx = ((y * w + x) * 4) as usize;
        pixels[idx..idx + 4].try_into().unwrap()
    };

    let px0 = exec
        .read_texture_rgba8(RT0)
        .await
        .expect("readback should succeed");
    let px1 = exec
        .read_texture_rgba8(RT1)
        .await
        .expect("readback should succeed");

    assert_eq!(read_center(&px0), [0, 255, 0, 255]);
    assert_eq!(read_center(&px1), [0, 255, 0, 255]);
}

#[test]
fn aerogpu_cmd_geometry_shader_line_list_adj_translated_prepass() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::aerogpu_cmd_geometry_shader_line_list_adj_translated_prepass"
        );
        // D3D primitive topology token for lineadj is 10 (matches `CmdPrimitiveTopology::LineListAdj`).
        run_adj_test(
            test_name,
            AerogpuPrimitiveTopology::LineListAdj,
            10,
            4,
            3,
        )
        .await;
    });
}

#[test]
fn aerogpu_cmd_geometry_shader_triangle_list_adj_translated_prepass() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::aerogpu_cmd_geometry_shader_triangle_list_adj_translated_prepass"
        );
        // D3D primitive topology token for triadj is 12 (matches `CmdPrimitiveTopology::TriangleListAdj`).
        run_adj_test(
            test_name,
            AerogpuPrimitiveTopology::TriangleListAdj,
            12,
            6,
            5,
        )
        .await;
    });
}
