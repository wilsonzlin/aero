use aero_d3d11::input_layout::fnv1a_32;
use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::{
    VecGuestMemory, AEROGPU_INPUT_LAYOUT_BLOB_MAGIC, AEROGPU_INPUT_LAYOUT_BLOB_VERSION,
};
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode,
    AerogpuCmdStreamHeader as ProtocolCmdStreamHeader, AerogpuPrimitiveTopology,
    AEROGPU_CLEAR_COLOR, AEROGPU_CMD_STREAM_MAGIC, AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
    AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::{AerogpuFormat, AEROGPU_ABI_VERSION_U32};

const DXBC_VS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/vs_passthrough.dxbc");
const DXBC_PS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/ps_passthrough.dxbc");

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

const OPCODE_CLEAR: u32 = AerogpuCmdOpcode::Clear as u32;
const OPCODE_DRAW: u32 = AerogpuCmdOpcode::Draw as u32;
const OPCODE_PRESENT: u32 = AerogpuCmdOpcode::Present as u32;

const AEROGPU_FORMAT_R8G8B8A8_UNORM: u32 = AerogpuFormat::R8G8B8A8Unorm as u32;
const AEROGPU_TOPOLOGY_TRIANGLELIST: u32 = AerogpuPrimitiveTopology::TriangleList as u32;

const CMD_STREAM_SIZE_BYTES_OFFSET: usize =
    core::mem::offset_of!(ProtocolCmdStreamHeader, size_bytes);
const CMD_HDR_SIZE_BYTES_OFFSET: usize = core::mem::offset_of!(ProtocolCmdHdr, size_bytes);

// DXGI_FORMAT from dxgiformat.h.
const DXGI_FORMAT_R32G32B32A32_FLOAT: u32 = 2;
const DXGI_FORMAT_R32G32B32_FLOAT: u32 = 6;

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

