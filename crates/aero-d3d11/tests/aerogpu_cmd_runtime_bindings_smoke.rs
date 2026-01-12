mod common;

use aero_d3d11::input_layout::fnv1a_32;
use aero_d3d11::runtime::aerogpu_execute::AerogpuCmdRuntime;
use aero_d3d11::runtime::aerogpu_state::{PrimitiveTopology, RasterizerState, VertexBufferBinding};

const DXBC_VS_MATRIX: &[u8] = include_bytes!("fixtures/vs_matrix.dxbc");
const DXBC_VS_PASSTHROUGH_TEXCOORD: &[u8] = include_bytes!("fixtures/vs_passthrough_texcoord.dxbc");
const DXBC_PS_SAMPLE: &[u8] = include_bytes!("fixtures/ps_sample.dxbc");
const ILAY_POS3_TEX2: &[u8] = include_bytes!("fixtures/ilay_pos3_tex2.bin");

fn align4(len: usize) -> usize {
    (len + 3) & !3
}

fn build_dxbc(chunks: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
    let chunk_count: u32 = chunks
        .len()
        .try_into()
        .expect("chunk count should fit in u32");

    let header_size = 4 + 16 + 4 + 4 + 4 + 4 * chunks.len();
    let mut offsets = Vec::with_capacity(chunks.len());
    let mut cursor = header_size;
    for (_fourcc, data) in chunks {
        offsets.push(cursor);
        cursor += 8 + align4(data.len());
    }

    let total_size: u32 = cursor.try_into().expect("dxbc size should fit in u32");

    let mut bytes = Vec::with_capacity(cursor);
    bytes.extend_from_slice(b"DXBC");
    bytes.extend_from_slice(&[0u8; 16]); // checksum (ignored)
    bytes.extend_from_slice(&1u32.to_le_bytes()); // "one"
    bytes.extend_from_slice(&total_size.to_le_bytes());
    bytes.extend_from_slice(&chunk_count.to_le_bytes());
    for offset in offsets {
        bytes.extend_from_slice(&(offset as u32).to_le_bytes());
    }

    for (fourcc, data) in chunks {
        bytes.extend_from_slice(fourcc);
        bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
        bytes.extend_from_slice(data);
        bytes.resize(bytes.len() + (align4(data.len()) - data.len()), 0);
    }
    bytes
}

