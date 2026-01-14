mod common;

use std::fs;

use aero_d3d11::input_layout::fnv1a_32;
use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_d3d11::FourCC;
use aero_dxbc::test_utils as dxbc_test_utils;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode,
    AerogpuCmdStreamHeader as ProtocolCmdStreamHeader, AerogpuPrimitiveTopology,
    AEROGPU_CLEAR_COLOR, AEROGPU_CMD_STREAM_MAGIC, AEROGPU_INPUT_LAYOUT_BLOB_MAGIC,
    AEROGPU_INPUT_LAYOUT_BLOB_VERSION, AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER,
    AEROGPU_RESOURCE_USAGE_RENDER_TARGET, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::{AerogpuFormat, AEROGPU_ABI_VERSION_U32};

const FOURCC_ISGN: FourCC = FourCC(*b"ISGN");
const FOURCC_OSGN: FourCC = FourCC(*b"OSGN");
const FOURCC_SHDR: FourCC = FourCC(*b"SHDR");

const OPCODE_CREATE_BUFFER: u32 = AerogpuCmdOpcode::CreateBuffer as u32;
const OPCODE_CREATE_TEXTURE2D: u32 = AerogpuCmdOpcode::CreateTexture2d as u32;
const OPCODE_UPLOAD_RESOURCE: u32 = AerogpuCmdOpcode::UploadResource as u32;
const OPCODE_CREATE_SHADER_DXBC: u32 = AerogpuCmdOpcode::CreateShaderDxbc as u32;
const OPCODE_BIND_SHADERS: u32 = AerogpuCmdOpcode::BindShaders as u32;
const OPCODE_CREATE_INPUT_LAYOUT: u32 = AerogpuCmdOpcode::CreateInputLayout as u32;
const OPCODE_SET_INPUT_LAYOUT: u32 = AerogpuCmdOpcode::SetInputLayout as u32;
const OPCODE_SET_RASTERIZER_STATE: u32 = AerogpuCmdOpcode::SetRasterizerState as u32;
const OPCODE_SET_CONSTANT_BUFFERS: u32 = AerogpuCmdOpcode::SetConstantBuffers as u32;
const OPCODE_SET_RENDER_TARGETS: u32 = AerogpuCmdOpcode::SetRenderTargets as u32;
const OPCODE_SET_VIEWPORT: u32 = AerogpuCmdOpcode::SetViewport as u32;
const OPCODE_SET_VERTEX_BUFFERS: u32 = AerogpuCmdOpcode::SetVertexBuffers as u32;
const OPCODE_SET_PRIMITIVE_TOPOLOGY: u32 = AerogpuCmdOpcode::SetPrimitiveTopology as u32;
const OPCODE_CLEAR: u32 = AerogpuCmdOpcode::Clear as u32;
const OPCODE_DRAW: u32 = AerogpuCmdOpcode::Draw as u32;
const OPCODE_PRESENT: u32 = AerogpuCmdOpcode::Present as u32;

const AEROGPU_FORMAT_R8G8B8A8_UNORM: u32 = AerogpuFormat::R8G8B8A8Unorm as u32;
const AEROGPU_TOPOLOGY_TRIANGLELIST: u32 = AerogpuPrimitiveTopology::TriangleList as u32;

const CMD_STREAM_SIZE_BYTES_OFFSET: usize =
    core::mem::offset_of!(ProtocolCmdStreamHeader, size_bytes);
const CMD_HDR_SIZE_BYTES_OFFSET: usize = core::mem::offset_of!(ProtocolCmdHdr, size_bytes);

fn load_fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    fs::read(&path).unwrap_or_else(|e| panic!("failed to read {path}: {e}"))
}

fn align4(len: usize) -> usize {
    (len + 3) & !3
}

fn begin_cmd(stream: &mut Vec<u8>, opcode: u32) -> usize {
    let start = stream.len();
    stream.extend_from_slice(&opcode.to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes()); // size placeholder
    start
}

fn end_cmd(stream: &mut [u8], start: usize) {
    let size = (stream.len() - start) as u32;
    stream[start + CMD_HDR_SIZE_BYTES_OFFSET..start + CMD_HDR_SIZE_BYTES_OFFSET + 4]
        .copy_from_slice(&size.to_le_bytes());
    assert_eq!(size % 4, 0, "command not 4-byte aligned");
}

