mod common;

use aero_d3d11::input_layout::fnv1a_32;
use aero_d3d11::runtime::aerogpu_execute::AerogpuCmdRuntime;
use aero_d3d11::runtime::aerogpu_state::{PrimitiveTopology, RasterizerState, VertexBufferBinding};
use aero_dxbc::{test_utils as dxbc_test_utils, FourCC};

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    pos: [f32; 4],
    color: [f32; 4],
}

fn make_sm5_program_tokens(stage_type: u16, body_tokens: &[u32]) -> Vec<u32> {
    // Version token layout assumed by our decoder:
    // type in bits 16.., major in bits 4..7, minor in bits 0..3.
    let version = ((stage_type as u32) << 16) | (5u32 << 4);
    let total_dwords = 2 + body_tokens.len();
    let mut tokens = Vec::with_capacity(total_dwords);
    tokens.push(version);
    tokens.push(total_dwords as u32);
    tokens.extend_from_slice(body_tokens);
    tokens
}

fn tokens_to_bytes(tokens: &[u32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(tokens.len() * 4);
    for &t in tokens {
        bytes.extend_from_slice(&t.to_le_bytes());
    }
    bytes
}

fn opcode_token(opcode: u32, len: u32) -> u32 {
    opcode | (len << 11)
}

fn operand_dst_token(operand_type: u32) -> u32 {
    // Our bootstrap translator only looks at the type bits, but the real SM4/5 decoder expects a
    // full operand encoding (including index dimension and write mask).
    //
    // - 4-component operand (`num_components = 2`)
    // - component selection mode = mask
    // - 1D immediate register index
    let mut token = 0u32;
    token |= 2; // num_components (4)
    token |= operand_type << 4;
    token |= (0b1111u32) << 12; // write mask XYZW
    token |= 1u32 << 20; // index dimension 1D
    token
}

fn operand_src_token(operand_type: u32) -> u32 {
    // - 4-component operand (`num_components = 2`)
    // - component selection mode = swizzle
    // - 1D immediate register index
    let mut token = 0u32;
    token |= 2; // num_components (4)
    token |= 1u32 << 2; // selection mode = swizzle
    token |= operand_type << 4;
    token |= 0xE4u32 << 12; // swizzle XYZW
    token |= 1u32 << 20; // index dimension 1D
    token
}

fn build_isgn_chunk(params: &[(&str, u32, u32)]) -> Vec<u8> {
    let entries: Vec<dxbc_test_utils::SignatureEntryDesc<'_>> = params
        .iter()
        .map(|&(name, index, reg)| dxbc_test_utils::SignatureEntryDesc {
            semantic_name: name,
            semantic_index: index,
            system_value_type: 0,
            component_type: 0,
            register: reg,
            mask: 0b1111,
            read_write_mask: 0b1111,
            stream: 0,
        })
        .collect();
    dxbc_test_utils::build_signature_chunk_v0(&entries)
}

fn build_osgn_chunk(params: &[(&str, u32, u32)]) -> Vec<u8> {
    // `ISGN` and `OSGN` chunks share the same binary layout.
    build_isgn_chunk(params)
}

fn build_test_vs_dxbc() -> Vec<u8> {
    const OPCODE_MOV: u32 = 0x01;
    const OPCODE_RET: u32 = 0x3e;

    const OPERAND_INPUT: u32 = 1;
    const OPERAND_OUTPUT: u32 = 2;

    // mov o0, v0
    let mov0 = [
        opcode_token(OPCODE_MOV, 5),
        operand_dst_token(OPERAND_OUTPUT),
        0,
        operand_src_token(OPERAND_INPUT),
        0,
    ];
    // mov o1, v1
    let mov1 = [
        opcode_token(OPCODE_MOV, 5),
        operand_dst_token(OPERAND_OUTPUT),
        1,
        operand_src_token(OPERAND_INPUT),
        1,
    ];
    let ret = [opcode_token(OPCODE_RET, 1)];

    // Stage type 1 is vertex.
    let tokens = make_sm5_program_tokens(
        1,
        &[mov0.as_slice(), mov1.as_slice(), ret.as_slice()].concat(),
    );
    let shex = tokens_to_bytes(&tokens);
    // Use mixed-case semantics to ensure our signature parsing is case-insensitive (the
    // input layout blob uses the canonical uppercase hashes).
    let isgn = build_isgn_chunk(&[("Position", 0, 0), ("CoLoR", 0, 1)]);
    dxbc_test_utils::build_container(&[(FourCC(*b"SHEX"), &shex), (FourCC(*b"ISGN"), &isgn)])
}

fn build_test_vs_dxbc_with_osgn() -> Vec<u8> {
    const OPCODE_MOV: u32 = 0x01;
    const OPCODE_RET: u32 = 0x3e;

    const OPERAND_INPUT: u32 = 1;
    const OPERAND_OUTPUT: u32 = 2;

    // mov o0, v0
    let mov0 = [
        opcode_token(OPCODE_MOV, 5),
        operand_dst_token(OPERAND_OUTPUT),
        0,
        operand_src_token(OPERAND_INPUT),
        0,
    ];
    // mov o1, v1
    let mov1 = [
        opcode_token(OPCODE_MOV, 5),
        operand_dst_token(OPERAND_OUTPUT),
        1,
        operand_src_token(OPERAND_INPUT),
        1,
    ];
    let ret = [opcode_token(OPCODE_RET, 1)];

    // Stage type 1 is vertex.
    let tokens = make_sm5_program_tokens(
        1,
        &[mov0.as_slice(), mov1.as_slice(), ret.as_slice()].concat(),
    );
    let shex = tokens_to_bytes(&tokens);
    let isgn = build_isgn_chunk(&[("Position", 0, 0), ("CoLoR", 0, 1)]);
    let osgn = build_osgn_chunk(&[("SV_Position", 0, 0), ("COLOR", 0, 1)]);
    dxbc_test_utils::build_container(&[
        (FourCC(*b"SHEX"), &shex),
        (FourCC(*b"ISGN"), &isgn),
        (FourCC(*b"OSGN"), &osgn),
    ])
}

fn build_test_ps_dxbc() -> Vec<u8> {
    const OPCODE_MOV: u32 = 0x01;
    const OPCODE_RET: u32 = 0x3e;

    const OPERAND_INPUT: u32 = 1;
    const OPERAND_OUTPUT: u32 = 2;

    // mov o0, v1
    let mov = [
        opcode_token(OPCODE_MOV, 5),
        operand_dst_token(OPERAND_OUTPUT),
        0,
        operand_src_token(OPERAND_INPUT),
        1,
    ];
    let ret = [opcode_token(OPCODE_RET, 1)];

    // Stage type 0 is pixel.
    let tokens = make_sm5_program_tokens(0, &[mov.as_slice(), ret.as_slice()].concat());
    let shex = tokens_to_bytes(&tokens);
    dxbc_test_utils::build_container(&[(FourCC(*b"SHEX"), &shex)])
}

fn build_test_ps_dxbc_with_signatures() -> Vec<u8> {
    const OPCODE_MOV: u32 = 0x01;
    const OPCODE_RET: u32 = 0x3e;

    const OPERAND_INPUT: u32 = 1;
    const OPERAND_OUTPUT: u32 = 2;

    // mov o0, v1
    let mov = [
        opcode_token(OPCODE_MOV, 5),
        operand_dst_token(OPERAND_OUTPUT),
        0,
        operand_src_token(OPERAND_INPUT),
        1,
    ];
    let ret = [opcode_token(OPCODE_RET, 1)];

    // Stage type 0 is pixel.
    let tokens = make_sm5_program_tokens(0, &[mov.as_slice(), ret.as_slice()].concat());
    let shex = tokens_to_bytes(&tokens);
    // Match the varying produced by the VS: v1 @ location(1).
    let isgn = build_isgn_chunk(&[("SV_Position", 0, 0), ("COLOR", 0, 1)]);
    let osgn = build_osgn_chunk(&[("SV_Target", 0, 0)]);
    dxbc_test_utils::build_container(&[
        (FourCC(*b"SHEX"), &shex),
        (FourCC(*b"ISGN"), &isgn),
        (FourCC(*b"OSGN"), &osgn),
    ])
}

fn build_input_layout_blob() -> Vec<u8> {
    // Blob format mirrored from `drivers/aerogpu/protocol/aerogpu_cmd.h`:
    // - header (16 bytes)
    // - N elements (28 bytes each)
    const MAGIC: u32 = 0x5941_4C49; // "ILAY"
    const VERSION: u32 = 1;
    const ELEMENT_COUNT: u32 = 2;

    // DXGI_FORMAT_R32G32B32A32_FLOAT
    const DXGI_FORMAT_R32G32B32A32_FLOAT: u32 = 2;

    let mut out = Vec::new();
    out.extend_from_slice(&MAGIC.to_le_bytes());
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&ELEMENT_COUNT.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // reserved0

    // Element 0: v0 position @ offset 0.
    out.extend_from_slice(&fnv1a_32(b"POSITION").to_le_bytes()); // semantic_name_hash
    out.extend_from_slice(&0u32.to_le_bytes()); // semantic_index
    out.extend_from_slice(&DXGI_FORMAT_R32G32B32A32_FLOAT.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // input_slot
    out.extend_from_slice(&0u32.to_le_bytes()); // aligned_byte_offset
    out.extend_from_slice(&0u32.to_le_bytes()); // input_slot_class (per-vertex)
    out.extend_from_slice(&0u32.to_le_bytes()); // instance_data_step_rate

    // Element 1: v1 color @ offset 16.
    out.extend_from_slice(&fnv1a_32(b"COLOR").to_le_bytes()); // semantic_name_hash
    out.extend_from_slice(&0u32.to_le_bytes()); // semantic_index
    out.extend_from_slice(&DXGI_FORMAT_R32G32B32A32_FLOAT.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // input_slot
    out.extend_from_slice(&16u32.to_le_bytes()); // aligned_byte_offset
    out.extend_from_slice(&0u32.to_le_bytes()); // input_slot_class (per-vertex)
    out.extend_from_slice(&0u32.to_le_bytes()); // instance_data_step_rate

    out
}

fn build_input_layout_blob_sparse_slots() -> Vec<u8> {
    // Like `build_input_layout_blob`, but places POSITION in slot 0 and COLOR in slot 15.
    //
    // This exercises the runtime's compact slot mapping (D3D11 supports up to 32 IA slots, while
    // WebGPU only supports 8 vertex buffers with dense indices).
    const MAGIC: u32 = 0x5941_4C49; // "ILAY"
    const VERSION: u32 = 1;
    const ELEMENT_COUNT: u32 = 2;

    // DXGI_FORMAT_R32G32B32A32_FLOAT
    const DXGI_FORMAT_R32G32B32A32_FLOAT: u32 = 2;

    let mut out = Vec::new();
    out.extend_from_slice(&MAGIC.to_le_bytes());
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&ELEMENT_COUNT.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // reserved0

    // Element 0: v0 POSITION @ slot 0, offset 0.
    out.extend_from_slice(&fnv1a_32(b"POSITION").to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // semantic_index
    out.extend_from_slice(&DXGI_FORMAT_R32G32B32A32_FLOAT.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // input_slot
    out.extend_from_slice(&0u32.to_le_bytes()); // aligned_byte_offset
    out.extend_from_slice(&0u32.to_le_bytes()); // input_slot_class (per-vertex)
    out.extend_from_slice(&0u32.to_le_bytes()); // instance_data_step_rate

    // Element 1: v1 COLOR @ slot 15, offset 0.
    out.extend_from_slice(&fnv1a_32(b"COLOR").to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // semantic_index
    out.extend_from_slice(&DXGI_FORMAT_R32G32B32A32_FLOAT.to_le_bytes());
    out.extend_from_slice(&15u32.to_le_bytes()); // input_slot
    out.extend_from_slice(&0u32.to_le_bytes()); // aligned_byte_offset
    out.extend_from_slice(&0u32.to_le_bytes()); // input_slot_class (per-vertex)
    out.extend_from_slice(&0u32.to_le_bytes()); // instance_data_step_rate

    out
}

#[test]
fn aerogpu_render_pipeline_is_cached_across_draws() {
    pollster::block_on(async {
        let mut rt = match AerogpuCmdRuntime::new_for_tests().await {
            Ok(rt) => rt,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const VS: u32 = 1;
        const PS: u32 = 2;
        const ILAY: u32 = 3;
        const VB: u32 = 4;
        const RTEX: u32 = 5;

        rt.create_shader_dxbc(VS, &build_test_vs_dxbc()).unwrap();
        rt.create_shader_dxbc(PS, &build_test_ps_dxbc()).unwrap();
        rt.create_input_layout(ILAY, &build_input_layout_blob())
            .unwrap();

        let vertices: [Vertex; 3] = [
            Vertex {
                pos: [-1.0, -1.0, 0.0, 1.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [3.0, -1.0, 0.0, 1.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [-1.0, 3.0, 0.0, 1.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
        ];

        rt.create_buffer(
            VB,
            std::mem::size_of_val(&vertices) as u64,
            wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        );
        rt.write_buffer(VB, 0, bytemuck::bytes_of(&vertices))
            .unwrap();

        rt.create_texture2d(
            RTEX,
            4,
            4,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );

        let mut colors = [None; 8];
        colors[0] = Some(RTEX);
        rt.set_render_targets(&colors, None);
        rt.bind_shaders(Some(VS), None, Some(PS));
        rt.set_input_layout(Some(ILAY));
        rt.set_vertex_buffers(
            0,
            &[VertexBufferBinding {
                buffer: VB,
                stride: std::mem::size_of::<Vertex>() as u32,
                offset: 0,
            }],
        );
        rt.set_primitive_topology(PrimitiveTopology::TriangleList);
        rt.set_rasterizer_state(RasterizerState {
            cull_mode: None,
            front_face: wgpu::FrontFace::Ccw,
            scissor_enable: false,
        });

        rt.draw(3, 1, 0, 0).unwrap();
        rt.draw(3, 1, 0, 0).unwrap();

        rt.poll_wait();

        let stats = rt.pipeline_cache_stats();
        assert_eq!(stats.render_pipeline_misses, 1);
        assert_eq!(stats.render_pipeline_hits, 1);
        assert_eq!(stats.render_pipelines, 1);

        let pixels = rt.read_texture_rgba8(RTEX).await.unwrap();
        assert_eq!(&pixels[0..4], &[255, 0, 0, 255]);
    });
}

#[test]
fn aerogpu_compacts_sparse_vertex_slots() {
    pollster::block_on(async {
        let mut rt = match AerogpuCmdRuntime::new_for_tests().await {
            Ok(rt) => rt,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const VS: u32 = 1;
        const PS: u32 = 2;
        const ILAY: u32 = 3;
        const VB_POS: u32 = 4;
        const VB_COLOR: u32 = 5;
        const RTEX: u32 = 6;

        rt.create_shader_dxbc(VS, &build_test_vs_dxbc()).unwrap();
        rt.create_shader_dxbc(PS, &build_test_ps_dxbc()).unwrap();
        rt.create_input_layout(ILAY, &build_input_layout_blob_sparse_slots())
            .unwrap();

        let positions: [[f32; 4]; 3] = [
            [-1.0, -1.0, 0.0, 1.0],
            [3.0, -1.0, 0.0, 1.0],
            [-1.0, 3.0, 0.0, 1.0],
        ];
        let colors: [[f32; 4]; 3] = [[1.0, 0.0, 0.0, 1.0]; 3];

        rt.create_buffer(
            VB_POS,
            std::mem::size_of_val(&positions) as u64,
            wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        );
        rt.write_buffer(VB_POS, 0, bytemuck::cast_slice(&positions))
            .unwrap();

        rt.create_buffer(
            VB_COLOR,
            std::mem::size_of_val(&colors) as u64,
            wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        );
        rt.write_buffer(VB_COLOR, 0, bytemuck::cast_slice(&colors))
            .unwrap();

        rt.create_texture2d(
            RTEX,
            4,
            4,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );

        let mut colors_rt = [None; 8];
        colors_rt[0] = Some(RTEX);
        rt.set_render_targets(&colors_rt, None);
        rt.bind_shaders(Some(VS), None, Some(PS));
        rt.set_input_layout(Some(ILAY));
        rt.set_vertex_buffers(
            0,
            &[VertexBufferBinding {
                buffer: VB_POS,
                stride: 16,
                offset: 0,
            }],
        );
        rt.set_vertex_buffers(
            15,
            &[VertexBufferBinding {
                buffer: VB_COLOR,
                stride: 16,
                offset: 0,
            }],
        );
        rt.set_primitive_topology(PrimitiveTopology::TriangleList);
        rt.set_rasterizer_state(RasterizerState {
            cull_mode: None,
            front_face: wgpu::FrontFace::Ccw,
            scissor_enable: false,
        });

        rt.draw(3, 1, 0, 0).unwrap();
        rt.draw(3, 1, 0, 0).unwrap();
        rt.poll_wait();

        let stats = rt.pipeline_cache_stats();
        assert_eq!(stats.render_pipeline_misses, 1);
        assert_eq!(stats.render_pipeline_hits, 1);

        let pixels = rt.read_texture_rgba8(RTEX).await.unwrap();
        assert_eq!(&pixels[0..4], &[255, 0, 0, 255]);
    });
}

#[test]
fn aerogpu_mixes_signature_driven_vs_with_bootstrap_ps() {
    // Regression test: ensure varying locations match when one stage is translated via signatures
    // and the other falls back to the bootstrap translator.
    pollster::block_on(async {
        let mut rt = match AerogpuCmdRuntime::new_for_tests().await {
            Ok(rt) => rt,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const VS: u32 = 1;
        const PS: u32 = 2;
        const ILAY: u32 = 3;
        const VB: u32 = 4;
        const RTEX: u32 = 5;

        rt.create_shader_dxbc(VS, &build_test_vs_dxbc_with_osgn())
            .unwrap();
        // No signatures for PS: forces bootstrap translation.
        rt.create_shader_dxbc(PS, &build_test_ps_dxbc()).unwrap();
        rt.create_input_layout(ILAY, &build_input_layout_blob())
            .unwrap();

        let vertices: [Vertex; 3] = [
            Vertex {
                pos: [-1.0, -1.0, 0.0, 1.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [3.0, -1.0, 0.0, 1.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [-1.0, 3.0, 0.0, 1.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
        ];

        rt.create_buffer(
            VB,
            std::mem::size_of_val(&vertices) as u64,
            wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        );
        rt.write_buffer(VB, 0, bytemuck::bytes_of(&vertices))
            .unwrap();

        rt.create_texture2d(
            RTEX,
            4,
            4,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );

        let mut colors = [None; 8];
        colors[0] = Some(RTEX);
        rt.set_render_targets(&colors, None);
        rt.bind_shaders(Some(VS), None, Some(PS));
        rt.set_input_layout(Some(ILAY));
        rt.set_vertex_buffers(
            0,
            &[VertexBufferBinding {
                buffer: VB,
                stride: std::mem::size_of::<Vertex>() as u32,
                offset: 0,
            }],
        );
        rt.set_primitive_topology(PrimitiveTopology::TriangleList);
        rt.set_rasterizer_state(RasterizerState {
            cull_mode: None,
            front_face: wgpu::FrontFace::Ccw,
            scissor_enable: false,
        });

        rt.draw(3, 1, 0, 0).unwrap();
        rt.poll_wait();

        let pixels = rt.read_texture_rgba8(RTEX).await.unwrap();
        assert_eq!(&pixels[0..4], &[255, 0, 0, 255]);
    });
}

#[test]
fn aerogpu_mixes_bootstrap_vs_with_signature_driven_ps() {
    pollster::block_on(async {
        let mut rt = match AerogpuCmdRuntime::new_for_tests().await {
            Ok(rt) => rt,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const VS: u32 = 1;
        const PS: u32 = 2;
        const ILAY: u32 = 3;
        const VB: u32 = 4;
        const RTEX: u32 = 5;

        // No OSGN for VS: forces bootstrap translation (ISGN is still present for ILAY mapping).
        rt.create_shader_dxbc(VS, &build_test_vs_dxbc()).unwrap();
        rt.create_shader_dxbc(PS, &build_test_ps_dxbc_with_signatures())
            .unwrap();
        rt.create_input_layout(ILAY, &build_input_layout_blob())
            .unwrap();

        let vertices: [Vertex; 3] = [
            Vertex {
                pos: [-1.0, -1.0, 0.0, 1.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [3.0, -1.0, 0.0, 1.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [-1.0, 3.0, 0.0, 1.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
        ];

        rt.create_buffer(
            VB,
            std::mem::size_of_val(&vertices) as u64,
            wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        );
        rt.write_buffer(VB, 0, bytemuck::bytes_of(&vertices))
            .unwrap();

        rt.create_texture2d(
            RTEX,
            4,
            4,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );

        let mut colors = [None; 8];
        colors[0] = Some(RTEX);
        rt.set_render_targets(&colors, None);
        rt.bind_shaders(Some(VS), None, Some(PS));
        rt.set_input_layout(Some(ILAY));
        rt.set_vertex_buffers(
            0,
            &[VertexBufferBinding {
                buffer: VB,
                stride: std::mem::size_of::<Vertex>() as u32,
                offset: 0,
            }],
        );
        rt.set_primitive_topology(PrimitiveTopology::TriangleList);
        rt.set_rasterizer_state(RasterizerState {
            cull_mode: None,
            front_face: wgpu::FrontFace::Ccw,
            scissor_enable: false,
        });

        rt.draw(3, 1, 0, 0).unwrap();
        rt.poll_wait();

        let pixels = rt.read_texture_rgba8(RTEX).await.unwrap();
        assert_eq!(&pixels[0..4], &[255, 0, 0, 255]);
    });
}
