mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode,
    AerogpuCmdStreamHeader as ProtocolCmdStreamHeader, AerogpuPrimitiveTopology,
    AEROGPU_CLEAR_COLOR, AEROGPU_CMD_STREAM_MAGIC, AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
    AEROGPU_RESOURCE_USAGE_TEXTURE, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::{AerogpuFormat, AEROGPU_ABI_VERSION_U32};

const CMD_STREAM_SIZE_BYTES_OFFSET: usize =
    core::mem::offset_of!(ProtocolCmdStreamHeader, size_bytes);
const CMD_HDR_SIZE_BYTES_OFFSET: usize = core::mem::offset_of!(ProtocolCmdHdr, size_bytes);

const OPCODE_CREATE_BUFFER: u32 = AerogpuCmdOpcode::CreateBuffer as u32;
const OPCODE_CREATE_SHADER_DXBC: u32 = AerogpuCmdOpcode::CreateShaderDxbc as u32;
const OPCODE_CREATE_INPUT_LAYOUT: u32 = AerogpuCmdOpcode::CreateInputLayout as u32;
const OPCODE_BIND_SHADERS: u32 = AerogpuCmdOpcode::BindShaders as u32;
const OPCODE_SET_INPUT_LAYOUT: u32 = AerogpuCmdOpcode::SetInputLayout as u32;
const OPCODE_SET_VERTEX_BUFFERS: u32 = AerogpuCmdOpcode::SetVertexBuffers as u32;
const OPCODE_SET_PRIMITIVE_TOPOLOGY: u32 = AerogpuCmdOpcode::SetPrimitiveTopology as u32;
const OPCODE_CREATE_TEXTURE2D: u32 = AerogpuCmdOpcode::CreateTexture2d as u32;
const OPCODE_SET_RENDER_TARGETS: u32 = AerogpuCmdOpcode::SetRenderTargets as u32;
const OPCODE_SET_VIEWPORT: u32 = AerogpuCmdOpcode::SetViewport as u32;
const OPCODE_CLEAR: u32 = AerogpuCmdOpcode::Clear as u32;
const OPCODE_DRAW: u32 = AerogpuCmdOpcode::Draw as u32;
const OPCODE_UPLOAD_RESOURCE: u32 = AerogpuCmdOpcode::UploadResource as u32;

const AEROGPU_FORMAT_R8G8B8A8_UNORM: u32 = AerogpuFormat::R8G8B8A8Unorm as u32;
const AEROGPU_TOPOLOGY_TRIANGLELIST: u32 = AerogpuPrimitiveTopology::TriangleList as u32;

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

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    pos: [f32; 3],
    color: [f32; 4],
}

