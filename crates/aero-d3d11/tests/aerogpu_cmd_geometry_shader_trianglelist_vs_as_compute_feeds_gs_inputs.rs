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
    AerogpuCullMode, AerogpuFillMode, AerogpuPrimitiveTopology, AerogpuShaderStage,
    AerogpuVertexBufferBinding, AEROGPU_CLEAR_COLOR, AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
    AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

// Regression: translated GS triangle-list compute prepass must feed GS inputs from VS outputs.
//
// The VS shifts the triangle right by +1 on X. With correct VS-as-compute feeding, the triangle no
// longer covers the center pixel (so it stays red). If the prepass reads IA directly (or only
// partially populates v#[]), the triangle remains centered and the center pixel turns green.

const GS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/gs_passthrough.dxbc");
const ILAY_POS3_COLOR: &[u8] = include_bytes!("fixtures/ilay_pos3_color.bin");

const FOURCC_ISGN: FourCC = FourCC(*b"ISGN");
const FOURCC_OSGN: FourCC = FourCC(*b"OSGN");
const FOURCC_SHDR: FourCC = FourCC(*b"SHDR");

fn push_u32(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_le_bytes());
}

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

fn build_vs_shift_x_only_dxbc() -> Vec<u8> {
    // vs_4_0:
    //   o0 = v0 + float4(1,0,0,0)
    //   o1 = v1
    //   ret
    //
    // Include (and write) a COLOR input/output so VS-as-compute input feeding is exercised even when
    // the GS reads only `v0` and the VS writes extra output regs (`o1`).
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

    // add o0, v0, imm
    let mut inst = vec![0u32];
    inst.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    inst.extend_from_slice(&reg_src(OPERAND_TYPE_INPUT, &[0], Swizzle::XYZW));
    inst.extend_from_slice(&imm_f32x4([1.0, 0.0, 0.0, 0.0]));
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

fn build_vs_shift_x_and_passthrough_color_dxbc() -> Vec<u8> {
    // vs_4_0:
    //   o0 = v0 + float4(1,0,0,0)
    //   o1 = v1
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

    // add o0, v0, imm
    let mut inst = vec![0u32];
    inst.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    inst.extend_from_slice(&reg_src(OPERAND_TYPE_INPUT, &[0], Swizzle::XYZW));
    inst.extend_from_slice(&imm_f32x4([1.0, 0.0, 0.0, 0.0]));
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

fn build_vs_shift_x_from_instance_color_step2_dxbc() -> Vec<u8> {
    // vs_4_0:
    //   o0 = v0 + v1.xyyy  (COLOR0.x supplies shift; y=0 ensures we don't modify y/z/w)
    //   ret
    //
    // COLOR0 is expected to be provided via per-instance vertex data with step rate 2.
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
            mask: 0x01,
        },
    ]);
    let osgn = build_signature_chunk(&[SigParam {
        semantic_name: "SV_Position",
        semantic_index: 0,
        register: 0,
        mask: 0x0f,
    }]);

    let mut body = Vec::<u32>::new();

    // add o0, v0, v1.xyyy
    let mut inst = vec![0u32];
    inst.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    inst.extend_from_slice(&reg_src(OPERAND_TYPE_INPUT, &[0], Swizzle::XYZW));
    inst.extend_from_slice(&reg_src(OPERAND_TYPE_INPUT, &[1], Swizzle([0, 1, 1, 1])));
    inst[0] = opcode_token(OPCODE_ADD, inst.len() as u32);
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

