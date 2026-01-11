use std::fs;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode,
    AerogpuCmdStreamHeader as ProtocolCmdStreamHeader, AerogpuPrimitiveTopology,
    AEROGPU_CLEAR_COLOR, AEROGPU_CMD_STREAM_MAGIC, AEROGPU_RASTERIZER_FLAG_DEPTH_CLIP_DISABLE,
    AEROGPU_RESOURCE_USAGE_RENDER_TARGET, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::{AerogpuFormat, AEROGPU_ABI_VERSION_U32};

const CMD_STREAM_SIZE_BYTES_OFFSET: usize =
    core::mem::offset_of!(ProtocolCmdStreamHeader, size_bytes);
const CMD_HDR_SIZE_BYTES_OFFSET: usize = core::mem::offset_of!(ProtocolCmdHdr, size_bytes);

fn load_fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    fs::read(&path).unwrap_or_else(|e| panic!("failed to read {path}: {e}"))
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

fn assert_all_pixels_eq(pixels: &[u8], expected: [u8; 4]) {
    assert_eq!(pixels.len() % 4, 0, "pixel buffer must be RGBA8");
    for (i, px) in pixels.chunks_exact(4).enumerate() {
        assert_eq!(px, expected, "pixel mismatch at index {i}");
    }
}

#[test]
fn aerogpu_cmd_depth_clip_toggle_clamps_z_when_disabled() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                eprintln!("wgpu unavailable ({e:#}); skipping depth clip test");
                return;
            }
        };

        const RT: u32 = 1;
        const VB: u32 = 2;
        const VS: u32 = 10;
        const PS: u32 = 11;
        const IL: u32 = 20;

        let vertices = [
            Vertex {
                pos: [-1.0, -1.0, 2.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [3.0, -1.0, 2.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [-1.0, 3.0, 2.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
        ];
        let vb_bytes = bytemuck::bytes_of(&vertices);

        let vs_dxbc = load_fixture("vs_passthrough.dxbc");
        let ps_dxbc = load_fixture("ps_passthrough.dxbc");
        let ilay = load_fixture("ilay_pos3_color.bin");

        // Create resources once.
        let mut setup = Vec::new();
        setup.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        setup.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        setup.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        setup.extend_from_slice(&0u32.to_le_bytes()); // flags
        setup.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        setup.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_TEXTURE2D (render target)
        let start = begin_cmd(&mut setup, AerogpuCmdOpcode::CreateTexture2d as u32);
        setup.extend_from_slice(&RT.to_le_bytes());
        setup.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes());
        setup.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        setup.extend_from_slice(&4u32.to_le_bytes()); // width
        setup.extend_from_slice(&4u32.to_le_bytes()); // height
        setup.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        setup.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        setup.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        setup.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        setup.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        setup.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut setup, start);

        // CREATE_BUFFER (vertex buffer)
        let start = begin_cmd(&mut setup, AerogpuCmdOpcode::CreateBuffer as u32);
        setup.extend_from_slice(&VB.to_le_bytes());
        setup.extend_from_slice(&AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER.to_le_bytes());
        setup.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes());
        setup.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        setup.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        setup.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut setup, start);

        // UPLOAD_RESOURCE (vertex buffer)
        let start = begin_cmd(&mut setup, AerogpuCmdOpcode::UploadResource as u32);
        setup.extend_from_slice(&VB.to_le_bytes());
        setup.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        setup.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        setup.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes());
        setup.extend_from_slice(vb_bytes);
        setup.resize(setup.len() + (align4(vb_bytes.len()) - vb_bytes.len()), 0);
        end_cmd(&mut setup, start);

        // CREATE_SHADER_DXBC (VS)
        let start = begin_cmd(&mut setup, AerogpuCmdOpcode::CreateShaderDxbc as u32);
        setup.extend_from_slice(&VS.to_le_bytes());
        setup.extend_from_slice(&0u32.to_le_bytes()); // vertex stage
        setup.extend_from_slice(&(vs_dxbc.len() as u32).to_le_bytes());
        setup.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        setup.extend_from_slice(&vs_dxbc);
        setup.resize(setup.len() + (align4(vs_dxbc.len()) - vs_dxbc.len()), 0);
        end_cmd(&mut setup, start);

        // CREATE_SHADER_DXBC (PS)
        let start = begin_cmd(&mut setup, AerogpuCmdOpcode::CreateShaderDxbc as u32);
        setup.extend_from_slice(&PS.to_le_bytes());
        setup.extend_from_slice(&1u32.to_le_bytes()); // pixel stage
        setup.extend_from_slice(&(ps_dxbc.len() as u32).to_le_bytes());
        setup.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        setup.extend_from_slice(&ps_dxbc);
        setup.resize(setup.len() + (align4(ps_dxbc.len()) - ps_dxbc.len()), 0);
        end_cmd(&mut setup, start);

        // CREATE_INPUT_LAYOUT
        let start = begin_cmd(&mut setup, AerogpuCmdOpcode::CreateInputLayout as u32);
        setup.extend_from_slice(&IL.to_le_bytes());
        setup.extend_from_slice(&(ilay.len() as u32).to_le_bytes());
        setup.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        setup.extend_from_slice(&ilay);
        setup.resize(setup.len() + (align4(ilay.len()) - ilay.len()), 0);
        end_cmd(&mut setup, start);

        // Patch stream size in header.
        let total_size = setup.len() as u32;
        setup[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
            .copy_from_slice(&total_size.to_le_bytes());

        let guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&setup, None, &guest_mem)
            .expect("setup cmd stream should succeed");
        exec.poll_wait();

        // Case 1: Depth clip enabled (default) -> triangle should be clipped away (z=2, w=1).
        let stream_enabled = build_draw_stream(RT, VB, VS, PS, IL, 0);
        exec.execute_cmd_stream(&stream_enabled, None, &guest_mem)
            .expect("draw (depth clip enabled) should succeed");
        exec.poll_wait();
        let pixels = exec
            .read_texture_rgba8(RT)
            .await
            .expect("readback should succeed");
        assert_all_pixels_eq(&pixels, [0, 0, 0, 255]);

        // Case 2: Depth clip disabled -> clamp z into 0..w and rasterize.
        let stream_disabled = build_draw_stream(
            RT,
            VB,
            VS,
            PS,
            IL,
            AEROGPU_RASTERIZER_FLAG_DEPTH_CLIP_DISABLE,
        );
        exec.execute_cmd_stream(&stream_disabled, None, &guest_mem)
            .expect("draw (depth clip disabled) should succeed");
        exec.poll_wait();
        let pixels = exec
            .read_texture_rgba8(RT)
            .await
            .expect("readback should succeed");
        assert_all_pixels_eq(&pixels, [255, 0, 0, 255]);
    });
}