#[test]
fn aerogpu_cmd_default_culls_ccw_triangles() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const VB_CCW: u32 = 1;
        const VB_CW: u32 = 2;
        const RT_CCW: u32 = 3;
        const RT_CW: u32 = 4;
        const VS: u32 = 10;
        const PS: u32 = 11;
        const IL: u32 = 20;

        let vertices_ccw = [
            Vertex {
                pos: [-1.0, -1.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [3.0, -1.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [-1.0, 3.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
        ];
        let vertices_cw = [
            Vertex {
                pos: [-1.0, -1.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [-1.0, 3.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [3.0, -1.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
        ];
        let vb_ccw_bytes = bytemuck::bytes_of(&vertices_ccw);
        let vb_cw_bytes = bytemuck::bytes_of(&vertices_cw);

        let dxbc_vs: &[u8] = include_bytes!("fixtures/vs_passthrough.dxbc");
        let dxbc_ps: &[u8] = include_bytes!("fixtures/ps_passthrough.dxbc");
        let ilay: &[u8] = include_bytes!("fixtures/ilay_pos3_color.bin");

        let mut stream = Vec::new();
        // Stream header (24 bytes)
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        for (handle, bytes) in [(VB_CCW, vb_ccw_bytes), (VB_CW, vb_cw_bytes)] {
            // CREATE_BUFFER (VB)
            let start = begin_cmd(&mut stream, OPCODE_CREATE_BUFFER);
            stream.extend_from_slice(&handle.to_le_bytes());
            stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER.to_le_bytes());
            stream.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
            stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
            stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
            end_cmd(&mut stream, start);

            // UPLOAD_RESOURCE (VB)
            let start = begin_cmd(&mut stream, OPCODE_UPLOAD_RESOURCE);
            stream.extend_from_slice(&handle.to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
            stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
            stream.extend_from_slice(&(bytes.len() as u64).to_le_bytes()); // size_bytes
            stream.extend_from_slice(bytes);
            stream.resize(stream.len() + (align4(bytes.len()) - bytes.len()), 0);
            end_cmd(&mut stream, start);
        }

        for handle in [RT_CCW, RT_CW] {
            // CREATE_TEXTURE2D (RT)
            let start = begin_cmd(&mut stream, OPCODE_CREATE_TEXTURE2D);
            stream.extend_from_slice(&handle.to_le_bytes());
            stream.extend_from_slice(
                &(AEROGPU_RESOURCE_USAGE_TEXTURE | AEROGPU_RESOURCE_USAGE_RENDER_TARGET)
                    .to_le_bytes(),
            );
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
        }

        // CREATE_SHADER_DXBC (VS)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_SHADER_DXBC);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // shader_stage = vertex
        stream.extend_from_slice(&(dxbc_vs.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(dxbc_vs);
        stream.resize(stream.len() + (align4(dxbc_vs.len()) - dxbc_vs.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (PS)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_SHADER_DXBC);
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&(dxbc_ps.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(dxbc_ps);
        stream.resize(stream.len() + (align4(dxbc_ps.len()) - dxbc_ps.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, OPCODE_CREATE_INPUT_LAYOUT);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&(ilay.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(ilay);
        stream.resize(stream.len() + (align4(ilay.len()) - ilay.len()), 0);
        end_cmd(&mut stream, start);

        // BIND_SHADERS
        let start = begin_cmd(&mut stream, OPCODE_BIND_SHADERS);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // cs
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, OPCODE_SET_INPUT_LAYOUT);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_PRIMITIVE_TOPOLOGY
        let start = begin_cmd(&mut stream, OPCODE_SET_PRIMITIVE_TOPOLOGY);
        stream.extend_from_slice(&AEROGPU_TOPOLOGY_TRIANGLELIST.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_VIEWPORT
        let start = begin_cmd(&mut stream, OPCODE_SET_VIEWPORT);
        stream.extend_from_slice(&0.0f32.to_bits().to_le_bytes()); // x
        stream.extend_from_slice(&0.0f32.to_bits().to_le_bytes()); // y
        stream.extend_from_slice(&4.0f32.to_bits().to_le_bytes()); // width
        stream.extend_from_slice(&4.0f32.to_bits().to_le_bytes()); // height
        stream.extend_from_slice(&0.0f32.to_bits().to_le_bytes()); // min_depth
        stream.extend_from_slice(&1.0f32.to_bits().to_le_bytes()); // max_depth
        end_cmd(&mut stream, start);

        for (rt, vb) in [(RT_CCW, VB_CCW), (RT_CW, VB_CW)] {
            // SET_RENDER_TARGETS
            let start = begin_cmd(&mut stream, OPCODE_SET_RENDER_TARGETS);
            stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
            stream.extend_from_slice(&0u32.to_le_bytes()); // depth_stencil
            stream.extend_from_slice(&rt.to_le_bytes());
            for _ in 0..7 {
                stream.extend_from_slice(&0u32.to_le_bytes());
            }
            end_cmd(&mut stream, start);

            // CLEAR (green)
            let start = begin_cmd(&mut stream, OPCODE_CLEAR);
            stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
            stream.extend_from_slice(&0.0f32.to_bits().to_le_bytes()); // r
            stream.extend_from_slice(&1.0f32.to_bits().to_le_bytes()); // g
            stream.extend_from_slice(&0.0f32.to_bits().to_le_bytes()); // b
            stream.extend_from_slice(&1.0f32.to_bits().to_le_bytes()); // a
            stream.extend_from_slice(&1.0f32.to_bits().to_le_bytes()); // depth
            stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
            end_cmd(&mut stream, start);

            // SET_VERTEX_BUFFERS
            let start = begin_cmd(&mut stream, OPCODE_SET_VERTEX_BUFFERS);
            stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
            stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
            stream.extend_from_slice(&vb.to_le_bytes());
            stream.extend_from_slice(&(core::mem::size_of::<Vertex>() as u32).to_le_bytes()); // stride
            stream.extend_from_slice(&0u32.to_le_bytes()); // offset
            stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
            end_cmd(&mut stream, start);

            // DRAW
            let start = begin_cmd(&mut stream, OPCODE_DRAW);
            stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
            stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
            stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
            stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
            end_cmd(&mut stream, start);
        }

        // Patch stream size in header.
        let total_size = stream.len() as u32;
        stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
            .copy_from_slice(&total_size.to_le_bytes());

        let guest_mem = VecGuestMemory::new(0x1000);
        exec.execute_cmd_stream(&stream, None, &guest_mem).unwrap();
        exec.poll_wait();

        let pixels_ccw = exec.read_texture_rgba8(RT_CCW).await.unwrap();
        assert_eq!(pixels_ccw.len(), 4 * 4 * 4);
        for px in pixels_ccw.chunks_exact(4) {
            assert_eq!(px, &[0, 255, 0, 255]);
        }

        let pixels_cw = exec.read_texture_rgba8(RT_CW).await.unwrap();
        assert_eq!(pixels_cw.len(), 4 * 4 * 4);
        for px in pixels_cw.chunks_exact(4) {
            assert_eq!(px, &[255, 0, 0, 255]);
        }
    });
}
