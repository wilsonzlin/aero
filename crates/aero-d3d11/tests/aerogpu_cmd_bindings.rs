mod common;

use aero_d3d11::input_layout::fnv1a_32;
use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode,
    AerogpuCmdStreamHeader as ProtocolCmdStreamHeader, AerogpuPrimitiveTopology,
    AEROGPU_CLEAR_COLOR, AEROGPU_CMD_STREAM_MAGIC, AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER,
    AEROGPU_RESOURCE_USAGE_RENDER_TARGET, AEROGPU_RESOURCE_USAGE_TEXTURE,
    AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::{AerogpuFormat, AEROGPU_ABI_VERSION_U32};

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

fn end_cmd(stream: &mut Vec<u8>, start: usize) {
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

        let guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &guest_mem)
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

        let guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &guest_mem)
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

        let guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &guest_mem)
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
        assert_eq!(pixels.len(), 2 * 1 * 4);
        assert_eq!(&pixels[0..4], &[255, 0, 0, 255]);
        assert_eq!(&pixels[4..8], &[0, 255, 0, 255]);
    });
}
