mod common;

use aero_d3d11::input_layout::{
    fnv1a_32, AEROGPU_INPUT_LAYOUT_BLOB_MAGIC, AEROGPU_INPUT_LAYOUT_BLOB_VERSION,
};
use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_d3d11::sm4::opcode::*;
use aero_d3d11::{FourCC, Swizzle, WriteMask};
use aero_dxbc::test_utils as dxbc_test_utils;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCullMode, AerogpuFillMode, AerogpuShaderStage, AerogpuShaderStageEx,
    AerogpuVertexBufferBinding, AEROGPU_CLEAR_COLOR, AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
    AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

// This test exercises draw instancing (DrawInstanced) semantics in the translator-backed geometry
// shader compute prepass path.
//
// The bug fixed by D11-003b is that the GS compute path previously ran only per-primitive (no
// per-instance dimension) and that the `gs_inputs` fill shader treated per-instance attributes as
// if they were indexed by `first_instance` only.

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
    out.extend_from_slice(&[
        v[0].to_bits(),
        v[1].to_bits(),
        v[2].to_bits(),
        v[3].to_bits(),
    ]);
    out
}

fn build_ilay_pos3_offset_instance_step_rate(step_rate: u32) -> Vec<u8> {
    // `struct aerogpu_input_layout_blob_header` + 2x `struct aerogpu_input_layout_element_dxgi`.
    //
    // DXGI_FORMAT_R32G32B32_FLOAT = 6.
    // DXGI_FORMAT_R32_FLOAT = 41.
    let mut out = Vec::new();
    out.extend_from_slice(&AEROGPU_INPUT_LAYOUT_BLOB_MAGIC.to_le_bytes());
    out.extend_from_slice(&AEROGPU_INPUT_LAYOUT_BLOB_VERSION.to_le_bytes());
    out.extend_from_slice(&2u32.to_le_bytes()); // element_count
    out.extend_from_slice(&0u32.to_le_bytes()); // reserved0

    let pos_hash = fnv1a_32(b"POSITION");
    let off_hash = fnv1a_32(b"OFFSET");

    // POSITION0: float3, slot 0, per-vertex.
    out.extend_from_slice(&pos_hash.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // semantic_index
    out.extend_from_slice(&6u32.to_le_bytes()); // dxgi_format (R32G32B32_FLOAT)
    out.extend_from_slice(&0u32.to_le_bytes()); // input_slot
    out.extend_from_slice(&0u32.to_le_bytes()); // aligned_byte_offset
    out.extend_from_slice(&0u32.to_le_bytes()); // input_slot_class (per-vertex)
    out.extend_from_slice(&0u32.to_le_bytes()); // instance_data_step_rate

    // OFFSET0: float1, slot 1, per-instance.
    out.extend_from_slice(&off_hash.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // semantic_index
    out.extend_from_slice(&41u32.to_le_bytes()); // dxgi_format (R32_FLOAT)
    out.extend_from_slice(&1u32.to_le_bytes()); // input_slot
    out.extend_from_slice(&0u32.to_le_bytes()); // aligned_byte_offset
    out.extend_from_slice(&1u32.to_le_bytes()); // input_slot_class (per-instance)
    out.extend_from_slice(&step_rate.to_le_bytes()); // instance_data_step_rate

    out
}

fn build_vs_pos3_passthrough_with_offset_input_dxbc() -> Vec<u8> {
    // vs_4_0:
    //   mov o0, v0
    //   mov o1.x, v1.x
    //   ret
    let isgn = build_signature_chunk(&[
        SigParam {
            semantic_name: "POSITION",
            semantic_index: 0,
            register: 0,
            mask: 0x07, // xyz
        },
        SigParam {
            semantic_name: "OFFSET",
            semantic_index: 0,
            register: 1,
            mask: 0x01, // x
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
            semantic_name: "OFFSET",
            semantic_index: 0,
            register: 1,
            mask: 0x01,
        },
    ]);

    let mut body = Vec::<u32>::new();

    // mov o0, v0
    let mut inst = vec![0u32];
    inst.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    inst.extend_from_slice(&reg_src(OPERAND_TYPE_INPUT, &[0], Swizzle::XYZW));
    inst[0] = opcode_token(OPCODE_MOV, inst.len() as u32);
    body.extend_from_slice(&inst);

    // mov o1.x, v1.x
    let mut inst = vec![0u32];
    inst.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 1, WriteMask::X));
    inst.extend_from_slice(&reg_src(OPERAND_TYPE_INPUT, &[1], Swizzle::XXXX));
    inst[0] = opcode_token(OPCODE_MOV, inst.len() as u32);
    body.extend_from_slice(&inst);

    body.push(opcode_token(OPCODE_RET, 1));

    let version = 0x0001_0040u32; // vs_4_0
    let mut tokens = Vec::with_capacity(2 + body.len());
    tokens.push(version);
    tokens.push(0); // length patched below
    tokens.extend_from_slice(&body);
    tokens[1] = tokens.len() as u32;

    let shdr = tokens_to_bytes(&tokens);
    build_dxbc(&[
        (FOURCC_ISGN, isgn),
        (FOURCC_OSGN, osgn),
        (FOURCC_SHDR, shdr),
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

    let mut body = Vec::<u32>::new();

    // mov o0, imm
    let mut inst = vec![0u32];
    inst.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    inst.extend_from_slice(&imm_f32x4([0.0, 1.0, 0.0, 1.0]));
    inst[0] = opcode_token(OPCODE_MOV, inst.len() as u32);
    body.extend_from_slice(&inst);

    body.push(opcode_token(OPCODE_RET, 1));

    let version = 0x0000_0040u32; // ps_4_0
    let mut tokens = Vec::with_capacity(2 + body.len());
    tokens.push(version);
    tokens.push(0);
    tokens.extend_from_slice(&body);
    tokens[1] = tokens.len() as u32;

    let shdr = tokens_to_bytes(&tokens);
    build_dxbc(&[
        (FOURCC_ISGN, isgn),
        (FOURCC_OSGN, osgn),
        (FOURCC_SHDR, shdr),
    ])
}

fn build_gs_point_to_triangle_with_instance_offset_dxbc() -> Vec<u8> {
    // gs_4_0:
    //   dcl_inputprimitive point
    //   dcl_outputtopology triangle_strip
    //   dcl_maxvertexcount 3
    //
    //   r0 = v0[0]            (base position)
    //   r0.x += v1[0].x       (per-draw-instance offset)
    //   emit 3 verts forming a triangle around r0
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
            semantic_name: "OFFSET",
            semantic_index: 0,
            register: 1,
            mask: 0x01,
        },
    ]);
    let osgn = build_signature_chunk(&[SigParam {
        semantic_name: "SV_Position",
        semantic_index: 0,
        register: 0,
        mask: 0x0f,
    }]);

    // Likely values from `d3d10tokenizedprogramformat.h`:
    // - primitive: point = 1
    // - output topology: triangle_strip = 5
    const PRIM_POINT: u32 = 1;
    const TOPO_TRIANGLE_STRIP: u32 = 5;
    const MAX_VERTS: u32 = 3;

    // A real DXBC stream would use specific `dcl_input`/`dcl_output` opcode IDs. The GS translator
    // only needs these to decode as declarations, so the exact numeric values are not important for
    // this test.
    const DCL_DUMMY: u32 = 0x300;

    let mut body = Vec::<u32>::new();
    body.extend_from_slice(&[
        opcode_token(OPCODE_DCL_GS_INPUT_PRIMITIVE, 2),
        PRIM_POINT,
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

    // mov r0, v0[0]
    let mut inst = vec![0u32];
    inst.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    inst.extend_from_slice(&reg_src(OPERAND_TYPE_INPUT, &[0, 0], Swizzle::XYZW));
    inst[0] = opcode_token(OPCODE_MOV, inst.len() as u32);
    body.extend_from_slice(&inst);

    // add r0.x, r0.x, v1[0].x
    let mut inst = vec![0u32];
    inst.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::X));
    inst.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, &[0], Swizzle::XXXX));
    inst.extend_from_slice(&reg_src(OPERAND_TYPE_INPUT, &[1, 0], Swizzle::XXXX));
    inst[0] = opcode_token(OPCODE_ADD, inst.len() as u32);
    body.extend_from_slice(&inst);

    let s = 0.25f32;
    let verts = [[0.0f32, s, 0.0, 0.0], [-s, -s, 0.0, 0.0], [s, -s, 0.0, 0.0]];
    for off in verts {
        // add o0, r0, imm
        let mut inst = vec![0u32];
        inst.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
        inst.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, &[0], Swizzle::XYZW));
        inst.extend_from_slice(&imm_f32x4(off));
        inst[0] = opcode_token(OPCODE_ADD, inst.len() as u32);
        body.extend_from_slice(&inst);

        body.push(opcode_token(OPCODE_EMIT, 1));
    }

    body.push(opcode_token(OPCODE_CUT, 1));
    body.push(opcode_token(OPCODE_RET, 1));

    let version = 0x0002_0040u32; // gs_4_0
    let mut tokens = Vec::with_capacity(2 + body.len());
    tokens.push(version);
    tokens.push(0);
    tokens.extend_from_slice(&body);
    tokens[1] = tokens.len() as u32;

    let shdr = tokens_to_bytes(&tokens);
    build_dxbc(&[
        (FOURCC_ISGN, isgn),
        (FOURCC_OSGN, osgn),
        (FOURCC_SHDR, shdr),
    ])
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct VertexPos3 {
    pos: [f32; 3],
}

