mod common;

use std::fs;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_d3d11::{DxbcFile, FourCC};
use aero_dxbc::test_utils as dxbc_test_utils;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode,
    AerogpuCmdStreamHeader as ProtocolCmdStreamHeader, AerogpuPrimitiveTopology,
    AEROGPU_CLEAR_COLOR, AEROGPU_CMD_STREAM_MAGIC, AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
    AEROGPU_RESOURCE_USAGE_TEXTURE, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::{AerogpuFormat, AEROGPU_ABI_VERSION_U32};

const FOURCC_ISGN: FourCC = FourCC(*b"ISGN");
const FOURCC_OSGN: FourCC = FourCC(*b"OSGN");
const FOURCC_SHDR: FourCC = FourCC(*b"SHDR");

const OPCODE_CREATE_BUFFER: u32 = AerogpuCmdOpcode::CreateBuffer as u32;
const OPCODE_CREATE_TEXTURE2D: u32 = AerogpuCmdOpcode::CreateTexture2d as u32;
const OPCODE_UPLOAD_RESOURCE: u32 = AerogpuCmdOpcode::UploadResource as u32;
const OPCODE_CREATE_SAMPLER: u32 = AerogpuCmdOpcode::CreateSampler as u32;
const OPCODE_CREATE_SHADER_DXBC: u32 = AerogpuCmdOpcode::CreateShaderDxbc as u32;
const OPCODE_BIND_SHADERS: u32 = AerogpuCmdOpcode::BindShaders as u32;
const OPCODE_CREATE_INPUT_LAYOUT: u32 = AerogpuCmdOpcode::CreateInputLayout as u32;
const OPCODE_SET_INPUT_LAYOUT: u32 = AerogpuCmdOpcode::SetInputLayout as u32;
const OPCODE_SET_TEXTURE: u32 = AerogpuCmdOpcode::SetTexture as u32;
const OPCODE_SET_SAMPLERS: u32 = AerogpuCmdOpcode::SetSamplers as u32;
const OPCODE_SET_RENDER_TARGETS: u32 = AerogpuCmdOpcode::SetRenderTargets as u32;
const OPCODE_SET_VIEWPORT: u32 = AerogpuCmdOpcode::SetViewport as u32;
const OPCODE_SET_SCISSOR: u32 = AerogpuCmdOpcode::SetScissor as u32;
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

struct SigParam<'a> {
    name: &'a str,
    semantic_index: u32,
    register: u32,
    mask: u8,
}

