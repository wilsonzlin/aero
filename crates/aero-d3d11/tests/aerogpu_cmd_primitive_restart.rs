mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode,
    AerogpuCmdStreamHeader as ProtocolCmdStreamHeader, AerogpuPrimitiveTopology,
    AEROGPU_CLEAR_COLOR, AEROGPU_CMD_STREAM_MAGIC, AEROGPU_RESOURCE_USAGE_INDEX_BUFFER,
    AEROGPU_RESOURCE_USAGE_RENDER_TARGET, AEROGPU_RESOURCE_USAGE_TEXTURE,
    AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::{AerogpuFormat, AEROGPU_ABI_VERSION_U32};

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

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    pos: [f32; 3],
    color: [f32; 4],
}

fn build_stream(
    vb_bytes: &[u8],
    ib_bytes: &[u8],
    topology: AerogpuPrimitiveTopology,
    index_format: u32,
    index_count: u32,
) -> Vec<u8> {
    const VB: u32 = 1;
    const IB: u32 = 2;
    const RT: u32 = 3;
    const VS: u32 = 10;
    const PS: u32 = 11;
    const IL: u32 = 20;

    const WIDTH: u32 = 64;
    const HEIGHT: u32 = 64;

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

    // CREATE_BUFFER (VB)
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
    stream.extend_from_slice(&VB.to_le_bytes());
    stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER.to_le_bytes());
    stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes()); // size_bytes
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

    // CREATE_BUFFER (IB)
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
    stream.extend_from_slice(&IB.to_le_bytes());
    stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_INDEX_BUFFER.to_le_bytes());
    stream.extend_from_slice(&(ib_bytes.len() as u64).to_le_bytes()); // size_bytes
    stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
    stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
    stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
    end_cmd(&mut stream, start);

    // UPLOAD_RESOURCE (IB)
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
    stream.extend_from_slice(&IB.to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
    stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
    stream.extend_from_slice(&(ib_bytes.len() as u64).to_le_bytes()); // size_bytes
    stream.extend_from_slice(ib_bytes);
    stream.resize(stream.len() + (align4(ib_bytes.len()) - ib_bytes.len()), 0);
    end_cmd(&mut stream, start);

    // CREATE_TEXTURE2D (RT)
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
    stream.extend_from_slice(&RT.to_le_bytes());
    stream.extend_from_slice(
        &(AEROGPU_RESOURCE_USAGE_TEXTURE | AEROGPU_RESOURCE_USAGE_RENDER_TARGET).to_le_bytes(),
    );
    stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
    stream.extend_from_slice(&WIDTH.to_le_bytes());
    stream.extend_from_slice(&HEIGHT.to_le_bytes());
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
    stream.extend_from_slice(&(dxbc_vs.len() as u32).to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
    stream.extend_from_slice(dxbc_vs);
    stream.resize(stream.len() + (align4(dxbc_vs.len()) - dxbc_vs.len()), 0);
    end_cmd(&mut stream, start);

    // CREATE_SHADER_DXBC (PS)
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
    stream.extend_from_slice(&PS.to_le_bytes());
    stream.extend_from_slice(&1u32.to_le_bytes()); // stage = pixel
    stream.extend_from_slice(&(dxbc_ps.len() as u32).to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
    stream.extend_from_slice(dxbc_ps);
    stream.resize(stream.len() + (align4(dxbc_ps.len()) - dxbc_ps.len()), 0);
    end_cmd(&mut stream, start);

    // CREATE_INPUT_LAYOUT
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateInputLayout as u32);
    stream.extend_from_slice(&IL.to_le_bytes());
    stream.extend_from_slice(&(ilay.len() as u32).to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
    stream.extend_from_slice(ilay);
    stream.resize(stream.len() + (align4(ilay.len()) - ilay.len()), 0);
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

    // SET_PRIMITIVE_TOPOLOGY
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetPrimitiveTopology as u32);
    stream.extend_from_slice(&(topology as u32).to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
    end_cmd(&mut stream, start);

    // SET_RASTERIZER_STATE (disable face culling).
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetRasterizerState as u32);
    stream.extend_from_slice(&0u32.to_le_bytes()); // fill_mode (solid)
    stream.extend_from_slice(&0u32.to_le_bytes()); // cull_mode (none)
    stream.extend_from_slice(&0u32.to_le_bytes()); // front_ccw (false)
    stream.extend_from_slice(&0u32.to_le_bytes()); // scissor_enable (false)
    stream.extend_from_slice(&0i32.to_le_bytes()); // depth_bias
    stream.extend_from_slice(&0u32.to_le_bytes()); // flags
    end_cmd(&mut stream, start);

    // SET_VIEWPORT
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetViewport as u32);
    stream.extend_from_slice(&0.0f32.to_bits().to_le_bytes()); // x
    stream.extend_from_slice(&0.0f32.to_bits().to_le_bytes()); // y
    stream.extend_from_slice(&(WIDTH as f32).to_bits().to_le_bytes()); // width
    stream.extend_from_slice(&(HEIGHT as f32).to_bits().to_le_bytes()); // height
    stream.extend_from_slice(&0.0f32.to_bits().to_le_bytes()); // min_depth
    stream.extend_from_slice(&1.0f32.to_bits().to_le_bytes()); // max_depth
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

    // CLEAR (opaque black)
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Clear as u32);
    stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
    for bits in [0.0f32, 0.0, 0.0, 1.0].map(f32::to_bits) {
        stream.extend_from_slice(&bits.to_le_bytes());
    }
    stream.extend_from_slice(&1.0f32.to_bits().to_le_bytes()); // depth
    stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
    end_cmd(&mut stream, start);

    // SET_VERTEX_BUFFERS
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetVertexBuffers as u32);
    stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
    stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
    stream.extend_from_slice(&VB.to_le_bytes());
    stream.extend_from_slice(&(core::mem::size_of::<Vertex>() as u32).to_le_bytes()); // stride_bytes
    stream.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
    stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
    end_cmd(&mut stream, start);

    // SET_INDEX_BUFFER
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetIndexBuffer as u32);
    stream.extend_from_slice(&IB.to_le_bytes());
    stream.extend_from_slice(&index_format.to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
    stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
    end_cmd(&mut stream, start);

    // DRAW_INDEXED
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::DrawIndexed as u32);
    stream.extend_from_slice(&index_count.to_le_bytes()); // index_count
    stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
    stream.extend_from_slice(&0u32.to_le_bytes()); // first_index
    stream.extend_from_slice(&0i32.to_le_bytes()); // base_vertex
    stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
    end_cmd(&mut stream, start);

    finish_stream(stream)
}