#[test]
fn aerogpu_cmd_geometry_shader_compute_prepass_instancing_smoke() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::aerogpu_cmd_geometry_shader_compute_prepass_instancing_smoke"
        );
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(test_name, &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };
        if !exec.supports_compute() {
            common::skip_or_panic(test_name, "compute unsupported");
            return;
        }
        if !exec.capabilities().supports_indirect_execution {
            common::skip_or_panic(test_name, "indirect unsupported");
            return;
        }
        if !common::require_gs_prepass_or_skip(&exec, test_name) {
            return;
        }
        // The translator-backed GS prepass uses 4 storage buffers in its compute bind group:
        // out_vertices, out_indices, out_state (indirect args + counters), and gs_inputs.
        //
        // `new_for_tests()` requests `wgpu::Limits::downlevel_defaults()`, which can clamp this to 4
        // on some backends. Skip rather than failing with a wgpu validation error.
        let max_storage = exec.device().limits().max_storage_buffers_per_shader_stage;
        if max_storage < 4 {
            common::skip_or_panic(
                test_name,
                &format!(
                    "translator GS prepass requires 4 storage buffers per shader stage, but device limit is {max_storage}"
                ),
            );
            return;
        }

        const VB_POS: u32 = 1;
        const VB_OFF: u32 = 2;
        const RT: u32 = 3;
        const VS: u32 = 4;
        const GS: u32 = 5;
        const PS: u32 = 6;
        const IL: u32 = 7;

        let vertex = VertexPos3 {
            pos: [0.0, 0.0, 0.0],
        };
        let offsets: [f32; 3] = [
            -0.5, // instance 0/1 (step_rate=2)
            0.5,  // instance 2 (step_rate=2)
            0.0,  // unused if step_rate is honored; becomes visible if step_rate is ignored
        ];

        let mut writer = AerogpuCmdWriter::new();
        writer.create_buffer(
            VB_POS,
            AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
            core::mem::size_of_val(&vertex) as u64,
            0,
            0,
        );
        writer.upload_resource(VB_POS, 0, bytemuck::bytes_of(&vertex));

        writer.create_buffer(
            VB_OFF,
            AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
            core::mem::size_of_val(&offsets) as u64,
            0,
            0,
        );
        writer.upload_resource(VB_OFF, 0, bytemuck::cast_slice(&offsets));

        let w = 64u32;
        let h = 64u32;
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

        writer.create_shader_dxbc(
            VS,
            AerogpuShaderStage::Vertex,
            &build_vs_pos3_passthrough_with_offset_input_dxbc(),
        );
        writer.create_shader_dxbc_ex(
            GS,
            AerogpuShaderStageEx::Geometry,
            &build_gs_point_to_triangle_with_instance_offset_dxbc(),
        );
        writer.create_shader_dxbc(PS, AerogpuShaderStage::Pixel, &build_ps_solid_green_dxbc());

        writer.create_input_layout(IL, &build_ilay_pos3_offset_instance_step_rate(2));
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
                    buffer: VB_OFF,
                    stride_bytes: 4,
                    offset_bytes: 0,
                    reserved0: 0,
                },
            ],
        );

        writer.set_primitive_topology(
            aero_protocol::aerogpu::aerogpu_cmd::AerogpuPrimitiveTopology::PointList,
        );
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

        // Clear to red; the pixel shader outputs solid green for any covered fragment.
        writer.clear(AEROGPU_CLEAR_COLOR, [1.0, 0.0, 0.0, 1.0], 1.0, 0);
        // 3 instances with step_rate=2 should produce:
        // - instances 0 and 1 at x=-0.5 (same per-instance element)
        // - instance 2 at x=+0.5
        writer.draw(1, 3, 0, 0);

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
            px(w / 4, y_mid),
            [0, 255, 0, 255],
            "left instance pixel mismatch"
        );
        assert_eq!(
            px(3 * w / 4, y_mid),
            [0, 255, 0, 255],
            "right instance pixel mismatch"
        );
        assert_eq!(
            px(w / 2, y_mid),
            [255, 0, 0, 255],
            "center pixel should remain clear (instance step rate/instance-id indexing broken?)"
        );
    });
}