#[test]
fn aerogpu_cmd_renders_with_sparse_vertex_buffer_slots() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                eprintln!("wgpu unavailable ({e:#}); skipping sparse slot test");
                return;
            }
        };

        const VB_COLOR: u32 = 1;
        const VB_POS: u32 = 2;
        const RT: u32 = 3;
        const VS: u32 = 10;
        const PS: u32 = 11;
        const IL: u32 = 20;

        let positions: [[f32; 3]; 3] = [[-1.0, -1.0, 0.0], [3.0, -1.0, 0.0], [-1.0, 3.0, 0.0]];
        let colors: [[f32; 4]; 3] = [[1.0, 0.0, 0.0, 1.0]; 3];

        let pos_bytes = bytemuck::cast_slice(&positions);
        let color_bytes = bytemuck::cast_slice(&colors);

        // ILAY blob with two elements in sparse D3D slots (0 and 15), intentionally declared out of
        // order (COLOR before POSITION). A correct implementation must:
        // 1) map elements to shader locations using (semantic_name, semantic_index) against VS ISGN
        // 2) compact sparse D3D IA slots into dense WebGPU vertex buffer slots.
        let mut ilay = Vec::new();
        ilay.extend_from_slice(&AEROGPU_INPUT_LAYOUT_BLOB_MAGIC.to_le_bytes());
        ilay.extend_from_slice(&AEROGPU_INPUT_LAYOUT_BLOB_VERSION.to_le_bytes());
        ilay.extend_from_slice(&2u32.to_le_bytes()); // element_count
        ilay.extend_from_slice(&0u32.to_le_bytes()); // reserved0

        // COLOR0: slot 0, offset 0
        ilay.extend_from_slice(&fnv1a_32(b"COLOR").to_le_bytes());
        ilay.extend_from_slice(&0u32.to_le_bytes()); // semantic_index
        ilay.extend_from_slice(&DXGI_FORMAT_R32G32B32A32_FLOAT.to_le_bytes());
        ilay.extend_from_slice(&0u32.to_le_bytes()); // input_slot
        ilay.extend_from_slice(&0u32.to_le_bytes()); // aligned_byte_offset
        ilay.extend_from_slice(&0u32.to_le_bytes()); // input_slot_class (per-vertex)
        ilay.extend_from_slice(&0u32.to_le_bytes()); // instance_data_step_rate

        // POSITION0: slot 15, offset 0
        ilay.extend_from_slice(&fnv1a_32(b"POSITION").to_le_bytes());
        ilay.extend_from_slice(&0u32.to_le_bytes()); // semantic_index
        ilay.extend_from_slice(&DXGI_FORMAT_R32G32B32_FLOAT.to_le_bytes());
        ilay.extend_from_slice(&15u32.to_le_bytes()); // input_slot
        ilay.extend_from_slice(&0u32.to_le_bytes()); // aligned_byte_offset
        ilay.extend_from_slice(&0u32.to_le_bytes()); // input_slot_class (per-vertex)
        ilay.extend_from_slice(&0u32.to_le_bytes()); // instance_data_step_rate

        let dxbc_vs = DXBC_VS_PASSTHROUGH;
        let dxbc_ps = DXBC_PS_PASSTHROUGH;

        let mut stream = Vec::new();
        // Stream header (24 bytes)
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_BUFFER + UPLOAD_RESOURCE (VB_COLOR).
        let start = begin_cmd(&mut stream, OPCODE_CREATE_BUFFER);
        stream.extend_from_slice(&VB_COLOR.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER.to_le_bytes());
        stream.extend_from_slice(&(color_bytes.len() as u64).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id (host)
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        let start = begin_cmd(&mut stream, OPCODE_UPLOAD_RESOURCE);
        stream.extend_from_slice(&VB_COLOR.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&(color_bytes.len() as u64).to_le_bytes());
        stream.extend_from_slice(color_bytes);
        stream.resize(
            stream.len() + (align4(color_bytes.len()) - color_bytes.len()),
            0,
        );
        end_cmd(&mut stream, start);

        // CREATE_BUFFER + UPLOAD_RESOURCE (VB_POS).
        let start = begin_cmd(&mut stream, OPCODE_CREATE_BUFFER);
        stream.extend_from_slice(&VB_POS.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER.to_le_bytes());
        stream.extend_from_slice(&(pos_bytes.len() as u64).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id (host)
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        let start = begin_cmd(&mut stream, OPCODE_UPLOAD_RESOURCE);
        stream.extend_from_slice(&VB_POS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&(pos_bytes.len() as u64).to_le_bytes());
        stream.extend_from_slice(pos_bytes);
        stream.resize(
            stream.len() + (align4(pos_bytes.len()) - pos_bytes.len()),
            0,
        );
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
        stream.extend_from_slice(dxbc_vs);
        stream.resize(stream.len() + (align4(dxbc_vs.len()) - dxbc_vs.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (PS)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_SHADER_DXBC);
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // stage = pixel
        stream.extend_from_slice(&(dxbc_ps.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(dxbc_ps);
        stream.resize(stream.len() + (align4(dxbc_ps.len()) - dxbc_ps.len()), 0);
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

        // SET_VERTEX_BUFFERS slot 0: color
        let start = begin_cmd(&mut stream, OPCODE_SET_VERTEX_BUFFERS);
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
        stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
        stream.extend_from_slice(&VB_COLOR.to_le_bytes());
        stream.extend_from_slice(&16u32.to_le_bytes()); // stride_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_VERTEX_BUFFERS slot 15: position
        let start = begin_cmd(&mut stream, OPCODE_SET_VERTEX_BUFFERS);
        stream.extend_from_slice(&15u32.to_le_bytes()); // start_slot
        stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
        stream.extend_from_slice(&VB_POS.to_le_bytes());
        stream.extend_from_slice(&12u32.to_le_bytes()); // stride_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
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
        stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
            .copy_from_slice(&total_size.to_le_bytes());

        let guest_mem = VecGuestMemory::new(0x1000);
        exec.execute_cmd_stream(&stream, None, &guest_mem).unwrap();
        exec.poll_wait();

        let pixels = exec.read_texture_rgba8(RT).await.unwrap();
        assert_eq!(pixels.len(), 4 * 4 * 4);
        for px in pixels.chunks_exact(4) {
            assert_eq!(px, &[255, 0, 0, 255]);
        }
    });
}