fn build_stream_two_ib_two_draws(
    vb_bytes: &[u8],
    ib_u16_bytes: &[u8],
    ib_u32_bytes: &[u8],
    topology: AerogpuPrimitiveTopology,
    first_is_u16: bool,
    index_count_u16: u32,
    index_count_u32: u32,
) -> Vec<u8> {
    const VB: u32 = 1;
    const IB16: u32 = 2;
    const RT: u32 = 3;
    const IB32: u32 = 4;
    const VS: u32 = 10;
    const PS: u32 = 11;
    const IL: u32 = 20;

    const WIDTH: u32 = 64;
    const HEIGHT: u32 = 64;

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

    // CREATE_BUFFER (VB)
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
    stream.extend_from_slice(&VB.to_le_bytes());
    stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER.to_le_bytes());
    stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes()); // size_bytes
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

    // CREATE_BUFFER (IB16)
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
    stream.extend_from_slice(&IB16.to_le_bytes());
    stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_INDEX_BUFFER.to_le_bytes());
    stream.extend_from_slice(&(ib_u16_bytes.len() as u64).to_le_bytes()); // size_bytes
    stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
    stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
    stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
    end_cmd(&mut stream, start);

    // UPLOAD_RESOURCE (IB16)
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
    stream.extend_from_slice(&IB16.to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
    stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
    stream.extend_from_slice(&(ib_u16_bytes.len() as u64).to_le_bytes()); // size_bytes
    stream.extend_from_slice(ib_u16_bytes);
    stream.resize(
        stream.len() + (align4(ib_u16_bytes.len()) - ib_u16_bytes.len()),
        0,
    );
    end_cmd(&mut stream, start);

    // CREATE_BUFFER (IB32)
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
    stream.extend_from_slice(&IB32.to_le_bytes());
    stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_INDEX_BUFFER.to_le_bytes());
    stream.extend_from_slice(&(ib_u32_bytes.len() as u64).to_le_bytes()); // size_bytes
    stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
    stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
    stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
    end_cmd(&mut stream, start);

    // UPLOAD_RESOURCE (IB32)
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
    stream.extend_from_slice(&IB32.to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
    stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
    stream.extend_from_slice(&(ib_u32_bytes.len() as u64).to_le_bytes()); // size_bytes
    stream.extend_from_slice(ib_u32_bytes);
    stream.resize(
        stream.len() + (align4(ib_u32_bytes.len()) - ib_u32_bytes.len()),
        0,
    );
    end_cmd(&mut stream, start);

    // CREATE_TEXTURE2D (RT)
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
    stream.extend_from_slice(&RT.to_le_bytes());
    stream.extend_from_slice(
        &(AEROGPU_RESOURCE_USAGE_TEXTURE | AEROGPU_RESOURCE_USAGE_RENDER_TARGET).to_le_bytes(),
    );
    stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
    stream.extend_from_slice(&WIDTH.to_le_bytes());
    stream.extend_from_slice(&HEIGHT.to_le_bytes());
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
    stream.extend_from_slice(&(dxbc_vs.len() as u32).to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
    stream.extend_from_slice(dxbc_vs);
    stream.resize(stream.len() + (align4(dxbc_vs.len()) - dxbc_vs.len()), 0);
    end_cmd(&mut stream, start);

    // CREATE_SHADER_DXBC (PS)
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
    stream.extend_from_slice(&PS.to_le_bytes());
    stream.extend_from_slice(&1u32.to_le_bytes()); // stage = pixel
    stream.extend_from_slice(&(dxbc_ps.len() as u32).to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
    stream.extend_from_slice(dxbc_ps);
    stream.resize(stream.len() + (align4(dxbc_ps.len()) - dxbc_ps.len()), 0);
    end_cmd(&mut stream, start);

    // CREATE_INPUT_LAYOUT
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateInputLayout as u32);
    stream.extend_from_slice(&IL.to_le_bytes());
    stream.extend_from_slice(&(ilay.len() as u32).to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
    stream.extend_from_slice(ilay);
    stream.resize(stream.len() + (align4(ilay.len()) - ilay.len()), 0);
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

    // SET_PRIMITIVE_TOPOLOGY
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetPrimitiveTopology as u32);
    stream.extend_from_slice(&(topology as u32).to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
    end_cmd(&mut stream, start);

    // SET_RASTERIZER_STATE (disable face culling).
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetRasterizerState as u32);
    stream.extend_from_slice(&0u32.to_le_bytes()); // fill_mode (solid)
    stream.extend_from_slice(&0u32.to_le_bytes()); // cull_mode (none)
    stream.extend_from_slice(&0u32.to_le_bytes()); // front_ccw (false)
    stream.extend_from_slice(&0u32.to_le_bytes()); // scissor_enable (false)
    stream.extend_from_slice(&0i32.to_le_bytes()); // depth_bias
    stream.extend_from_slice(&0u32.to_le_bytes()); // flags
    end_cmd(&mut stream, start);

    // SET_VIEWPORT
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetViewport as u32);
    stream.extend_from_slice(&0.0f32.to_bits().to_le_bytes()); // x
    stream.extend_from_slice(&0.0f32.to_bits().to_le_bytes()); // y
    stream.extend_from_slice(&(WIDTH as f32).to_bits().to_le_bytes()); // width
    stream.extend_from_slice(&(HEIGHT as f32).to_bits().to_le_bytes()); // height
    stream.extend_from_slice(&0.0f32.to_bits().to_le_bytes()); // min_depth
    stream.extend_from_slice(&1.0f32.to_bits().to_le_bytes()); // max_depth
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

    // CLEAR (opaque black)
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Clear as u32);
    stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
    for bits in [0.0f32, 0.0, 0.0, 1.0].map(f32::to_bits) {
        stream.extend_from_slice(&bits.to_le_bytes());
    }
    stream.extend_from_slice(&1.0f32.to_bits().to_le_bytes()); // depth
    stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
    end_cmd(&mut stream, start);

    // SET_VERTEX_BUFFERS
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetVertexBuffers as u32);
    stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
    stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
    stream.extend_from_slice(&VB.to_le_bytes());
    stream.extend_from_slice(&(core::mem::size_of::<Vertex>() as u32).to_le_bytes()); // stride_bytes
    stream.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
    stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
    end_cmd(&mut stream, start);

    let (first_handle, first_format, first_count, second_handle, second_format, second_count) =
        if first_is_u16 {
            (IB16, 0u32, index_count_u16, IB32, 1u32, index_count_u32)
        } else {
            (IB32, 1u32, index_count_u32, IB16, 0u32, index_count_u16)
        };

    // SET_INDEX_BUFFER (first)
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetIndexBuffer as u32);
    stream.extend_from_slice(&first_handle.to_le_bytes());
    stream.extend_from_slice(&first_format.to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
    stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
    end_cmd(&mut stream, start);

    // DRAW_INDEXED (first)
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::DrawIndexed as u32);
    stream.extend_from_slice(&first_count.to_le_bytes()); // index_count
    stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
    stream.extend_from_slice(&0u32.to_le_bytes()); // first_index
    stream.extend_from_slice(&0i32.to_le_bytes()); // base_vertex
    stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
    end_cmd(&mut stream, start);

    // SET_INDEX_BUFFER (second)
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetIndexBuffer as u32);
    stream.extend_from_slice(&second_handle.to_le_bytes());
    stream.extend_from_slice(&second_format.to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
    stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
    end_cmd(&mut stream, start);

    // DRAW_INDEXED (second)
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::DrawIndexed as u32);
    stream.extend_from_slice(&second_count.to_le_bytes()); // index_count
    stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
    stream.extend_from_slice(&0u32.to_le_bytes()); // first_index
    stream.extend_from_slice(&0i32.to_le_bytes()); // base_vertex
    stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
    end_cmd(&mut stream, start);

    finish_stream(stream)
}

