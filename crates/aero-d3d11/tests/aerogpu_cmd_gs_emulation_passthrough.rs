mod common;

use aero_d3d11::binding_model::EXPANDED_VERTEX_MAX_VARYINGS;
use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_d3d11::sm4::opcode::*;
use aero_d3d11::{Swizzle, WriteMask};
use aero_dxbc::{test_utils as dxbc_test_utils, FourCC};
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCullMode, AerogpuFillMode, AerogpuPrimitiveTopology, AerogpuShaderStage,
    AEROGPU_CLEAR_COLOR, AEROGPU_RESOURCE_USAGE_RENDER_TARGET, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

const DXBC_VS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/vs_passthrough.dxbc");

const FOURCC_ISGN: FourCC = FourCC(*b"ISGN");
const FOURCC_OSGN: FourCC = FourCC(*b"OSGN");
const FOURCC_SHDR: FourCC = FourCC(*b"SHDR");

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
        })
        .collect();
    dxbc_test_utils::build_signature_chunk_v0(&entries)
}

fn build_dxbc(chunks: &[(FourCC, Vec<u8>)]) -> Vec<u8> {
    dxbc_test_utils::build_container_owned(chunks)
}

fn build_ps_sum_varyings_dxbc(varying_count: u32) -> Vec<u8> {
    assert!(varying_count >= 2);

    let mut isgn_params = Vec::new();
    for reg in 1..=varying_count {
        isgn_params.push(SigParam {
            semantic_name: "TEXCOORD",
            semantic_index: reg - 1,
            register: reg,
            mask: 0x0f,
        });
    }
    let isgn = build_signature_chunk(&isgn_params);
    let osgn = build_signature_chunk(&[SigParam {
        semantic_name: "SV_Target",
        semantic_index: 0,
        register: 0,
        mask: 0x0f,
    }]);

    let mut body = Vec::<u32>::new();

    // add r0, v1, v2
    let mut inst = vec![0u32];
    inst.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    inst.extend_from_slice(&reg_src(OPERAND_TYPE_INPUT, &[1], Swizzle::XYZW));
    inst.extend_from_slice(&reg_src(OPERAND_TYPE_INPUT, &[2], Swizzle::XYZW));
    inst[0] = opcode_token(OPCODE_ADD, inst.len() as u32);
    body.extend_from_slice(&inst);

    for reg in 3..=varying_count {
        let mut inst = vec![0u32];
        inst.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
        inst.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, &[0], Swizzle::XYZW));
        inst.extend_from_slice(&reg_src(OPERAND_TYPE_INPUT, &[reg], Swizzle::XYZW));
        inst[0] = opcode_token(OPCODE_ADD, inst.len() as u32);
        body.extend_from_slice(&inst);
    }

    // mov o0, r0
    let mut inst = vec![0u32];
    inst.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    inst.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, &[0], Swizzle::XYZW));
    inst[0] = opcode_token(OPCODE_MOV, inst.len() as u32);
    body.extend_from_slice(&inst);

    body.push(opcode_token(OPCODE_RET, 1));

    let version = 0x0000_0040u32; // ps_4_0
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

