use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_d3d11::FourCC;
use aero_gpu::{GuestMemory, VecGuestMemory};
use aero_protocol::aerogpu::aerogpu_cmd::AEROGPU_CMD_STREAM_MAGIC;
use aero_protocol::aerogpu::aerogpu_pci::AEROGPU_ABI_VERSION_U32;
use aero_protocol::aerogpu::aerogpu_ring::AerogpuAllocEntry;

const FOURCC_SHEX: FourCC = FourCC(*b"SHEX");

const OPCODE_CREATE_BUFFER: u32 = 0x0100;
const OPCODE_CREATE_TEXTURE2D: u32 = 0x0101;
const OPCODE_RESOURCE_DIRTY_RANGE: u32 = 0x0103;

const OPCODE_CREATE_SHADER_DXBC: u32 = 0x0200;
const OPCODE_BIND_SHADERS: u32 = 0x0202;
const OPCODE_CREATE_INPUT_LAYOUT: u32 = 0x0204;
const OPCODE_SET_INPUT_LAYOUT: u32 = 0x0206;

const OPCODE_SET_RENDER_TARGETS: u32 = 0x0400;
const OPCODE_SET_VIEWPORT: u32 = 0x0401;
const OPCODE_SET_SCISSOR: u32 = 0x0402;

const OPCODE_SET_VERTEX_BUFFERS: u32 = 0x0500;
const OPCODE_SET_PRIMITIVE_TOPOLOGY: u32 = 0x0502;

const OPCODE_CLEAR: u32 = 0x0600;
const OPCODE_DRAW: u32 = 0x0601;
const OPCODE_PRESENT: u32 = 0x0700;

const AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER: u32 = 1u32 << 0;
const AEROGPU_RESOURCE_USAGE_RENDER_TARGET: u32 = 1u32 << 4;

const AEROGPU_FORMAT_R8G8B8A8_UNORM: u32 = 3;

const AEROGPU_CLEAR_COLOR: u32 = 1u32 << 0;

const AEROGPU_TOPOLOGY_TRIANGLELIST: u32 = 4;

// ILAY input layout blob format.
const AEROGPU_INPUT_LAYOUT_BLOB_MAGIC: u32 = 0x5941_4C49; // "ILAY"
const AEROGPU_INPUT_LAYOUT_BLOB_VERSION: u32 = 1;

// DXGI_FORMAT_R32G32B32A32_FLOAT
const DXGI_FORMAT_R32G32B32A32_FLOAT: u32 = 2;

fn make_dxbc_with_single_chunk(fourcc: FourCC, chunk_data: &[u8]) -> Vec<u8> {
    let header_size = 4 + 16 + 4 + 4 + 4 + 4; // magic + checksum + one + total + count + offset[0]
    let chunk_offset = header_size;
    let total_size = header_size + 8 + chunk_data.len();

    let mut bytes = Vec::with_capacity(total_size);
    bytes.extend_from_slice(b"DXBC");
    bytes.extend_from_slice(&[0u8; 16]); // checksum
    bytes.extend_from_slice(&1u32.to_le_bytes()); // "one"
    bytes.extend_from_slice(&(total_size as u32).to_le_bytes());
    bytes.extend_from_slice(&1u32.to_le_bytes()); // chunk count
    bytes.extend_from_slice(&(chunk_offset as u32).to_le_bytes());

    bytes.extend_from_slice(&fourcc.0);
    bytes.extend_from_slice(&(chunk_data.len() as u32).to_le_bytes());
    bytes.extend_from_slice(chunk_data);

    bytes
}

fn make_sm5_program_tokens(stage_type: u16, body_tokens: &[u32]) -> Vec<u32> {
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
    operand_type << 4
}

fn make_passthrough_vs() -> Vec<u8> {
    // Translator bootstrap opcodes.
    const OPCODE_MOV: u32 = 0x01;
    const OPCODE_RET: u32 = 0x3e;
    const OPERAND_INPUT: u32 = 1;
    const OPERAND_OUTPUT: u32 = 2;

    // mov o0, v0 (position)
    let mov0 = [
        opcode_token(OPCODE_MOV, 5),
        operand_token(OPERAND_OUTPUT),
        0,
        operand_token(OPERAND_INPUT),
        0,
    ];
    // mov o1, v1 (color)
    let mov1 = [
        opcode_token(OPCODE_MOV, 5),
        operand_token(OPERAND_OUTPUT),
        1,
        operand_token(OPERAND_INPUT),
        1,
    ];
    let ret = [opcode_token(OPCODE_RET, 1)];

    let tokens = make_sm5_program_tokens(
        1,
        &[mov0.as_slice(), mov1.as_slice(), ret.as_slice()].concat(),
    );
    make_dxbc_with_single_chunk(FOURCC_SHEX, &tokens_to_bytes(&tokens))
}

