mod common;

use aero_d3d11::input_layout::fnv1a_32;
use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_dxbc::{test_utils as dxbc_test_utils, FourCC};
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode,
    AerogpuCmdStreamHeader as ProtocolCmdStreamHeader, AerogpuPrimitiveTopology,
    AEROGPU_CLEAR_COLOR, AEROGPU_CMD_STREAM_MAGIC, AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
    AEROGPU_RESOURCE_USAGE_TEXTURE, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::{AerogpuFormat, AEROGPU_ABI_VERSION_U32};

const DXBC_PS_SAMPLE: &[u8] = include_bytes!("fixtures/ps_sample.dxbc");

const OPCODE_CREATE_BUFFER: u32 = AerogpuCmdOpcode::CreateBuffer as u32;
const OPCODE_CREATE_TEXTURE2D: u32 = AerogpuCmdOpcode::CreateTexture2d as u32;
const OPCODE_UPLOAD_RESOURCE: u32 = AerogpuCmdOpcode::UploadResource as u32;

const OPCODE_CREATE_SHADER_DXBC: u32 = AerogpuCmdOpcode::CreateShaderDxbc as u32;
const OPCODE_BIND_SHADERS: u32 = AerogpuCmdOpcode::BindShaders as u32;
const OPCODE_CREATE_INPUT_LAYOUT: u32 = AerogpuCmdOpcode::CreateInputLayout as u32;
const OPCODE_SET_INPUT_LAYOUT: u32 = AerogpuCmdOpcode::SetInputLayout as u32;

const OPCODE_SET_RENDER_TARGETS: u32 = AerogpuCmdOpcode::SetRenderTargets as u32;
const OPCODE_SET_VIEWPORT: u32 = AerogpuCmdOpcode::SetViewport as u32;
const OPCODE_SET_SCISSOR: u32 = AerogpuCmdOpcode::SetScissor as u32;

const OPCODE_SET_VERTEX_BUFFERS: u32 = AerogpuCmdOpcode::SetVertexBuffers as u32;
const OPCODE_SET_PRIMITIVE_TOPOLOGY: u32 = AerogpuCmdOpcode::SetPrimitiveTopology as u32;
const OPCODE_SET_TEXTURE: u32 = AerogpuCmdOpcode::SetTexture as u32;

const OPCODE_CLEAR: u32 = AerogpuCmdOpcode::Clear as u32;
const OPCODE_DRAW: u32 = AerogpuCmdOpcode::Draw as u32;

const AEROGPU_FORMAT_R8G8B8A8_UNORM: u32 = AerogpuFormat::R8G8B8A8Unorm as u32;
const AEROGPU_TOPOLOGY_TRIANGLELIST: u32 = AerogpuPrimitiveTopology::TriangleList as u32;

const CMD_STREAM_SIZE_BYTES_OFFSET: usize =
    core::mem::offset_of!(ProtocolCmdStreamHeader, size_bytes);
const CMD_HDR_SIZE_BYTES_OFFSET: usize = core::mem::offset_of!(ProtocolCmdHdr, size_bytes);

// Mirrors `D3D11_APPEND_ALIGNED_ELEMENT`.
const D3D11_APPEND_ALIGNED_ELEMENT: u32 = 0xFFFF_FFFF;

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

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    // Use a newly supported DXGI format: DXGI_FORMAT_R8G8_UNORM.
    uv: [u8; 2],
    _pad: [u8; 2],
    pos: [f32; 3],
}