fn build_signature_chunk(params: &[SigParam<'_>]) -> Vec<u8> {
    let entries: Vec<dxbc_test_utils::SignatureEntryDesc<'_>> = params
        .iter()
        .map(|p| dxbc_test_utils::SignatureEntryDesc {
            semantic_name: p.name,
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

fn make_vs_passthrough_pos_tex2() -> Vec<u8> {
    let vs_fixture = load_fixture("vs_passthrough.dxbc");
    let dxbc = DxbcFile::parse(&vs_fixture).expect("vs_passthrough fixture should parse as DXBC");
    let shdr = dxbc
        .get_chunk(FOURCC_SHDR)
        .expect("vs_passthrough missing SHDR chunk");

    let mut words: Vec<u32> = shdr
        .data
        .chunks_exact(4)
        .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
        .collect();
    assert!(
        words.len() >= 13,
        "unexpected vs_passthrough SHDR length {}",
        words.len()
    );
    // The fixture contains two `mov` instructions. Swap the destination output registers so the
    // shader writes:
    // - o1 <- v0 (SV_Position)
    // - o0 <- v1 (TEXCOORD0)
    words[7] = 1;
    words[12] = 0;

    let mut shdr_patched = Vec::with_capacity(words.len() * 4);
    for w in words {
        shdr_patched.extend_from_slice(&w.to_le_bytes());
    }

    let isgn = build_signature_chunk(&[
        SigParam {
            name: "POSITION",
            semantic_index: 0,
            register: 0,
            mask: 0x7, // xyz
        },
        SigParam {
            name: "TEXCOORD",
            semantic_index: 0,
            register: 1,
            mask: 0x3, // xy
        },
    ]);
    let osgn = build_signature_chunk(&[
        SigParam {
            name: "TEXCOORD",
            semantic_index: 0,
            register: 0,
            mask: 0x3, // xy
        },
        SigParam {
            name: "SV_Position",
            semantic_index: 0,
            register: 1,
            mask: 0xF,
        },
    ]);

    dxbc_test_utils::build_container(&[
        (FOURCC_ISGN, &isgn),
        (FOURCC_OSGN, &osgn),
        (FOURCC_SHDR, &shdr_patched),
    ])
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    pos: [f32; 3],
    uv: [f32; 2],
}

#[test]
fn aerogpu_cmd_texture_sampling_sanity() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const VB: u32 = 1;
        const TEX: u32 = 2;
        const RT: u32 = 3;
        const VS: u32 = 10;
        const PS: u32 = 11;
        const IL: u32 = 20;
        const SAMP: u32 = 30;

        let ps_dxbc = load_fixture("ps_sample.dxbc");
        let ilay = load_fixture("ilay_pos3_tex2.bin");
        let vs_dxbc = make_vs_passthrough_pos_tex2();

        let vertices = [
            Vertex {
                pos: [-1.0, -1.0, 0.0],
                uv: [0.0, 1.0],
            },
            Vertex {
                pos: [-1.0, 3.0, 0.0],
                uv: [0.0, -1.0],
            },
            Vertex {
                pos: [3.0, -1.0, 0.0],
                uv: [2.0, 1.0],
            },
        ];
        let vb_bytes = bytemuck::bytes_of(&vertices);

        // 2x2 RGBA pattern (top-left origin).
        // Row 0 (top):    red, green
        // Row 1 (bottom): blue, white
        let tex_bytes: [u8; 16] = [
            255, 0, 0, 255, // red
            0, 255, 0, 255, // green
            0, 0, 255, 255, // blue
            255, 255, 255, 255, // white
        ];

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

        // CREATE_TEXTURE2D (TEX)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_TEXTURE2D);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_FORMAT_R8G8B8A8_UNORM.to_le_bytes());
        stream.extend_from_slice(&2u32.to_le_bytes()); // width
        stream.extend_from_slice(&2u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (TEX)
        let start = begin_cmd(&mut stream, OPCODE_UPLOAD_RESOURCE);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset
        stream.extend_from_slice(&(tex_bytes.len() as u64).to_le_bytes());
        stream.extend_from_slice(&tex_bytes);
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (RT)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_TEXTURE2D);
        stream.extend_from_slice(&RT.to_le_bytes());
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

        // CREATE_SAMPLER (nearest + clamp)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_SAMPLER);
        stream.extend_from_slice(&SAMP.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // filter = nearest
        stream.extend_from_slice(&0u32.to_le_bytes()); // address_u = clamp
        stream.extend_from_slice(&0u32.to_le_bytes()); // address_v = clamp
        stream.extend_from_slice(&0u32.to_le_bytes()); // address_w = clamp
        end_cmd(&mut stream, start);

        // SET_TEXTURE (PS t0)
        let start = begin_cmd(&mut stream, OPCODE_SET_TEXTURE);
        stream.extend_from_slice(&1u32.to_le_bytes()); // stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // slot
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_SAMPLERS (PS s0)
        let start = begin_cmd(&mut stream, OPCODE_SET_SAMPLERS);
        stream.extend_from_slice(&1u32.to_le_bytes()); // stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
        stream.extend_from_slice(&1u32.to_le_bytes()); // sampler_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&SAMP.to_le_bytes()); // samplers[0]
        end_cmd(&mut stream, start);

        // SET_RENDER_TARGETS
        let start = begin_cmd(&mut stream, OPCODE_SET_RENDER_TARGETS);
        stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // depth_stencil
        stream.extend_from_slice(&RT.to_le_bytes()); // rt0
        for _ in 1..8 {
            stream.extend_from_slice(&0u32.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        // SET_VIEWPORT
        let start = begin_cmd(&mut stream, OPCODE_SET_VIEWPORT);
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // x
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // y
        stream.extend_from_slice(&64f32.to_bits().to_le_bytes()); // w
        stream.extend_from_slice(&64f32.to_bits().to_le_bytes()); // h
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // min_depth
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // max_depth
        end_cmd(&mut stream, start);

        // SET_SCISSOR
        let start = begin_cmd(&mut stream, OPCODE_SET_SCISSOR);
        stream.extend_from_slice(&0i32.to_le_bytes()); // x
        stream.extend_from_slice(&0i32.to_le_bytes()); // y
        stream.extend_from_slice(&64i32.to_le_bytes()); // w
        stream.extend_from_slice(&64i32.to_le_bytes()); // h
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

        // CLEAR (black)
        let start = begin_cmd(&mut stream, OPCODE_CLEAR);
        stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // r
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // g
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // b
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // a
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // depth
        stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
        end_cmd(&mut stream, start);

        // DRAW
        let start = begin_cmd(&mut stream, OPCODE_DRAW);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

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

        let pixels = exec.read_texture_rgba8(RT).await.unwrap();
        assert_eq!(pixels.len(), 64 * 64 * 4);

        let sample = |x: usize, y: usize| -> [u8; 4] {
            let off = (y * 64 + x) * 4;
            pixels[off..off + 4].try_into().unwrap()
        };

        // Nearest sampling should produce stable quadrants from the 2x2 source texture.
        assert_eq!(sample(16, 16), [255, 0, 0, 255]); // top-left
        assert_eq!(sample(48, 16), [0, 255, 0, 255]); // top-right
        assert_eq!(sample(16, 48), [0, 0, 255, 255]); // bottom-left
        assert_eq!(sample(48, 48), [255, 255, 255, 255]); // bottom-right
    });
}
