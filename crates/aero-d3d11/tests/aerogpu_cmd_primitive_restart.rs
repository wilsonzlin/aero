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
