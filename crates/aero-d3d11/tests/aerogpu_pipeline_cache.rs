use aero_d3d11::runtime::aerogpu_execute::AerogpuCmdRuntime;
use aero_d3d11::runtime::aerogpu_state::{PrimitiveTopology, RasterizerState, VertexBufferBinding};

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    pos: [f32; 4],
    color: [f32; 4],
}

fn make_dxbc_with_single_chunk(fourcc: [u8; 4], chunk_data: &[u8]) -> Vec<u8> {
    // Minimal DXBC container sufficient for `aero_dxbc` + our bootstrap SM4/5 parser.
    let header_size = 4 + 16 + 4 + 4 + 4 + 4; // magic + checksum + one + total + count + offset[0]
    let chunk_offset = header_size;
    let total_size = header_size + 8 + chunk_data.len();

    let mut bytes = Vec::with_capacity(total_size);
    bytes.extend_from_slice(b"DXBC");
    bytes.extend_from_slice(&[0u8; 16]); // checksum (ignored)
    bytes.extend_from_slice(&1u32.to_le_bytes()); // "one"
    bytes.extend_from_slice(&(total_size as u32).to_le_bytes());
    bytes.extend_from_slice(&1u32.to_le_bytes()); // chunk count
    bytes.extend_from_slice(&(chunk_offset as u32).to_le_bytes());

    bytes.extend_from_slice(&fourcc);
    bytes.extend_from_slice(&(chunk_data.len() as u32).to_le_bytes());
    bytes.extend_from_slice(chunk_data);

    bytes
}

fn make_sm5_program_tokens(stage_type: u16, body_tokens: &[u32]) -> Vec<u32> {
    // Version token layout assumed by our decoder:
    // type in bits 16.., major in bits 4..7, minor in bits 0..3.
    let version = ((stage_type as u32) << 16) | (5u32 << 4) | 0u32;
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

fn operand_token(operand_type: u32) -> u32 {
    // Our minimal operand decoder reads type from bits 4..=11.
    operand_type << 4
}

fn build_test_vs_dxbc() -> Vec<u8> {
    const OPCODE_MOV: u32 = 0x01;
    const OPCODE_RET: u32 = 0x3e;

    const OPERAND_INPUT: u32 = 1;
    const OPERAND_OUTPUT: u32 = 2;

    // mov o0, v0
    let mov0 = [
        opcode_token(OPCODE_MOV, 5),
        operand_token(OPERAND_OUTPUT),
        0,
        operand_token(OPERAND_INPUT),
        0,
    ];
    // mov o1, v1
    let mov1 = [
        opcode_token(OPCODE_MOV, 5),
        operand_token(OPERAND_OUTPUT),
        1,
        operand_token(OPERAND_INPUT),
        1,
    ];
    let ret = [opcode_token(OPCODE_RET, 1)];

    // Stage type 1 is vertex.
    let tokens = make_sm5_program_tokens(
        1,
        &[mov0.as_slice(), mov1.as_slice(), ret.as_slice()].concat(),
    );
    make_dxbc_with_single_chunk(*b"SHEX", &tokens_to_bytes(&tokens))
}

fn build_test_ps_dxbc() -> Vec<u8> {
    const OPCODE_MOV: u32 = 0x01;
    const OPCODE_RET: u32 = 0x3e;

    const OPERAND_INPUT: u32 = 1;
    const OPERAND_OUTPUT: u32 = 2;

    // mov o0, v1
    let mov = [
        opcode_token(OPCODE_MOV, 5),
        operand_token(OPERAND_OUTPUT),
        0,
        operand_token(OPERAND_INPUT),
        1,
    ];
    let ret = [opcode_token(OPCODE_RET, 1)];

    // Stage type 0 is pixel.
    let tokens = make_sm5_program_tokens(0, &[mov.as_slice(), ret.as_slice()].concat());
    make_dxbc_with_single_chunk(*b"SHEX", &tokens_to_bytes(&tokens))
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
    out.extend_from_slice(&0u32.to_le_bytes()); // semantic_name_hash (ignored)
    out.extend_from_slice(&0u32.to_le_bytes()); // semantic_index (ignored)
    out.extend_from_slice(&DXGI_FORMAT_R32G32B32A32_FLOAT.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // input_slot
    out.extend_from_slice(&0u32.to_le_bytes()); // aligned_byte_offset
    out.extend_from_slice(&0u32.to_le_bytes()); // input_slot_class (per-vertex)
    out.extend_from_slice(&0u32.to_le_bytes()); // instance_data_step_rate

    // Element 1: v1 color @ offset 16.
    out.extend_from_slice(&0u32.to_le_bytes()); // semantic_name_hash (ignored)
    out.extend_from_slice(&1u32.to_le_bytes()); // semantic_index (ignored)
    out.extend_from_slice(&DXGI_FORMAT_R32G32B32A32_FLOAT.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // input_slot
    out.extend_from_slice(&16u32.to_le_bytes()); // aligned_byte_offset
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
                eprintln!("wgpu unavailable ({e:#}); skipping aerogpu pipeline cache test");
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
        rt.bind_shaders(Some(VS), Some(PS));
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