fn push_f32(out: &mut Vec<u8>, v: f32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_vec4(out: &mut Vec<u8>, v: [f32; 4]) {
    for f in v {
        push_f32(out, f);
    }
}

fn push_expanded_vertex(out: &mut Vec<u8>, pos: [f32; 4], varyings_ones: &[u32]) {
    // Matches `runtime/wgsl_link.rs` `ExpandedVertex`:
    //   pos: vec4<f32>
    //   varyings: array<vec4<f32>, 32>
    push_vec4(out, pos);
    for loc in 0..EXPANDED_VERTEX_MAX_VARYINGS {
        let v = if varyings_ones.contains(&loc) {
            [1.0, 0.0, 0.0, 1.0]
        } else {
            [0.0; 4]
        };
        push_vec4(out, v);
    }
}

fn assert_all_pixels_eq(pixels: &[u8], expected: [u8; 4]) {
    assert_eq!(pixels.len() % 4, 0, "pixel buffer must be RGBA8");
    for (i, px) in pixels.chunks_exact(4).enumerate() {
        assert_eq!(px, expected, "pixel mismatch at index {i}");
    }
}

#[test]
fn aerogpu_cmd_gs_emulation_passthrough_vs_supports_more_than_16_varyings_without_vertex_attributes(
) {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        if !exec.caps().supports_compute {
            common::skip_or_panic(module_path!(), "storage buffers unsupported (no compute)");
            return;
        }

        // Using 16 `vec4<f32>` varyings requires 64 user-defined inter-stage components.
        // Older/downlevel backends can expose only the WebGPU minimum (60), so skip when the device
        // cannot support the test shader.
        let required_components = 16u32 * 4;
        let max_components = exec.device().limits().max_inter_stage_shader_components;
        if max_components < required_components {
            common::skip_or_panic(
                module_path!(),
                &format!(
                    "max_inter_stage_shader_components too low ({max_components} < {required_components})"
                ),
            );
            return;
        }

        const EXPANDED_VB: u32 = 1;
        const RT: u32 = 2;
        const VS: u32 = 3;
        const PS: u32 = 4;

        let varyings: Vec<u32> = (1u32..=16).collect();

        let mut expanded = Vec::new();
        // Fullscreen triangle in clip-space.
        push_expanded_vertex(&mut expanded, [-1.0, -1.0, 0.0, 1.0], &varyings);
        push_expanded_vertex(&mut expanded, [-1.0, 3.0, 0.0, 1.0], &varyings);
        push_expanded_vertex(&mut expanded, [3.0, -1.0, 0.0, 1.0], &varyings);

        let ps_dxbc = build_ps_sum_varyings_dxbc(16);

        let mut setup = AerogpuCmdWriter::new();
        setup.create_buffer(
            EXPANDED_VB,
            AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
            expanded.len() as u64,
            0,
            0,
        );
        setup.upload_resource(EXPANDED_VB, 0, &expanded);

        setup.create_texture2d(
            RT,
            AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
            AerogpuFormat::R8G8B8A8Unorm as u32,
            16,
            16,
            1,
            1,
            0,
            0,
            0,
        );

        setup.create_shader_dxbc(VS, AerogpuShaderStage::Vertex, DXBC_VS_PASSTHROUGH);
        setup.create_shader_dxbc(PS, AerogpuShaderStage::Pixel, &ps_dxbc);

        let setup_stream = setup.finish();
        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&setup_stream, None, &mut guest_mem)
            .expect("setup cmd stream should succeed");

        // Enable the emulated vertex-pulling path and point it at our expanded vertex buffer.
        exec.set_emulated_expanded_vertex_buffer(Some(EXPANDED_VB));

        let mut draw = AerogpuCmdWriter::new();
        draw.bind_shaders(VS, PS, 0);
        draw.set_render_targets(&[RT], 0);
        draw.set_viewport(0.0, 0.0, 16.0, 16.0, 0.0, 1.0);
        draw.clear(AEROGPU_CLEAR_COLOR, [0.0, 0.0, 0.0, 1.0], 1.0, 0);
        draw.set_primitive_topology(AerogpuPrimitiveTopology::TriangleList);
        draw.set_rasterizer_state_ext(
            AerogpuFillMode::Solid,
            AerogpuCullMode::None,
            false,
            false,
            0,
            false,
        );
        draw.draw(3, 1, 0, 0);
        let draw_stream = draw.finish();

        exec.execute_cmd_stream(&draw_stream, None, &mut guest_mem)
            .expect("draw cmd stream should succeed");
        exec.poll_wait();

        let pixels = exec
            .read_texture_rgba8(RT)
            .await
            .expect("readback should succeed");
        assert_all_pixels_eq(&pixels, [255, 0, 0, 255]);
    });
}