fn make_dxbc(chunks: &[(FourCC, Vec<u8>)]) -> Vec<u8> {
    dxbc_test_utils::build_container_owned(chunks)
}

struct SigParam<'a> {
    name: &'a str,
    semantic_index: u32,
    register: u32,
    mask: u8,
}

fn build_signature_chunk(params: &[SigParam<'_>]) -> Vec<u8> {
    let param_count = u32::try_from(params.len()).expect("too many signature params");
    let header_len = 8usize;
    let entry_size = 24usize;
    let table_len = params.len() * entry_size;

    let mut strings = Vec::<u8>::new();
    let mut name_offsets = Vec::<u32>::with_capacity(params.len());
    for p in params {
        name_offsets.push((header_len + table_len + strings.len()) as u32);
        strings.extend_from_slice(p.name.as_bytes());
        strings.push(0);
    }

    let mut bytes = Vec::with_capacity(header_len + table_len + strings.len());
    bytes.extend_from_slice(&param_count.to_le_bytes());
    bytes.extend_from_slice(&(header_len as u32).to_le_bytes());

    for (p, &name_off) in params.iter().zip(name_offsets.iter()) {
        bytes.extend_from_slice(&name_off.to_le_bytes());
        bytes.extend_from_slice(&p.semantic_index.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes()); // system_value_type
        bytes.extend_from_slice(&0u32.to_le_bytes()); // component_type
        bytes.extend_from_slice(&p.register.to_le_bytes());
        bytes.push(p.mask);
        bytes.push(p.mask); // read_write_mask
        bytes.push(0); // stream
        bytes.push(0); // min_precision
    }
    bytes.extend_from_slice(&strings);
    bytes
}

fn make_ps_solid_red_dxbc() -> Vec<u8> {
    // Hand-authored minimal DXBC container: empty ISGN + OSGN(SV_Target0) + SHDR(token stream).
    //
    // Token stream (SM4 subset):
    //   mov o0, l(1, 0, 0, 1)
    //   ret
    let isgn = build_signature_chunk(&[]);
    let osgn = build_signature_chunk(&[SigParam {
        name: "SV_Target",
        semantic_index: 0,
        register: 0,
        mask: 0xF,
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

    make_dxbc(&[
        (FOURCC_ISGN, isgn),
        (FOURCC_OSGN, osgn),
        (FOURCC_SHDR, shdr),
    ])
}

fn matrix_bytes(tx: f32) -> Vec<u8> {
    let rows: [[f32; 4]; 4] = [
        [1.0, 0.0, 0.0, tx],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ];
    let mut out = Vec::with_capacity(64);
    for r in rows {
        for f in r {
            out.extend_from_slice(&f.to_le_bytes());
        }
    }
    out
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    pos: [f32; 3],
}

#[test]
fn aerogpu_cmd_dynamic_constant_buffer_sanity() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const VB: u32 = 1;
        const CB: u32 = 2;
        const RT_A: u32 = 3;
        const RT_B: u32 = 4;
        const VS: u32 = 10;
        const PS: u32 = 11;
        const IL: u32 = 20;

        let vs_dxbc = load_fixture("vs_matrix.dxbc");
        let ps_dxbc = make_ps_solid_red_dxbc();

        let vertices = [
            Vertex {
                pos: [-0.9, -0.5, 0.0],
            },
            Vertex {
                pos: [-0.1, -0.5, 0.0],
            },
            Vertex {
                pos: [-0.9, 0.5, 0.0],
            },
        ];
        let vb_bytes = bytemuck::bytes_of(&vertices);

        let cb_identity = matrix_bytes(0.0);
        let cb_translate = matrix_bytes(1.0);

        // Input layout blob: POSITION0 only, R32G32B32_FLOAT.
        let mut ilay = Vec::new();
        ilay.extend_from_slice(&AEROGPU_INPUT_LAYOUT_BLOB_MAGIC.to_le_bytes());
        ilay.extend_from_slice(&AEROGPU_INPUT_LAYOUT_BLOB_VERSION.to_le_bytes());
        ilay.extend_from_slice(&1u32.to_le_bytes()); // element_count
        ilay.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        ilay.extend_from_slice(&fnv1a_32(b"POSITION").to_le_bytes());
        ilay.extend_from_slice(&0u32.to_le_bytes()); // semantic_index
        ilay.extend_from_slice(&6u32.to_le_bytes()); // DXGI_FORMAT_R32G32B32_FLOAT
        ilay.extend_from_slice(&0u32.to_le_bytes()); // input_slot
        ilay.extend_from_slice(&0u32.to_le_bytes()); // aligned_byte_offset
        ilay.extend_from_slice(&0u32.to_le_bytes()); // per-vertex
        ilay.extend_from_slice(&0u32.to_le_bytes()); // step rate
        assert_eq!(ilay.len(), 44);

        let mut stream = Vec::new();
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes placeholder
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_BUFFER (VB)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_BUFFER);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER.to_le_bytes());
        stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (VB)
        let start = begin_cmd(&mut stream, OPCODE_UPLOAD_RESOURCE);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset
        stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes());
        stream.extend_from_slice(vb_bytes);
        stream.resize(stream.len() + (align4(vb_bytes.len()) - vb_bytes.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_BUFFER (CB)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_BUFFER);
        stream.extend_from_slice(&CB.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER.to_le_bytes());
        stream.extend_from_slice(&(cb_identity.len() as u64).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (CB identity)
        let start = begin_cmd(&mut stream, OPCODE_UPLOAD_RESOURCE);
        stream.extend_from_slice(&CB.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset
        stream.extend_from_slice(&(cb_identity.len() as u64).to_le_bytes());
        stream.extend_from_slice(&cb_identity);
        stream.resize(
            stream.len() + (align4(cb_identity.len()) - cb_identity.len()),
            0,
        );
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (RT_A)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_TEXTURE2D);
        stream.extend_from_slice(&RT_A.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_FORMAT_R8G8B8A8_UNORM.to_le_bytes());
        stream.extend_from_slice(&64u32.to_le_bytes()); // width
        stream.extend_from_slice(&64u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (RT_B)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_TEXTURE2D);
        stream.extend_from_slice(&RT_B.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_FORMAT_R8G8B8A8_UNORM.to_le_bytes());
        stream.extend_from_slice(&64u32.to_le_bytes()); // width
        stream.extend_from_slice(&64u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (VS)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_SHADER_DXBC);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // stage = vertex
        stream.extend_from_slice(&(vs_dxbc.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&vs_dxbc);
        stream.resize(stream.len() + (align4(vs_dxbc.len()) - vs_dxbc.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (PS)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_SHADER_DXBC);
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // stage = pixel
        stream.extend_from_slice(&(ps_dxbc.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&ps_dxbc);
        stream.resize(stream.len() + (align4(ps_dxbc.len()) - ps_dxbc.len()), 0);
        end_cmd(&mut stream, start);

        // BIND_SHADERS
        let start = begin_cmd(&mut stream, OPCODE_BIND_SHADERS);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // cs
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, OPCODE_CREATE_INPUT_LAYOUT);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&(ilay.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&ilay);
        stream.resize(stream.len() + (align4(ilay.len()) - ilay.len()), 0);
        end_cmd(&mut stream, start);

        // SET_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, OPCODE_SET_INPUT_LAYOUT);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_VERTEX_BUFFERS
        let start = begin_cmd(&mut stream, OPCODE_SET_VERTEX_BUFFERS);
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
        stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&(std::mem::size_of::<Vertex>() as u32).to_le_bytes()); // stride
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_PRIMITIVE_TOPOLOGY
        let start = begin_cmd(&mut stream, OPCODE_SET_PRIMITIVE_TOPOLOGY);
        stream.extend_from_slice(&AEROGPU_TOPOLOGY_TRIANGLELIST.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_CONSTANT_BUFFERS (VS b0)
        let start = begin_cmd(&mut stream, OPCODE_SET_CONSTANT_BUFFERS);
        stream.extend_from_slice(&0u32.to_le_bytes()); // stage = vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
        stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
                                                       // bindings[0]
        stream.extend_from_slice(&CB.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (0 = full)
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_VIEWPORT (shared)
        let start = begin_cmd(&mut stream, OPCODE_SET_VIEWPORT);
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // x
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // y
        stream.extend_from_slice(&64f32.to_bits().to_le_bytes()); // w
        stream.extend_from_slice(&64f32.to_bits().to_le_bytes()); // h
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // min_depth
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // max_depth
        end_cmd(&mut stream, start);

        // SET_RASTERIZER_STATE: disable culling so the test is independent of winding.
        let start = begin_cmd(&mut stream, OPCODE_SET_RASTERIZER_STATE);
        stream.extend_from_slice(&0u32.to_le_bytes()); // fill_mode = solid
        stream.extend_from_slice(&0u32.to_le_bytes()); // cull_mode = none
        stream.extend_from_slice(&0u32.to_le_bytes()); // front_ccw = false
        stream.extend_from_slice(&0u32.to_le_bytes()); // scissor_enable = false
        stream.extend_from_slice(&0i32.to_le_bytes()); // depth_bias
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        end_cmd(&mut stream, start);

        let emit_draw_to_rt = |stream: &mut Vec<u8>, rt: u32| {
            // SET_RENDER_TARGETS
            let start = begin_cmd(stream, OPCODE_SET_RENDER_TARGETS);
            stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
            stream.extend_from_slice(&0u32.to_le_bytes()); // depth_stencil
            stream.extend_from_slice(&rt.to_le_bytes()); // rt0
            for _ in 1..8 {
                stream.extend_from_slice(&0u32.to_le_bytes());
            }
            end_cmd(stream, start);

            // CLEAR (black)
            let start = begin_cmd(stream, OPCODE_CLEAR);
            stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
            stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // r
            stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // g
            stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // b
            stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // a
            stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // depth
            stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
            end_cmd(stream, start);

            // DRAW
            let start = begin_cmd(stream, OPCODE_DRAW);
            stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
            stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
            stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
            stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
            end_cmd(stream, start);
        };

        // Draw with the identity matrix.
        emit_draw_to_rt(&mut stream, RT_A);

        // Update constant buffer (translation) between draws.
        let start = begin_cmd(&mut stream, OPCODE_UPLOAD_RESOURCE);
        stream.extend_from_slice(&CB.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset
        stream.extend_from_slice(&(cb_translate.len() as u64).to_le_bytes());
        stream.extend_from_slice(&cb_translate);
        stream.resize(
            stream.len() + (align4(cb_translate.len()) - cb_translate.len()),
            0,
        );
        end_cmd(&mut stream, start);

        // Draw with the translated matrix.
        emit_draw_to_rt(&mut stream, RT_B);

        // PRESENT
        let start = begin_cmd(&mut stream, OPCODE_PRESENT);
        stream.extend_from_slice(&0u32.to_le_bytes()); // scanout_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        end_cmd(&mut stream, start);

        let total_size = stream.len() as u32;
        stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
            .copy_from_slice(&total_size.to_le_bytes());

        let mut guest_mem = VecGuestMemory::new(0x1000);
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("cmd stream execution failed");
        exec.poll_wait();

        let pixels_a = exec.read_texture_rgba8(RT_A).await.unwrap();
        let pixels_b = exec.read_texture_rgba8(RT_B).await.unwrap();

        let clear = [0u8, 0u8, 0u8, 255u8];

        let mut a_only = None;
        let mut b_only = None;
        let mut a_non_clear = 0usize;
        let mut b_non_clear = 0usize;

        for y in 0..64usize {
            for x in 0..64usize {
                let off = (y * 64 + x) * 4;
                let pa: [u8; 4] = pixels_a[off..off + 4].try_into().unwrap();
                let pb: [u8; 4] = pixels_b[off..off + 4].try_into().unwrap();

                let a_draw = pa != clear;
                let b_draw = pb != clear;
                if a_draw {
                    a_non_clear += 1;
                }
                if b_draw {
                    b_non_clear += 1;
                }

                if a_only.is_none() && a_draw && !b_draw {
                    a_only = Some((x, y, pa));
                }
                if b_only.is_none() && !a_draw && b_draw {
                    b_only = Some((x, y, pb));
                }
            }
        }

        assert!(
            a_non_clear > 0,
            "expected draw A to write at least one pixel"
        );
        assert!(
            b_non_clear > 0,
            "expected draw B to write at least one pixel"
        );

        assert!(
            a_only.is_some(),
            "expected at least one pixel covered only by draw A"
        );
        assert!(
            b_only.is_some(),
            "expected at least one pixel covered only by draw B"
        );

        // Extra sanity: drawn pixels should be solid red.
        let red = [255u8, 0u8, 0u8, 255u8];
        for px in pixels_a.chunks_exact(4).chain(pixels_b.chunks_exact(4)) {
            if px != clear {
                assert_eq!(px, red);
            }
        }
    });
}