fn build_ilay_pos3_instance_color_step2() -> Vec<u8> {
    let mut blob = Vec::new();
    push_u32(&mut blob, AEROGPU_INPUT_LAYOUT_BLOB_MAGIC);
    push_u32(&mut blob, AEROGPU_INPUT_LAYOUT_BLOB_VERSION);
    push_u32(&mut blob, 2); // element_count
    push_u32(&mut blob, 0); // reserved0

    // POSITION0: R32G32B32_FLOAT, slot 0, per-vertex.
    push_u32(&mut blob, fnv1a_32(b"POSITION"));
    push_u32(&mut blob, 0); // semantic index
    push_u32(&mut blob, 6); // DXGI_FORMAT_R32G32B32_FLOAT
    push_u32(&mut blob, 0); // input_slot
    push_u32(&mut blob, 0); // aligned_byte_offset
    push_u32(&mut blob, 0); // per-vertex
    push_u32(&mut blob, 0); // step rate

    // COLOR0: R32_FLOAT, slot 1, per-instance, step_rate=2.
    push_u32(&mut blob, fnv1a_32(b"COLOR"));
    push_u32(&mut blob, 0); // semantic index
    push_u32(&mut blob, 41); // DXGI_FORMAT_R32_FLOAT
    push_u32(&mut blob, 1); // input_slot
    push_u32(&mut blob, 0); // aligned_byte_offset
    push_u32(&mut blob, 1); // per-instance
    push_u32(&mut blob, 2); // instance_data_step_rate

    blob
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct VertexPos3Color4 {
    pos: [f32; 3],
    color: [f32; 4],
}

#[test]
fn aerogpu_cmd_geometry_shader_trianglelist_vs_as_compute_feeds_gs_inputs() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::aerogpu_cmd_geometry_shader_trianglelist_vs_as_compute_feeds_gs_inputs"
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
                pos: [0.0, 0.5, 0.0],
                color: [0.0, 0.0, 0.0, 1.0],
            },
            VertexPos3Color4 {
                pos: [0.5, -0.5, 0.0],
                color: [0.0, 0.0, 0.0, 1.0],
            },
        ];
        let vb_bytes = bytemuck::bytes_of(&vertices);

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

        // Disable culling so output is visible regardless of winding.
        writer.set_rasterizer_state(
            AerogpuFillMode::Solid,
            AerogpuCullMode::None,
            false,
            false,
            0,
            0,
        );

        writer.create_shader_dxbc(
            VS,
            AerogpuShaderStage::Vertex,
            &build_vs_shift_x_only_dxbc(),
        );
        writer.create_shader_dxbc(GS, AerogpuShaderStage::Geometry, GS_PASSTHROUGH);
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
        writer.set_primitive_topology(AerogpuPrimitiveTopology::TriangleList);
        writer.bind_shaders_ex(VS, PS, 0, GS, 0, 0);

        writer.clear(AEROGPU_CLEAR_COLOR, [1.0, 0.0, 0.0, 1.0], 1.0, 0);
        writer.draw(3, 1, 0, 0);

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

        // With correct VS-as-compute feeding, the triangle shifts far right and the center pixel
        // remains red.
        assert_eq!(px(w / 2, h / 2), [255, 0, 0, 255], "center pixel mismatch");
        // Ensure something drew: a pixel near the right edge should be green.
        assert_eq!(
            px(w - 4, h / 2),
            [0, 255, 0, 255],
            "right-edge pixel mismatch"
        );
    });
}

#[test]
fn aerogpu_cmd_geometry_shader_trianglelist_vs_as_compute_allows_extra_vs_outputs() {
    // Regression: VS-as-compute feeding should tolerate the VS writing output registers that the
    // GS does not consume. D3D11 allows this; only the subset of VS outputs referenced by the GS
    // should be stored in the `gs_inputs` register file.
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::aerogpu_cmd_geometry_shader_trianglelist_vs_as_compute_allows_extra_vs_outputs"
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
                pos: [0.0, 0.5, 0.0],
                color: [0.0, 0.0, 0.0, 1.0],
            },
            VertexPos3Color4 {
                pos: [0.5, -0.5, 0.0],
                color: [0.0, 0.0, 0.0, 1.0],
            },
        ];
        let vb_bytes = bytemuck::bytes_of(&vertices);

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

        // Disable culling so output is visible regardless of winding.
        writer.set_rasterizer_state(
            AerogpuFillMode::Solid,
            AerogpuCullMode::None,
            false,
            false,
            0,
            0,
        );

        writer.create_shader_dxbc(
            VS,
            AerogpuShaderStage::Vertex,
            &build_vs_shift_x_and_passthrough_color_dxbc(),
        );
        writer.create_shader_dxbc(GS, AerogpuShaderStage::Geometry, GS_PASSTHROUGH);
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
        writer.set_primitive_topology(AerogpuPrimitiveTopology::TriangleList);
        writer.bind_shaders_ex(VS, PS, 0, GS, 0, 0);

        writer.clear(AEROGPU_CLEAR_COLOR, [1.0, 0.0, 0.0, 1.0], 1.0, 0);
        writer.draw(3, 1, 0, 0);

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

        // The triangle should be shifted far right, leaving the center pixel untouched (red).
        assert_eq!(px(w / 2, h / 2), [255, 0, 0, 255], "center pixel mismatch");
        // Ensure something drew: a pixel near the right edge should be green.
        assert_eq!(
            px(w - 4, h / 2),
            [0, 255, 0, 255],
            "right-edge pixel mismatch"
        );
    });
}