fn make_passthrough_ps() -> Vec<u8> {
    const OPCODE_MOV: u32 = 0x01;
    const OPCODE_RET: u32 = 0x3e;
    const OPERAND_INPUT: u32 = 1;
    const OPERAND_OUTPUT: u32 = 2;

    // mov o0, v1 (return interpolated color)
    let mov = [
        opcode_token(OPCODE_MOV, 5),
        operand_token(OPERAND_OUTPUT),
        0,
        operand_token(OPERAND_INPUT),
        1,
    ];
    let ret = [opcode_token(OPCODE_RET, 1)];

    let tokens = make_sm5_program_tokens(0, &[mov.as_slice(), ret.as_slice()].concat());
    make_dxbc_with_single_chunk(FOURCC_SHEX, &tokens_to_bytes(&tokens))
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

fn end_cmd(stream: &mut Vec<u8>, start: usize) {
    let size = (stream.len() - start) as u32;
    stream[start + 4..start + 8].copy_from_slice(&size.to_le_bytes());
    assert_eq!(size % 4, 0, "command not 4-byte aligned");
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    pos: [f32; 4],
    color: [f32; 4],
}

#[test]
fn aerogpu_cmd_renders_fullscreen_triangle() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                eprintln!("wgpu unavailable ({e:#}); skipping aerogpu_cmd smoke test");
                return;
            }
        };

        const VB: u32 = 1;
        const RT: u32 = 2;
        const VS: u32 = 10;
        const PS: u32 = 11;
        const IL: u32 = 20;

        let vertices = [
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
        let vb_bytes = bytemuck::bytes_of(&vertices);
        let vb_size = vb_bytes.len() as u64;

        let mut guest_mem = VecGuestMemory::new(0x1000);
        let alloc_id = 1u32;
        let alloc_gpa = 0x100u64;
        guest_mem.write(alloc_gpa, vb_bytes).unwrap();

        let allocs = [AerogpuAllocEntry {
            alloc_id,
            flags: 0,
            gpa: alloc_gpa,
            size_bytes: vb_size,
            reserved0: 0,
        }];

        // Build ILAY blob with two Float32x4 inputs at offsets 0 and 16 in slot 0.
        let mut ilay = Vec::new();
        ilay.extend_from_slice(&AEROGPU_INPUT_LAYOUT_BLOB_MAGIC.to_le_bytes());
        ilay.extend_from_slice(&AEROGPU_INPUT_LAYOUT_BLOB_VERSION.to_le_bytes());
        ilay.extend_from_slice(&2u32.to_le_bytes()); // element_count
        ilay.extend_from_slice(&0u32.to_le_bytes()); // reserved0

        for (offset, semantic_index) in [(0u32, 0u32), (16u32, 0u32)] {
            ilay.extend_from_slice(&0u32.to_le_bytes()); // semantic_name_hash
            ilay.extend_from_slice(&semantic_index.to_le_bytes());
            ilay.extend_from_slice(&DXGI_FORMAT_R32G32B32A32_FLOAT.to_le_bytes());
            ilay.extend_from_slice(&0u32.to_le_bytes()); // input_slot
            ilay.extend_from_slice(&offset.to_le_bytes()); // aligned_byte_offset
            ilay.extend_from_slice(&0u32.to_le_bytes()); // input_slot_class (per-vertex)
            ilay.extend_from_slice(&0u32.to_le_bytes()); // instance_data_step_rate
        }

        let dxbc_vs = make_passthrough_vs();
        let dxbc_ps = make_passthrough_ps();

        let mut stream = Vec::new();
        // Stream header (24 bytes)
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_BUFFER (VB)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_BUFFER);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&(AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER).to_le_bytes());
        stream.extend_from_slice(&vb_size.to_le_bytes());
        stream.extend_from_slice(&alloc_id.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // RESOURCE_DIRTY_RANGE (full VB)
        let start = begin_cmd(&mut stream, OPCODE_RESOURCE_DIRTY_RANGE);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&vb_size.to_le_bytes()); // size_bytes
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (RT)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_TEXTURE2D);
        stream.extend_from_slice(&RT.to_le_bytes());
        stream.extend_from_slice(&(AEROGPU_RESOURCE_USAGE_RENDER_TARGET).to_le_bytes());
        stream.extend_from_slice(&AEROGPU_FORMAT_R8G8B8A8_UNORM.to_le_bytes());
        stream.extend_from_slice(&4u32.to_le_bytes()); // width
        stream.extend_from_slice(&4u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id (host alloc)
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

        // SCISSOR 0..4
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
        let start = begin_cmd(&mut stream, OPCODE_CREATE_SHADER_DXBC);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // stage = vertex
        stream.extend_from_slice(&(dxbc_vs.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&dxbc_vs);
        stream.resize(stream.len() + (align4(dxbc_vs.len()) - dxbc_vs.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (PS)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_SHADER_DXBC);
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // stage = pixel
        stream.extend_from_slice(&(dxbc_ps.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&dxbc_ps);
        stream.resize(stream.len() + (align4(dxbc_ps.len()) - dxbc_ps.len()), 0);
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

        // Patch stream size in header.
        let total_size = stream.len() as u32;
        stream[8..12].copy_from_slice(&total_size.to_le_bytes());

        exec.execute_cmd_stream(&stream, Some(&allocs), &guest_mem)
            .unwrap();
        exec.poll_wait();

        let pixels = exec.read_texture_rgba8(RT).await.unwrap();
        assert_eq!(pixels.len(), 4 * 4 * 4);
        for px in pixels.chunks_exact(4) {
            assert_eq!(px, &[255, 0, 0, 255]);
        }
    });
}
