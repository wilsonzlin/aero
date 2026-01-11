mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::AEROGPU_CMD_STREAM_MAGIC;
use aero_protocol::aerogpu::aerogpu_pci::AEROGPU_ABI_VERSION_U32;
use aero_protocol::aerogpu::aerogpu_ring::AerogpuAllocEntry;
use bytemuck::{Pod, Zeroable};

const OPCODE_CREATE_BUFFER: u32 = 0x0100;
const OPCODE_CREATE_TEXTURE2D: u32 = 0x0101;
const OPCODE_RESOURCE_DIRTY_RANGE: u32 = 0x0103;

const OPCODE_CREATE_SHADER_DXBC: u32 = 0x0200;
const OPCODE_BIND_SHADERS: u32 = 0x0202;

const OPCODE_CREATE_INPUT_LAYOUT: u32 = 0x0204;
const OPCODE_SET_INPUT_LAYOUT: u32 = 0x0206;

const OPCODE_SET_DEPTH_STENCIL_STATE: u32 = 0x0301;

const OPCODE_SET_RENDER_TARGETS: u32 = 0x0400;
const OPCODE_SET_VIEWPORT: u32 = 0x0401;

const OPCODE_SET_VERTEX_BUFFERS: u32 = 0x0500;
const OPCODE_SET_PRIMITIVE_TOPOLOGY: u32 = 0x0502;

const OPCODE_CLEAR: u32 = 0x0600;
const OPCODE_DRAW: u32 = 0x0601;
const OPCODE_PRESENT: u32 = 0x0700;

const AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER: u32 = 1u32 << 0;
const AEROGPU_RESOURCE_USAGE_RENDER_TARGET: u32 = 1u32 << 4;
const AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL: u32 = 1u32 << 5;

const AEROGPU_FORMAT_R8G8B8A8_UNORM: u32 = 3;
const AEROGPU_FORMAT_D32_FLOAT: u32 = 33;

const AEROGPU_CLEAR_COLOR: u32 = 1u32 << 0;
const AEROGPU_CLEAR_DEPTH: u32 = 1u32 << 1;

const AEROGPU_TOPOLOGY_TRIANGLELIST: u32 = 4;

// Aerogpu compare func values (aerogpu_cmd.h).
const AEROGPU_COMPARE_LESS: u32 = 1;

const DXBC_VS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/vs_passthrough.dxbc");
const DXBC_PS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/ps_passthrough.dxbc");
const ILAY_POS3_COLOR: &[u8] = include_bytes!("fixtures/ilay_pos3_color.bin");

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
#[derive(Copy, Clone, Pod, Zeroable)]
struct Vertex {
    pos: [f32; 3],
    color: [f32; 4],
}

#[test]
fn aerogpu_cmd_depth_testing_keeps_near_fragment() {
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
        const DS: u32 = 3;
        const VS: u32 = 10;
        const PS: u32 = 11;
        const IL: u32 = 20;

        // Draw a fullscreen triangle twice, but in reverse depth order:
        // - Near (green) first: depth = 0.2
        // - Far (red) second: depth = 0.8
        // With LESS depth testing + depth writes, the second draw should fail and the output stays
        // green. Without depth testing, the second draw wins and the output would be red.
        let vertices = [
            // Near
            Vertex {
                pos: [-1.0, -1.0, 0.2],
                color: [0.0, 1.0, 0.0, 1.0],
            },
            Vertex {
                pos: [3.0, -1.0, 0.2],
                color: [0.0, 1.0, 0.0, 1.0],
            },
            Vertex {
                pos: [-1.0, 3.0, 0.2],
                color: [0.0, 1.0, 0.0, 1.0],
            },
            // Far
            Vertex {
                pos: [-1.0, -1.0, 0.8],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [3.0, -1.0, 0.8],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [-1.0, 3.0, 0.8],
                color: [1.0, 0.0, 0.0, 1.0],
            },
        ];
        let vb_bytes = bytemuck::bytes_of(&vertices);
        let vb_size = vb_bytes.len() as u64;

        let guest_mem = VecGuestMemory::new(0x2000);
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

        // CREATE_TEXTURE2D (Depth)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_TEXTURE2D);
        stream.extend_from_slice(&DS.to_le_bytes());
        stream.extend_from_slice(&(AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL).to_le_bytes());
        stream.extend_from_slice(&AEROGPU_FORMAT_D32_FLOAT.to_le_bytes());
        stream.extend_from_slice(&4u32.to_le_bytes()); // width
        stream.extend_from_slice(&4u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id (host alloc)
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_RENDER_TARGETS (RT + DS)
        let start = begin_cmd(&mut stream, OPCODE_SET_RENDER_TARGETS);
        stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
        stream.extend_from_slice(&DS.to_le_bytes()); // depth_stencil
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

        // CLEAR (black + depth=1)
        let start = begin_cmd(&mut stream, OPCODE_CLEAR);
        stream.extend_from_slice(&(AEROGPU_CLEAR_COLOR | AEROGPU_CLEAR_DEPTH).to_le_bytes());
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
        stream.extend_from_slice(&(DXBC_VS_PASSTHROUGH.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(DXBC_VS_PASSTHROUGH);
        stream.resize(
            stream.len() + (align4(DXBC_VS_PASSTHROUGH.len()) - DXBC_VS_PASSTHROUGH.len()),
            0,
        );
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (PS)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_SHADER_DXBC);
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // stage = pixel
        stream.extend_from_slice(&(DXBC_PS_PASSTHROUGH.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(DXBC_PS_PASSTHROUGH);
        stream.resize(
            stream.len() + (align4(DXBC_PS_PASSTHROUGH.len()) - DXBC_PS_PASSTHROUGH.len()),
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
        stream.extend_from_slice(&(ILAY_POS3_COLOR.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(ILAY_POS3_COLOR);
        stream.resize(
            stream.len() + (align4(ILAY_POS3_COLOR.len()) - ILAY_POS3_COLOR.len()),
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
        stream.extend_from_slice(&(std::mem::size_of::<Vertex>() as u32).to_le_bytes()); // stride
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_PRIMITIVE_TOPOLOGY
        let start = begin_cmd(&mut stream, OPCODE_SET_PRIMITIVE_TOPOLOGY);
        stream.extend_from_slice(&AEROGPU_TOPOLOGY_TRIANGLELIST.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_DEPTH_STENCIL_STATE: depth test enabled (LESS), depth writes enabled.
        let start = begin_cmd(&mut stream, OPCODE_SET_DEPTH_STENCIL_STATE);
        stream.extend_from_slice(&1u32.to_le_bytes()); // depth_enable
        stream.extend_from_slice(&1u32.to_le_bytes()); // depth_write_enable
        stream.extend_from_slice(&AEROGPU_COMPARE_LESS.to_le_bytes()); // depth_func
        stream.extend_from_slice(&0u32.to_le_bytes()); // stencil_enable
        stream.push(0xFF); // stencil_read_mask
        stream.push(0xFF); // stencil_write_mask
        stream.extend_from_slice(&[0u8; 2]); // reserved0
        end_cmd(&mut stream, start);

        // DRAW near (first 3 vertices)
        let start = begin_cmd(&mut stream, OPCODE_DRAW);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        // DRAW far (next 3 vertices). Should fail the depth test.
        let start = begin_cmd(&mut stream, OPCODE_DRAW);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&3u32.to_le_bytes()); // first_vertex
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
            assert_eq!(px, &[0, 255, 0, 255]);
        }
    });
}
