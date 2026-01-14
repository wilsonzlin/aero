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

fn begin_stream() -> Vec<u8> {
    let mut stream = Vec::new();
    stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
    stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
    stream.extend_from_slice(&0u32.to_le_bytes()); // flags
    stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
    stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1
    stream
}

fn patch_stream_size(stream: &mut [u8]) {
    let total_size = stream.len() as u32;
    stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
        .copy_from_slice(&total_size.to_le_bytes());
}

fn push_depth_state(stream: &mut Vec<u8>) {
    let start = begin_cmd(stream, AerogpuCmdOpcode::SetDepthStencilState as u32);
    stream.extend_from_slice(&1u32.to_le_bytes()); // depth_enable
    stream.extend_from_slice(&1u32.to_le_bytes()); // depth_write_enable
    stream.extend_from_slice(&(AerogpuCompareFunc::Less as u32).to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes()); // stencil_enable
    stream.extend_from_slice(&[0u8, 0u8, 0u8, 0u8]); // read/write mask + reserved0
    end_cmd(stream, start);
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    pos: [f32; 3],
    color: [f32; 4],
}

#[test]
fn aerogpu_cmd_depth_only_render_pass_writes_depth() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const VB: u32 = 1;
        const C: u32 = 2;
        const D: u32 = 3;
        const VS: u32 = 10;
        const PS: u32 = 11;
        const IL: u32 = 20;

        // Fullscreen triangle (two copies at different depths):
        // - Pass 1 (depth-only): z = 0.5
        // - Pass 2: z = 0.6, red output, depth test LESS (should be rejected by depth=0.5)
        let vertices = [
            // Pass 1: z = 0.5
            Vertex {
                pos: [-1.0, -1.0, 0.5],
                color: [1.0, 1.0, 1.0, 1.0],
            },
            Vertex {
                pos: [-1.0, 3.0, 0.5],
                color: [1.0, 1.0, 1.0, 1.0],
            },
            Vertex {
                pos: [3.0, -1.0, 0.5],
                color: [1.0, 1.0, 1.0, 1.0],
            },
            // Pass 2: z = 0.6 (red)
            Vertex {
                pos: [-1.0, -1.0, 0.6],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [-1.0, 3.0, 0.6],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [3.0, -1.0, 0.6],
                color: [1.0, 0.0, 0.0, 1.0],
            },
        ];
        let vb_bytes = bytemuck::bytes_of(&vertices);

        let mut guest_mem = VecGuestMemory::new(0);

        let mut stream = begin_stream();

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
        stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes());
        stream.extend_from_slice(vb_bytes);
        stream.resize(stream.len() + (align4(vb_bytes.len()) - vb_bytes.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (Color C)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&C.to_le_bytes());
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

        // CREATE_TEXTURE2D (Depth D)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&D.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::D32Float as u32).to_le_bytes());
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

        // CREATE_INPUT_LAYOUT
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
        stream.extend_from_slice(&(core::mem::size_of::<Vertex>() as u32).to_le_bytes()); // stride
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_PRIMITIVE_TOPOLOGY (TriangleList)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetPrimitiveTopology as u32);
        stream.extend_from_slice(&(AerogpuPrimitiveTopology::TriangleList as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
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

        // Depth enabled (LESS + writes).
        push_depth_state(&mut stream);

        // Pass 1: depth-only (no color RTV bound), clear depth, draw at z=0.5.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetRenderTargets as u32);
        // Use a non-zero RTV count with a NULL handle to ensure the executor accepts "all-NULL"
        // RTV arrays (i.e. no color target bound, but with an explicit slot count).
        stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
        stream.extend_from_slice(&D.to_le_bytes()); // depth_stencil
        stream.extend_from_slice(&0u32.to_le_bytes()); // colors[0] = NULL
        for _ in 0..7 {
            stream.extend_from_slice(&0u32.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Clear as u32);
        stream.extend_from_slice(&(AEROGPU_CLEAR_DEPTH).to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // color.r
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // color.g
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // color.b
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // color.a
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // depth
        stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
        end_cmd(&mut stream, start);

        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Draw as u32);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        // Pass 2: bind color+depth, clear color to black, draw at z=0.6 with red output (should
        // fail depth test against 0.5 from pass 1).
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetRenderTargets as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
        stream.extend_from_slice(&D.to_le_bytes()); // depth_stencil
        stream.extend_from_slice(&C.to_le_bytes()); // colors[0]
        for _ in 0..7 {
            stream.extend_from_slice(&0u32.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Clear as u32);
        stream.extend_from_slice(&(AEROGPU_CLEAR_COLOR).to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // r
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // g
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // b
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // a
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // depth (ignored)
        stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
        end_cmd(&mut stream, start);

        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Draw as u32);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&3u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        // PRESENT (forces a submit boundary like real guest streams tend to do).
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Present as u32);
        stream.extend_from_slice(&0u32.to_le_bytes()); // scanout_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        end_cmd(&mut stream, start);

        patch_stream_size(&mut stream);

        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let pixels = exec
            .read_texture_rgba8(C)
            .await
            .expect("readback should succeed");
        for px in pixels.chunks_exact(4) {
            assert_eq!(
                px,
                &[0, 0, 0, 255],
                "color draw should be rejected by depth"
            );
        }
    });
}