fn build_stream_copy_buffer_to_index_then_draw(
    vb_bytes: &[u8],
    src_bytes: &[u8],
    topology: AerogpuPrimitiveTopology,
    index_format: u32,
    index_count: u32,
) -> Vec<u8> {
    const VB: u32 = 1;
    const IB: u32 = 2;
    const RT: u32 = 3;
    const STAGING: u32 = 4;
    const VS: u32 = 10;
    const PS: u32 = 11;
    const IL: u32 = 20;

    const WIDTH: u32 = 64;
    const HEIGHT: u32 = 64;

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

    // CREATE_BUFFER (VB)
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
    stream.extend_from_slice(&VB.to_le_bytes());
    stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER.to_le_bytes());
    stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes()); // size_bytes
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

    // CREATE_BUFFER (STAGING, no special usage flags).
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
    stream.extend_from_slice(&STAGING.to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes()); // usage_flags
    stream.extend_from_slice(&(src_bytes.len() as u64).to_le_bytes()); // size_bytes
    stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
    stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
    stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
    end_cmd(&mut stream, start);

    // UPLOAD_RESOURCE (STAGING)
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
    stream.extend_from_slice(&STAGING.to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
    stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
    stream.extend_from_slice(&(src_bytes.len() as u64).to_le_bytes()); // size_bytes
    stream.extend_from_slice(src_bytes);
    stream.resize(
        stream.len() + (align4(src_bytes.len()) - src_bytes.len()),
        0,
    );
    end_cmd(&mut stream, start);

    // CREATE_BUFFER (IB)
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
    stream.extend_from_slice(&IB.to_le_bytes());
    stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_INDEX_BUFFER.to_le_bytes());
    stream.extend_from_slice(&(src_bytes.len() as u64).to_le_bytes()); // size_bytes
    stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
    stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
    stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
    end_cmd(&mut stream, start);

    // COPY_BUFFER (STAGING -> IB)
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CopyBuffer as u32);
    stream.extend_from_slice(&IB.to_le_bytes()); // dst_buffer
    stream.extend_from_slice(&STAGING.to_le_bytes()); // src_buffer
    stream.extend_from_slice(&0u64.to_le_bytes()); // dst_offset_bytes
    stream.extend_from_slice(&0u64.to_le_bytes()); // src_offset_bytes
    stream.extend_from_slice(&(src_bytes.len() as u64).to_le_bytes()); // size_bytes
    stream.extend_from_slice(&0u32.to_le_bytes()); // flags
    stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
    end_cmd(&mut stream, start);

    // CREATE_TEXTURE2D (RT)
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
    stream.extend_from_slice(&RT.to_le_bytes());
    stream.extend_from_slice(
        &(AEROGPU_RESOURCE_USAGE_TEXTURE | AEROGPU_RESOURCE_USAGE_RENDER_TARGET).to_le_bytes(),
    );
    stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
    stream.extend_from_slice(&WIDTH.to_le_bytes());
    stream.extend_from_slice(&HEIGHT.to_le_bytes());
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
    stream.extend_from_slice(&(dxbc_vs.len() as u32).to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
    stream.extend_from_slice(dxbc_vs);
    stream.resize(stream.len() + (align4(dxbc_vs.len()) - dxbc_vs.len()), 0);
    end_cmd(&mut stream, start);

    // CREATE_SHADER_DXBC (PS)
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
    stream.extend_from_slice(&PS.to_le_bytes());
    stream.extend_from_slice(&1u32.to_le_bytes()); // stage = pixel
    stream.extend_from_slice(&(dxbc_ps.len() as u32).to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
    stream.extend_from_slice(dxbc_ps);
    stream.resize(stream.len() + (align4(dxbc_ps.len()) - dxbc_ps.len()), 0);
    end_cmd(&mut stream, start);

    // CREATE_INPUT_LAYOUT
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateInputLayout as u32);
    stream.extend_from_slice(&IL.to_le_bytes());
    stream.extend_from_slice(&(ilay.len() as u32).to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
    stream.extend_from_slice(ilay);
    stream.resize(stream.len() + (align4(ilay.len()) - ilay.len()), 0);
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

    // SET_PRIMITIVE_TOPOLOGY
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetPrimitiveTopology as u32);
    stream.extend_from_slice(&(topology as u32).to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
    end_cmd(&mut stream, start);

    // SET_RASTERIZER_STATE (disable face culling).
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetRasterizerState as u32);
    stream.extend_from_slice(&0u32.to_le_bytes()); // fill_mode (solid)
    stream.extend_from_slice(&0u32.to_le_bytes()); // cull_mode (none)
    stream.extend_from_slice(&0u32.to_le_bytes()); // front_ccw (false)
    stream.extend_from_slice(&0u32.to_le_bytes()); // scissor_enable (false)
    stream.extend_from_slice(&0i32.to_le_bytes()); // depth_bias
    stream.extend_from_slice(&0u32.to_le_bytes()); // flags
    end_cmd(&mut stream, start);

    // SET_VIEWPORT
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetViewport as u32);
    stream.extend_from_slice(&0.0f32.to_bits().to_le_bytes()); // x
    stream.extend_from_slice(&0.0f32.to_bits().to_le_bytes()); // y
    stream.extend_from_slice(&(WIDTH as f32).to_bits().to_le_bytes()); // width
    stream.extend_from_slice(&(HEIGHT as f32).to_bits().to_le_bytes()); // height
    stream.extend_from_slice(&0.0f32.to_bits().to_le_bytes()); // min_depth
    stream.extend_from_slice(&1.0f32.to_bits().to_le_bytes()); // max_depth
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

    // CLEAR (opaque black)
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Clear as u32);
    stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
    for bits in [0.0f32, 0.0, 0.0, 1.0].map(f32::to_bits) {
        stream.extend_from_slice(&bits.to_le_bytes());
    }
    stream.extend_from_slice(&1.0f32.to_bits().to_le_bytes()); // depth
    stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
    end_cmd(&mut stream, start);

    // SET_VERTEX_BUFFERS
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetVertexBuffers as u32);
    stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
    stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
    stream.extend_from_slice(&VB.to_le_bytes());
    stream.extend_from_slice(&(core::mem::size_of::<Vertex>() as u32).to_le_bytes()); // stride_bytes
    stream.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
    stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
    end_cmd(&mut stream, start);

    // SET_INDEX_BUFFER
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetIndexBuffer as u32);
    stream.extend_from_slice(&IB.to_le_bytes());
    stream.extend_from_slice(&index_format.to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
    stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
    end_cmd(&mut stream, start);

    // DRAW_INDEXED
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::DrawIndexed as u32);
    stream.extend_from_slice(&index_count.to_le_bytes()); // index_count
    stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
    stream.extend_from_slice(&0u32.to_le_bytes()); // first_index
    stream.extend_from_slice(&0i32.to_le_bytes()); // base_vertex
    stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
    end_cmd(&mut stream, start);

    finish_stream(stream)
}

