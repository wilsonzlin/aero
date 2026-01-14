mod common;

use aero_d3d11::input_layout::{
    fnv1a_32, AEROGPU_INPUT_LAYOUT_BLOB_MAGIC, AEROGPU_INPUT_LAYOUT_BLOB_VERSION,
};
use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_d3d11::sm4::opcode::*;
use aero_d3d11::{DxbcFile, FourCC, Sm4Program, Swizzle, WriteMask};
use aero_dxbc::test_utils as dxbc_test_utils;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCullMode, AerogpuFillMode, AerogpuPrimitiveTopology, AerogpuShaderStage,
    AerogpuVertexBufferBinding, AEROGPU_CLEAR_COLOR, AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
    AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

// Regression: VS-as-compute must respect D3D11 per-instance step-rate when pulling IA inputs, even
// in the GS prepass path.
//
// We draw a single point with `instance_count=1` and `first_instance=1` using an input layout that
// supplies a per-instance OFFSET attribute with `instance_data_step_rate=2`.
//
// Correct behavior: element_index = floor(instance_id / step_rate) = floor(1/2) = 0, so VS reads
// OFFSET[0] (+1.0 on X) and the emitted quad lands far right, leaving the center pixel untouched.
//
// Broken behavior (no step-rate division): VS reads OFFSET[1] (0.0), so the quad is emitted at the
// original position and covers the center pixel.

const GS_CUT: &[u8] = include_bytes!("fixtures/gs_cut.dxbc");
const PS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/ps_passthrough.dxbc");

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