fn make_dxbc(chunks: &[(FourCC, Vec<u8>)]) -> Vec<u8> {
    dxbc_test_utils::build_container_owned(chunks)
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
    // Our translator expects a full operand encoding:
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

#[derive(Clone, Copy)]
struct SigParam {
    name: &'static str,
    index: u32,
    reg: u32,
    mask: u8,
}

fn build_sig_chunk(params: &[SigParam]) -> Vec<u8> {
    let entries: Vec<dxbc_test_utils::SignatureEntryDesc<'_>> = params
        .iter()
        .map(|p| dxbc_test_utils::SignatureEntryDesc {
            semantic_name: p.name,
            semantic_index: p.index,
            system_value_type: 0,
            component_type: 0,
            register: p.reg,
            mask: p.mask,
            read_write_mask: 0b1111,
            stream: 0,
        })
        .collect();
    dxbc_test_utils::build_signature_chunk_v0(&entries)
}

fn build_vs_pos3_tex2_to_pos_tex_dxbc() -> Vec<u8> {
    const OPCODE_MOV: u32 = 0x01;
    const OPCODE_RET: u32 = 0x3e;

    const OPERAND_INPUT: u32 = 1;
    const OPERAND_OUTPUT: u32 = 2;

    // mov o0, v1  (TEXCOORD0)
    let mov0 = [
        opcode_token(OPCODE_MOV, 5),
        operand_dst_token(OPERAND_OUTPUT),
        0,
        operand_src_token(OPERAND_INPUT),
        1,
    ];
    // mov o1, v0  (SV_Position)
    let mov1 = [
        opcode_token(OPCODE_MOV, 5),
        operand_dst_token(OPERAND_OUTPUT),
        1,
        operand_src_token(OPERAND_INPUT),
        0,
    ];
    let ret = [opcode_token(OPCODE_RET, 1)];

    // Stage type 1 is vertex.
    let tokens = make_sm5_program_tokens(
        1,
        &[mov0.as_slice(), mov1.as_slice(), ret.as_slice()].concat(),
    );
    make_dxbc(&[
        (FourCC(*b"SHEX"), tokens_to_bytes(&tokens)),
        (
            FourCC(*b"ISGN"),
            build_sig_chunk(&[
                SigParam {
                    name: "POSITION",
                    index: 0,
                    reg: 0,
                    mask: 0b0111,
                },
                SigParam {
                    name: "TEXCOORD",
                    index: 0,
                    reg: 1,
                    mask: 0b0011,
                },
            ]),
        ),
        (
            FourCC(*b"OSGN"),
            build_sig_chunk(&[
                SigParam {
                    name: "TEXCOORD",
                    index: 0,
                    reg: 0,
                    mask: 0b0011,
                },
                SigParam {
                    name: "SV_Position",
                    index: 0,
                    reg: 1,
                    mask: 0b1111,
                },
            ]),
        ),
    ])
}

fn build_ilay_uv8_pos3() -> Vec<u8> {
    // ILAY header: magic, version, element_count, reserved0
    let mut blob = Vec::new();
    blob.extend_from_slice(&0x5941_4C49u32.to_le_bytes()); // "ILAY"
    blob.extend_from_slice(&1u32.to_le_bytes()); // version
    blob.extend_from_slice(&2u32.to_le_bytes()); // element_count
    blob.extend_from_slice(&0u32.to_le_bytes()); // reserved0

    let tex_hash = fnv1a_32(b"TEXCOORD");
    let pos_hash = fnv1a_32(b"POSITION");

    // Element 0: TEXCOORD0, DXGI_FORMAT_R8G8_UNORM (49), slot 0, offset 0.
    blob.extend_from_slice(&tex_hash.to_le_bytes());
    blob.extend_from_slice(&0u32.to_le_bytes()); // semantic_index
    blob.extend_from_slice(&49u32.to_le_bytes()); // dxgi_format
    blob.extend_from_slice(&0u32.to_le_bytes()); // input_slot
    blob.extend_from_slice(&0u32.to_le_bytes()); // aligned_byte_offset
    blob.extend_from_slice(&0u32.to_le_bytes()); // input_slot_class (per-vertex)
    blob.extend_from_slice(&0u32.to_le_bytes()); // instance_data_step_rate

    // Element 1: POSITION0, DXGI_FORMAT_R32G32B32_FLOAT (6), slot 0, append.
    blob.extend_from_slice(&pos_hash.to_le_bytes());
    blob.extend_from_slice(&0u32.to_le_bytes()); // semantic_index
    blob.extend_from_slice(&6u32.to_le_bytes()); // dxgi_format
    blob.extend_from_slice(&0u32.to_le_bytes()); // input_slot
    blob.extend_from_slice(&D3D11_APPEND_ALIGNED_ELEMENT.to_le_bytes()); // aligned_byte_offset
    blob.extend_from_slice(&0u32.to_le_bytes()); // input_slot_class (per-vertex)
    blob.extend_from_slice(&0u32.to_le_bytes()); // instance_data_step_rate

    blob
}

#[test]
fn aerogpu_cmd_renders_with_r8g8_unorm_vertex_attribute() {
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

        let vertices = [
            Vertex {
                uv: [0, 0],
                _pad: [0, 0],
                pos: [-1.0, -1.0, 0.0],
            },
            Vertex {
                uv: [0, 255],
                _pad: [0, 0],
                pos: [-1.0, 3.0, 0.0],
            },
            Vertex {
                uv: [255, 0],
                _pad: [0, 0],
                pos: [3.0, -1.0, 0.0],
            },
        ];

        let vb_bytes = bytemuck::bytes_of(&vertices);
        let vb_size = vb_bytes.len() as u64;

        // 2x2 texture, all texels are opaque green.
        let tex_bytes: [u8; 16] = [
            0, 255, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255,
        ];

        let ilay_bytes = build_ilay_uv8_pos3();

        let mut stream = Vec::new();
        // Stream header (24 bytes)
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_BUFFER (VB, host allocated)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_BUFFER);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER.to_le_bytes());
        stream.extend_from_slice(&vb_size.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (VB)
        let start = begin_cmd(&mut stream, OPCODE_UPLOAD_RESOURCE);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&vb_size.to_le_bytes()); // size_bytes
        stream.extend_from_slice(vb_bytes);
        stream.resize(stream.len() + (align4(vb_bytes.len()) - vb_bytes.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (SRV)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_TEXTURE2D);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_FORMAT_R8G8B8A8_UNORM.to_le_bytes());
        stream.extend_from_slice(&2u32.to_le_bytes()); // width
        stream.extend_from_slice(&2u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes (host allocated)
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (SRV texture, full upload)
        let start = begin_cmd(&mut stream, OPCODE_UPLOAD_RESOURCE);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&(tex_bytes.len() as u64).to_le_bytes()); // size_bytes
        stream.extend_from_slice(&tex_bytes);
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (RT)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_TEXTURE2D);
        stream.extend_from_slice(&RT.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_FORMAT_R8G8B8A8_UNORM.to_le_bytes());
        stream.extend_from_slice(&4u32.to_le_bytes()); // width
        stream.extend_from_slice(&4u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_RENDER_TARGETS
        let start = begin_cmd(&mut stream, OPCODE_SET_RENDER_TARGETS);
        stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // depth_stencil
        stream.extend_from_slice(&RT.to_le_bytes());
        for _ in 0..7 {
            stream.extend_from_slice(&0u32.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        // VIEWPORT 0..4
        let start = begin_cmd(&mut stream, OPCODE_SET_VIEWPORT);
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&4f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&4f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes());
        end_cmd(&mut stream, start);

        // SCISSOR 0..4 (scissor is disabled by default in the executor; still set to match guest)
        let start = begin_cmd(&mut stream, OPCODE_SET_SCISSOR);
        stream.extend_from_slice(&0i32.to_le_bytes());
        stream.extend_from_slice(&0i32.to_le_bytes());
        stream.extend_from_slice(&4i32.to_le_bytes());
        stream.extend_from_slice(&4i32.to_le_bytes());
        end_cmd(&mut stream, start);

        // CLEAR (black)
        let start = begin_cmd(&mut stream, OPCODE_CLEAR);
        stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // depth
        stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (VS)
        let vs_bytes = build_vs_pos3_tex2_to_pos_tex_dxbc();
        let start = begin_cmd(&mut stream, OPCODE_CREATE_SHADER_DXBC);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // stage = vertex
        stream.extend_from_slice(&(vs_bytes.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&vs_bytes);
        stream.resize(stream.len() + (align4(vs_bytes.len()) - vs_bytes.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (PS)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_SHADER_DXBC);
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // stage = pixel
        stream.extend_from_slice(&(DXBC_PS_SAMPLE.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(DXBC_PS_SAMPLE);
        stream.resize(
            stream.len() + (align4(DXBC_PS_SAMPLE.len()) - DXBC_PS_SAMPLE.len()),
            0,
        );
        end_cmd(&mut stream, start);

        // BIND_SHADERS
        let start = begin_cmd(&mut stream, OPCODE_BIND_SHADERS);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // cs
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_INPUT_LAYOUT (ILAY blob)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_INPUT_LAYOUT);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&(ilay_bytes.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&ilay_bytes);
        stream.resize(
            stream.len() + (align4(ilay_bytes.len()) - ilay_bytes.len()),
            0,
        );
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
        stream.extend_from_slice(&(core::mem::size_of::<Vertex>() as u32).to_le_bytes()); // stride
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_PRIMITIVE_TOPOLOGY
        let start = begin_cmd(&mut stream, OPCODE_SET_PRIMITIVE_TOPOLOGY);
        stream.extend_from_slice(&AEROGPU_TOPOLOGY_TRIANGLELIST.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_TEXTURE (PS t0)
        let start = begin_cmd(&mut stream, OPCODE_SET_TEXTURE);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // slot
        stream.extend_from_slice(&TEX.to_le_bytes()); // texture handle
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // DRAW
        let start = begin_cmd(&mut stream, OPCODE_DRAW);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        // Patch stream size in header.
        let total_size = stream.len() as u32;
        stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
            .copy_from_slice(&total_size.to_le_bytes());

        let mut guest_mem = VecGuestMemory::new(0x1000);
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .unwrap();
        exec.poll_wait();

        let pixels = exec.read_texture_rgba8(RT).await.unwrap();
        assert_eq!(pixels.len(), 4 * 4 * 4);
        for px in pixels.chunks_exact(4) {
            assert_eq!(px, &[0, 255, 0, 255]);
        }
    });
}