fn assert_restart_gap(pixels: &[u8], width: u32, height: u32) {
    assert_eq!(pixels.len(), width as usize * height as usize * 4);
    let w = width as usize;
    let px = |x: usize, y: usize| -> &[u8] {
        let idx = (y * w + x) * 4;
        &pixels[idx..idx + 4]
    };

    // Sample both above and below the strip cut. Without primitive restart, one of these gap
    // pixels would be covered by triangles connecting the two strips through the restart sentinel.
    let left_x = 8usize;
    let mid_x = 32usize;
    let right_x = 60usize;
    let top_y = 16usize;
    let bottom_y = 48usize;

    for y in [top_y, bottom_y] {
        assert_eq!(px(left_x, y), &[255, 255, 255, 255]);
        assert_eq!(px(right_x, y), &[255, 255, 255, 255]);
        assert_eq!(px(mid_x, y), &[0, 0, 0, 255]);
    }
}

fn assert_restart_gap_line_strip(pixels: &[u8], width: u32, height: u32) {
    assert_eq!(pixels.len(), width as usize * height as usize * 4);
    let w = width as usize;
    let px = |x: usize, y: usize| -> &[u8] {
        let idx = (y * w + x) * 4;
        &pixels[idx..idx + 4]
    };

    // Unlike triangle strips, line rasterization can be sensitive to endpoint inclusion rules.
    // Sample along the interior of each vertical line segment to ensure stable coverage.
    let left_x = 8usize;
    let mid_x = 32usize;
    let right_x = 60usize;
    let top_y = 16usize;
    let mid_y = 32usize;

    assert_eq!(px(left_x, mid_y), &[255, 255, 255, 255]);
    assert_eq!(px(right_x, mid_y), &[255, 255, 255, 255]);
    assert_eq!(px(mid_x, top_y), &[0, 0, 0, 255]);
}

