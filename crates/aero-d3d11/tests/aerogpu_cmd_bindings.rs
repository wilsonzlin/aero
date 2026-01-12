mod common;

use aero_d3d11::input_layout::fnv1a_32;
use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuBlendFactor, AerogpuBlendOp, AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode,
    AerogpuCmdStreamHeader as ProtocolCmdStreamHeader, AerogpuCompareFunc,
    AerogpuPrimitiveTopology, AEROGPU_CLEAR_COLOR, AEROGPU_CLEAR_DEPTH, AEROGPU_CMD_STREAM_MAGIC,
    AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER, AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL,
    AEROGPU_RESOURCE_USAGE_INDEX_BUFFER, AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
    AEROGPU_RESOURCE_USAGE_TEXTURE, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::{AerogpuFormat, AEROGPU_ABI_VERSION_U32};
use aero_protocol::aerogpu::aerogpu_ring::AerogpuAllocEntry;

const DXBC_VS_MATRIX: &[u8] = include_bytes!("fixtures/vs_matrix.dxbc");
const DXBC_PS_SAMPLE: &[u8] = include_bytes!("fixtures/ps_sample.dxbc");
const ILAY_POS3_TEX2: &[u8] = include_bytes!("fixtures/ilay_pos3_tex2.bin");

const CMD_STREAM_SIZE_BYTES_OFFSET: usize =
    core::mem::offset_of!(ProtocolCmdStreamHeader, size_bytes);
const CMD_HDR_SIZE_BYTES_OFFSET: usize = core::mem::offset_of!(ProtocolCmdHdr, size_bytes);

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

fn finish_stream(mut stream: Vec<u8>) -> Vec<u8> {
    let total_size = stream.len() as u32;
    stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
        .copy_from_slice(&total_size.to_le_bytes());
    stream
}

fn build_ilay_pos3() -> Vec<u8> {
    // Blob format mirrored from `drivers/aerogpu/protocol/aerogpu_cmd.h`:
    // - header (16 bytes)
    // - N elements (28 bytes each)
    const MAGIC: u32 = 0x5941_4C49; // "ILAY"
    const VERSION: u32 = 1;

    // DXGI_FORMAT_R32G32B32_FLOAT
    const DXGI_FORMAT_R32G32B32_FLOAT: u32 = 6;

    let mut out = Vec::new();
    out.extend_from_slice(&MAGIC.to_le_bytes());
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes()); // element_count
    out.extend_from_slice(&0u32.to_le_bytes()); // reserved0

    out.extend_from_slice(&fnv1a_32(b"POSITION").to_le_bytes()); // semantic_name_hash
    out.extend_from_slice(&0u32.to_le_bytes()); // semantic_index
    out.extend_from_slice(&DXGI_FORMAT_R32G32B32_FLOAT.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // input_slot
    out.extend_from_slice(&0u32.to_le_bytes()); // aligned_byte_offset
    out.extend_from_slice(&0u32.to_le_bytes()); // input_slot_class (per-vertex)
    out.extend_from_slice(&0u32.to_le_bytes()); // instance_data_step_rate

    out
}

fn make_dxbc(chunks: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
    // Minimal DXBC container sufficient for `aero_dxbc` + the SM4/5 parser.
    let chunk_count = u32::try_from(chunks.len()).expect("too many chunks for test DXBC");
    let header_size = 4 + 16 + 4 + 4 + 4 + (chunks.len() * 4);

    let mut offsets = Vec::with_capacity(chunks.len());
    let mut cursor = header_size;
    for (_fourcc, data) in chunks {
        offsets.push(cursor as u32);
        cursor += 8 + data.len();
    }
    let total_size = cursor as u32;

    let mut bytes = Vec::with_capacity(cursor);
    bytes.extend_from_slice(b"DXBC");
    bytes.extend_from_slice(&[0u8; 16]); // checksum (ignored)
    bytes.extend_from_slice(&1u32.to_le_bytes()); // "one"
    bytes.extend_from_slice(&total_size.to_le_bytes());
    bytes.extend_from_slice(&chunk_count.to_le_bytes());
    for off in offsets {
        bytes.extend_from_slice(&off.to_le_bytes());
    }
    for (fourcc, data) in chunks {
        bytes.extend_from_slice(fourcc);
        bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
        bytes.extend_from_slice(data);
    }

    assert_eq!(bytes.len(), total_size as usize);
    bytes
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

fn operand_src_imm32_token() -> u32 {
    // Immediate 32-bit float4 literal.
    let mut token = 0u32;
    token |= 2; // num_components (4)
    token |= 1u32 << 2; // selection mode = swizzle
    token |= 4u32 << 4; // OPERAND_TYPE_IMMEDIATE32
    token |= 0xE4u32 << 12; // swizzle XYZW
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
    // Mirrors the format parsed by `aero_dxbc::parse_signature_chunk_with_fourcc`.
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
        bytes.extend_from_slice(&p.index.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes()); // system_value_type
        bytes.extend_from_slice(&0u32.to_le_bytes()); // component_type
        bytes.extend_from_slice(&p.reg.to_le_bytes());
        bytes.push(p.mask); // mask
        bytes.push(0b1111); // read_write_mask
        bytes.push(0); // stream
        bytes.push(0); // min_precision
    }
    bytes.extend_from_slice(&strings);
    bytes
}