#[test]
fn aerogpu_cmd_geometry_shader_trianglelist_vs_as_compute_respects_instance_step_rate() {
    // Regression: triangle-list translated GS input feeding must respect D3D instance-step-rate
    // semantics for per-instance vertex inputs (at least for the first instance).
    //
    // Setup:
    // - Instance data is supplied as COLOR0.x with step_rate=2.
    // - Draw uses instance_count=1 and first_instance=1.
    // - Correct element index is floor(InstanceID / 2) = floor(1 / 2) = 0, so COLOR0.x = 1.0.
    // - If the prepass ignores step_rate, it will read element 1 instead (0.0), leaving the
    //   triangle centered and turning the center pixel green.
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::aerogpu_cmd_geometry_shader_trianglelist_vs_as_compute_respects_instance_step_rate"
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
        const VB_INST: u32 = 2;
        const RT: u32 = 3;
        const VS: u32 = 4;
        const GS: u32 = 5;
        const PS: u32 = 6;
        const IL: u32 = 7;

        let vertices = [
            VertexPos3Color4 {
                pos: [-0.5, -0.5, 0.0],
                color: [0.0, 0.0, 0.0, 1.0],
            },
            VertexPos3Color4 {
                pos: [0.0, 0.5, 0.0],
                color: [0.0, 0.0, 0.0, 1.0],
            },
            VertexPos3Color4 {
                pos: [0.5, -0.5, 0.0],
                color: [0.0, 0.0, 0.0, 1.0],
            },
        ];
        let vb_pos_bytes = bytemuck::bytes_of(&vertices);

        // Two per-instance elements: instance IDs 0-1 -> element0 (=1.0), 2-3 -> element1 (=0.0).
        let instance_shifts = [1.0f32, 0.0f32];
        let vb_inst_bytes = bytemuck::bytes_of(&instance_shifts);

        let mut writer = AerogpuCmdWriter::new();
        writer.create_buffer(
            VB_POS,
            AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
            vb_pos_bytes.len() as u64,
            0,
            0,
        );
        writer.upload_resource(VB_POS, 0, vb_pos_bytes);
        writer.create_buffer(
            VB_INST,
            AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
            vb_inst_bytes.len() as u64,
            0,
            0,
        );
        writer.upload_resource(VB_INST, 0, vb_inst_bytes);

        // Use an odd-sized render target so NDC (0,0) maps exactly to the center pixel.
        let w = 65u32;
        let h = 65u32;
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

        // Disable culling so output is visible regardless of winding.
        writer.set_rasterizer_state(
            AerogpuFillMode::Solid,
            AerogpuCullMode::None,
            false,
            false,
            0,
            0,
        );

        writer.create_shader_dxbc(
            VS,
            AerogpuShaderStage::Vertex,
            &build_vs_shift_x_from_instance_color_step2_dxbc(),
        );
        writer.create_shader_dxbc(GS, AerogpuShaderStage::Geometry, GS_PASSTHROUGH);
        writer.create_shader_dxbc(PS, AerogpuShaderStage::Pixel, &build_ps_solid_green_dxbc());

        let ilay = build_ilay_pos3_instance_color_step2();
        writer.create_input_layout(IL, &ilay);
        writer.set_input_layout(IL);
        writer.set_vertex_buffers(
            0,
            &[
                AerogpuVertexBufferBinding {
                    buffer: VB_POS,
                    stride_bytes: core::mem::size_of::<VertexPos3Color4>() as u32,
                    offset_bytes: 0,
                    reserved0: 0,
                },
                AerogpuVertexBufferBinding {
                    buffer: VB_INST,
                    stride_bytes: core::mem::size_of::<f32>() as u32,
                    offset_bytes: 0,
                    reserved0: 0,
                },
            ],
        );
        writer.set_primitive_topology(AerogpuPrimitiveTopology::TriangleList);
        writer.bind_shaders_ex(VS, PS, 0, GS, 0, 0);

        writer.clear(AEROGPU_CLEAR_COLOR, [1.0, 0.0, 0.0, 1.0], 1.0, 0);
        // Draw 1 instance, but start at first_instance=1 to exercise instance-step-rate division.
        writer.draw(3, 1, 0, 1);

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

        // With correct step-rate handling, COLOR0.x=1.0 and the triangle shifts right, leaving the
        // center pixel red.
        assert_eq!(px(w / 2, h / 2), [255, 0, 0, 255], "center pixel mismatch");
        // Ensure something drew: a pixel near the right edge should be green.
        assert_eq!(
            px(w - 4, h / 2),
            [0, 255, 0, 255],
            "right-edge pixel mismatch"
        );
    });
}