#[derive(Clone, Copy)]
struct SigParam {
    semantic_name: &'static str,
    semantic_index: u32,
    register: u32,
    mask: u8,
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

fn build_ps_solid_red_dxbc() -> Vec<u8> {
    // Hand-authored minimal DXBC container: empty ISGN + OSGN(SV_Target0) + SHDR(token stream).
    //
    // Token stream (SM4 subset):
    //   mov o0, l(1, 0, 0, 1)
    //   ret
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

    let red = 1.0f32.to_bits();
    let zero = 0.0f32.to_bits();
    let one = 1.0f32.to_bits();

    let mut tokens = vec![
        version_token,
        0, // length patched below
        mov_token,
        dst_o0,
        0, // o0 index
        imm_vec4,
        red,
        zero,
        zero,
        one,
        ret_token,
    ];
    tokens[1] = tokens.len() as u32;

    let mut shdr = Vec::with_capacity(tokens.len() * 4);
    for t in tokens {
        shdr.extend_from_slice(&t.to_le_bytes());
    }

    build_dxbc(&[(*b"ISGN", isgn), (*b"OSGN", osgn), (*b"SHDR", shdr)])
}

fn build_ilay_pos3() -> Vec<u8> {
    // Build an ILAY blob that matches the `vs_matrix.dxbc` fixture input signature: POSITION0 only.
    //
    // struct aerogpu_input_layout_blob_header (16 bytes)
    // struct aerogpu_input_layout_element_dxgi (28 bytes)
    let mut blob = Vec::new();
    blob.extend_from_slice(&0x5941_4C49u32.to_le_bytes()); // "ILAY"
    blob.extend_from_slice(&1u32.to_le_bytes()); // version
    blob.extend_from_slice(&1u32.to_le_bytes()); // element_count
    blob.extend_from_slice(&0u32.to_le_bytes()); // reserved0

    // POSITION0: R32G32B32_FLOAT @ slot0 offset0
    blob.extend_from_slice(&fnv1a_32(b"POSITION").to_le_bytes());
    blob.extend_from_slice(&0u32.to_le_bytes()); // semantic_index
    blob.extend_from_slice(&6u32.to_le_bytes()); // DXGI_FORMAT_R32G32B32_FLOAT
    blob.extend_from_slice(&0u32.to_le_bytes()); // input_slot
    blob.extend_from_slice(&0u32.to_le_bytes()); // aligned_byte_offset
    blob.extend_from_slice(&0u32.to_le_bytes()); // input_slot_class (per-vertex)
    blob.extend_from_slice(&0u32.to_le_bytes()); // instance_data_step_rate

    blob
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct VertexPos3 {
    pos: [f32; 3],
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct VertexPos3Tex2 {
    pos: [f32; 3],
    uv: [f32; 2],
}

#[test]
fn aerogpu_cmd_runtime_binds_constant_buffer_smoke() {
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
        const IL: u32 = 3;
        const VB: u32 = 4;
        const CB: u32 = 5;
        const RTEX: u32 = 6;

        rt.create_shader_dxbc(VS, DXBC_VS_MATRIX).unwrap();
        rt.create_shader_dxbc(PS, &build_ps_solid_red_dxbc())
            .unwrap();
        rt.create_input_layout(IL, &build_ilay_pos3()).unwrap();

        // Fullscreen triangle.
        let vertices = [
            VertexPos3 {
                pos: [-1.0, -1.0, 0.0],
            },
            VertexPos3 {
                pos: [-1.0, 3.0, 0.0],
            },
            VertexPos3 {
                pos: [3.0, -1.0, 0.0],
            },
        ];

        rt.create_buffer(
            VB,
            std::mem::size_of_val(&vertices) as u64,
            wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        );
        rt.write_buffer(VB, 0, bytemuck::bytes_of(&vertices))
            .unwrap();

        // Identity 4x4 matrix as 16 u32 words (f32 bit patterns).
        let cb_words: [u32; 16] = [
            1.0f32.to_bits(),
            0.0f32.to_bits(),
            0.0f32.to_bits(),
            0.0f32.to_bits(),
            0.0f32.to_bits(),
            1.0f32.to_bits(),
            0.0f32.to_bits(),
            0.0f32.to_bits(),
            0.0f32.to_bits(),
            0.0f32.to_bits(),
            1.0f32.to_bits(),
            0.0f32.to_bits(),
            0.0f32.to_bits(),
            0.0f32.to_bits(),
            0.0f32.to_bits(),
            1.0f32.to_bits(),
        ];
        rt.create_buffer(
            CB,
            std::mem::size_of_val(&cb_words) as u64,
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        );
        rt.write_buffer(CB, 0, bytemuck::cast_slice(&cb_words))
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
        rt.set_input_layout(Some(IL));
        rt.set_vertex_buffers(
            0,
            &[VertexBufferBinding {
                buffer: VB,
                stride: std::mem::size_of::<VertexPos3>() as u32,
                offset: 0,
            }],
        );
        rt.set_primitive_topology(PrimitiveTopology::TriangleList);
        rt.set_rasterizer_state(RasterizerState {
            cull_mode: None,
            front_face: wgpu::FrontFace::Ccw,
            scissor_enable: false,
        });
        rt.set_vs_constant_buffer(0, Some(CB));

        rt.draw(3, 1, 0, 0).unwrap();
        rt.poll_wait();

        let pixels = rt.read_texture_rgba8(RTEX).await.unwrap();
        for (i, px) in pixels.chunks_exact(4).enumerate() {
            assert_eq!(px, &[255, 0, 0, 255], "pixel {i}");
        }
    });
}

#[test]
fn aerogpu_cmd_runtime_binds_texture_sampling_smoke() {
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
        const IL: u32 = 3;
        const VB: u32 = 4;
        const RTEX: u32 = 5;
        const TEX: u32 = 6;

        rt.create_shader_dxbc(VS, DXBC_VS_PASSTHROUGH_TEXCOORD)
            .unwrap();
        rt.create_shader_dxbc(PS, DXBC_PS_SAMPLE).unwrap();
        rt.create_input_layout(IL, ILAY_POS3_TEX2).unwrap();

        // Fullscreen triangle with UVs.
        let vertices = [
            VertexPos3Tex2 {
                pos: [-1.0, -1.0, 0.0],
                uv: [0.0, 0.0],
            },
            VertexPos3Tex2 {
                pos: [-1.0, 3.0, 0.0],
                uv: [0.0, 2.0],
            },
            VertexPos3Tex2 {
                pos: [3.0, -1.0, 0.0],
                uv: [2.0, 0.0],
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
            TEX,
            2,
            2,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        );
        let green_px: [u8; 4] = [0, 255, 0, 255];
        let tex_data = [
            green_px, green_px, //
            green_px, green_px, //
        ];
        rt.write_texture_rgba8(TEX, 2, 2, 2 * 4, bytemuck::bytes_of(&tex_data))
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
        rt.set_input_layout(Some(IL));
        rt.set_vertex_buffers(
            0,
            &[VertexBufferBinding {
                buffer: VB,
                stride: std::mem::size_of::<VertexPos3Tex2>() as u32,
                offset: 0,
            }],
        );
        rt.set_primitive_topology(PrimitiveTopology::TriangleList);
        rt.set_rasterizer_state(RasterizerState {
            cull_mode: None,
            front_face: wgpu::FrontFace::Ccw,
            scissor_enable: false,
        });
        rt.set_ps_texture(0, Some(TEX));

        rt.draw(3, 1, 0, 0).unwrap();
        rt.poll_wait();

        let pixels = rt.read_texture_rgba8(RTEX).await.unwrap();
        for (i, px) in pixels.chunks_exact(4).enumerate() {
            assert_eq!(px, &[0, 255, 0, 255], "pixel {i}");
        }
    });
}