fn build_ps_solid_red_dxbc() -> Vec<u8> {
    const OPCODE_MOV: u32 = 0x01;
    const OPCODE_RET: u32 = 0x3e;

    const OPERAND_OUTPUT: u32 = 2;

    let imm = [
        1.0f32.to_bits(),
        0.0f32.to_bits(),
        0.0f32.to_bits(),
        1.0f32.to_bits(),
    ];

    // mov o0, l(1,0,0,1)
    let mov = [
        opcode_token(OPCODE_MOV, 8),
        operand_dst_token(OPERAND_OUTPUT),
        0,
        operand_src_imm32_token(),
        imm[0],
        imm[1],
        imm[2],
        imm[3],
    ];
    let ret = [opcode_token(OPCODE_RET, 1)];

    // Stage type 0 is pixel.
    let tokens = make_sm5_program_tokens(0, &[mov.as_slice(), ret.as_slice()].concat());
    make_dxbc(&[
        (*b"SHEX", tokens_to_bytes(&tokens)),
        // Empty input signature; translator should emit a fragment entry point with no inputs.
        (*b"ISGN", build_sig_chunk(&[])),
        (
            *b"OSGN",
            build_sig_chunk(&[SigParam {
                name: "SV_Target",
                index: 0,
                reg: 0,
                mask: 0b1111,
            }]),
        ),
    ])
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
        (*b"SHEX", tokens_to_bytes(&tokens)),
        (
            *b"ISGN",
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
            *b"OSGN",
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

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct VertexPos3 {
    pos: [f32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct VertexPos3Tex2 {
    pos: [f32; 3],
    uv: [f32; 2],
}

#[test]
fn aerogpu_cmd_binds_constant_buffer_cb0() {
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
        const RT: u32 = 3;
        const VS: u32 = 10;
        const PS: u32 = 11;
        const IL: u32 = 20;

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
        let vb_bytes = bytemuck::bytes_of(&vertices);

        let ps_red = build_ps_solid_red_dxbc();
        let ilay = build_ilay_pos3();
        let identity: [f32; 16] = [
            1.0, 0.0, 0.0, 0.0, //
            0.0, 1.0, 0.0, 0.0, //
            0.0, 0.0, 1.0, 0.0, //
            0.0, 0.0, 0.0, 1.0, //
        ];
        let cb_size_bytes = (identity.len() * 4) as u64;

        let mut stream = Vec::new();
        // Stream header (24 bytes)
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_BUFFER (host allocated VB)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER.to_le_bytes());
        stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (VB)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes()); // size_bytes
        stream.extend_from_slice(vb_bytes);
        stream.resize(stream.len() + (align4(vb_bytes.len()) - vb_bytes.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_BUFFER (constant buffer)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
        stream.extend_from_slice(&CB.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER.to_le_bytes());
        stream.extend_from_slice(&cb_size_bytes.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (constant buffer)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&CB.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&cb_size_bytes.to_le_bytes());
        for f in identity {
            stream.extend_from_slice(&f.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (RT)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&RT.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&4u32.to_le_bytes()); // width
        stream.extend_from_slice(&4u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (VS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // stage = vertex
        stream.extend_from_slice(&(DXBC_VS_MATRIX.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(DXBC_VS_MATRIX);
        stream.resize(
            stream.len() + (align4(DXBC_VS_MATRIX.len()) - DXBC_VS_MATRIX.len()),
            0,
        );
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (PS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // stage = pixel
        stream.extend_from_slice(&(ps_red.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&ps_red);
        stream.resize(stream.len() + (align4(ps_red.len()) - ps_red.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&(ilay.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&ilay);
        stream.resize(stream.len() + (align4(ilay.len()) - ilay.len()), 0);
        end_cmd(&mut stream, start);

        // SET_RENDER_TARGETS (1 RT, no DS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetRenderTargets as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // depth_stencil
        stream.extend_from_slice(&RT.to_le_bytes());
        for _ in 0..7 {
            stream.extend_from_slice(&0u32.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        // SET_VIEWPORT 0..4
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetViewport as u32);
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&4f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&4f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes());
        end_cmd(&mut stream, start);

        // BIND_SHADERS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::BindShaders as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // cs
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_VERTEX_BUFFERS (slot 0)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetVertexBuffers as u32);
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
        stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
        stream.extend_from_slice(&VB.to_le_bytes()); // buffer
        stream.extend_from_slice(&(core::mem::size_of::<VertexPos3>() as u32).to_le_bytes()); // stride_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_PRIMITIVE_TOPOLOGY
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetPrimitiveTopology as u32);
        stream.extend_from_slice(&(AerogpuPrimitiveTopology::TriangleList as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_CONSTANT_BUFFERS (VS cb0)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetConstantBuffers as u32);
        stream.extend_from_slice(&0u32.to_le_bytes()); // stage = vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
        stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&CB.to_le_bytes()); // buffer
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&(cb_size_bytes as u32).to_le_bytes()); // size_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CLEAR to black.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Clear as u32);
        stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
        for bits in [0.0f32, 0.0, 0.0, 1.0].map(f32::to_bits) {
            stream.extend_from_slice(&bits.to_le_bytes());
        }
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // depth
        stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
        end_cmd(&mut stream, start);

        // DRAW
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Draw as u32);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        let stream = finish_stream(stream);

        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let pixels = exec.read_texture_rgba8(RT).await.unwrap();
        assert_eq!(&pixels[0..4], &[255, 0, 0, 255]);
    });
}

#[test]
fn aerogpu_cmd_binds_texture_and_sampler() {
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

        let vs = build_vs_pos3_tex2_to_pos_tex_dxbc();

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
        let vb_bytes = bytemuck::bytes_of(&vertices);

        let mut stream = Vec::new();
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_BUFFER (host allocated VB)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER.to_le_bytes());
        stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (VB)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes()); // size_bytes
        stream.extend_from_slice(vb_bytes);
        stream.resize(stream.len() + (align4(vb_bytes.len()) - vb_bytes.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (sampled texture)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (1x1 red)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&4u64.to_le_bytes()); // size_bytes
        stream.extend_from_slice(&[255u8, 0, 0, 255]);
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (RT)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&RT.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&4u32.to_le_bytes()); // width
        stream.extend_from_slice(&4u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (VS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // stage = vertex
        stream.extend_from_slice(&(vs.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&vs);
        stream.resize(stream.len() + (align4(vs.len()) - vs.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (PS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
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

        // CREATE_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&(ILAY_POS3_TEX2.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(ILAY_POS3_TEX2);
        stream.resize(
            stream.len() + (align4(ILAY_POS3_TEX2.len()) - ILAY_POS3_TEX2.len()),
            0,
        );
        end_cmd(&mut stream, start);

        // SET_RENDER_TARGETS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetRenderTargets as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // depth_stencil
        stream.extend_from_slice(&RT.to_le_bytes());
        for _ in 0..7 {
            stream.extend_from_slice(&0u32.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        // SET_VIEWPORT 0..4
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetViewport as u32);
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&4f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&4f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes());
        end_cmd(&mut stream, start);

        // BIND_SHADERS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::BindShaders as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // cs
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_VERTEX_BUFFERS (slot 0)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetVertexBuffers as u32);
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
        stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
        stream.extend_from_slice(&VB.to_le_bytes()); // buffer
        stream.extend_from_slice(&(core::mem::size_of::<VertexPos3Tex2>() as u32).to_le_bytes()); // stride_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_PRIMITIVE_TOPOLOGY
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetPrimitiveTopology as u32);
        stream.extend_from_slice(&(AerogpuPrimitiveTopology::TriangleList as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_TEXTURE (PS t0)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetTexture as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // slot
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_SAMPLER_STATE (PS s0 minfilter = point)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetSamplerState as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // slot
        stream.extend_from_slice(&6u32.to_le_bytes()); // D3DSAMP_MINFILTER
        stream.extend_from_slice(&1u32.to_le_bytes()); // D3DTEXF_POINT
        end_cmd(&mut stream, start);

        // CLEAR to black.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Clear as u32);
        stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
        for bits in [0.0f32, 0.0, 0.0, 1.0].map(f32::to_bits) {
            stream.extend_from_slice(&bits.to_le_bytes());
        }
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // depth
        stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
        end_cmd(&mut stream, start);

        // DRAW
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Draw as u32);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        let stream = finish_stream(stream);

        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let pixels = exec.read_texture_rgba8(RT).await.unwrap();
        assert_eq!(&pixels[0..4], &[255, 0, 0, 255]);
    });
}

#[test]
fn aerogpu_cmd_rebinds_texture_between_draws() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const VB: u32 = 1;
        const TEX_RED: u32 = 2;
        const TEX_GREEN: u32 = 3;
        const RT: u32 = 4;
        const VS: u32 = 10;
        const PS: u32 = 11;
        const IL: u32 = 20;

        let vs = build_vs_pos3_tex2_to_pos_tex_dxbc();

        let vertices = [
            VertexPos3Tex2 {
                pos: [-1.0, -3.0, 0.0],
                uv: [0.5, 0.5],
            },
            VertexPos3Tex2 {
                pos: [-1.0, 1.0, 0.0],
                uv: [0.5, 0.5],
            },
            VertexPos3Tex2 {
                pos: [3.0, 1.0, 0.0],
                uv: [0.5, 0.5],
            },
        ];
        let vb_bytes = bytemuck::bytes_of(&vertices);

        let mut stream = Vec::new();
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_BUFFER (host allocated VB)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER.to_le_bytes());
        stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (VB)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes()); // size_bytes
        stream.extend_from_slice(vb_bytes);
        stream.resize(stream.len() + (align4(vb_bytes.len()) - vb_bytes.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (red)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&TEX_RED.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (red)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&TEX_RED.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&4u64.to_le_bytes()); // size_bytes
        stream.extend_from_slice(&[255u8, 0, 0, 255]);
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (green)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&TEX_GREEN.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (green)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&TEX_GREEN.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&4u64.to_le_bytes()); // size_bytes
        stream.extend_from_slice(&[0u8, 255, 0, 255]);
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (RT 2x1)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&RT.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&2u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (VS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // stage = vertex
        stream.extend_from_slice(&(vs.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&vs);
        stream.resize(stream.len() + (align4(vs.len()) - vs.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (PS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
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

        // CREATE_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&(ILAY_POS3_TEX2.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(ILAY_POS3_TEX2);
        stream.resize(
            stream.len() + (align4(ILAY_POS3_TEX2.len()) - ILAY_POS3_TEX2.len()),
            0,
        );
        end_cmd(&mut stream, start);

        // SET_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // BIND_SHADERS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::BindShaders as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // cs
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_VERTEX_BUFFERS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetVertexBuffers as u32);
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
        stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&(core::mem::size_of::<VertexPos3Tex2>() as u32).to_le_bytes()); // stride_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_RENDER_TARGETS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetRenderTargets as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // depth_stencil
        stream.extend_from_slice(&RT.to_le_bytes());
        for _ in 0..7 {
            stream.extend_from_slice(&0u32.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        // SET_PRIMITIVE_TOPOLOGY
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetPrimitiveTopology as u32);
        stream.extend_from_slice(&(AerogpuPrimitiveTopology::TriangleList as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CLEAR to opaque black.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Clear as u32);
        stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
        for bits in [0.0f32, 0.0, 0.0, 1.0].map(f32::to_bits) {
            stream.extend_from_slice(&bits.to_le_bytes());
        }
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // depth
        stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
        end_cmd(&mut stream, start);

        let mut draw = |x: f32, texture: u32| {
            // VIEWPORT x..x+1
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetViewport as u32);
            stream.extend_from_slice(&x.to_bits().to_le_bytes());
            stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
            stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // width
            stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // height
            stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
            stream.extend_from_slice(&1f32.to_bits().to_le_bytes());
            end_cmd(&mut stream, start);

            // SET_TEXTURE (PS t0)
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetTexture as u32);
            stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
            stream.extend_from_slice(&0u32.to_le_bytes()); // slot
            stream.extend_from_slice(&texture.to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
            end_cmd(&mut stream, start);

            // DRAW
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Draw as u32);
            stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
            stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
            stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
            stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
            end_cmd(&mut stream, start);
        };

        draw(0.0, TEX_RED);
        draw(1.0, TEX_GREEN);
        // Repeat to exercise bind-group caching.
        draw(0.0, TEX_RED);
        draw(1.0, TEX_GREEN);

        let stream = finish_stream(stream);

        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let stats = exec.cache_stats();
        assert_eq!(stats.samplers.misses, 1);
        assert_eq!(stats.samplers.entries, 1);
        assert_eq!(stats.bind_group_layouts.misses, 2);
        assert_eq!(stats.bind_group_layouts.entries, 2);
        // One bind group is created for the empty VS bind group (group 0), plus two for the
        // alternating textures in the PS bind group (group 1).
        assert_eq!(stats.bind_groups.misses, 3);
        assert_eq!(stats.bind_groups.hits, 2);
        assert_eq!(stats.bind_groups.entries, 3);

        let pixels = exec.read_texture_rgba8(RT).await.unwrap();
        assert_eq!(pixels.len(), 2 * 4);
        assert_eq!(&pixels[0..4], &[255, 0, 0, 255]);
        assert_eq!(&pixels[4..8], &[0, 255, 0, 255]);
    });
}

#[test]
fn aerogpu_cmd_rebinds_allocation_backed_texture_between_draws_uploads_second_texture() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const VB: u32 = 1;
        const TEX_RED: u32 = 2;
        const TEX_GREEN: u32 = 3;
        const RT: u32 = 4;
        const VS: u32 = 10;
        const PS: u32 = 11;
        const IL: u32 = 20;

        let vs = build_vs_pos3_tex2_to_pos_tex_dxbc();

        let vertices = [
            VertexPos3Tex2 {
                pos: [-1.0, -3.0, 0.0],
                uv: [0.5, 0.5],
            },
            VertexPos3Tex2 {
                pos: [-1.0, 1.0, 0.0],
                uv: [0.5, 0.5],
            },
            VertexPos3Tex2 {
                pos: [3.0, 1.0, 0.0],
                uv: [0.5, 0.5],
            },
        ];
        let vb_bytes = bytemuck::bytes_of(&vertices);

        // Allocation-backed textures start `dirty=true` on the host and are uploaded lazily on
        // first use. This test binds TEX_RED before the first draw (so it gets uploaded), then
        // binds TEX_GREEN between draws. Since TEX_GREEN was not used by any prior draw in the
        // pass, the executor should upload it without restarting the render pass.
        let alloc_id = 1u32;
        let alloc_gpa = 0x100u64;
        let allocs = [AerogpuAllocEntry {
            alloc_id,
            flags: 0,
            gpa: alloc_gpa,
            size_bytes: 0x1000,
            reserved0: 0,
        }];

        let mut stream = Vec::new();
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_BUFFER (host allocated VB)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER.to_le_bytes());
        stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (VB)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes()); // size_bytes
        stream.extend_from_slice(vb_bytes);
        stream.resize(stream.len() + (align4(vb_bytes.len()) - vb_bytes.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (red, allocation-backed 1x1)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&TEX_RED.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&4u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&alloc_id.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (green, allocation-backed 1x1)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&TEX_GREEN.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&4u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&alloc_id.to_le_bytes());
        stream.extend_from_slice(&4u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (RT 2x1)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&RT.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&2u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (VS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // stage = vertex
        stream.extend_from_slice(&(vs.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&vs);
        stream.resize(stream.len() + (align4(vs.len()) - vs.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (PS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
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

        // CREATE_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&(ILAY_POS3_TEX2.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(ILAY_POS3_TEX2);
        stream.resize(
            stream.len() + (align4(ILAY_POS3_TEX2.len()) - ILAY_POS3_TEX2.len()),
            0,
        );
        end_cmd(&mut stream, start);

        // SET_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // BIND_SHADERS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::BindShaders as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // cs
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_VERTEX_BUFFERS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetVertexBuffers as u32);
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
        stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&(core::mem::size_of::<VertexPos3Tex2>() as u32).to_le_bytes()); // stride_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_RENDER_TARGETS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetRenderTargets as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // depth_stencil
        stream.extend_from_slice(&RT.to_le_bytes());
        for _ in 0..7 {
            stream.extend_from_slice(&0u32.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        // SET_PRIMITIVE_TOPOLOGY
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetPrimitiveTopology as u32);
        stream.extend_from_slice(&(AerogpuPrimitiveTopology::TriangleList as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CLEAR to opaque black.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Clear as u32);
        stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
        for bits in [0.0f32, 0.0, 0.0, 1.0].map(f32::to_bits) {
            stream.extend_from_slice(&bits.to_le_bytes());
        }
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // depth
        stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
        end_cmd(&mut stream, start);

        let mut draw = |x: f32, texture: u32| {
            // VIEWPORT x..x+1
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetViewport as u32);
            stream.extend_from_slice(&x.to_bits().to_le_bytes());
            stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
            stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // width
            stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // height
            stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
            stream.extend_from_slice(&1f32.to_bits().to_le_bytes());
            end_cmd(&mut stream, start);

            // SET_TEXTURE (PS t0)
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetTexture as u32);
            stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
            stream.extend_from_slice(&0u32.to_le_bytes()); // slot
            stream.extend_from_slice(&texture.to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
            end_cmd(&mut stream, start);

            // DRAW
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Draw as u32);
            stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
            stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
            stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
            stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
            end_cmd(&mut stream, start);
        };

        draw(0.0, TEX_RED);
        draw(1.0, TEX_GREEN);

        let stream = finish_stream(stream);

        let mut guest_mem = VecGuestMemory::new(0x2000);
        guest_mem
            .write(alloc_gpa, &[255u8, 0, 0, 255])
            .expect("write red texel");
        guest_mem
            .write(alloc_gpa + 4, &[0u8, 255, 0, 255])
            .expect("write green texel");

        exec.execute_cmd_stream(&stream, Some(&allocs), &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let stats = exec.cache_stats();
        assert_eq!(stats.bind_group_layouts.misses, 2);
        assert_eq!(stats.bind_group_layouts.hits, 0);
        assert_eq!(stats.bind_group_layouts.entries, 2);

        let pixels = exec.read_texture_rgba8(RT).await.unwrap();
        assert_eq!(pixels.len(), 2 * 4);
        assert_eq!(&pixels[0..4], &[255, 0, 0, 255]);
        assert_eq!(&pixels[4..8], &[0, 255, 0, 255]);
    });
}

#[test]
fn aerogpu_cmd_binding_dirty_resources_to_unused_slots_does_not_restart_render_pass() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const VB: u32 = 1;
        const TEX_USED: u32 = 2;
        const CB_UNUSED: u32 = 3;
        const TEX_UNUSED: u32 = 4;
        const RT: u32 = 5;
        const VS: u32 = 10;
        const PS: u32 = 11;
        const IL: u32 = 20;

        let vs = build_vs_pos3_tex2_to_pos_tex_dxbc();

        let vertices = [
            VertexPos3Tex2 {
                pos: [-1.0, -3.0, 0.0],
                uv: [0.5, 0.5],
            },
            VertexPos3Tex2 {
                pos: [-1.0, 1.0, 0.0],
                uv: [0.5, 0.5],
            },
            VertexPos3Tex2 {
                pos: [3.0, 1.0, 0.0],
                uv: [0.5, 0.5],
            },
        ];
        let vb_bytes = bytemuck::bytes_of(&vertices);

        let alloc_id = 1u32;
        let alloc_gpa = 0x100u64;
        let allocs = [AerogpuAllocEntry {
            alloc_id,
            flags: 0,
            gpa: alloc_gpa,
            size_bytes: 0x1000,
            reserved0: 0,
        }];

        let mut stream = Vec::new();
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_BUFFER (host allocated VB)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER.to_le_bytes());
        stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (VB)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes()); // size_bytes
        stream.extend_from_slice(vb_bytes);
        stream.resize(stream.len() + (align4(vb_bytes.len()) - vb_bytes.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_BUFFER (CB_UNUSED, allocation-backed)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
        stream.extend_from_slice(&CB_UNUSED.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER.to_le_bytes());
        stream.extend_from_slice(&64u64.to_le_bytes()); // size_bytes
        stream.extend_from_slice(&alloc_id.to_le_bytes());
        stream.extend_from_slice(&0x100u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (TEX_USED, host allocated 1x1)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&TEX_USED.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (TEX_USED)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&TEX_USED.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&4u64.to_le_bytes()); // size_bytes
        stream.extend_from_slice(&[255u8, 0, 0, 255]); // red
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (TEX_UNUSED, allocation-backed 1x1)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&TEX_UNUSED.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&4u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&alloc_id.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (RT 1x1)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&RT.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (VS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // stage = vertex
        stream.extend_from_slice(&(vs.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&vs);
        stream.resize(stream.len() + (align4(vs.len()) - vs.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (PS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
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

        // CREATE_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&(ILAY_POS3_TEX2.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(ILAY_POS3_TEX2);
        stream.resize(
            stream.len() + (align4(ILAY_POS3_TEX2.len()) - ILAY_POS3_TEX2.len()),
            0,
        );
        end_cmd(&mut stream, start);

        // SET_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // BIND_SHADERS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::BindShaders as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // cs
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_VERTEX_BUFFERS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetVertexBuffers as u32);
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
        stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&(core::mem::size_of::<VertexPos3Tex2>() as u32).to_le_bytes()); // stride_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_RENDER_TARGETS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetRenderTargets as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // depth_stencil
        stream.extend_from_slice(&RT.to_le_bytes());
        for _ in 0..7 {
            stream.extend_from_slice(&0u32.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        // SET_PRIMITIVE_TOPOLOGY
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetPrimitiveTopology as u32);
        stream.extend_from_slice(&(AerogpuPrimitiveTopology::TriangleList as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_VIEWPORT (x=0, y=0, width=1, height=1)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetViewport as u32);
        stream.extend_from_slice(&0.0f32.to_bits().to_le_bytes()); // x
        stream.extend_from_slice(&0.0f32.to_bits().to_le_bytes()); // y
        stream.extend_from_slice(&1.0f32.to_bits().to_le_bytes()); // width
        stream.extend_from_slice(&1.0f32.to_bits().to_le_bytes()); // height
        stream.extend_from_slice(&0.0f32.to_bits().to_le_bytes()); // min_depth
        stream.extend_from_slice(&1.0f32.to_bits().to_le_bytes()); // max_depth
        end_cmd(&mut stream, start);

        // SET_TEXTURE (PS t0 = TEX_USED)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetTexture as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // slot
        stream.extend_from_slice(&TEX_USED.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // DRAW
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Draw as u32);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        // SET_CONSTANT_BUFFERS (VS cb1 = CB_UNUSED; VS has no cb bindings)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetConstantBuffers as u32);
        stream.extend_from_slice(&0u32.to_le_bytes()); // shader_stage = vertex
        stream.extend_from_slice(&1u32.to_le_bytes()); // start_slot
        stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&CB_UNUSED.to_le_bytes()); // buffer
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (0 = full)
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_TEXTURE (PS t1 = TEX_UNUSED; PS samples only t0)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetTexture as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&1u32.to_le_bytes()); // slot
        stream.extend_from_slice(&TEX_UNUSED.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // DRAW
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Draw as u32);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        let stream = finish_stream(stream);

        let mut guest_mem = VecGuestMemory::new(0x2000);
        exec.execute_cmd_stream(&stream, Some(&allocs), &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let stats = exec.cache_stats();
        assert_eq!(stats.bind_group_layouts.misses, 2);
        assert_eq!(stats.bind_group_layouts.hits, 0);
        assert_eq!(stats.bind_group_layouts.entries, 2);
        assert_eq!(stats.bind_groups.misses, 2);
        assert_eq!(stats.bind_groups.hits, 1);
        assert_eq!(stats.bind_groups.entries, 2);

        let pixels = exec.read_texture_rgba8(RT).await.unwrap();
        assert_eq!(pixels.len(), 4);
        assert_eq!(&pixels[0..4], &[255, 0, 0, 255]);
    });
}

#[test]
fn aerogpu_cmd_rebinds_vertex_buffer_between_draws_without_restarting_render_pass() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const VB_A: u32 = 1;
        const VB_B: u32 = 2;
        const TEX: u32 = 3;
        const RT: u32 = 4;
        const VS: u32 = 10;
        const PS: u32 = 11;
        const IL: u32 = 20;

        let vs = build_vs_pos3_tex2_to_pos_tex_dxbc();

        let vertices_a = [
            VertexPos3Tex2 {
                pos: [-1.0, -3.0, 0.0],
                uv: [0.25, 0.5],
            },
            VertexPos3Tex2 {
                pos: [-1.0, 1.0, 0.0],
                uv: [0.25, 0.5],
            },
            VertexPos3Tex2 {
                pos: [3.0, 1.0, 0.0],
                uv: [0.25, 0.5],
            },
        ];
        let vertices_b = [
            VertexPos3Tex2 {
                pos: [-1.0, -3.0, 0.0],
                uv: [0.75, 0.5],
            },
            VertexPos3Tex2 {
                pos: [-1.0, 1.0, 0.0],
                uv: [0.75, 0.5],
            },
            VertexPos3Tex2 {
                pos: [3.0, 1.0, 0.0],
                uv: [0.75, 0.5],
            },
        ];
        let vb_a_bytes = bytemuck::bytes_of(&vertices_a);
        let vb_b_bytes = bytemuck::bytes_of(&vertices_b);

        let mut stream = Vec::new();
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        for (handle, data) in [(VB_A, vb_a_bytes), (VB_B, vb_b_bytes)] {
            // CREATE_BUFFER (VB)
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
            stream.extend_from_slice(&handle.to_le_bytes());
            stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER.to_le_bytes());
            stream.extend_from_slice(&(data.len() as u64).to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
            stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
            stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
            end_cmd(&mut stream, start);

            // UPLOAD_RESOURCE (VB)
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
            stream.extend_from_slice(&handle.to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
            stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
            stream.extend_from_slice(&(data.len() as u64).to_le_bytes()); // size_bytes
            stream.extend_from_slice(data);
            stream.resize(stream.len() + (align4(data.len()) - data.len()), 0);
            end_cmd(&mut stream, start);
        }

        // CREATE_TEXTURE2D (TEX 2x1)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&2u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (TEX): [red, green]
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&8u64.to_le_bytes()); // size_bytes
        stream.extend_from_slice(&[255u8, 0, 0, 255, 0, 255, 0, 255]);
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (RT 2x1)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&RT.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&2u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (VS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // stage = vertex
        stream.extend_from_slice(&(vs.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&vs);
        stream.resize(stream.len() + (align4(vs.len()) - vs.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (PS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
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

        // CREATE_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&(ILAY_POS3_TEX2.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(ILAY_POS3_TEX2);
        stream.resize(
            stream.len() + (align4(ILAY_POS3_TEX2.len()) - ILAY_POS3_TEX2.len()),
            0,
        );
        end_cmd(&mut stream, start);

        // SET_RENDER_TARGETS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetRenderTargets as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // depth_stencil
        stream.extend_from_slice(&RT.to_le_bytes());
        for _ in 0..7 {
            stream.extend_from_slice(&0u32.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        // BIND_SHADERS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::BindShaders as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // cs
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_VERTEX_BUFFERS (slot 0 = VB_A)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetVertexBuffers as u32);
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
        stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
        stream.extend_from_slice(&VB_A.to_le_bytes()); // buffer
        stream.extend_from_slice(&(core::mem::size_of::<VertexPos3Tex2>() as u32).to_le_bytes()); // stride_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_PRIMITIVE_TOPOLOGY
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetPrimitiveTopology as u32);
        stream.extend_from_slice(&(AerogpuPrimitiveTopology::TriangleList as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_TEXTURE (PS t0)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetTexture as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // slot
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CLEAR to black.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Clear as u32);
        stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
        for bits in [0.0f32, 0.0, 0.0, 1.0].map(f32::to_bits) {
            stream.extend_from_slice(&bits.to_le_bytes());
        }
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // depth
        stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
        end_cmd(&mut stream, start);

        // VIEWPORT x=0 width=1
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetViewport as u32);
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // x
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // y
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // width
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // height
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // min_depth
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // max_depth
        end_cmd(&mut stream, start);

        // DRAW (left pixel red)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Draw as u32);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        // SET_VERTEX_BUFFERS (slot 0 = VB_B) between draws.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetVertexBuffers as u32);
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
        stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
        stream.extend_from_slice(&VB_B.to_le_bytes()); // buffer
        stream.extend_from_slice(&(core::mem::size_of::<VertexPos3Tex2>() as u32).to_le_bytes()); // stride_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // VIEWPORT x=1 width=1
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetViewport as u32);
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // x
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // y
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // width
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // height
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // min_depth
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // max_depth
        end_cmd(&mut stream, start);

        // DRAW (right pixel green)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Draw as u32);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        let stream = finish_stream(stream);

        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let stats = exec.cache_stats();
        assert_eq!(stats.bind_group_layouts.misses, 2);
        assert_eq!(stats.bind_group_layouts.hits, 0);
        assert_eq!(stats.bind_group_layouts.entries, 2);

        let pixels = exec.read_texture_rgba8(RT).await.unwrap();
        assert_eq!(pixels.len(), 2 * 4);
        assert_eq!(&pixels[0..4], &[255, 0, 0, 255]);
        assert_eq!(&pixels[4..8], &[0, 255, 0, 255]);
    });
}

#[test]
fn aerogpu_cmd_rebinds_allocation_backed_vertex_buffer_between_draws_does_not_restart_render_pass()
{
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const VB_A: u32 = 1;
        const VB_B: u32 = 2;
        const TEX: u32 = 3;
        const RT: u32 = 4;
        const VS: u32 = 10;
        const PS: u32 = 11;
        const IL: u32 = 20;

        let vs = build_vs_pos3_tex2_to_pos_tex_dxbc();

        let vertices_a = [
            VertexPos3Tex2 {
                pos: [-1.0, -3.0, 0.0],
                uv: [0.25, 0.5],
            },
            VertexPos3Tex2 {
                pos: [-1.0, 1.0, 0.0],
                uv: [0.25, 0.5],
            },
            VertexPos3Tex2 {
                pos: [3.0, 1.0, 0.0],
                uv: [0.25, 0.5],
            },
        ];
        let vertices_b = [
            VertexPos3Tex2 {
                pos: [-1.0, -3.0, 0.0],
                uv: [0.75, 0.5],
            },
            VertexPos3Tex2 {
                pos: [-1.0, 1.0, 0.0],
                uv: [0.75, 0.5],
            },
            VertexPos3Tex2 {
                pos: [3.0, 1.0, 0.0],
                uv: [0.75, 0.5],
            },
        ];
        let vb_a_bytes = bytemuck::bytes_of(&vertices_a);
        let vb_b_bytes = bytemuck::bytes_of(&vertices_b);

        let alloc_id = 1u32;
        let alloc_gpa = 0x100u64;
        let allocs = [AerogpuAllocEntry {
            alloc_id,
            flags: 0,
            gpa: alloc_gpa,
            size_bytes: 0x1000,
            reserved0: 0,
        }];

        let mut stream = Vec::new();
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        for (handle, data, backing_offset_bytes) in
            [(VB_A, vb_a_bytes, 0u32), (VB_B, vb_b_bytes, 0x100u32)]
        {
            // CREATE_BUFFER (allocation-backed VB)
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
            stream.extend_from_slice(&handle.to_le_bytes());
            stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER.to_le_bytes());
            stream.extend_from_slice(&(data.len() as u64).to_le_bytes());
            stream.extend_from_slice(&alloc_id.to_le_bytes());
            stream.extend_from_slice(&backing_offset_bytes.to_le_bytes());
            stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
            end_cmd(&mut stream, start);
        }

        // CREATE_TEXTURE2D (TEX 2x1)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&2u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (TEX): [red, green]
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&8u64.to_le_bytes()); // size_bytes
        stream.extend_from_slice(&[255u8, 0, 0, 255, 0, 255, 0, 255]);
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (RT 2x1)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&RT.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&2u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (VS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // stage = vertex
        stream.extend_from_slice(&(vs.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&vs);
        stream.resize(stream.len() + (align4(vs.len()) - vs.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (PS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
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

        // CREATE_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&(ILAY_POS3_TEX2.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(ILAY_POS3_TEX2);
        stream.resize(
            stream.len() + (align4(ILAY_POS3_TEX2.len()) - ILAY_POS3_TEX2.len()),
            0,
        );
        end_cmd(&mut stream, start);

        // SET_RENDER_TARGETS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetRenderTargets as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // depth_stencil
        stream.extend_from_slice(&RT.to_le_bytes());
        for _ in 0..7 {
            stream.extend_from_slice(&0u32.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        // BIND_SHADERS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::BindShaders as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // cs
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_VERTEX_BUFFERS (slot 0 = VB_A)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetVertexBuffers as u32);
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
        stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
        stream.extend_from_slice(&VB_A.to_le_bytes()); // buffer
        stream.extend_from_slice(&(core::mem::size_of::<VertexPos3Tex2>() as u32).to_le_bytes()); // stride_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_PRIMITIVE_TOPOLOGY
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetPrimitiveTopology as u32);
        stream.extend_from_slice(&(AerogpuPrimitiveTopology::TriangleList as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_TEXTURE (PS t0)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetTexture as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // slot
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CLEAR to black.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Clear as u32);
        stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
        for bits in [0.0f32, 0.0, 0.0, 1.0].map(f32::to_bits) {
            stream.extend_from_slice(&bits.to_le_bytes());
        }
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // depth
        stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
        end_cmd(&mut stream, start);

        // VIEWPORT x=0 width=1
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetViewport as u32);
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // x
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // y
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // width
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // height
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // min_depth
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // max_depth
        end_cmd(&mut stream, start);

        // DRAW (left pixel red)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Draw as u32);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        // SET_VERTEX_BUFFERS (slot 0 = VB_B) between draws.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetVertexBuffers as u32);
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
        stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
        stream.extend_from_slice(&VB_B.to_le_bytes()); // buffer
        stream.extend_from_slice(&(core::mem::size_of::<VertexPos3Tex2>() as u32).to_le_bytes()); // stride_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // VIEWPORT x=1 width=1
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetViewport as u32);
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // x
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // y
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // width
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // height
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // min_depth
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // max_depth
        end_cmd(&mut stream, start);

        // DRAW (right pixel green)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Draw as u32);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        let stream = finish_stream(stream);

        let mut guest_mem = VecGuestMemory::new(0x2000);
        guest_mem.write(alloc_gpa, vb_a_bytes).expect("write VB_A");
        guest_mem
            .write(alloc_gpa + 0x100, vb_b_bytes)
            .expect("write VB_B");

        exec.execute_cmd_stream(&stream, Some(&allocs), &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let stats = exec.cache_stats();
        assert_eq!(stats.bind_group_layouts.misses, 2);
        assert_eq!(stats.bind_group_layouts.hits, 0);
        assert_eq!(stats.bind_group_layouts.entries, 2);

        let pixels = exec.read_texture_rgba8(RT).await.unwrap();
        assert_eq!(pixels.len(), 2 * 4);
        assert_eq!(&pixels[0..4], &[255, 0, 0, 255]);
        assert_eq!(&pixels[4..8], &[0, 255, 0, 255]);
    });
}

#[test]
fn aerogpu_cmd_rebinds_allocation_backed_index_buffer_between_draws_does_not_restart_render_pass() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const VB: u32 = 1;
        const IB_A: u32 = 2;
        const IB_B: u32 = 3;
        const TEX: u32 = 4;
        const RT: u32 = 5;
        const VS: u32 = 10;
        const PS: u32 = 11;
        const IL: u32 = 20;

        let vs = build_vs_pos3_tex2_to_pos_tex_dxbc();

        let vertices = [
            // Triangle sampling the left texel (red).
            VertexPos3Tex2 {
                pos: [-1.0, -3.0, 0.0],
                uv: [0.25, 0.5],
            },
            VertexPos3Tex2 {
                pos: [-1.0, 1.0, 0.0],
                uv: [0.25, 0.5],
            },
            VertexPos3Tex2 {
                pos: [3.0, 1.0, 0.0],
                uv: [0.25, 0.5],
            },
            // Triangle sampling the right texel (green).
            VertexPos3Tex2 {
                pos: [-1.0, -3.0, 0.0],
                uv: [0.75, 0.5],
            },
            VertexPos3Tex2 {
                pos: [-1.0, 1.0, 0.0],
                uv: [0.75, 0.5],
            },
            VertexPos3Tex2 {
                pos: [3.0, 1.0, 0.0],
                uv: [0.75, 0.5],
            },
        ];
        let vb_bytes = bytemuck::bytes_of(&vertices);

        let indices_a: [u16; 3] = [0, 1, 2];
        let indices_b: [u16; 3] = [3, 4, 5];

        let alloc_id = 1u32;
        let alloc_gpa = 0x100u64;
        let allocs = [AerogpuAllocEntry {
            alloc_id,
            flags: 0,
            gpa: alloc_gpa,
            size_bytes: 0x1000,
            reserved0: 0,
        }];

        let mut stream = Vec::new();
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_BUFFER (VB)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER.to_le_bytes());
        stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (VB)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes()); // size_bytes
        stream.extend_from_slice(vb_bytes);
        stream.resize(stream.len() + (align4(vb_bytes.len()) - vb_bytes.len()), 0);
        end_cmd(&mut stream, start);

        for (handle, backing_offset_bytes) in [(IB_A, 0u32), (IB_B, 0x100u32)] {
            // CREATE_BUFFER (allocation-backed index buffer)
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
            stream.extend_from_slice(&handle.to_le_bytes());
            stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_INDEX_BUFFER.to_le_bytes());
            stream.extend_from_slice(&(6u64).to_le_bytes()); // size_bytes
            stream.extend_from_slice(&alloc_id.to_le_bytes());
            stream.extend_from_slice(&backing_offset_bytes.to_le_bytes());
            stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
            end_cmd(&mut stream, start);
        }

        // CREATE_TEXTURE2D (TEX 2x1)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&2u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (TEX): [red, green]
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&8u64.to_le_bytes()); // size_bytes
        stream.extend_from_slice(&[255u8, 0, 0, 255, 0, 255, 0, 255]);
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (RT 2x1)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&RT.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&2u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (VS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // stage = vertex
        stream.extend_from_slice(&(vs.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&vs);
        stream.resize(stream.len() + (align4(vs.len()) - vs.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (PS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
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

        // CREATE_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&(ILAY_POS3_TEX2.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(ILAY_POS3_TEX2);
        stream.resize(
            stream.len() + (align4(ILAY_POS3_TEX2.len()) - ILAY_POS3_TEX2.len()),
            0,
        );
        end_cmd(&mut stream, start);

        // SET_RENDER_TARGETS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetRenderTargets as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // depth_stencil
        stream.extend_from_slice(&RT.to_le_bytes());
        for _ in 0..7 {
            stream.extend_from_slice(&0u32.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        // SET_VIEWPORT x=0 width=1
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetViewport as u32);
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // x
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // y
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // width
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // height
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // min_depth
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // max_depth
        end_cmd(&mut stream, start);

        // BIND_SHADERS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::BindShaders as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // cs
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_VERTEX_BUFFERS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetVertexBuffers as u32);
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
        stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
        stream.extend_from_slice(&VB.to_le_bytes()); // buffer
        stream.extend_from_slice(&(core::mem::size_of::<VertexPos3Tex2>() as u32).to_le_bytes()); // stride_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_INDEX_BUFFER (IB_A)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetIndexBuffer as u32);
        stream.extend_from_slice(&IB_A.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // format = u16
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_PRIMITIVE_TOPOLOGY
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetPrimitiveTopology as u32);
        stream.extend_from_slice(&(AerogpuPrimitiveTopology::TriangleList as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_TEXTURE (PS t0)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetTexture as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // slot
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CLEAR to opaque black.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Clear as u32);
        stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
        for bits in [0.0f32, 0.0, 0.0, 1.0].map(f32::to_bits) {
            stream.extend_from_slice(&bits.to_le_bytes());
        }
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // depth
        stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
        end_cmd(&mut stream, start);

        // DRAW_INDEXED (left pixel red)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::DrawIndexed as u32);
        stream.extend_from_slice(&3u32.to_le_bytes()); // index_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_index
        stream.extend_from_slice(&0i32.to_le_bytes()); // base_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        // VIEWPORT x=1 width=1
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetViewport as u32);
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // x
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // y
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // width
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // height
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // min_depth
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // max_depth
        end_cmd(&mut stream, start);

        // SET_INDEX_BUFFER (IB_B) between draws.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetIndexBuffer as u32);
        stream.extend_from_slice(&IB_B.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // format = u16
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // DRAW_INDEXED (right pixel green)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::DrawIndexed as u32);
        stream.extend_from_slice(&3u32.to_le_bytes()); // index_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_index
        stream.extend_from_slice(&0i32.to_le_bytes()); // base_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        let stream = finish_stream(stream);

        let mut guest_mem = VecGuestMemory::new(0x2000);
        guest_mem
            .write(alloc_gpa, bytemuck::bytes_of(&indices_a))
            .expect("write IB_A indices");
        guest_mem
            .write(alloc_gpa + 0x100, bytemuck::bytes_of(&indices_b))
            .expect("write IB_B indices");

        exec.execute_cmd_stream(&stream, Some(&allocs), &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let stats = exec.cache_stats();
        assert_eq!(stats.bind_group_layouts.misses, 2);
        assert_eq!(stats.bind_group_layouts.hits, 0);
        assert_eq!(stats.bind_group_layouts.entries, 2);

        let pixels = exec.read_texture_rgba8(RT).await.unwrap();
        assert_eq!(pixels.len(), 2 * 4);
        assert_eq!(&pixels[0..4], &[255, 0, 0, 255]);
        assert_eq!(&pixels[4..8], &[0, 255, 0, 255]);
    });
}

#[test]
fn aerogpu_cmd_rebinds_allocation_backed_constant_buffer_between_draws_does_not_restart_render_pass(
) {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const VB: u32 = 1;
        const CB_A: u32 = 2;
        const CB_B: u32 = 3;
        const RT: u32 = 4;
        const VS: u32 = 10;
        const PS: u32 = 11;
        const IL: u32 = 20;

        let ps_red = build_ps_solid_red_dxbc();
        let ilay = build_ilay_pos3();

        let vertices = [
            VertexPos3 {
                pos: [-1.0, -3.0, 0.0],
            },
            VertexPos3 {
                pos: [-1.0, 1.0, 0.0],
            },
            VertexPos3 {
                pos: [0.0, 1.0, 0.0],
            },
        ];
        let vb_bytes = bytemuck::bytes_of(&vertices);

        let matrix_a: [f32; 16] = [
            1.0, 0.0, 0.0, 0.0, //
            0.0, 1.0, 0.0, 0.0, //
            0.0, 0.0, 1.0, 0.0, //
            0.0, 0.0, 0.0, 1.0, //
        ];
        let matrix_b: [f32; 16] = [
            1.0, 0.0, 0.0, 1.0, //
            0.0, 1.0, 0.0, 0.0, //
            0.0, 0.0, 1.0, 0.0, //
            0.0, 0.0, 0.0, 1.0, //
        ];

        let alloc_id = 1u32;
        let alloc_gpa = 0x100u64;
        let allocs = [AerogpuAllocEntry {
            alloc_id,
            flags: 0,
            gpa: alloc_gpa,
            size_bytes: 0x1000,
            reserved0: 0,
        }];

        let mut stream = Vec::new();
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_BUFFER (VB)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER.to_le_bytes());
        stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (VB)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes()); // size_bytes
        stream.extend_from_slice(vb_bytes);
        stream.resize(stream.len() + (align4(vb_bytes.len()) - vb_bytes.len()), 0);
        end_cmd(&mut stream, start);

        for (handle, backing_offset_bytes) in [(CB_A, 0u32), (CB_B, 0x100u32)] {
            // CREATE_BUFFER (allocation-backed constant buffer)
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
            stream.extend_from_slice(&handle.to_le_bytes());
            stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER.to_le_bytes());
            stream.extend_from_slice(&(64u64).to_le_bytes()); // size_bytes
            stream.extend_from_slice(&alloc_id.to_le_bytes());
            stream.extend_from_slice(&backing_offset_bytes.to_le_bytes());
            stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
            end_cmd(&mut stream, start);
        }

        // CREATE_TEXTURE2D (RT 2x1)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&RT.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&2u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (VS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // stage = vertex
        stream.extend_from_slice(&(DXBC_VS_MATRIX.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(DXBC_VS_MATRIX);
        stream.resize(
            stream.len() + (align4(DXBC_VS_MATRIX.len()) - DXBC_VS_MATRIX.len()),
            0,
        );
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (PS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // stage = pixel
        stream.extend_from_slice(&(ps_red.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&ps_red);
        stream.resize(stream.len() + (align4(ps_red.len()) - ps_red.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&(ilay.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&ilay);
        stream.resize(stream.len() + (align4(ilay.len()) - ilay.len()), 0);
        end_cmd(&mut stream, start);

        // SET_RENDER_TARGETS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetRenderTargets as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // depth_stencil
        stream.extend_from_slice(&RT.to_le_bytes());
        for _ in 0..7 {
            stream.extend_from_slice(&0u32.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        // SET_VIEWPORT 0..2
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetViewport as u32);
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&2f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes());
        end_cmd(&mut stream, start);

        // BIND_SHADERS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::BindShaders as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // cs
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_VERTEX_BUFFERS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetVertexBuffers as u32);
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
        stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
        stream.extend_from_slice(&VB.to_le_bytes()); // buffer
        stream.extend_from_slice(&(core::mem::size_of::<VertexPos3>() as u32).to_le_bytes()); // stride_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_PRIMITIVE_TOPOLOGY
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetPrimitiveTopology as u32);
        stream.extend_from_slice(&(AerogpuPrimitiveTopology::TriangleList as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_CONSTANT_BUFFERS (VS cb0 = CB_A)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetConstantBuffers as u32);
        stream.extend_from_slice(&0u32.to_le_bytes()); // stage = vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
        stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&CB_A.to_le_bytes()); // buffer
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&64u32.to_le_bytes()); // size_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CLEAR to opaque black.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Clear as u32);
        stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
        for bits in [0.0f32, 0.0, 0.0, 1.0].map(f32::to_bits) {
            stream.extend_from_slice(&bits.to_le_bytes());
        }
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // depth
        stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
        end_cmd(&mut stream, start);

        // DRAW (left pixel red)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Draw as u32);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        // SET_CONSTANT_BUFFERS (VS cb0 = CB_B) between draws.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetConstantBuffers as u32);
        stream.extend_from_slice(&0u32.to_le_bytes()); // stage = vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
        stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&CB_B.to_le_bytes()); // buffer
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&64u32.to_le_bytes()); // size_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // DRAW (right pixel red)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Draw as u32);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        let stream = finish_stream(stream);

        let mut guest_mem = VecGuestMemory::new(0x2000);
        guest_mem
            .write(alloc_gpa, bytemuck::bytes_of(&matrix_a))
            .expect("write CB_A matrix");
        guest_mem
            .write(alloc_gpa + 0x100, bytemuck::bytes_of(&matrix_b))
            .expect("write CB_B matrix");

        exec.execute_cmd_stream(&stream, Some(&allocs), &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let stats = exec.cache_stats();
        assert_eq!(stats.bind_group_layouts.hits, 0);

        let pixels = exec.read_texture_rgba8(RT).await.unwrap();
        assert_eq!(pixels.len(), 2 * 4);
        assert_eq!(&pixels[0..4], &[255, 0, 0, 255]);
        assert_eq!(&pixels[4..8], &[255, 0, 0, 255]);
    });
}

#[test]
fn aerogpu_cmd_rebinds_pipeline_state_noops_between_draws_without_restarting_render_pass() {
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

        let vs = build_vs_pos3_tex2_to_pos_tex_dxbc();

        let vertices = [
            VertexPos3Tex2 {
                pos: [-1.0, -3.0, 0.0],
                uv: [0.5, 0.5],
            },
            VertexPos3Tex2 {
                pos: [-1.0, 1.0, 0.0],
                uv: [0.5, 0.5],
            },
            VertexPos3Tex2 {
                pos: [3.0, 1.0, 0.0],
                uv: [0.5, 0.5],
            },
        ];
        let vb_bytes = bytemuck::bytes_of(&vertices);

        let mut stream = Vec::new();
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_BUFFER (VB)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER.to_le_bytes());
        stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (VB)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes()); // size_bytes
        stream.extend_from_slice(vb_bytes);
        stream.resize(stream.len() + (align4(vb_bytes.len()) - vb_bytes.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (TEX 1x1)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (TEX)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&4u64.to_le_bytes()); // size_bytes
        stream.extend_from_slice(&[255u8, 0, 0, 255]); // red
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (RT 1x1)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&RT.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (VS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // stage = vertex
        stream.extend_from_slice(&(vs.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&vs);
        stream.resize(stream.len() + (align4(vs.len()) - vs.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (PS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
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

        // CREATE_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&(ILAY_POS3_TEX2.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(ILAY_POS3_TEX2);
        stream.resize(
            stream.len() + (align4(ILAY_POS3_TEX2.len()) - ILAY_POS3_TEX2.len()),
            0,
        );
        end_cmd(&mut stream, start);

        // SET_RENDER_TARGETS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetRenderTargets as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // depth_stencil
        stream.extend_from_slice(&RT.to_le_bytes());
        for _ in 0..7 {
            stream.extend_from_slice(&0u32.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        // SET_VIEWPORT 0..1
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetViewport as u32);
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes());
        end_cmd(&mut stream, start);

        // BIND_SHADERS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::BindShaders as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // cs
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_VERTEX_BUFFERS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetVertexBuffers as u32);
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
        stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
        stream.extend_from_slice(&VB.to_le_bytes()); // buffer
        stream.extend_from_slice(&(core::mem::size_of::<VertexPos3Tex2>() as u32).to_le_bytes()); // stride_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_PRIMITIVE_TOPOLOGY
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetPrimitiveTopology as u32);
        stream.extend_from_slice(&(AerogpuPrimitiveTopology::TriangleList as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_TEXTURE (PS t0)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetTexture as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // slot
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CLEAR to black.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Clear as u32);
        stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
        for bits in [0.0f32, 0.0, 0.0, 1.0].map(f32::to_bits) {
            stream.extend_from_slice(&bits.to_le_bytes());
        }
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // depth
        stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
        end_cmd(&mut stream, start);

        // DRAW
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Draw as u32);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        // Rebind pipeline state without changing it.
        // BIND_SHADERS (same VS/PS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::BindShaders as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // cs
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_INPUT_LAYOUT (same IL)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_PRIMITIVE_TOPOLOGY (same)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetPrimitiveTopology as u32);
        stream.extend_from_slice(&(AerogpuPrimitiveTopology::TriangleList as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // DRAW
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Draw as u32);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        let stream = finish_stream(stream);

        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let stats = exec.cache_stats();
        assert_eq!(stats.bind_group_layouts.misses, 2);
        assert_eq!(stats.bind_group_layouts.hits, 0);
        assert_eq!(stats.bind_group_layouts.entries, 2);

        let pixels = exec.read_texture_rgba8(RT).await.unwrap();
        assert_eq!(pixels.len(), 4);
        assert_eq!(&pixels[0..4], &[255, 0, 0, 255]);
    });
}

#[test]
fn aerogpu_cmd_updates_blend_constant_between_draws_without_restarting_render_pass() {
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

        let vs = build_vs_pos3_tex2_to_pos_tex_dxbc();

        let vertices = [
            VertexPos3Tex2 {
                pos: [-1.0, -3.0, 0.0],
                uv: [0.5, 0.5],
            },
            VertexPos3Tex2 {
                pos: [-1.0, 1.0, 0.0],
                uv: [0.5, 0.5],
            },
            VertexPos3Tex2 {
                pos: [3.0, 1.0, 0.0],
                uv: [0.5, 0.5],
            },
        ];
        let vb_bytes = bytemuck::bytes_of(&vertices);

        let mut stream = Vec::new();
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_BUFFER (VB)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER.to_le_bytes());
        stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (VB)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes()); // size_bytes
        stream.extend_from_slice(vb_bytes);
        stream.resize(stream.len() + (align4(vb_bytes.len()) - vb_bytes.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (TEX 1x1)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (TEX): green
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&4u64.to_le_bytes()); // size_bytes
        stream.extend_from_slice(&[0u8, 255, 0, 255]);
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (RT 1x1)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&RT.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (VS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // stage = vertex
        stream.extend_from_slice(&(vs.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&vs);
        stream.resize(stream.len() + (align4(vs.len()) - vs.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (PS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
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

        // CREATE_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&(ILAY_POS3_TEX2.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(ILAY_POS3_TEX2);
        stream.resize(
            stream.len() + (align4(ILAY_POS3_TEX2.len()) - ILAY_POS3_TEX2.len()),
            0,
        );
        end_cmd(&mut stream, start);

        // SET_RENDER_TARGETS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetRenderTargets as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // depth_stencil
        stream.extend_from_slice(&RT.to_le_bytes());
        for _ in 0..7 {
            stream.extend_from_slice(&0u32.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        // SET_VIEWPORT 0..1
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetViewport as u32);
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes());
        end_cmd(&mut stream, start);

        // BIND_SHADERS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::BindShaders as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // cs
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_VERTEX_BUFFERS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetVertexBuffers as u32);
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
        stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
        stream.extend_from_slice(&VB.to_le_bytes()); // buffer
        stream.extend_from_slice(&(core::mem::size_of::<VertexPos3Tex2>() as u32).to_le_bytes()); // stride_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_PRIMITIVE_TOPOLOGY
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetPrimitiveTopology as u32);
        stream.extend_from_slice(&(AerogpuPrimitiveTopology::TriangleList as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_TEXTURE (PS t0)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetTexture as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // slot
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_BLEND_STATE (blend constant = 0)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetBlendState as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // enable
        stream.extend_from_slice(&(AerogpuBlendFactor::Constant as u32).to_le_bytes());
        stream.extend_from_slice(&(AerogpuBlendFactor::InvConstant as u32).to_le_bytes());
        stream.extend_from_slice(&(AerogpuBlendOp::Add as u32).to_le_bytes());
        stream.extend_from_slice(&0xFu32.to_le_bytes()); // write mask + padding
        stream.extend_from_slice(&(AerogpuBlendFactor::One as u32).to_le_bytes()); // src_factor_alpha
        stream.extend_from_slice(&(AerogpuBlendFactor::Zero as u32).to_le_bytes()); // dst_factor_alpha
        stream.extend_from_slice(&(AerogpuBlendOp::Add as u32).to_le_bytes()); // blend_op_alpha
        for c in [0.0f32; 4] {
            stream.extend_from_slice(&c.to_bits().to_le_bytes());
        }
        stream.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes()); // sample_mask
        end_cmd(&mut stream, start);

        // CLEAR to red.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Clear as u32);
        stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // depth
        stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
        end_cmd(&mut stream, start);

        // DRAW (blend constant = 0, should keep red)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Draw as u32);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        // SET_BLEND_STATE (blend constant = 1) between draws.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetBlendState as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // enable
        stream.extend_from_slice(&(AerogpuBlendFactor::Constant as u32).to_le_bytes());
        stream.extend_from_slice(&(AerogpuBlendFactor::InvConstant as u32).to_le_bytes());
        stream.extend_from_slice(&(AerogpuBlendOp::Add as u32).to_le_bytes());
        stream.extend_from_slice(&0xFu32.to_le_bytes()); // write mask + padding
        stream.extend_from_slice(&(AerogpuBlendFactor::One as u32).to_le_bytes()); // src_factor_alpha
        stream.extend_from_slice(&(AerogpuBlendFactor::Zero as u32).to_le_bytes()); // dst_factor_alpha
        stream.extend_from_slice(&(AerogpuBlendOp::Add as u32).to_le_bytes()); // blend_op_alpha
        for c in [1.0f32; 4] {
            stream.extend_from_slice(&c.to_bits().to_le_bytes());
        }
        stream.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes()); // sample_mask
        end_cmd(&mut stream, start);

        // DRAW (blend constant = 1, should overwrite with green)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Draw as u32);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        let stream = finish_stream(stream);

        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let stats = exec.cache_stats();
        assert_eq!(stats.bind_group_layouts.misses, 2);
        assert_eq!(stats.bind_group_layouts.hits, 0);
        assert_eq!(stats.bind_group_layouts.entries, 2);

        let pixels = exec.read_texture_rgba8(RT).await.unwrap();
        assert_eq!(pixels.len(), 4);
        assert_eq!(&pixels[0..4], &[0, 255, 0, 255]);
    });
}

#[test]
fn aerogpu_cmd_enables_scissor_between_draws_without_restarting_render_pass() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const VB: u32 = 1;
        const TEX_RED: u32 = 2;
        const TEX_GREEN: u32 = 3;
        const RT: u32 = 4;
        const VS: u32 = 10;
        const PS: u32 = 11;
        const IL: u32 = 20;

        let vs = build_vs_pos3_tex2_to_pos_tex_dxbc();

        let vertices = [
            VertexPos3Tex2 {
                pos: [-1.0, -3.0, 0.0],
                uv: [0.5, 0.5],
            },
            VertexPos3Tex2 {
                pos: [-1.0, 1.0, 0.0],
                uv: [0.5, 0.5],
            },
            VertexPos3Tex2 {
                pos: [3.0, 1.0, 0.0],
                uv: [0.5, 0.5],
            },
        ];
        let vb_bytes = bytemuck::bytes_of(&vertices);

        let mut stream = Vec::new();
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_BUFFER (VB)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER.to_le_bytes());
        stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (VB)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes()); // size_bytes
        stream.extend_from_slice(vb_bytes);
        stream.resize(stream.len() + (align4(vb_bytes.len()) - vb_bytes.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (TEX_RED 1x1)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&TEX_RED.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (TEX_RED)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&TEX_RED.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&4u64.to_le_bytes()); // size_bytes
        stream.extend_from_slice(&[255u8, 0, 0, 255]);
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (TEX_GREEN 1x1)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&TEX_GREEN.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (TEX_GREEN)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&TEX_GREEN.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&4u64.to_le_bytes()); // size_bytes
        stream.extend_from_slice(&[0u8, 255, 0, 255]);
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (RT 2x1)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&RT.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&2u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (VS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // stage = vertex
        stream.extend_from_slice(&(vs.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&vs);
        stream.resize(stream.len() + (align4(vs.len()) - vs.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (PS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
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

        // CREATE_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&(ILAY_POS3_TEX2.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(ILAY_POS3_TEX2);
        stream.resize(
            stream.len() + (align4(ILAY_POS3_TEX2.len()) - ILAY_POS3_TEX2.len()),
            0,
        );
        end_cmd(&mut stream, start);

        // SET_RENDER_TARGETS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetRenderTargets as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // depth_stencil
        stream.extend_from_slice(&RT.to_le_bytes());
        for _ in 0..7 {
            stream.extend_from_slice(&0u32.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        // SET_VIEWPORT 0..2
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetViewport as u32);
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&2f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes());
        end_cmd(&mut stream, start);

        // BIND_SHADERS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::BindShaders as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // cs
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_VERTEX_BUFFERS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetVertexBuffers as u32);
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
        stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
        stream.extend_from_slice(&VB.to_le_bytes()); // buffer
        stream.extend_from_slice(&(core::mem::size_of::<VertexPos3Tex2>() as u32).to_le_bytes()); // stride_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_PRIMITIVE_TOPOLOGY
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetPrimitiveTopology as u32);
        stream.extend_from_slice(&(AerogpuPrimitiveTopology::TriangleList as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_TEXTURE (PS t0 = red)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetTexture as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // slot
        stream.extend_from_slice(&TEX_RED.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CLEAR to black.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Clear as u32);
        stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
        for bits in [0.0f32, 0.0, 0.0, 1.0].map(f32::to_bits) {
            stream.extend_from_slice(&bits.to_le_bytes());
        }
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // depth
        stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
        end_cmd(&mut stream, start);

        // DRAW (fills both pixels red).
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Draw as u32);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        // SET_SCISSOR x=1 width=1 (while scissor disabled).
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetScissor as u32);
        stream.extend_from_slice(&1i32.to_le_bytes()); // x
        stream.extend_from_slice(&0i32.to_le_bytes()); // y
        stream.extend_from_slice(&1i32.to_le_bytes()); // width
        stream.extend_from_slice(&1i32.to_le_bytes()); // height
        end_cmd(&mut stream, start);

        // SET_RASTERIZER_STATE (enable scissor, otherwise no-op).
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetRasterizerState as u32);
        stream.extend_from_slice(&0u32.to_le_bytes()); // fill_mode = solid
        stream.extend_from_slice(&2u32.to_le_bytes()); // cull_mode = back
        stream.extend_from_slice(&0u32.to_le_bytes()); // front_ccw = false
        stream.extend_from_slice(&1u32.to_le_bytes()); // scissor_enable = true
        stream.extend_from_slice(&0i32.to_le_bytes()); // depth_bias
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        end_cmd(&mut stream, start);

        // SET_TEXTURE (PS t0 = green)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetTexture as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // slot
        stream.extend_from_slice(&TEX_GREEN.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // DRAW (scissored, should touch only right pixel).
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Draw as u32);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        let stream = finish_stream(stream);

        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let stats = exec.cache_stats();
        assert_eq!(stats.bind_group_layouts.misses, 2);
        assert_eq!(stats.bind_group_layouts.hits, 0);
        assert_eq!(stats.bind_group_layouts.entries, 2);

        let pixels = exec.read_texture_rgba8(RT).await.unwrap();
        // 2x1 RGBA8 (2 pixels, 4 bytes per pixel).
        assert_eq!(pixels.len(), 2 * 4);
        assert_eq!(&pixels[0..4], &[255, 0, 0, 255]);
        assert_eq!(&pixels[4..8], &[0, 255, 0, 255]);
    });
}

#[test]
fn aerogpu_cmd_rebinds_depth_stencil_state_noops_between_draws_without_restarting_render_pass() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const VB: u32 = 1;
        const TEX_RED: u32 = 2;
        const TEX_GREEN: u32 = 3;
        const RT: u32 = 4;
        const DS: u32 = 5;
        const VS: u32 = 10;
        const PS: u32 = 11;
        const IL: u32 = 20;

        let vs = build_vs_pos3_tex2_to_pos_tex_dxbc();

        let vertices = [
            VertexPos3Tex2 {
                pos: [-1.0, -3.0, 0.0],
                uv: [0.5, 0.5],
            },
            VertexPos3Tex2 {
                pos: [-1.0, 1.0, 0.0],
                uv: [0.5, 0.5],
            },
            VertexPos3Tex2 {
                pos: [3.0, 1.0, 0.0],
                uv: [0.5, 0.5],
            },
        ];
        let vb_bytes = bytemuck::bytes_of(&vertices);

        let mut stream = Vec::new();
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_BUFFER (VB)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER.to_le_bytes());
        stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (VB)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes()); // size_bytes
        stream.extend_from_slice(vb_bytes);
        stream.resize(stream.len() + (align4(vb_bytes.len()) - vb_bytes.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (TEX_RED 1x1)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&TEX_RED.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (TEX_RED)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&TEX_RED.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&4u64.to_le_bytes()); // size_bytes
        stream.extend_from_slice(&[255u8, 0, 0, 255]);
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (TEX_GREEN 1x1)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&TEX_GREEN.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (TEX_GREEN)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&TEX_GREEN.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&4u64.to_le_bytes()); // size_bytes
        stream.extend_from_slice(&[0u8, 255, 0, 255]);
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (RT 2x1)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&RT.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&2u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (DS 2x1)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&DS.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::D24UnormS8Uint as u32).to_le_bytes());
        stream.extend_from_slice(&2u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (VS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // stage = vertex
        stream.extend_from_slice(&(vs.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&vs);
        stream.resize(stream.len() + (align4(vs.len()) - vs.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (PS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
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

        // CREATE_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&(ILAY_POS3_TEX2.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(ILAY_POS3_TEX2);
        stream.resize(
            stream.len() + (align4(ILAY_POS3_TEX2.len()) - ILAY_POS3_TEX2.len()),
            0,
        );
        end_cmd(&mut stream, start);

        // SET_RENDER_TARGETS (RT + DS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetRenderTargets as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
        stream.extend_from_slice(&DS.to_le_bytes());
        stream.extend_from_slice(&RT.to_le_bytes());
        for _ in 0..7 {
            stream.extend_from_slice(&0u32.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        // SET_DEPTH_STENCIL_STATE (depth_enable=1, write_enable=0, func=Always).
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetDepthStencilState as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // depth_enable
        stream.extend_from_slice(&0u32.to_le_bytes()); // depth_write_enable
        stream.extend_from_slice(&(AerogpuCompareFunc::Always as u32).to_le_bytes()); // depth_func
        stream.extend_from_slice(&0u32.to_le_bytes()); // stencil_enable
        stream.push(0xFF); // stencil_read_mask
        stream.push(0xFF); // stencil_write_mask
        stream.extend_from_slice(&[0u8; 2]); // reserved0
        end_cmd(&mut stream, start);

        // CLEAR to black, depth=1.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Clear as u32);
        stream.extend_from_slice(&(AEROGPU_CLEAR_COLOR | AEROGPU_CLEAR_DEPTH).to_le_bytes());
        for bits in [0.0f32, 0.0, 0.0, 1.0].map(f32::to_bits) {
            stream.extend_from_slice(&bits.to_le_bytes());
        }
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // depth
        stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
        end_cmd(&mut stream, start);

        // BIND_SHADERS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::BindShaders as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // cs
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_VERTEX_BUFFERS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetVertexBuffers as u32);
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
        stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
        stream.extend_from_slice(&VB.to_le_bytes()); // buffer
        stream.extend_from_slice(&(core::mem::size_of::<VertexPos3Tex2>() as u32).to_le_bytes()); // stride_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_PRIMITIVE_TOPOLOGY
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetPrimitiveTopology as u32);
        stream.extend_from_slice(&(AerogpuPrimitiveTopology::TriangleList as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_VIEWPORT x=0 width=1
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetViewport as u32);
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes());
        end_cmd(&mut stream, start);

        // SET_TEXTURE (PS t0 = red)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetTexture as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // slot
        stream.extend_from_slice(&TEX_RED.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // DRAW (left pixel red)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Draw as u32);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        // SET_DEPTH_STENCIL_STATE (no-op) between draws.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetDepthStencilState as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // depth_enable
        stream.extend_from_slice(&0u32.to_le_bytes()); // depth_write_enable
        stream.extend_from_slice(&(AerogpuCompareFunc::Always as u32).to_le_bytes()); // depth_func
        stream.extend_from_slice(&0u32.to_le_bytes()); // stencil_enable
        stream.push(0xFF); // stencil_read_mask
        stream.push(0xFF); // stencil_write_mask
        stream.extend_from_slice(&[0u8; 2]); // reserved0
        end_cmd(&mut stream, start);

        // SET_VIEWPORT x=1 width=1
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetViewport as u32);
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes());
        end_cmd(&mut stream, start);

        // SET_TEXTURE (PS t0 = green)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetTexture as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // slot
        stream.extend_from_slice(&TEX_GREEN.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // DRAW (right pixel green)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Draw as u32);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        let stream = finish_stream(stream);

        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let stats = exec.cache_stats();
        assert_eq!(stats.bind_group_layouts.misses, 2);
        assert_eq!(stats.bind_group_layouts.hits, 0);
        assert_eq!(stats.bind_group_layouts.entries, 2);

        let pixels = exec.read_texture_rgba8(RT).await.unwrap();
        // 2x1 RGBA8 (2 pixels, 4 bytes per pixel).
        assert_eq!(pixels.len(), 2 * 4);
        assert_eq!(&pixels[0..4], &[255, 0, 0, 255]);
        assert_eq!(&pixels[4..8], &[0, 255, 0, 255]);
    });
}

#[test]
fn aerogpu_cmd_redundant_set_render_targets_between_draws_does_not_restart_render_pass() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const VB: u32 = 1;
        const TEX_RED: u32 = 2;
        const TEX_GREEN: u32 = 3;
        const RT: u32 = 4;
        const VS: u32 = 10;
        const PS: u32 = 11;
        const IL: u32 = 20;

        let vs = build_vs_pos3_tex2_to_pos_tex_dxbc();

        let vertices = [
            VertexPos3Tex2 {
                pos: [-1.0, -3.0, 0.0],
                uv: [0.5, 0.5],
            },
            VertexPos3Tex2 {
                pos: [-1.0, 1.0, 0.0],
                uv: [0.5, 0.5],
            },
            VertexPos3Tex2 {
                pos: [3.0, 1.0, 0.0],
                uv: [0.5, 0.5],
            },
        ];
        let vb_bytes = bytemuck::bytes_of(&vertices);

        let mut stream = Vec::new();
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_BUFFER (VB)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER.to_le_bytes());
        stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (VB)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes()); // size_bytes
        stream.extend_from_slice(vb_bytes);
        stream.resize(stream.len() + (align4(vb_bytes.len()) - vb_bytes.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (TEX_RED 1x1)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&TEX_RED.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (TEX_RED)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&TEX_RED.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&4u64.to_le_bytes()); // size_bytes
        stream.extend_from_slice(&[255u8, 0, 0, 255]);
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (TEX_GREEN 1x1)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&TEX_GREEN.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (TEX_GREEN)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&TEX_GREEN.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&4u64.to_le_bytes()); // size_bytes
        stream.extend_from_slice(&[0u8, 255, 0, 255]);
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (RT 2x1)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&RT.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&2u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (VS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // stage = vertex
        stream.extend_from_slice(&(vs.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&vs);
        stream.resize(stream.len() + (align4(vs.len()) - vs.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (PS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
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

        // CREATE_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&(ILAY_POS3_TEX2.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(ILAY_POS3_TEX2);
        stream.resize(
            stream.len() + (align4(ILAY_POS3_TEX2.len()) - ILAY_POS3_TEX2.len()),
            0,
        );
        end_cmd(&mut stream, start);

        // SET_RENDER_TARGETS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetRenderTargets as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // depth_stencil
        stream.extend_from_slice(&RT.to_le_bytes());
        for _ in 0..7 {
            stream.extend_from_slice(&0u32.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        // BIND_SHADERS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::BindShaders as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // cs
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_VERTEX_BUFFERS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetVertexBuffers as u32);
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
        stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
        stream.extend_from_slice(&VB.to_le_bytes()); // buffer
        stream.extend_from_slice(&(core::mem::size_of::<VertexPos3Tex2>() as u32).to_le_bytes()); // stride_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_PRIMITIVE_TOPOLOGY
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetPrimitiveTopology as u32);
        stream.extend_from_slice(&(AerogpuPrimitiveTopology::TriangleList as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_TEXTURE (PS t0 = red)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetTexture as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // slot
        stream.extend_from_slice(&TEX_RED.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CLEAR to black.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Clear as u32);
        stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
        for bits in [0.0f32, 0.0, 0.0, 1.0].map(f32::to_bits) {
            stream.extend_from_slice(&bits.to_le_bytes());
        }
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // depth
        stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
        end_cmd(&mut stream, start);

        // VIEWPORT x=0 width=1
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetViewport as u32);
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // x
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // y
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // width
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // height
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // min_depth
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // max_depth
        end_cmd(&mut stream, start);

        // DRAW (left pixel red)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Draw as u32);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        // SET_RENDER_TARGETS again (no-op) between draws.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetRenderTargets as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // depth_stencil
        stream.extend_from_slice(&RT.to_le_bytes());
        for _ in 0..7 {
            stream.extend_from_slice(&0u32.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        // VIEWPORT x=1 width=1
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetViewport as u32);
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // x
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // y
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // width
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // height
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // min_depth
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // max_depth
        end_cmd(&mut stream, start);

        // SET_TEXTURE (PS t0 = green)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetTexture as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // slot
        stream.extend_from_slice(&TEX_GREEN.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // DRAW (right pixel green)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Draw as u32);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        let stream = finish_stream(stream);

        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let stats = exec.cache_stats();
        assert_eq!(stats.bind_group_layouts.misses, 2);
        assert_eq!(stats.bind_group_layouts.hits, 0);
        assert_eq!(stats.bind_group_layouts.entries, 2);

        let pixels = exec.read_texture_rgba8(RT).await.unwrap();
        // 2x1 RGBA8 (2 pixels, 4 bytes per pixel).
        assert_eq!(pixels.len(), 2 * 4);
        assert_eq!(&pixels[0..4], &[255, 0, 0, 255]);
        assert_eq!(&pixels[4..8], &[0, 255, 0, 255]);
    });
}

#[test]
fn aerogpu_cmd_noop_clear_between_draws_does_not_restart_render_pass() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const VB: u32 = 1;
        const TEX_RED: u32 = 2;
        const TEX_GREEN: u32 = 3;
        const RT: u32 = 4;
        const VS: u32 = 10;
        const PS: u32 = 11;
        const IL: u32 = 20;

        let vs = build_vs_pos3_tex2_to_pos_tex_dxbc();

        let vertices = [
            VertexPos3Tex2 {
                pos: [-1.0, -3.0, 0.0],
                uv: [0.5, 0.5],
            },
            VertexPos3Tex2 {
                pos: [-1.0, 1.0, 0.0],
                uv: [0.5, 0.5],
            },
            VertexPos3Tex2 {
                pos: [3.0, 1.0, 0.0],
                uv: [0.5, 0.5],
            },
        ];
        let vb_bytes = bytemuck::bytes_of(&vertices);

        let mut stream = Vec::new();
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_BUFFER (VB)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER.to_le_bytes());
        stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (VB)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes()); // size_bytes
        stream.extend_from_slice(vb_bytes);
        stream.resize(stream.len() + (align4(vb_bytes.len()) - vb_bytes.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (TEX_RED 1x1)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&TEX_RED.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (TEX_RED)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&TEX_RED.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&4u64.to_le_bytes()); // size_bytes
        stream.extend_from_slice(&[255u8, 0, 0, 255]);
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (TEX_GREEN 1x1)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&TEX_GREEN.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (TEX_GREEN)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&TEX_GREEN.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&4u64.to_le_bytes()); // size_bytes
        stream.extend_from_slice(&[0u8, 255, 0, 255]);
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (RT 2x1)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&RT.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&2u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (VS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // stage = vertex
        stream.extend_from_slice(&(vs.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&vs);
        stream.resize(stream.len() + (align4(vs.len()) - vs.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (PS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
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

        // CREATE_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&(ILAY_POS3_TEX2.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(ILAY_POS3_TEX2);
        stream.resize(
            stream.len() + (align4(ILAY_POS3_TEX2.len()) - ILAY_POS3_TEX2.len()),
            0,
        );
        end_cmd(&mut stream, start);

        // SET_RENDER_TARGETS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetRenderTargets as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // depth_stencil
        stream.extend_from_slice(&RT.to_le_bytes());
        for _ in 0..7 {
            stream.extend_from_slice(&0u32.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        // BIND_SHADERS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::BindShaders as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // cs
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_VERTEX_BUFFERS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetVertexBuffers as u32);
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
        stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
        stream.extend_from_slice(&VB.to_le_bytes()); // buffer
        stream.extend_from_slice(&(core::mem::size_of::<VertexPos3Tex2>() as u32).to_le_bytes()); // stride_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_PRIMITIVE_TOPOLOGY
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetPrimitiveTopology as u32);
        stream.extend_from_slice(&(AerogpuPrimitiveTopology::TriangleList as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_TEXTURE (PS t0 = red)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetTexture as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // slot
        stream.extend_from_slice(&TEX_RED.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CLEAR to black.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Clear as u32);
        stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
        for bits in [0.0f32, 0.0, 0.0, 1.0].map(f32::to_bits) {
            stream.extend_from_slice(&bits.to_le_bytes());
        }
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // depth
        stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
        end_cmd(&mut stream, start);

        // VIEWPORT x=0 width=1
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetViewport as u32);
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // x
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // y
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // width
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // height
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // min_depth
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // max_depth
        end_cmd(&mut stream, start);

        // DRAW (left pixel red)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Draw as u32);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        // CLEAR (no-op) between draws.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Clear as u32);
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        for bits in [0.0f32; 4].map(f32::to_bits) {
            stream.extend_from_slice(&bits.to_le_bytes());
        }
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // depth
        stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
        end_cmd(&mut stream, start);

        // VIEWPORT x=1 width=1
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetViewport as u32);
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // x
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // y
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // width
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // height
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // min_depth
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // max_depth
        end_cmd(&mut stream, start);

        // SET_TEXTURE (PS t0 = green)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetTexture as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // slot
        stream.extend_from_slice(&TEX_GREEN.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // DRAW (right pixel green)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Draw as u32);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        let stream = finish_stream(stream);

        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let stats = exec.cache_stats();
        assert_eq!(stats.bind_group_layouts.misses, 2);
        assert_eq!(stats.bind_group_layouts.hits, 0);
        assert_eq!(stats.bind_group_layouts.entries, 2);

        let pixels = exec.read_texture_rgba8(RT).await.unwrap();
        // 2x1 RGBA8 (2 pixels, 4 bytes per pixel).
        assert_eq!(pixels.len(), 2 * 4);
        assert_eq!(&pixels[0..4], &[255, 0, 0, 255]);
        assert_eq!(&pixels[4..8], &[0, 255, 0, 255]);
    });
}

#[test]
fn aerogpu_cmd_resource_dirty_range_between_draws_unused_does_not_restart_render_pass() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const VB: u32 = 1;
        const TEX_RED: u32 = 2;
        const TEX_GREEN: u32 = 3;
        const RT: u32 = 4;
        const UNUSED_BUFFER: u32 = 5;
        const VS: u32 = 10;
        const PS: u32 = 11;
        const IL: u32 = 20;

        let vs = build_vs_pos3_tex2_to_pos_tex_dxbc();

        let vertices = [
            VertexPos3Tex2 {
                pos: [-1.0, -3.0, 0.0],
                uv: [0.5, 0.5],
            },
            VertexPos3Tex2 {
                pos: [-1.0, 1.0, 0.0],
                uv: [0.5, 0.5],
            },
            VertexPos3Tex2 {
                pos: [3.0, 1.0, 0.0],
                uv: [0.5, 0.5],
            },
        ];
        let vb_bytes = bytemuck::bytes_of(&vertices);

        let alloc_id = 1u32;
        let alloc_gpa = 0x100u64;
        let allocs = [AerogpuAllocEntry {
            alloc_id,
            flags: 0,
            gpa: alloc_gpa,
            size_bytes: 0x1000,
            reserved0: 0,
        }];

        let mut stream = Vec::new();
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_BUFFER (VB)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER.to_le_bytes());
        stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (VB)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes()); // size_bytes
        stream.extend_from_slice(vb_bytes);
        stream.resize(stream.len() + (align4(vb_bytes.len()) - vb_bytes.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_BUFFER (unused, allocation-backed)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
        stream.extend_from_slice(&UNUSED_BUFFER.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER.to_le_bytes());
        stream.extend_from_slice(&64u64.to_le_bytes());
        stream.extend_from_slice(&alloc_id.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (TEX_RED 1x1)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&TEX_RED.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (TEX_RED)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&TEX_RED.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&4u64.to_le_bytes()); // size_bytes
        stream.extend_from_slice(&[255u8, 0, 0, 255]); // red
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (TEX_GREEN 1x1)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&TEX_GREEN.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (TEX_GREEN)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&TEX_GREEN.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&4u64.to_le_bytes()); // size_bytes
        stream.extend_from_slice(&[0u8, 255, 0, 255]); // green
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (RT 2x1)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&RT.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&2u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (VS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // stage = vertex
        stream.extend_from_slice(&(vs.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&vs);
        stream.resize(stream.len() + (align4(vs.len()) - vs.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (PS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
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

        // CREATE_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&(ILAY_POS3_TEX2.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(ILAY_POS3_TEX2);
        stream.resize(
            stream.len() + (align4(ILAY_POS3_TEX2.len()) - ILAY_POS3_TEX2.len()),
            0,
        );
        end_cmd(&mut stream, start);

        // SET_RENDER_TARGETS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetRenderTargets as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // depth_stencil
        stream.extend_from_slice(&RT.to_le_bytes());
        for _ in 0..7 {
            stream.extend_from_slice(&0u32.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        // BIND_SHADERS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::BindShaders as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // cs
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_VERTEX_BUFFERS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetVertexBuffers as u32);
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
        stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
        stream.extend_from_slice(&VB.to_le_bytes()); // buffer
        stream.extend_from_slice(&(core::mem::size_of::<VertexPos3Tex2>() as u32).to_le_bytes()); // stride_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_PRIMITIVE_TOPOLOGY
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetPrimitiveTopology as u32);
        stream.extend_from_slice(&(AerogpuPrimitiveTopology::TriangleList as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CLEAR to black.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Clear as u32);
        stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
        for bits in [0.0f32, 0.0, 0.0, 1.0].map(f32::to_bits) {
            stream.extend_from_slice(&bits.to_le_bytes());
        }
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // depth
        stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
        end_cmd(&mut stream, start);

        // SET_TEXTURE (PS t0 = red)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetTexture as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // slot
        stream.extend_from_slice(&TEX_RED.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // VIEWPORT x=0 width=1
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetViewport as u32);
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // x
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // y
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // width
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // height
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // min_depth
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // max_depth
        end_cmd(&mut stream, start);

        // DRAW (left pixel red)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Draw as u32);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        // RESOURCE_DIRTY_RANGE (unused resource) between draws.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::ResourceDirtyRange as u32);
        stream.extend_from_slice(&UNUSED_BUFFER.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&16u64.to_le_bytes()); // size_bytes
        end_cmd(&mut stream, start);

        // VIEWPORT x=1 width=1
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetViewport as u32);
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // x
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // y
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // width
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // height
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // min_depth
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // max_depth
        end_cmd(&mut stream, start);

        // SET_TEXTURE (PS t0 = green)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetTexture as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // slot
        stream.extend_from_slice(&TEX_GREEN.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // DRAW (right pixel green)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Draw as u32);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        let stream = finish_stream(stream);

        let mut guest_mem = VecGuestMemory::new(0x2000);
        exec.execute_cmd_stream(&stream, Some(&allocs), &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let stats = exec.cache_stats();
        assert_eq!(stats.bind_group_layouts.misses, 2);
        assert_eq!(stats.bind_group_layouts.hits, 0);
        assert_eq!(stats.bind_group_layouts.entries, 2);

        let pixels = exec.read_texture_rgba8(RT).await.unwrap();
        // 2x1 RGBA8 (2 pixels, 4 bytes per pixel).
        assert_eq!(pixels.len(), 2 * 4);
        assert_eq!(&pixels[0..4], &[255, 0, 0, 255]);
        assert_eq!(&pixels[4..8], &[0, 255, 0, 255]);
    });
}

#[test]
fn aerogpu_cmd_create_sampler_between_draws_does_not_restart_render_pass() {
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
        const SAMPLER_REPEAT: u32 = 30;
        const VS: u32 = 10;
        const PS: u32 = 11;
        const IL: u32 = 20;

        let vs = build_vs_pos3_tex2_to_pos_tex_dxbc();

        let vertices = [
            VertexPos3Tex2 {
                pos: [-1.0, -3.0, 0.0],
                uv: [1.25, 0.5],
            },
            VertexPos3Tex2 {
                pos: [-1.0, 1.0, 0.0],
                uv: [1.25, 0.5],
            },
            VertexPos3Tex2 {
                pos: [3.0, 1.0, 0.0],
                uv: [1.25, 0.5],
            },
        ];
        let vb_bytes = bytemuck::bytes_of(&vertices);

        let mut stream = Vec::new();
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_BUFFER (VB)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER.to_le_bytes());
        stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (VB)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes()); // size_bytes
        stream.extend_from_slice(vb_bytes);
        stream.resize(stream.len() + (align4(vb_bytes.len()) - vb_bytes.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (TEX 2x1)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&2u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (TEX): [red, green]
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&8u64.to_le_bytes()); // size_bytes
        stream.extend_from_slice(&[255u8, 0, 0, 255, 0, 255, 0, 255]);
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (RT 2x1)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&RT.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&2u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (VS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // stage = vertex
        stream.extend_from_slice(&(vs.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&vs);
        stream.resize(stream.len() + (align4(vs.len()) - vs.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (PS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
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

        // CREATE_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&(ILAY_POS3_TEX2.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(ILAY_POS3_TEX2);
        stream.resize(
            stream.len() + (align4(ILAY_POS3_TEX2.len()) - ILAY_POS3_TEX2.len()),
            0,
        );
        end_cmd(&mut stream, start);

        // SET_RENDER_TARGETS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetRenderTargets as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // depth_stencil
        stream.extend_from_slice(&RT.to_le_bytes());
        for _ in 0..7 {
            stream.extend_from_slice(&0u32.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        // BIND_SHADERS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::BindShaders as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // cs
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_VERTEX_BUFFERS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetVertexBuffers as u32);
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
        stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
        stream.extend_from_slice(&VB.to_le_bytes()); // buffer
        stream.extend_from_slice(&(core::mem::size_of::<VertexPos3Tex2>() as u32).to_le_bytes()); // stride_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_PRIMITIVE_TOPOLOGY
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetPrimitiveTopology as u32);
        stream.extend_from_slice(&(AerogpuPrimitiveTopology::TriangleList as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_TEXTURE (PS t0)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetTexture as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // slot
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CLEAR to black.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Clear as u32);
        stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
        for bits in [0.0f32, 0.0, 0.0, 1.0].map(f32::to_bits) {
            stream.extend_from_slice(&bits.to_le_bytes());
        }
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // depth
        stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
        end_cmd(&mut stream, start);

        // VIEWPORT x=0 width=1
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetViewport as u32);
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // x
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // y
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // width
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // height
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // min_depth
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // max_depth
        end_cmd(&mut stream, start);

        // DRAW (left pixel, default clamp sampler => green)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Draw as u32);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        // CREATE_SAMPLER between draws (repeat address mode).
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateSampler as u32);
        stream.extend_from_slice(&SAMPLER_REPEAT.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // filter = nearest
        stream.extend_from_slice(&1u32.to_le_bytes()); // address_mode_u = repeat
        stream.extend_from_slice(&1u32.to_le_bytes()); // address_mode_v = repeat
        stream.extend_from_slice(&1u32.to_le_bytes()); // address_mode_w = repeat
        end_cmd(&mut stream, start);

        // SET_SAMPLERS (PS s0 = repeat sampler) between draws.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetSamplers as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
        stream.extend_from_slice(&1u32.to_le_bytes()); // sampler_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&SAMPLER_REPEAT.to_le_bytes());
        end_cmd(&mut stream, start);

        // VIEWPORT x=1 width=1
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetViewport as u32);
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // x
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // y
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // width
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // height
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // min_depth
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // max_depth
        end_cmd(&mut stream, start);

        // DRAW (right pixel, repeat sampler => red)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Draw as u32);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        let stream = finish_stream(stream);

        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let stats = exec.cache_stats();
        assert_eq!(stats.bind_group_layouts.misses, 2);
        assert_eq!(stats.bind_group_layouts.hits, 0);
        assert_eq!(stats.bind_group_layouts.entries, 2);

        let pixels = exec.read_texture_rgba8(RT).await.unwrap();
        // 2x1 RGBA8 (2 pixels, 4 bytes per pixel).
        assert_eq!(pixels.len(), 2 * 4);
        assert_eq!(&pixels[0..4], &[0, 255, 0, 255]);
        assert_eq!(&pixels[4..8], &[255, 0, 0, 255]);
    });
}

#[test]
fn aerogpu_cmd_create_buffer_between_draws_does_not_restart_render_pass() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const VB: u32 = 1;
        const TEX_RED: u32 = 2;
        const TEX_GREEN: u32 = 3;
        const RT: u32 = 4;
        const UNUSED: u32 = 5;
        const VS: u32 = 10;
        const PS: u32 = 11;
        const IL: u32 = 20;

        let vs = build_vs_pos3_tex2_to_pos_tex_dxbc();

        let vertices = [
            VertexPos3Tex2 {
                pos: [-1.0, -3.0, 0.0],
                uv: [0.5, 0.5],
            },
            VertexPos3Tex2 {
                pos: [-1.0, 1.0, 0.0],
                uv: [0.5, 0.5],
            },
            VertexPos3Tex2 {
                pos: [3.0, 1.0, 0.0],
                uv: [0.5, 0.5],
            },
        ];
        let vb_bytes = bytemuck::bytes_of(&vertices);

        let mut stream = Vec::new();
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_BUFFER (VB)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER.to_le_bytes());
        stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (VB)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes()); // size_bytes
        stream.extend_from_slice(vb_bytes);
        stream.resize(stream.len() + (align4(vb_bytes.len()) - vb_bytes.len()), 0);
        end_cmd(&mut stream, start);

        for (handle, texel) in [
            (TEX_RED, [255u8, 0, 0, 255]),
            (TEX_GREEN, [0u8, 255, 0, 255]),
        ] {
            // CREATE_TEXTURE2D (1x1)
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
            stream.extend_from_slice(&handle.to_le_bytes());
            stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
            stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
            stream.extend_from_slice(&1u32.to_le_bytes()); // width
            stream.extend_from_slice(&1u32.to_le_bytes()); // height
            stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
            stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
            stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
            stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
            stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
            stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
            end_cmd(&mut stream, start);

            // UPLOAD_RESOURCE (texture)
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
            stream.extend_from_slice(&handle.to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
            stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
            stream.extend_from_slice(&4u64.to_le_bytes()); // size_bytes
            stream.extend_from_slice(&texel);
            end_cmd(&mut stream, start);
        }

        // CREATE_TEXTURE2D (RT 2x1)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&RT.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&2u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (VS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // stage = vertex
        stream.extend_from_slice(&(vs.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&vs);
        stream.resize(stream.len() + (align4(vs.len()) - vs.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (PS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
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

        // CREATE_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&(ILAY_POS3_TEX2.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(ILAY_POS3_TEX2);
        stream.resize(
            stream.len() + (align4(ILAY_POS3_TEX2.len()) - ILAY_POS3_TEX2.len()),
            0,
        );
        end_cmd(&mut stream, start);

        // SET_RENDER_TARGETS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetRenderTargets as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // depth_stencil
        stream.extend_from_slice(&RT.to_le_bytes());
        for _ in 0..7 {
            stream.extend_from_slice(&0u32.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        // BIND_SHADERS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::BindShaders as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // cs
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_VERTEX_BUFFERS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetVertexBuffers as u32);
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
        stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
        stream.extend_from_slice(&VB.to_le_bytes()); // buffer
        stream.extend_from_slice(&(core::mem::size_of::<VertexPos3Tex2>() as u32).to_le_bytes()); // stride_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_PRIMITIVE_TOPOLOGY
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetPrimitiveTopology as u32);
        stream.extend_from_slice(&(AerogpuPrimitiveTopology::TriangleList as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CLEAR to black.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Clear as u32);
        stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
        for bits in [0.0f32, 0.0, 0.0, 1.0].map(f32::to_bits) {
            stream.extend_from_slice(&bits.to_le_bytes());
        }
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // depth
        stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
        end_cmd(&mut stream, start);

        // VIEWPORT x=0 width=1
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetViewport as u32);
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // x
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // y
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // width
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // height
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // min_depth
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // max_depth
        end_cmd(&mut stream, start);

        // SET_TEXTURE (PS t0 = red)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetTexture as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // slot
        stream.extend_from_slice(&TEX_RED.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // DRAW (left pixel red)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Draw as u32);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        // CREATE_BUFFER between draws (unused by pipeline).
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
        stream.extend_from_slice(&UNUSED.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER.to_le_bytes());
        stream.extend_from_slice(&64u64.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // VIEWPORT x=1 width=1
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetViewport as u32);
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // x
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // y
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // width
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // height
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // min_depth
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // max_depth
        end_cmd(&mut stream, start);

        // SET_TEXTURE (PS t0 = green)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetTexture as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // slot
        stream.extend_from_slice(&TEX_GREEN.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // DRAW (right pixel green)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Draw as u32);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        let stream = finish_stream(stream);

        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let stats = exec.cache_stats();
        assert_eq!(stats.bind_group_layouts.misses, 2);
        assert_eq!(stats.bind_group_layouts.hits, 0);
        assert_eq!(stats.bind_group_layouts.entries, 2);

        let pixels = exec.read_texture_rgba8(RT).await.unwrap();
        // 2x1 RGBA8 (2 pixels, 4 bytes per pixel).
        assert_eq!(pixels.len(), 2 * 4);
        assert_eq!(&pixels[0..4], &[255, 0, 0, 255]);
        assert_eq!(&pixels[4..8], &[0, 255, 0, 255]);
    });
}

#[test]
fn aerogpu_cmd_destroy_resource_between_draws_unused_does_not_restart_render_pass() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const VB: u32 = 1;
        const TEX_RED: u32 = 2;
        const TEX_GREEN: u32 = 3;
        const RT: u32 = 4;
        const UNUSED: u32 = 5;
        const VS: u32 = 10;
        const PS: u32 = 11;
        const IL: u32 = 20;

        let vs = build_vs_pos3_tex2_to_pos_tex_dxbc();

        let vertices = [
            VertexPos3Tex2 {
                pos: [-1.0, -3.0, 0.0],
                uv: [0.5, 0.5],
            },
            VertexPos3Tex2 {
                pos: [-1.0, 1.0, 0.0],
                uv: [0.5, 0.5],
            },
            VertexPos3Tex2 {
                pos: [3.0, 1.0, 0.0],
                uv: [0.5, 0.5],
            },
        ];
        let vb_bytes = bytemuck::bytes_of(&vertices);

        let mut stream = Vec::new();
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_BUFFER (VB)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER.to_le_bytes());
        stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (VB)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes()); // size_bytes
        stream.extend_from_slice(vb_bytes);
        stream.resize(stream.len() + (align4(vb_bytes.len()) - vb_bytes.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_BUFFER (unused)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
        stream.extend_from_slice(&UNUSED.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER.to_le_bytes());
        stream.extend_from_slice(&64u64.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        for (handle, texel) in [
            (TEX_RED, [255u8, 0, 0, 255]),
            (TEX_GREEN, [0u8, 255, 0, 255]),
        ] {
            // CREATE_TEXTURE2D (1x1)
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
            stream.extend_from_slice(&handle.to_le_bytes());
            stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
            stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
            stream.extend_from_slice(&1u32.to_le_bytes()); // width
            stream.extend_from_slice(&1u32.to_le_bytes()); // height
            stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
            stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
            stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
            stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
            stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
            stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
            end_cmd(&mut stream, start);

            // UPLOAD_RESOURCE (texture)
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
            stream.extend_from_slice(&handle.to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
            stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
            stream.extend_from_slice(&4u64.to_le_bytes()); // size_bytes
            stream.extend_from_slice(&texel);
            end_cmd(&mut stream, start);
        }

        // CREATE_TEXTURE2D (RT 2x1)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&RT.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&2u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (VS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // stage = vertex
        stream.extend_from_slice(&(vs.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&vs);
        stream.resize(stream.len() + (align4(vs.len()) - vs.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (PS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
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

        // CREATE_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&(ILAY_POS3_TEX2.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(ILAY_POS3_TEX2);
        stream.resize(
            stream.len() + (align4(ILAY_POS3_TEX2.len()) - ILAY_POS3_TEX2.len()),
            0,
        );
        end_cmd(&mut stream, start);

        // SET_RENDER_TARGETS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetRenderTargets as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // depth_stencil
        stream.extend_from_slice(&RT.to_le_bytes());
        for _ in 0..7 {
            stream.extend_from_slice(&0u32.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        // BIND_SHADERS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::BindShaders as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // cs
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_VERTEX_BUFFERS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetVertexBuffers as u32);
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
        stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
        stream.extend_from_slice(&VB.to_le_bytes()); // buffer
        stream.extend_from_slice(&(core::mem::size_of::<VertexPos3Tex2>() as u32).to_le_bytes()); // stride_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_PRIMITIVE_TOPOLOGY
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetPrimitiveTopology as u32);
        stream.extend_from_slice(&(AerogpuPrimitiveTopology::TriangleList as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CLEAR to black.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Clear as u32);
        stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
        for bits in [0.0f32, 0.0, 0.0, 1.0].map(f32::to_bits) {
            stream.extend_from_slice(&bits.to_le_bytes());
        }
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // depth
        stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
        end_cmd(&mut stream, start);

        // VIEWPORT x=0 width=1
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetViewport as u32);
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // x
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // y
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // width
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // height
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // min_depth
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // max_depth
        end_cmd(&mut stream, start);

        // SET_TEXTURE (PS t0 = red)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetTexture as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // slot
        stream.extend_from_slice(&TEX_RED.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // DRAW (left pixel red)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Draw as u32);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        // DESTROY_RESOURCE between draws (unused by the pass).
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::DestroyResource as u32);
        stream.extend_from_slice(&UNUSED.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // VIEWPORT x=1 width=1
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetViewport as u32);
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // x
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // y
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // width
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // height
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // min_depth
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // max_depth
        end_cmd(&mut stream, start);

        // SET_TEXTURE (PS t0 = green)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetTexture as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // slot
        stream.extend_from_slice(&TEX_GREEN.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // DRAW (right pixel green)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Draw as u32);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        let stream = finish_stream(stream);

        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let stats = exec.cache_stats();
        assert_eq!(stats.bind_group_layouts.misses, 2);
        assert_eq!(stats.bind_group_layouts.hits, 0);
        assert_eq!(stats.bind_group_layouts.entries, 2);

        let pixels = exec.read_texture_rgba8(RT).await.unwrap();
        // 2x1 RGBA8 (2 pixels, 4 bytes per pixel).
        assert_eq!(pixels.len(), 2 * 4);
        assert_eq!(&pixels[0..4], &[255, 0, 0, 255]);
        assert_eq!(&pixels[4..8], &[0, 255, 0, 255]);
    });
}

#[test]
fn aerogpu_cmd_noop_transfer_commands_between_draws_do_not_restart_render_pass() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const VB: u32 = 1;
        const TEX_RED: u32 = 2;
        const TEX_GREEN: u32 = 3;
        const RT: u32 = 4;
        const VS: u32 = 10;
        const PS: u32 = 11;
        const IL: u32 = 20;

        let vs = build_vs_pos3_tex2_to_pos_tex_dxbc();

        let vertices = [
            VertexPos3Tex2 {
                pos: [-1.0, -3.0, 0.0],
                uv: [0.5, 0.5],
            },
            VertexPos3Tex2 {
                pos: [-1.0, 1.0, 0.0],
                uv: [0.5, 0.5],
            },
            VertexPos3Tex2 {
                pos: [3.0, 1.0, 0.0],
                uv: [0.5, 0.5],
            },
        ];
        let vb_bytes = bytemuck::bytes_of(&vertices);

        let mut stream = Vec::new();
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_BUFFER (VB)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER.to_le_bytes());
        stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (VB)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes()); // size_bytes
        stream.extend_from_slice(vb_bytes);
        stream.resize(stream.len() + (align4(vb_bytes.len()) - vb_bytes.len()), 0);
        end_cmd(&mut stream, start);

        for (handle, texel) in [
            (TEX_RED, [255u8, 0, 0, 255]),
            (TEX_GREEN, [0u8, 255, 0, 255]),
        ] {
            // CREATE_TEXTURE2D (1x1)
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
            stream.extend_from_slice(&handle.to_le_bytes());
            stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
            stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
            stream.extend_from_slice(&1u32.to_le_bytes()); // width
            stream.extend_from_slice(&1u32.to_le_bytes()); // height
            stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
            stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
            stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
            stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
            stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
            stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
            end_cmd(&mut stream, start);

            // UPLOAD_RESOURCE (texture)
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
            stream.extend_from_slice(&handle.to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
            stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
            stream.extend_from_slice(&4u64.to_le_bytes()); // size_bytes
            stream.extend_from_slice(&texel);
            end_cmd(&mut stream, start);
        }

        // CREATE_TEXTURE2D (RT 2x1)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&RT.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&2u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (VS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // stage = vertex
        stream.extend_from_slice(&(vs.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&vs);
        stream.resize(stream.len() + (align4(vs.len()) - vs.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (PS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
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

        // CREATE_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&(ILAY_POS3_TEX2.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(ILAY_POS3_TEX2);
        stream.resize(
            stream.len() + (align4(ILAY_POS3_TEX2.len()) - ILAY_POS3_TEX2.len()),
            0,
        );
        end_cmd(&mut stream, start);

        // SET_RENDER_TARGETS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetRenderTargets as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // depth_stencil
        stream.extend_from_slice(&RT.to_le_bytes());
        for _ in 0..7 {
            stream.extend_from_slice(&0u32.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        // BIND_SHADERS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::BindShaders as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // cs
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_VERTEX_BUFFERS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetVertexBuffers as u32);
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
        stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
        stream.extend_from_slice(&VB.to_le_bytes()); // buffer
        stream.extend_from_slice(&(core::mem::size_of::<VertexPos3Tex2>() as u32).to_le_bytes()); // stride_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_PRIMITIVE_TOPOLOGY
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetPrimitiveTopology as u32);
        stream.extend_from_slice(&(AerogpuPrimitiveTopology::TriangleList as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CLEAR to black.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Clear as u32);
        stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
        for bits in [0.0f32, 0.0, 0.0, 1.0].map(f32::to_bits) {
            stream.extend_from_slice(&bits.to_le_bytes());
        }
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // depth
        stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
        end_cmd(&mut stream, start);

        // VIEWPORT x=0 width=1
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetViewport as u32);
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // x
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // y
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // width
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // height
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // min_depth
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // max_depth
        end_cmd(&mut stream, start);

        // SET_TEXTURE (PS t0 = red)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetTexture as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // slot
        stream.extend_from_slice(&TEX_RED.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // DRAW (left pixel red)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Draw as u32);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        // No-op UPLOAD_RESOURCE between draws.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&0u32.to_le_bytes()); // resource_handle
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // size_bytes
        end_cmd(&mut stream, start);

        // No-op COPY_BUFFER between draws.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CopyBuffer as u32);
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_buffer
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_buffer
        stream.extend_from_slice(&0u64.to_le_bytes()); // dst_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // src_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // size_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // No-op COPY_TEXTURE2D between draws.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CopyTexture2d as u32);
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_texture
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_texture
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_mip_level
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_array_layer
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_mip_level
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_array_layer
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_x
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_y
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_x
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_y
        stream.extend_from_slice(&0u32.to_le_bytes()); // width
        stream.extend_from_slice(&0u32.to_le_bytes()); // height
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // VIEWPORT x=1 width=1
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetViewport as u32);
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // x
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // y
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // width
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // height
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // min_depth
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // max_depth
        end_cmd(&mut stream, start);

        // SET_TEXTURE (PS t0 = green)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetTexture as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // slot
        stream.extend_from_slice(&TEX_GREEN.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // DRAW (right pixel green)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Draw as u32);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        let stream = finish_stream(stream);

        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let stats = exec.cache_stats();
        assert_eq!(stats.bind_group_layouts.misses, 2);
        assert_eq!(stats.bind_group_layouts.hits, 0);
        assert_eq!(stats.bind_group_layouts.entries, 2);

        let pixels = exec.read_texture_rgba8(RT).await.unwrap();
        // 2x1 RGBA8 (2 pixels, 4 bytes per pixel).
        assert_eq!(pixels.len(), 2 * 4);
        assert_eq!(&pixels[0..4], &[255, 0, 0, 255]);
        assert_eq!(&pixels[4..8], &[0, 255, 0, 255]);
    });
}

#[test]
fn aerogpu_cmd_set_shader_constants_f_between_draws_unused_does_not_restart_render_pass() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const VB: u32 = 1;
        const TEX_RED: u32 = 2;
        const TEX_GREEN: u32 = 3;
        const RT: u32 = 4;
        const VS: u32 = 10;
        const PS: u32 = 11;
        const IL: u32 = 20;

        let vs = build_vs_pos3_tex2_to_pos_tex_dxbc();

        let vertices = [
            VertexPos3Tex2 {
                pos: [-1.0, -3.0, 0.0],
                uv: [0.5, 0.5],
            },
            VertexPos3Tex2 {
                pos: [-1.0, 1.0, 0.0],
                uv: [0.5, 0.5],
            },
            VertexPos3Tex2 {
                pos: [3.0, 1.0, 0.0],
                uv: [0.5, 0.5],
            },
        ];
        let vb_bytes = bytemuck::bytes_of(&vertices);

        let mut stream = Vec::new();
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_BUFFER (VB)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER.to_le_bytes());
        stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (VB)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes()); // size_bytes
        stream.extend_from_slice(vb_bytes);
        stream.resize(stream.len() + (align4(vb_bytes.len()) - vb_bytes.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (TEX_RED 1x1)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&TEX_RED.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (TEX_RED)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&TEX_RED.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&4u64.to_le_bytes()); // size_bytes
        stream.extend_from_slice(&[255u8, 0, 0, 255]);
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (TEX_GREEN 1x1)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&TEX_GREEN.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (TEX_GREEN)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&TEX_GREEN.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&4u64.to_le_bytes()); // size_bytes
        stream.extend_from_slice(&[0u8, 255, 0, 255]);
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (RT 2x1)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&RT.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&2u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (VS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // stage = vertex
        stream.extend_from_slice(&(vs.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&vs);
        stream.resize(stream.len() + (align4(vs.len()) - vs.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (PS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
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

        // CREATE_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&(ILAY_POS3_TEX2.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(ILAY_POS3_TEX2);
        stream.resize(
            stream.len() + (align4(ILAY_POS3_TEX2.len()) - ILAY_POS3_TEX2.len()),
            0,
        );
        end_cmd(&mut stream, start);

        // SET_RENDER_TARGETS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetRenderTargets as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // depth_stencil
        stream.extend_from_slice(&RT.to_le_bytes());
        for _ in 0..7 {
            stream.extend_from_slice(&0u32.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        // BIND_SHADERS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::BindShaders as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // cs
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_VERTEX_BUFFERS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetVertexBuffers as u32);
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
        stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
        stream.extend_from_slice(&VB.to_le_bytes()); // buffer
        stream.extend_from_slice(&(core::mem::size_of::<VertexPos3Tex2>() as u32).to_le_bytes()); // stride_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_PRIMITIVE_TOPOLOGY
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetPrimitiveTopology as u32);
        stream.extend_from_slice(&(AerogpuPrimitiveTopology::TriangleList as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_TEXTURE (PS t0 = red)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetTexture as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // slot
        stream.extend_from_slice(&TEX_RED.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CLEAR to black.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Clear as u32);
        stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
        for bits in [0.0f32, 0.0, 0.0, 1.0].map(f32::to_bits) {
            stream.extend_from_slice(&bits.to_le_bytes());
        }
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // depth
        stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
        end_cmd(&mut stream, start);

        // VIEWPORT x=0 width=1
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetViewport as u32);
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // x
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // y
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // width
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // height
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // min_depth
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // max_depth
        end_cmd(&mut stream, start);

        // DRAW (left pixel red)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Draw as u32);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        // SET_SHADER_CONSTANTS_F (VS) between draws. Shaders in this test do not reference legacy
        // constant registers, so this should not force a render pass restart.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetShaderConstantsF as u32);
        stream.extend_from_slice(&0u32.to_le_bytes()); // stage = vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_register
        stream.extend_from_slice(&1u32.to_le_bytes()); // vec4_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        for f in [1.0f32, 2.0, 3.0, 4.0] {
            stream.extend_from_slice(&f.to_bits().to_le_bytes());
        }
        end_cmd(&mut stream, start);

        // SET_TEXTURE (PS t0 = green)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetTexture as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // slot
        stream.extend_from_slice(&TEX_GREEN.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // VIEWPORT x=1 width=1
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetViewport as u32);
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // x
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // y
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // width
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // height
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // min_depth
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // max_depth
        end_cmd(&mut stream, start);

        // DRAW (right pixel green)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Draw as u32);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        let stream = finish_stream(stream);

        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let stats = exec.cache_stats();
        assert_eq!(stats.bind_group_layouts.misses, 2);
        assert_eq!(stats.bind_group_layouts.hits, 0);
        assert_eq!(stats.bind_group_layouts.entries, 2);

        let pixels = exec.read_texture_rgba8(RT).await.unwrap();
        assert_eq!(pixels.len(), 2 * 4);
        assert_eq!(&pixels[0..4], &[255, 0, 0, 255]);
        assert_eq!(&pixels[4..8], &[0, 255, 0, 255]);
    });
}

#[test]
fn aerogpu_cmd_upload_resource_first_use_texture_between_draws_does_not_restart_render_pass() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const VB: u32 = 1;
        const TEX_RED: u32 = 2;
        const TEX_GREEN: u32 = 3;
        const RT: u32 = 4;
        const VS: u32 = 10;
        const PS: u32 = 11;
        const IL: u32 = 20;

        let vs = build_vs_pos3_tex2_to_pos_tex_dxbc();

        let vertices = [
            VertexPos3Tex2 {
                pos: [-1.0, -3.0, 0.0],
                uv: [0.5, 0.5],
            },
            VertexPos3Tex2 {
                pos: [-1.0, 1.0, 0.0],
                uv: [0.5, 0.5],
            },
            VertexPos3Tex2 {
                pos: [3.0, 1.0, 0.0],
                uv: [0.5, 0.5],
            },
        ];
        let vb_bytes = bytemuck::bytes_of(&vertices);

        let mut stream = Vec::new();
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_BUFFER (VB)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER.to_le_bytes());
        stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (VB)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes()); // size_bytes
        stream.extend_from_slice(vb_bytes);
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (TEX_RED 1x1)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&TEX_RED.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (TEX_RED)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&TEX_RED.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&4u64.to_le_bytes()); // size_bytes
        stream.extend_from_slice(&[255u8, 0, 0, 255]);
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (TEX_GREEN 1x1)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&TEX_GREEN.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (RT 2x1)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&RT.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&2u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (VS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // stage = vertex
        stream.extend_from_slice(&(vs.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&vs);
        stream.resize(stream.len() + (align4(vs.len()) - vs.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (PS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
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

        // CREATE_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&(ILAY_POS3_TEX2.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(ILAY_POS3_TEX2);
        stream.resize(
            stream.len() + (align4(ILAY_POS3_TEX2.len()) - ILAY_POS3_TEX2.len()),
            0,
        );
        end_cmd(&mut stream, start);

        // SET_RENDER_TARGETS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetRenderTargets as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // depth_stencil
        stream.extend_from_slice(&RT.to_le_bytes());
        for _ in 0..7 {
            stream.extend_from_slice(&0u32.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        // VIEWPORT x=0 width=1
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetViewport as u32);
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // x
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // y
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // width
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // height
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // min_depth
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // max_depth
        end_cmd(&mut stream, start);

        // BIND_SHADERS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::BindShaders as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // cs
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_VERTEX_BUFFERS (slot 0)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetVertexBuffers as u32);
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
        stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
        stream.extend_from_slice(&VB.to_le_bytes()); // buffer
        stream.extend_from_slice(&(core::mem::size_of::<VertexPos3Tex2>() as u32).to_le_bytes()); // stride_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_PRIMITIVE_TOPOLOGY
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetPrimitiveTopology as u32);
        stream.extend_from_slice(&(AerogpuPrimitiveTopology::TriangleList as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_TEXTURE (PS t0 = TEX_RED)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetTexture as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // slot
        stream.extend_from_slice(&TEX_RED.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CLEAR to opaque black.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Clear as u32);
        stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
        for bits in [0.0f32, 0.0, 0.0, 1.0].map(f32::to_bits) {
            stream.extend_from_slice(&bits.to_le_bytes());
        }
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // depth
        stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
        end_cmd(&mut stream, start);

        // DRAW (left pixel red)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Draw as u32);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (TEX_GREEN) between draws.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&TEX_GREEN.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&4u64.to_le_bytes()); // size_bytes
        stream.extend_from_slice(&[0u8, 255, 0, 255]);
        end_cmd(&mut stream, start);

        // SET_TEXTURE (PS t0 = TEX_GREEN)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetTexture as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // slot
        stream.extend_from_slice(&TEX_GREEN.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // VIEWPORT x=1 width=1
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetViewport as u32);
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // x
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // y
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // width
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // height
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // min_depth
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // max_depth
        end_cmd(&mut stream, start);

        // DRAW (right pixel green)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Draw as u32);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        let stream = finish_stream(stream);

        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let stats = exec.cache_stats();
        assert_eq!(stats.bind_group_layouts.misses, 2);
        assert_eq!(stats.bind_group_layouts.hits, 0);
        assert_eq!(stats.bind_group_layouts.entries, 2);

        let pixels = exec.read_texture_rgba8(RT).await.unwrap();
        assert_eq!(pixels.len(), 2 * 4);
        assert_eq!(&pixels[0..4], &[255, 0, 0, 255]);
        assert_eq!(&pixels[4..8], &[0, 255, 0, 255]);
    });
}

#[test]
fn aerogpu_cmd_upload_resource_first_use_vertex_buffer_between_draws_does_not_restart_render_pass()
{
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const VB_A: u32 = 1;
        const VB_B: u32 = 2;
        const TEX: u32 = 3;
        const RT: u32 = 4;
        const VS: u32 = 10;
        const PS: u32 = 11;
        const IL: u32 = 20;

        let vs = build_vs_pos3_tex2_to_pos_tex_dxbc();

        let vertices_a = [
            VertexPos3Tex2 {
                pos: [-1.0, -3.0, 0.0],
                uv: [0.25, 0.5],
            },
            VertexPos3Tex2 {
                pos: [-1.0, 1.0, 0.0],
                uv: [0.25, 0.5],
            },
            VertexPos3Tex2 {
                pos: [3.0, 1.0, 0.0],
                uv: [0.25, 0.5],
            },
        ];
        let vertices_b = [
            VertexPos3Tex2 {
                pos: [-1.0, -3.0, 0.0],
                uv: [0.75, 0.5],
            },
            VertexPos3Tex2 {
                pos: [-1.0, 1.0, 0.0],
                uv: [0.75, 0.5],
            },
            VertexPos3Tex2 {
                pos: [3.0, 1.0, 0.0],
                uv: [0.75, 0.5],
            },
        ];
        let vb_a_bytes = bytemuck::bytes_of(&vertices_a);
        let vb_b_bytes = bytemuck::bytes_of(&vertices_b);

        let mut stream = Vec::new();
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        for (vb, data) in [(VB_A, vb_a_bytes), (VB_B, vb_b_bytes)] {
            // CREATE_BUFFER (VB)
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
            stream.extend_from_slice(&vb.to_le_bytes());
            stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER.to_le_bytes());
            stream.extend_from_slice(&(data.len() as u64).to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
            stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
            stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
            end_cmd(&mut stream, start);
        }

        // UPLOAD_RESOURCE (VB_A)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&VB_A.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&(vb_a_bytes.len() as u64).to_le_bytes()); // size_bytes
        stream.extend_from_slice(vb_a_bytes);
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (TEX 2x1)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&2u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (TEX): [red, green]
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&8u64.to_le_bytes()); // size_bytes
        stream.extend_from_slice(&[255u8, 0, 0, 255, 0, 255, 0, 255]);
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (RT 2x1)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&RT.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&2u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (VS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // stage = vertex
        stream.extend_from_slice(&(vs.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&vs);
        stream.resize(stream.len() + (align4(vs.len()) - vs.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (PS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
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

        // CREATE_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&(ILAY_POS3_TEX2.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(ILAY_POS3_TEX2);
        stream.resize(
            stream.len() + (align4(ILAY_POS3_TEX2.len()) - ILAY_POS3_TEX2.len()),
            0,
        );
        end_cmd(&mut stream, start);

        // SET_RENDER_TARGETS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetRenderTargets as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // depth_stencil
        stream.extend_from_slice(&RT.to_le_bytes());
        for _ in 0..7 {
            stream.extend_from_slice(&0u32.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        // VIEWPORT x=0 width=1
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetViewport as u32);
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // x
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // y
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // width
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // height
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // min_depth
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // max_depth
        end_cmd(&mut stream, start);

        // BIND_SHADERS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::BindShaders as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // cs
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_VERTEX_BUFFERS (slot 0 = VB_A)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetVertexBuffers as u32);
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
        stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
        stream.extend_from_slice(&VB_A.to_le_bytes()); // buffer
        stream.extend_from_slice(&(core::mem::size_of::<VertexPos3Tex2>() as u32).to_le_bytes()); // stride_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_PRIMITIVE_TOPOLOGY
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetPrimitiveTopology as u32);
        stream.extend_from_slice(&(AerogpuPrimitiveTopology::TriangleList as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_TEXTURE (PS t0)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetTexture as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // slot
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CLEAR to opaque black.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Clear as u32);
        stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
        for bits in [0.0f32, 0.0, 0.0, 1.0].map(f32::to_bits) {
            stream.extend_from_slice(&bits.to_le_bytes());
        }
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // depth
        stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
        end_cmd(&mut stream, start);

        // DRAW (left pixel red)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Draw as u32);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (VB_B) between draws.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&VB_B.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&(vb_b_bytes.len() as u64).to_le_bytes()); // size_bytes
        stream.extend_from_slice(vb_b_bytes);
        end_cmd(&mut stream, start);

        // SET_VERTEX_BUFFERS (slot 0 = VB_B) between draws.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetVertexBuffers as u32);
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
        stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
        stream.extend_from_slice(&VB_B.to_le_bytes()); // buffer
        stream.extend_from_slice(&(core::mem::size_of::<VertexPos3Tex2>() as u32).to_le_bytes()); // stride_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // VIEWPORT x=1 width=1
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetViewport as u32);
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // x
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // y
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // width
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // height
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // min_depth
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // max_depth
        end_cmd(&mut stream, start);

        // DRAW (right pixel green)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Draw as u32);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        let stream = finish_stream(stream);

        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let stats = exec.cache_stats();
        assert_eq!(stats.bind_group_layouts.misses, 2);
        assert_eq!(stats.bind_group_layouts.hits, 0);
        assert_eq!(stats.bind_group_layouts.entries, 2);

        let pixels = exec.read_texture_rgba8(RT).await.unwrap();
        assert_eq!(pixels.len(), 2 * 4);
        assert_eq!(&pixels[0..4], &[255, 0, 0, 255]);
        assert_eq!(&pixels[4..8], &[0, 255, 0, 255]);
    });
}

#[test]
fn aerogpu_cmd_set_shader_constants_f_between_draws_preserves_order_with_prior_encoder_copy() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const VB: u32 = 1;
        const RT: u32 = 2;
        const VS: u32 = 10;
        const PS: u32 = 11;
        const IL: u32 = 20;

        let ps_red = build_ps_solid_red_dxbc();

        let ilay = build_ilay_pos3();

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
        let vb_bytes = bytemuck::bytes_of(&vertices);

        let translate_x_10: [f32; 16] = [
            1.0, 0.0, 0.0, 10.0, //
            0.0, 1.0, 0.0, 0.0, //
            0.0, 0.0, 1.0, 0.0, //
            0.0, 0.0, 0.0, 1.0, //
        ];
        let identity: [f32; 16] = [
            1.0, 0.0, 0.0, 0.0, //
            0.0, 1.0, 0.0, 0.0, //
            0.0, 0.0, 1.0, 0.0, //
            0.0, 0.0, 0.0, 1.0, //
        ];

        let mut stream = Vec::new();
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_BUFFER (VB)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER.to_le_bytes());
        stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (VB)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes()); // size_bytes
        stream.extend_from_slice(vb_bytes);
        stream.resize(stream.len() + (align4(vb_bytes.len()) - vb_bytes.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (RT 4x4)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&RT.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&4u32.to_le_bytes()); // width
        stream.extend_from_slice(&4u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (VS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // stage = vertex
        stream.extend_from_slice(&(DXBC_VS_MATRIX.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(DXBC_VS_MATRIX);
        stream.resize(
            stream.len() + (align4(DXBC_VS_MATRIX.len()) - DXBC_VS_MATRIX.len()),
            0,
        );
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (PS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // stage = pixel
        stream.extend_from_slice(&(ps_red.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&ps_red);
        stream.resize(stream.len() + (align4(ps_red.len()) - ps_red.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&(ilay.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&ilay);
        stream.resize(stream.len() + (align4(ilay.len()) - ilay.len()), 0);
        end_cmd(&mut stream, start);

        // SET_RENDER_TARGETS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetRenderTargets as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // depth_stencil
        stream.extend_from_slice(&RT.to_le_bytes());
        for _ in 0..7 {
            stream.extend_from_slice(&0u32.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        // SET_VIEWPORT 0..4
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetViewport as u32);
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&4f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&4f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes());
        end_cmd(&mut stream, start);

        // BIND_SHADERS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::BindShaders as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // cs
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_VERTEX_BUFFERS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetVertexBuffers as u32);
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
        stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
        stream.extend_from_slice(&VB.to_le_bytes()); // buffer
        stream.extend_from_slice(&(core::mem::size_of::<VertexPos3>() as u32).to_le_bytes()); // stride_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_PRIMITIVE_TOPOLOGY
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetPrimitiveTopology as u32);
        stream.extend_from_slice(&(AerogpuPrimitiveTopology::TriangleList as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_BLEND_STATE (sample_mask = 0, so DRAW is skipped)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetBlendState as u32);
        stream.extend_from_slice(&0u32.to_le_bytes()); // enable
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_factor
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_factor
        stream.extend_from_slice(&0u32.to_le_bytes()); // blend_op
        stream.extend_from_slice(&0xFu32.to_le_bytes()); // write mask + padding
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_factor_alpha
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_factor_alpha
        stream.extend_from_slice(&0u32.to_le_bytes()); // blend_op_alpha
        for c in [0.0f32; 4] {
            stream.extend_from_slice(&c.to_bits().to_le_bytes());
        }
        stream.extend_from_slice(&0u32.to_le_bytes()); // sample_mask
        end_cmd(&mut stream, start);

        // CLEAR to black.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Clear as u32);
        stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
        for bits in [0.0f32, 0.0, 0.0, 1.0].map(f32::to_bits) {
            stream.extend_from_slice(&bits.to_le_bytes());
        }
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // depth
        stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
        end_cmd(&mut stream, start);

        // SET_SHADER_CONSTANTS_F (VS, matrix translate +10 x).
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetShaderConstantsF as u32);
        stream.extend_from_slice(&0u32.to_le_bytes()); // stage = vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_register
        stream.extend_from_slice(&4u32.to_le_bytes()); // vec4_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        for f in translate_x_10 {
            stream.extend_from_slice(&f.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        // DRAW (skipped because sample_mask == 0)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Draw as u32);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        // SET_SHADER_CONSTANTS_F (VS, identity) between draws. This must not be reordered ahead of
        // the earlier constants upload.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetShaderConstantsF as u32);
        stream.extend_from_slice(&0u32.to_le_bytes()); // stage = vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_register
        stream.extend_from_slice(&4u32.to_le_bytes()); // vec4_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        for f in identity {
            stream.extend_from_slice(&f.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        // SET_BLEND_STATE (sample_mask = all ones)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetBlendState as u32);
        stream.extend_from_slice(&0u32.to_le_bytes()); // enable
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_factor
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_factor
        stream.extend_from_slice(&0u32.to_le_bytes()); // blend_op
        stream.extend_from_slice(&0xFu32.to_le_bytes()); // write mask + padding
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_factor_alpha
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_factor_alpha
        stream.extend_from_slice(&0u32.to_le_bytes()); // blend_op_alpha
        for c in [0.0f32; 4] {
            stream.extend_from_slice(&c.to_bits().to_le_bytes());
        }
        stream.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes()); // sample_mask
        end_cmd(&mut stream, start);

        // DRAW (should be red if identity matrix is applied last)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Draw as u32);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        let stream = finish_stream(stream);

        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let pixels = exec.read_texture_rgba8(RT).await.unwrap();
        assert_eq!(&pixels[0..4], &[255, 0, 0, 255]);
    });
}

#[test]
fn aerogpu_cmd_set_render_state_between_draws_does_not_restart_render_pass() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const VB: u32 = 1;
        const TEX_RED: u32 = 2;
        const TEX_GREEN: u32 = 3;
        const RT: u32 = 4;
        const VS: u32 = 10;
        const PS: u32 = 11;
        const IL: u32 = 20;

        let vs = build_vs_pos3_tex2_to_pos_tex_dxbc();

        let vertices = [
            VertexPos3Tex2 {
                pos: [-1.0, -3.0, 0.0],
                uv: [0.5, 0.5],
            },
            VertexPos3Tex2 {
                pos: [-1.0, 1.0, 0.0],
                uv: [0.5, 0.5],
            },
            VertexPos3Tex2 {
                pos: [3.0, 1.0, 0.0],
                uv: [0.5, 0.5],
            },
        ];
        let vb_bytes = bytemuck::bytes_of(&vertices);

        let mut stream = Vec::new();
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_BUFFER (VB)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER.to_le_bytes());
        stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (VB)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes()); // size_bytes
        stream.extend_from_slice(vb_bytes);
        stream.resize(stream.len() + (align4(vb_bytes.len()) - vb_bytes.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (TEX_RED 1x1)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&TEX_RED.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (TEX_RED)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&TEX_RED.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&4u64.to_le_bytes()); // size_bytes
        stream.extend_from_slice(&[255u8, 0, 0, 255]);
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (TEX_GREEN 1x1)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&TEX_GREEN.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (TEX_GREEN)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&TEX_GREEN.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&4u64.to_le_bytes()); // size_bytes
        stream.extend_from_slice(&[0u8, 255, 0, 255]);
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (RT 2x1)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&RT.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&2u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (VS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // stage = vertex
        stream.extend_from_slice(&(vs.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&vs);
        stream.resize(stream.len() + (align4(vs.len()) - vs.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (PS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
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

        // CREATE_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&(ILAY_POS3_TEX2.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(ILAY_POS3_TEX2);
        stream.resize(
            stream.len() + (align4(ILAY_POS3_TEX2.len()) - ILAY_POS3_TEX2.len()),
            0,
        );
        end_cmd(&mut stream, start);

        // SET_RENDER_TARGETS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetRenderTargets as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // depth_stencil
        stream.extend_from_slice(&RT.to_le_bytes());
        for _ in 0..7 {
            stream.extend_from_slice(&0u32.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        // BIND_SHADERS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::BindShaders as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // cs
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_VERTEX_BUFFERS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetVertexBuffers as u32);
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
        stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
        stream.extend_from_slice(&VB.to_le_bytes()); // buffer
        stream.extend_from_slice(&(core::mem::size_of::<VertexPos3Tex2>() as u32).to_le_bytes()); // stride_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_PRIMITIVE_TOPOLOGY
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetPrimitiveTopology as u32);
        stream.extend_from_slice(&(AerogpuPrimitiveTopology::TriangleList as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_TEXTURE (PS t0 = red)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetTexture as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // slot
        stream.extend_from_slice(&TEX_RED.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CLEAR to black.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Clear as u32);
        stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
        for bits in [0.0f32, 0.0, 0.0, 1.0].map(f32::to_bits) {
            stream.extend_from_slice(&bits.to_le_bytes());
        }
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // depth
        stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
        end_cmd(&mut stream, start);

        // VIEWPORT x=0 width=1
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetViewport as u32);
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // x
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // y
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // width
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // height
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // min_depth
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // max_depth
        end_cmd(&mut stream, start);

        // DRAW (left pixel red)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Draw as u32);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        // SET_RENDER_STATE between draws. The D3D11 executor ignores this opcode, so it should not
        // force a render pass restart.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetRenderState as u32);
        stream.extend_from_slice(&0xDEAD_BEEFu32.to_le_bytes()); // state
        stream.extend_from_slice(&1u32.to_le_bytes()); // value
        end_cmd(&mut stream, start);

        // SET_TEXTURE (PS t0 = green)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetTexture as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // slot
        stream.extend_from_slice(&TEX_GREEN.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // VIEWPORT x=1 width=1
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetViewport as u32);
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // x
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // y
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // width
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // height
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // min_depth
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // max_depth
        end_cmd(&mut stream, start);

        // DRAW (right pixel green)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Draw as u32);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        let stream = finish_stream(stream);

        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let stats = exec.cache_stats();
        assert_eq!(stats.bind_group_layouts.misses, 2);
        assert_eq!(stats.bind_group_layouts.hits, 0);
        assert_eq!(stats.bind_group_layouts.entries, 2);

        let pixels = exec.read_texture_rgba8(RT).await.unwrap();
        assert_eq!(pixels.len(), 2 * 4);
        assert_eq!(&pixels[0..4], &[255, 0, 0, 255]);
        assert_eq!(&pixels[4..8], &[0, 255, 0, 255]);
    });
}