fn assert_restart_gap_line_strip_u32(pixels: &[u8], width: u32, height: u32) {
    assert_eq!(pixels.len(), width as usize * height as usize * 4);
    let w = width as usize;
    let px = |x: usize, y: usize| -> &[u8] {
        let idx = (y * w + x) * 4;
        &pixels[idx..idx + 4]
    };

    // The geometry draws two vertical lines (left and right). If primitive restart is *disabled*,
    // the `0xFFFF_FFFF` index becomes an out-of-bounds vertex fetch which WebGPU/wgpu's robust
    // buffer access resolves to zero. This turns the strip into:
    //   left_line -> (left_top -> origin) -> (origin -> right_top) -> right_line
    // and should draw a diagonal bridge through the upper-left quadrant.
    //
    // Sample a few pixels along that expected diagonal (y == -x) that are far away from the
    // intended vertical lines.
    let left_x = 8usize;
    let right_x = 55usize;
    let mid_y = 32usize;

    assert_eq!(
        px(left_x, mid_y),
        &[255, 255, 255, 255],
        "left line missing"
    );
    assert_eq!(
        px(right_x, mid_y),
        &[255, 255, 255, 255],
        "right line missing"
    );

    let bg = &[0, 0, 0, 255];
    for (x, y) in [(20usize, 20usize), (24, 24), (28, 28)] {
        assert_eq!(
            px(x, y),
            bg,
            "expected diagonal bridge pixel ({x},{y}) to remain background (primitive restart broken?)"
        );
    }
}

#[test]
fn aerogpu_cmd_enables_primitive_restart_for_triangle_strip() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const WIDTH: u32 = 64;
        const HEIGHT: u32 = 64;
        const RT: u32 = 3;

        // Two disconnected triangles, with a gap around x=0.
        let vertices = [
            // Left triangle.
            Vertex {
                pos: [-1.0, -1.0, 0.0],
                color: [1.0, 1.0, 1.0, 1.0],
            },
            Vertex {
                pos: [-1.0, 1.0, 0.0],
                color: [1.0, 1.0, 1.0, 1.0],
            },
            Vertex {
                pos: [-0.2, 0.0, 0.0],
                color: [1.0, 1.0, 1.0, 1.0],
            },
            // Right triangle.
            Vertex {
                pos: [0.2, -1.0, 0.0],
                color: [1.0, 1.0, 1.0, 1.0],
            },
            Vertex {
                pos: [1.0, 1.0, 0.0],
                color: [1.0, 1.0, 1.0, 1.0],
            },
            Vertex {
                pos: [1.0, -1.0, 0.0],
                color: [1.0, 1.0, 1.0, 1.0],
            },
        ];
        let vb_bytes = bytemuck::bytes_of(&vertices);

        // Triangle strip indices with a primitive-restart cut between the triangles.
        let indices: [u32; 7] = [0, 1, 2, 0xFFFF_FFFF, 3, 4, 5];
        let ib_bytes = bytemuck::bytes_of(&indices);
        let stream = build_stream(
            vb_bytes,
            ib_bytes,
            AerogpuPrimitiveTopology::TriangleStrip,
            1,
            indices.len() as u32,
        );

        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let pixels = exec.read_texture_rgba8(RT).await.unwrap();
        assert_restart_gap(&pixels, WIDTH, HEIGHT);
    });
}