fn build_draw_stream(
    rt: u32,
    vb: u32,
    vs: u32,
    ps: u32,
    il: u32,
    rasterizer_flags: u32,
) -> Vec<u8> {
    let mut stream = Vec::new();
    stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
    stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
    stream.extend_from_slice(&0u32.to_le_bytes()); // flags
    stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
    stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

    // BIND_SHADERS
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::BindShaders as u32);
    stream.extend_from_slice(&vs.to_le_bytes());
    stream.extend_from_slice(&ps.to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes()); // cs
    stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
    end_cmd(&mut stream, start);

    // SET_INPUT_LAYOUT
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetInputLayout as u32);
    stream.extend_from_slice(&il.to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
    end_cmd(&mut stream, start);

    // SET_RENDER_TARGETS
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetRenderTargets as u32);
    stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
    stream.extend_from_slice(&0u32.to_le_bytes()); // depth_stencil
    stream.extend_from_slice(&rt.to_le_bytes());
    for _ in 0..7 {
        stream.extend_from_slice(&0u32.to_le_bytes());
    }
    end_cmd(&mut stream, start);

    // SET_VIEWPORT
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetViewport as u32);
    stream.extend_from_slice(&0.0f32.to_bits().to_le_bytes()); // x
    stream.extend_from_slice(&0.0f32.to_bits().to_le_bytes()); // y
    stream.extend_from_slice(&4.0f32.to_bits().to_le_bytes()); // width
    stream.extend_from_slice(&4.0f32.to_bits().to_le_bytes()); // height
    stream.extend_from_slice(&0.0f32.to_bits().to_le_bytes()); // min_depth
    stream.extend_from_slice(&1.0f32.to_bits().to_le_bytes()); // max_depth
    end_cmd(&mut stream, start);

    // CLEAR (black)
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Clear as u32);
    stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
    stream.extend_from_slice(&0.0f32.to_bits().to_le_bytes()); // r
    stream.extend_from_slice(&0.0f32.to_bits().to_le_bytes()); // g
    stream.extend_from_slice(&0.0f32.to_bits().to_le_bytes()); // b
    stream.extend_from_slice(&1.0f32.to_bits().to_le_bytes()); // a
    stream.extend_from_slice(&1.0f32.to_bits().to_le_bytes()); // depth
    stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
    end_cmd(&mut stream, start);

    // SET_VERTEX_BUFFERS
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetVertexBuffers as u32);
    stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
    stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
    stream.extend_from_slice(&vb.to_le_bytes());
    stream.extend_from_slice(&(core::mem::size_of::<Vertex>() as u32).to_le_bytes()); // stride_bytes
    stream.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
    stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
    end_cmd(&mut stream, start);

    // SET_PRIMITIVE_TOPOLOGY
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetPrimitiveTopology as u32);
    stream.extend_from_slice(&(AerogpuPrimitiveTopology::TriangleList as u32).to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
    end_cmd(&mut stream, start);

    // SET_RASTERIZER_STATE (cull none, depth clip controlled by flags)
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetRasterizerState as u32);
    stream.extend_from_slice(&0u32.to_le_bytes()); // fill_mode (solid)
    stream.extend_from_slice(&0u32.to_le_bytes()); // cull_mode (none)
    stream.extend_from_slice(&0u32.to_le_bytes()); // front_ccw (false)
    stream.extend_from_slice(&0u32.to_le_bytes()); // scissor_enable (false)
    stream.extend_from_slice(&0i32.to_le_bytes()); // depth_bias
    stream.extend_from_slice(&rasterizer_flags.to_le_bytes());
    end_cmd(&mut stream, start);

    // DRAW
    let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Draw as u32);
    stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
    stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
    stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
    stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
    end_cmd(&mut stream, start);

    // Patch stream size in header.
    let total_size = stream.len() as u32;
    stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
        .copy_from_slice(&total_size.to_le_bytes());

    stream
}