fn build_vs_add_instance_offset_and_passthrough_color_dxbc() -> Vec<u8> {
    // vs_4_0:
    //   o0 = v0 + v2   (apply per-instance clip-space offset)
    //   o1 = v1        (pass color)
    //   ret
    let isgn = build_signature_chunk(&[
        SigParam {
            semantic_name: "POSITION",
            semantic_index: 0,
            register: 0,
            mask: 0x07,
        },
        SigParam {
            semantic_name: "COLOR",
            semantic_index: 0,
            register: 1,
            mask: 0x0f,
        },
        SigParam {
            semantic_name: "OFFSET",
            semantic_index: 0,
            register: 2,
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

    let mut body = Vec::<u32>::new();

    // add o0, v0, v2
    let mut inst = vec![0u32];
    inst.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    inst.extend_from_slice(&reg_src(OPERAND_TYPE_INPUT, &[0], Swizzle::XYZW));
    inst.extend_from_slice(&reg_src(OPERAND_TYPE_INPUT, &[2], Swizzle::XYZW));
    inst[0] = opcode_token(OPCODE_ADD, inst.len() as u32);
    body.extend_from_slice(&inst);

    // mov o1, v1
    let mut inst = vec![0u32];
    inst.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 1, WriteMask::XYZW));
    inst.extend_from_slice(&reg_src(OPERAND_TYPE_INPUT, &[1], Swizzle::XYZW));
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

fn push_u32(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_le_bytes());
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct VertexPos3Color4 {
    pos: [f32; 3],
    color: [f32; 4],
}

#[test]
fn aerogpu_cmd_geometry_shader_vs_as_compute_respects_instance_step_rate() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::aerogpu_cmd_geometry_shader_vs_as_compute_respects_instance_step_rate"
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

        // ILAY: POSITION (per-vertex), COLOR (per-vertex), OFFSET (per-instance step rate 2).
        //
        // DXGI format numbers come from `dxgiformat.h`:
        // - DXGI_FORMAT_R32G32B32_FLOAT = 6
        // - DXGI_FORMAT_R32G32B32A32_FLOAT = 2
        let mut ilay = Vec::new();
        push_u32(&mut ilay, AEROGPU_INPUT_LAYOUT_BLOB_MAGIC);
        push_u32(&mut ilay, AEROGPU_INPUT_LAYOUT_BLOB_VERSION);
        push_u32(&mut ilay, 3); // element_count
        push_u32(&mut ilay, 0); // reserved0
                                // POSITION0: slot0 offset0, per-vertex.
        push_u32(&mut ilay, fnv1a_32(b"POSITION"));
        push_u32(&mut ilay, 0); // semantic_index
        push_u32(&mut ilay, 6); // DXGI_FORMAT_R32G32B32_FLOAT
        push_u32(&mut ilay, 0); // input_slot
        push_u32(&mut ilay, 0); // aligned_byte_offset
        push_u32(&mut ilay, 0); // per-vertex
        push_u32(&mut ilay, 0); // instance_data_step_rate
                                // COLOR0: slot0 offset12, per-vertex.
        push_u32(&mut ilay, fnv1a_32(b"COLOR"));
        push_u32(&mut ilay, 0);
        push_u32(&mut ilay, 2); // DXGI_FORMAT_R32G32B32A32_FLOAT
        push_u32(&mut ilay, 0);
        push_u32(&mut ilay, 12);
        push_u32(&mut ilay, 0);
        push_u32(&mut ilay, 0);
        // OFFSET0: slot1 offset0, per-instance, step rate 2.
        push_u32(&mut ilay, fnv1a_32(b"OFFSET"));
        push_u32(&mut ilay, 0);
        push_u32(&mut ilay, 2); // DXGI_FORMAT_R32G32B32A32_FLOAT
        push_u32(&mut ilay, 1);
        push_u32(&mut ilay, 0);
        push_u32(&mut ilay, 1); // per-instance
        push_u32(&mut ilay, 2); // instance_data_step_rate

        const VB_VERTEX: u32 = 1;
        const VB_INSTANCE: u32 = 2;
        const RT: u32 = 3;
        const VS: u32 = 4;
        const GS: u32 = 5;
        const PS: u32 = 6;
        const IL: u32 = 7;

        let vertex = VertexPos3Color4 {
            pos: [0.0, 0.0, 0.0],
            color: [0.0, 1.0, 0.0, 1.0],
        };
        let vb_vertex_bytes = bytemuck::bytes_of(&vertex);
        let instance_offsets: [[f32; 4]; 2] = [
            [1.0, 0.0, 0.0, 0.0], // element0: shift far right
            [0.0, 0.0, 0.0, 0.0], // element1: no shift
        ];
        let vb_instance_bytes = bytemuck::bytes_of(&instance_offsets);

        let mut writer = AerogpuCmdWriter::new();
        writer.create_buffer(
            VB_VERTEX,
            AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
            vb_vertex_bytes.len() as u64,
            0,
            0,
        );
        writer.upload_resource(VB_VERTEX, 0, vb_vertex_bytes);
        writer.create_buffer(
            VB_INSTANCE,
            AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
            vb_instance_bytes.len() as u64,
            0,
            0,
        );
        writer.upload_resource(VB_INSTANCE, 0, vb_instance_bytes);

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

        writer.set_rasterizer_state(
            AerogpuFillMode::Solid,
            AerogpuCullMode::None,
            false,
            false,
            0,
            0,
        );

        let vs_dxbc = build_vs_add_instance_offset_and_passthrough_color_dxbc();
        // Sanity-check the DXBC decodes into an SM4 module (required by VS-as-compute path).
        {
            let dxbc = DxbcFile::parse(&vs_dxbc).expect("VS DXBC should parse");
            let program = Sm4Program::parse_from_dxbc(&dxbc).expect("VS DXBC should decode");
            let module =
                aero_d3d11::sm4::decode_program(&program).expect("VS SM4 decode should work");
            let _ = module;
        }
        writer.create_shader_dxbc(VS, AerogpuShaderStage::Vertex, &vs_dxbc);
        writer.create_shader_dxbc(GS, AerogpuShaderStage::Geometry, GS_CUT);
        writer.create_shader_dxbc(PS, AerogpuShaderStage::Pixel, PS_PASSTHROUGH);

        writer.create_input_layout(IL, &ilay);
        writer.set_input_layout(IL);

        writer.set_vertex_buffers(
            0,
            &[
                AerogpuVertexBufferBinding {
                    buffer: VB_VERTEX,
                    stride_bytes: core::mem::size_of::<VertexPos3Color4>() as u32,
                    offset_bytes: 0,
                    reserved0: 0,
                },
                AerogpuVertexBufferBinding {
                    buffer: VB_INSTANCE,
                    stride_bytes: 16,
                    offset_bytes: 0,
                    reserved0: 0,
                },
            ],
        );
        writer.set_primitive_topology(AerogpuPrimitiveTopology::PointList);
        writer.bind_shaders_ex(VS, PS, 0, GS, 0, 0);

        writer.clear(AEROGPU_CLEAR_COLOR, [1.0, 0.0, 0.0, 1.0], 1.0, 0);
        // Draw a single instance with non-zero `first_instance` to exercise step-rate division.
        writer.draw(1, 1, 0, 1);

        let stream = writer.finish();

        let mut guest_mem = VecGuestMemory::new(0);
        if let Err(err) = exec.execute_cmd_stream(&stream, None, &mut guest_mem) {
            if common::skip_if_compute_or_indirect_unsupported(test_name, &err) {
                return;
            }
            if err.to_string().contains("INDIRECT_FIRST_INSTANCE") {
                common::skip_or_panic(test_name, "indirect-first-instance unsupported");
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

        // The quad should be emitted far right, leaving the center pixel untouched (red).
        assert_eq!(px(w / 2, h / 2), [255, 0, 0, 255], "center pixel mismatch");
        // Ensure something drew: a pixel near the right edge should be the vertex color (green).
        assert_eq!(
            px(w - 4, h / 2),
            [0, 255, 0, 255],
            "right-edge pixel mismatch"
        );
    });
}