#[test]
fn aerogpu_cmd_enables_primitive_restart_for_triangle_strip_u16() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const WIDTH: u32 = 64;
        const HEIGHT: u32 = 64;
        const RT: u32 = 3;

        // Same geometry as the u32 test, but make the u16 restart index (0xFFFF) be in-bounds by
        // allocating 65536 vertices. If primitive restart is disabled, the restart sentinel becomes
        // a real vertex reference (index 65535) and the strip will connect across the gap.
        let mut vertices = vec![
            Vertex {
                pos: [0.0, 0.0, 0.0],
                color: [0.0, 0.0, 0.0, 0.0],
            };
            65536
        ];
        let white = [1.0, 1.0, 1.0, 1.0];
        vertices[0] = Vertex {
            pos: [-1.0, -1.0, 0.0],
            color: white,
        };
        vertices[1] = Vertex {
            pos: [-1.0, 1.0, 0.0],
            color: white,
        };
        vertices[2] = Vertex {
            pos: [-0.25, 0.0, 0.0],
            color: white,
        };
        vertices[3] = Vertex {
            pos: [0.2, -1.0, 0.0],
            color: white,
        };
        vertices[4] = Vertex {
            pos: [1.0, 1.0, 0.0],
            color: white,
        };
        vertices[5] = Vertex {
            pos: [1.0, -1.0, 0.0],
            color: white,
        };
        vertices[65535] = Vertex {
            pos: [0.0, 1.0, 0.0],
            color: white,
        };
        let vb_bytes = bytemuck::cast_slice(vertices.as_slice());

        let indices: [u16; 7] = [0, 1, 2, 0xFFFF, 3, 4, 5];
        let ib_bytes = bytemuck::bytes_of(&indices);

        let stream = build_stream(
            vb_bytes,
            ib_bytes,
            AerogpuPrimitiveTopology::TriangleStrip,
            0,
            indices.len() as u32,
        );

        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let pixels = exec.read_texture_rgba8(RT).await.unwrap();
        assert_restart_gap(&pixels, WIDTH, HEIGHT);
    });
}

#[test]
fn aerogpu_cmd_enables_primitive_restart_for_line_strip_u16() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const WIDTH: u32 = 64;
        const HEIGHT: u32 = 64;
        const RT: u32 = 3;

        // Use the same sampling coordinates as `assert_restart_gap_line_strip`.
        let x_left = -47.0 / 64.0;
        let x_mid = 1.0 / 64.0;
        let x_right = 57.0 / 64.0;
        let y_top = 31.0 / 64.0;

        // Make the u16 restart index (0xFFFF) be in-bounds. If primitive restart is disabled, the
        // sentinel becomes a real vertex reference (index 65535) and the strip will connect across
        // the gap through that vertex.
        let mut vertices = vec![
            Vertex {
                pos: [0.0, 0.0, 0.0],
                color: [0.0, 0.0, 0.0, 0.0],
            };
            65536
        ];
        let white = [1.0, 1.0, 1.0, 1.0];
        vertices[0] = Vertex {
            pos: [x_left, -1.0, 0.0],
            color: white,
        };
        vertices[1] = Vertex {
            pos: [x_left, y_top, 0.0],
            color: white,
        };
        vertices[2] = Vertex {
            pos: [x_right, y_top, 0.0],
            color: white,
        };
        vertices[3] = Vertex {
            pos: [x_right, -1.0, 0.0],
            color: white,
        };
        vertices[65535] = Vertex {
            pos: [x_mid, y_top, 0.0],
            color: white,
        };
        let vb_bytes = bytemuck::cast_slice(vertices.as_slice());

        let indices: [u16; 5] = [0, 1, 0xFFFF, 2, 3];
        let ib_bytes = bytemuck::bytes_of(&indices);

        let stream = build_stream(
            vb_bytes,
            ib_bytes,
            AerogpuPrimitiveTopology::LineStrip,
            0,
            indices.len() as u32,
        );

        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let pixels = exec.read_texture_rgba8(RT).await.unwrap();
        assert_restart_gap_line_strip(&pixels, WIDTH, HEIGHT);
    });
}

#[test]
fn aerogpu_cmd_enables_primitive_restart_for_line_strip_u32() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const WIDTH: u32 = 64;
        const HEIGHT: u32 = 64;
        const RT: u32 = 3;

        // Place the vertical lines on exact pixel centers (same convention as the existing u16
        // line-strip test):
        // - x=-47/64 => pixel x=8
        // - x=+47/64 => pixel x=55
        // - y=±47/64 => pixel y=8/55
        let x_left = -47.0 / 64.0;
        let x_right = 47.0 / 64.0;
        let y_top = 47.0 / 64.0;
        let y_bottom = -47.0 / 64.0;

        let white = [1.0, 1.0, 1.0, 1.0];
        let vertices = [
            Vertex {
                pos: [x_left, y_bottom, 0.0],
                color: white,
            },
            Vertex {
                pos: [x_left, y_top, 0.0],
                color: white,
            },
            Vertex {
                pos: [x_right, y_top, 0.0],
                color: white,
            },
            Vertex {
                pos: [x_right, y_bottom, 0.0],
                color: white,
            },
        ];
        let vb_bytes = bytemuck::bytes_of(&vertices);

        // Line strip indices with a primitive-restart value between the strips.
        //
        // We can't allocate enough vertices for 0xFFFF_FFFF to be in-bounds. This relies on
        // WebGPU/wgpu's robust buffer access behavior: if primitive restart is *not* enabled, the
        // out-of-bounds vertex fetch should produce a zeroed position at the origin, which stitches
        // the strip and draws a diagonal bridge.
        let indices: [u32; 5] = [0, 1, 0xFFFF_FFFF, 2, 3];
        let ib_bytes = bytemuck::bytes_of(&indices);

        let stream = build_stream(
            vb_bytes,
            ib_bytes,
            AerogpuPrimitiveTopology::LineStrip,
            1,
            indices.len() as u32,
        );

        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let pixels = exec.read_texture_rgba8(RT).await.unwrap();
        assert_restart_gap_line_strip_u32(&pixels, WIDTH, HEIGHT);
    });
}

