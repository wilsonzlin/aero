mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode,
    AerogpuCmdStreamHeader as ProtocolCmdStreamHeader, AerogpuCompareFunc,
    AerogpuPrimitiveTopology, AEROGPU_CLEAR_COLOR, AEROGPU_CLEAR_DEPTH, AEROGPU_CMD_STREAM_MAGIC,
    AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL, AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
    AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::{AerogpuFormat, AEROGPU_ABI_VERSION_U32};
use aero_protocol::aerogpu::aerogpu_ring::AerogpuAllocEntry;

const DXBC_VS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/vs_passthrough.dxbc");
const DXBC_PS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/ps_passthrough.dxbc");
const ILAY_POS3_COLOR: &[u8] = include_bytes!("fixtures/ilay_pos3_color.bin");

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

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    pos: [f32; 3],
    color: [f32; 4],
}

#[test]
fn aerogpu_cmd_depth_test_rejects_far_fragments() {
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

        let vertices = [
            // Near triangle (green) - should win depth test.
            Vertex {
                pos: [-1.0, -1.0, 0.2],
                color: [0.0, 1.0, 0.0, 1.0],
            },
            Vertex {
                pos: [-1.0, 3.0, 0.2],
                color: [0.0, 1.0, 0.0, 1.0],
            },
            Vertex {
                pos: [3.0, -1.0, 0.2],
                color: [0.0, 1.0, 0.0, 1.0],
            },
            // Far triangle (red) - drawn after, but should be rejected.
            Vertex {
                pos: [-1.0, -1.0, 0.8],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [-1.0, 3.0, 0.8],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [3.0, -1.0, 0.8],
                color: [1.0, 0.0, 0.0, 1.0],
            },
        ];
        let vb_bytes = bytemuck::cast_slice(&vertices);
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

        let mut stream = Vec::new();
        // Stream header (24 bytes)
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_BUFFER (VB, guest-backed)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&(AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER).to_le_bytes());
        stream.extend_from_slice(&vb_size.to_le_bytes());
        stream.extend_from_slice(&alloc_id.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // RESOURCE_DIRTY_RANGE (full VB)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::ResourceDirtyRange as u32);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&vb_size.to_le_bytes()); // size_bytes
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (RT)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&RT.to_le_bytes());
        stream.extend_from_slice(&(AEROGPU_RESOURCE_USAGE_RENDER_TARGET).to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&4u32.to_le_bytes()); // width
        stream.extend_from_slice(&4u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id (host alloc)
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (DS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&DS.to_le_bytes());
        stream.extend_from_slice(&(AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL).to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::D32Float as u32).to_le_bytes());
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
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetRenderTargets as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
        stream.extend_from_slice(&DS.to_le_bytes()); // depth_stencil
        stream.extend_from_slice(&RT.to_le_bytes()); // colors[0]
        for _ in 0..7 {
            stream.extend_from_slice(&0u32.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        // VIEWPORT 0..4
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetViewport as u32);
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&4f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&4f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes());
        end_cmd(&mut stream, start);

        // SET_DEPTH_STENCIL_STATE: depth test LESS + write enabled.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetDepthStencilState as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // depth_enable
        stream.extend_from_slice(&1u32.to_le_bytes()); // depth_write_enable
        stream.extend_from_slice(&(AerogpuCompareFunc::Less as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // stencil_enable
        stream.push(0xFF); // stencil_read_mask
        stream.push(0xFF); // stencil_write_mask
        stream.extend_from_slice(&[0u8; 2]); // reserved0
        end_cmd(&mut stream, start);

        // CLEAR (black + depth=1.0)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Clear as u32);
        stream.extend_from_slice(&(AEROGPU_CLEAR_COLOR | AEROGPU_CLEAR_DEPTH).to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // depth
        stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (VS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
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
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
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
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::BindShaders as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // cs
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_INPUT_LAYOUT (ILAY blob)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateInputLayout as u32);
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
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_VERTEX_BUFFERS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetVertexBuffers as u32);
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
        stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&(std::mem::size_of::<Vertex>() as u32).to_le_bytes()); // stride
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_PRIMITIVE_TOPOLOGY
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetPrimitiveTopology as u32);
        stream.extend_from_slice(&(AerogpuPrimitiveTopology::TriangleList as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // DRAW near triangle first.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Draw as u32);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        // DRAW far triangle second (should fail depth test).
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Draw as u32);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&3u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        // Patch stream size in header.
        let total_size = stream.len() as u32;
        stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
            .copy_from_slice(&total_size.to_le_bytes());

        exec.execute_cmd_stream(&stream, Some(&allocs), &mut guest_mem)
            .unwrap();
        exec.poll_wait();

        let pixels = exec.read_texture_rgba8(RT).await.unwrap();
        assert_eq!(pixels.len(), 4 * 4 * 4);
        for px in pixels.chunks_exact(4) {
            assert_eq!(px, &[0, 255, 0, 255]);
        }
    });
}