#[test]
fn aerogpu_cmd_rebuilds_strip_pipeline_when_index_format_changes() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const WIDTH: u32 = 64;
        const HEIGHT: u32 = 64;
        const RT: u32 = 3;

        // First draw: a simple top quad using a u16 index buffer.
        //
        // Second draw: uses a u32 index buffer containing the value `0xFFFF` (vertex 65535). This
        // *must not* be treated as a strip-restart marker when the index format is u32 (restart
        // marker is `0xFFFF_FFFF`). If the executor fails to rebuild the strip pipeline after
        // switching formats, the stale `strip_index_format=Uint16` would treat `0xFFFF` as a
        // restart and the gap would not be filled.
        let white = [1.0, 1.0, 1.0, 1.0];
        let mut vertices = vec![
            Vertex {
                pos: [0.0, 0.0, 0.0],
                color: [0.0, 0.0, 0.0, 0.0],
            };
            65536
        ];
        // Top quad (indices 0..4).
        vertices[0] = Vertex {
            pos: [-0.9, 0.2, 0.0],
            color: white,
        };
        vertices[1] = Vertex {
            pos: [-0.9, 0.9, 0.0],
            color: white,
        };
        vertices[2] = Vertex {
            pos: [0.9, 0.2, 0.0],
            color: white,
        };
        vertices[3] = Vertex {
            pos: [0.9, 0.9, 0.0],
            color: white,
        };

        // Bottom geometry: two triangles with a gap around x=0.
        vertices[4] = Vertex {
            pos: [-1.0, -1.0, 0.0],
            color: white,
        };
        vertices[5] = Vertex {
            pos: [-1.0, -0.2, 0.0],
            color: white,
        };
        vertices[6] = Vertex {
            pos: [-0.2, -0.6, 0.0],
            color: white,
        };
        vertices[7] = Vertex {
            pos: [0.2, -1.0, 0.0],
            color: white,
        };
        vertices[8] = Vertex {
            pos: [1.0, -0.2, 0.0],
            color: white,
        };
        vertices[9] = Vertex {
            pos: [1.0, -1.0, 0.0],
            color: white,
        };
        // Bridge vertex for the u32 draw (index 65535 / 0xFFFF).
        vertices[65535] = Vertex {
            pos: [0.0, -0.6, 0.0],
            color: white,
        };
        let vb_bytes = bytemuck::cast_slice(vertices.as_slice());

        let indices_u16: [u16; 4] = [0, 1, 2, 3];
        let indices_u32: [u32; 7] = [4, 5, 6, 0xFFFF, 7, 8, 9];
        let stream = build_stream_two_ib_two_draws(
            vb_bytes,
            bytemuck::bytes_of(&indices_u16),
            bytemuck::bytes_of(&indices_u32),
            AerogpuPrimitiveTopology::TriangleStrip,
            true,
            indices_u16.len() as u32,
            indices_u32.len() as u32,
        );

        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let pixels = exec.read_texture_rgba8(RT).await.unwrap();
        assert_eq!(pixels.len(), WIDTH as usize * HEIGHT as usize * 4);
        let w = WIDTH as usize;
        let px = |x: usize, y: usize| -> &[u8] {
            let idx = (y * w + x) * 4;
            &pixels[idx..idx + 4]
        };
        assert_eq!(px(32, 16), &[255, 255, 255, 255], "top quad missing");
        assert_eq!(
            px(8, 56),
            &[255, 255, 255, 255],
            "expected bottom-left triangle coverage"
        );
        assert_eq!(
            px(60, 56),
            &[255, 255, 255, 255],
            "expected bottom-right triangle coverage"
        );
        assert_eq!(
            px(32, 56),
            &[255, 255, 255, 255],
            "expected the u32 draw to treat 0xFFFF as a vertex (not restart) and fill the gap"
        );
        assert_eq!(px(32, 32), &[0, 0, 0, 255], "expected a gap between draws");
    });
}

#[test]
fn aerogpu_cmd_rebuilds_strip_pipeline_when_index_format_changes_u32_to_u16() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const WIDTH: u32 = 64;
        const HEIGHT: u32 = 64;
        const RT: u32 = 3;

        // First draw: u32 index buffer.
        //
        // Second draw: u16 index buffer that includes 0xFFFF. This must be treated as a strip-restart
        // marker when the format is u16. If the executor fails to rebuild the strip pipeline after
        // switching formats, the stale `strip_index_format=Uint32` would treat 0xFFFF as a *real*
        // vertex index (65535) and incorrectly bridge the strip across the gap.
        let white = [1.0, 1.0, 1.0, 1.0];
        let mut vertices = vec![
            Vertex {
                pos: [0.0, 0.0, 0.0],
                color: [0.0, 0.0, 0.0, 0.0],
            };
            65536
        ];
        // Top quad (indices 0..4).
        vertices[0] = Vertex {
            pos: [-0.9, 0.2, 0.0],
            color: white,
        };
        vertices[1] = Vertex {
            pos: [-0.9, 0.9, 0.0],
            color: white,
        };
        vertices[2] = Vertex {
            pos: [0.9, 0.2, 0.0],
            color: white,
        };
        vertices[3] = Vertex {
            pos: [0.9, 0.9, 0.0],
            color: white,
        };

        // Bottom geometry: two triangles with a gap around x=0.
        vertices[4] = Vertex {
            pos: [-1.0, -1.0, 0.0],
            color: white,
        };
        vertices[5] = Vertex {
            pos: [-1.0, 1.0, 0.0],
            color: white,
        };
        vertices[6] = Vertex {
            pos: [-0.25, 0.0, 0.0],
            color: white,
        };
        vertices[7] = Vertex {
            pos: [0.2, -1.0, 0.0],
            color: white,
        };
        vertices[8] = Vertex {
            pos: [1.0, 1.0, 0.0],
            color: white,
        };
        vertices[9] = Vertex {
            pos: [1.0, -1.0, 0.0],
            color: white,
        };
        // Bridge vertex for the u16 draw (index 65535 / 0xFFFF).
        vertices[65535] = Vertex {
            pos: [0.0, 0.0, 0.0],
            color: white,
        };
        let vb_bytes = bytemuck::cast_slice(vertices.as_slice());

        let indices_u32: [u32; 4] = [0, 1, 2, 3];
        let indices_u16: [u16; 7] = [4, 5, 6, 0xFFFF, 7, 8, 9];
        let stream = build_stream_two_ib_two_draws(
            vb_bytes,
            bytemuck::bytes_of(&indices_u16),
            bytemuck::bytes_of(&indices_u32),
            AerogpuPrimitiveTopology::TriangleStrip,
            false,
            indices_u16.len() as u32,
            indices_u32.len() as u32,
        );

        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let pixels = exec.read_texture_rgba8(RT).await.unwrap();
        assert_eq!(pixels.len(), WIDTH as usize * HEIGHT as usize * 4);
        let w = WIDTH as usize;
        let px = |x: usize, y: usize| -> &[u8] {
            let idx = (y * w + x) * 4;
            &pixels[idx..idx + 4]
        };

        assert_eq!(px(32, 16), &[255, 255, 255, 255], "top quad missing");
        assert_eq!(
            px(8, 48),
            &[255, 255, 255, 255],
            "expected bottom-left triangle coverage"
        );
        assert_eq!(
            px(60, 48),
            &[255, 255, 255, 255],
            "expected bottom-right triangle coverage"
        );
        assert_eq!(
            px(32, 48),
            &[0, 0, 0, 255],
            "expected u16 restart (0xFFFF) to split the strip and preserve the gap"
        );
    });
}

#[test]
fn aerogpu_cmd_primitive_restart_after_copy_buffer_to_host_owned_index_buffer() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        // CopyBuffer into a host-owned index buffer (no guest backing) should still allow primitive
        // restart emulation on wgpu GL (requires CPU-visible indices).
        let mut vertices = vec![
            Vertex {
                pos: [0.0, 0.0, 0.0],
                color: [0.0, 0.0, 0.0, 0.0],
            };
            65536
        ];
        let white = [1.0, 1.0, 1.0, 1.0];
        // Draw two quads separated by a restart in a single strip.
        vertices[0] = Vertex {
            pos: [-1.0, -1.0, 0.0],
            color: white,
        };
        vertices[1] = Vertex {
            pos: [-1.0, 1.0, 0.0],
            color: white,
        };
        vertices[2] = Vertex {
            pos: [-0.1, -1.0, 0.0],
            color: white,
        };
        vertices[3] = Vertex {
            pos: [-0.1, 1.0, 0.0],
            color: white,
        };
        vertices[4] = Vertex {
            pos: [0.1, -1.0, 0.0],
            color: white,
        };
        vertices[5] = Vertex {
            pos: [0.1, 1.0, 0.0],
            color: white,
        };
        vertices[6] = Vertex {
            pos: [1.0, -1.0, 0.0],
            color: white,
        };
        vertices[7] = Vertex {
            pos: [1.0, 1.0, 0.0],
            color: white,
        };
        // Make 0xFFFF in-bounds if restart is ignored.
        vertices[65535] = Vertex {
            pos: [0.0, 0.0, 0.0],
            color: white,
        };

        let vb_bytes = bytemuck::cast_slice(vertices.as_slice());
        let indices: [u16; 9] = [0, 1, 2, 3, 0xFFFF, 4, 5, 6, 7];
        let ib_src_bytes = bytemuck::bytes_of(&indices);

        let stream = build_stream_copy_buffer_to_index_then_draw(
            vb_bytes,
            ib_src_bytes,
            AerogpuPrimitiveTopology::TriangleStrip,
            0,
            indices.len() as u32,
        );

        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        const WIDTH: u32 = 64;
        const HEIGHT: u32 = 64;
        const RT: u32 = 3;
        let pixels = exec.read_texture_rgba8(RT).await.unwrap();
        assert_restart_gap(&pixels, WIDTH, HEIGHT);
    });
}
