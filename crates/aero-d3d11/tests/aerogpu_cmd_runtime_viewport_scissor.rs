mod common;

use aero_d3d11::runtime::aerogpu_execute::AerogpuCmdRuntime;
use aero_d3d11::runtime::aerogpu_state::{
    PrimitiveTopology, RasterizerState, ScissorRect, VertexBufferBinding, Viewport,
};

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    pos: [f32; 3],
    color: [f32; 4],
}

const DXBC_VS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/vs_passthrough.dxbc");
const DXBC_PS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/ps_passthrough.dxbc");
const ILAY_POS3_COLOR: &[u8] = include_bytes!("fixtures/ilay_pos3_color.bin");

#[test]
fn aerogpu_cmd_runtime_clamps_viewport_to_render_target() {
    pollster::block_on(async {
        let mut rt = match AerogpuCmdRuntime::new_for_tests().await {
            Ok(rt) => rt,
            Err(err) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({err:#})"));
                return;
            }
        };

        const VS: u32 = 1;
        const PS: u32 = 2;
        const IL: u32 = 3;
        const VB: u32 = 4;
        const RTEX: u32 = 5;

        rt.create_shader_dxbc(VS, DXBC_VS_PASSTHROUGH).unwrap();
        rt.create_shader_dxbc(PS, DXBC_PS_PASSTHROUGH).unwrap();
        rt.create_input_layout(IL, ILAY_POS3_COLOR).unwrap();

        let vertices: [Vertex; 3] = [
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

        rt.create_buffer(
            VB,
            std::mem::size_of_val(&vertices) as u64,
            wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        );
        rt.write_buffer(VB, 0, bytemuck::bytes_of(&vertices))
            .unwrap();

        rt.create_texture2d(
            RTEX,
            4,
            4,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );

        let mut colors = [None; 8];
        colors[0] = Some(RTEX);
        rt.set_render_targets(&colors, None);

        rt.bind_shaders(Some(VS), None, Some(PS));
        rt.set_input_layout(Some(IL));
        rt.set_vertex_buffers(
            0,
            &[VertexBufferBinding {
                buffer: VB,
                stride: std::mem::size_of::<Vertex>() as u32,
                offset: 0,
            }],
        );
        rt.set_primitive_topology(PrimitiveTopology::TriangleList);
        rt.set_rasterizer_state(RasterizerState {
            cull_mode: None,
            front_face: wgpu::FrontFace::Ccw,
            scissor_enable: false,
        });

        // Viewport extends outside the render target; it should be clamped to (0,0)-(2,2).
        rt.set_viewport(Some(Viewport {
            x: -2.0,
            y: -2.0,
            width: 4.0,
            height: 4.0,
            min_depth: 0.0,
            max_depth: 1.0,
        }));

        rt.draw(3, 1, 0, 0).unwrap();
        rt.poll_wait();

        let pixels = rt.read_texture_rgba8(RTEX).await.unwrap();
        assert_eq!(pixels.len(), 4 * 4 * 4);
        for y in 0..4usize {
            for x in 0..4usize {
                let idx = (y * 4 + x) * 4;
                let px = &pixels[idx..idx + 4];
                if x < 2 && y < 2 {
                    assert_eq!(px, &[255, 0, 0, 255], "pixel ({x},{y})");
                } else {
                    assert_eq!(px, &[0, 0, 0, 255], "pixel ({x},{y})");
                }
            }
        }
    });
}

#[test]
fn aerogpu_cmd_runtime_viewport_out_of_bounds_draws_nothing() {
    pollster::block_on(async {
        let mut rt = match AerogpuCmdRuntime::new_for_tests().await {
            Ok(rt) => rt,
            Err(err) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({err:#})"));
                return;
            }
        };

        const VS: u32 = 1;
        const PS: u32 = 2;
        const IL: u32 = 3;
        const VB: u32 = 4;
        const RTEX: u32 = 5;

        rt.create_shader_dxbc(VS, DXBC_VS_PASSTHROUGH).unwrap();
        rt.create_shader_dxbc(PS, DXBC_PS_PASSTHROUGH).unwrap();
        rt.create_input_layout(IL, ILAY_POS3_COLOR).unwrap();

        let vertices: [Vertex; 3] = [
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

        rt.create_buffer(
            VB,
            std::mem::size_of_val(&vertices) as u64,
            wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        );
        rt.write_buffer(VB, 0, bytemuck::bytes_of(&vertices))
            .unwrap();

        rt.create_texture2d(
            RTEX,
            4,
            4,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );

        let mut colors = [None; 8];
        colors[0] = Some(RTEX);
        rt.set_render_targets(&colors, None);

        rt.bind_shaders(Some(VS), None, Some(PS));
        rt.set_input_layout(Some(IL));
        rt.set_vertex_buffers(
            0,
            &[VertexBufferBinding {
                buffer: VB,
                stride: std::mem::size_of::<Vertex>() as u32,
                offset: 0,
            }],
        );
        rt.set_primitive_topology(PrimitiveTopology::TriangleList);
        rt.set_rasterizer_state(RasterizerState {
            cull_mode: None,
            front_face: wgpu::FrontFace::Ccw,
            scissor_enable: false,
        });

        // Viewport lies completely outside the render target -> no pixels should be drawn.
        rt.set_viewport(Some(Viewport {
            x: 10.0,
            y: 0.0,
            width: 1.0,
            height: 4.0,
            min_depth: 0.0,
            max_depth: 1.0,
        }));

        rt.draw(3, 1, 0, 0).unwrap();
        rt.poll_wait();

        let pixels = rt.read_texture_rgba8(RTEX).await.unwrap();
        assert_eq!(pixels.len(), 4 * 4 * 4);
        for (i, px) in pixels.chunks_exact(4).enumerate() {
            assert_eq!(px, &[0, 0, 0, 255], "pixel index {i}");
        }
    });
}

#[test]
fn aerogpu_cmd_runtime_clamps_scissor_to_render_target() {
    pollster::block_on(async {
        let mut rt = match AerogpuCmdRuntime::new_for_tests().await {
            Ok(rt) => rt,
            Err(err) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({err:#})"));
                return;
            }
        };

        const VS: u32 = 1;
        const PS: u32 = 2;
        const IL: u32 = 3;
        const VB: u32 = 4;
        const RTEX: u32 = 5;

        rt.create_shader_dxbc(VS, DXBC_VS_PASSTHROUGH).unwrap();
        rt.create_shader_dxbc(PS, DXBC_PS_PASSTHROUGH).unwrap();
        rt.create_input_layout(IL, ILAY_POS3_COLOR).unwrap();

        let vertices: [Vertex; 3] = [
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

        rt.create_buffer(
            VB,
            std::mem::size_of_val(&vertices) as u64,
            wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        );
        rt.write_buffer(VB, 0, bytemuck::bytes_of(&vertices))
            .unwrap();

        rt.create_texture2d(
            RTEX,
            4,
            4,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );

        let mut colors = [None; 8];
        colors[0] = Some(RTEX);
        rt.set_render_targets(&colors, None);

        rt.bind_shaders(Some(VS), None, Some(PS));
        rt.set_input_layout(Some(IL));
        rt.set_vertex_buffers(
            0,
            &[VertexBufferBinding {
                buffer: VB,
                stride: std::mem::size_of::<Vertex>() as u32,
                offset: 0,
            }],
        );
        rt.set_primitive_topology(PrimitiveTopology::TriangleList);
        rt.set_rasterizer_state(RasterizerState {
            cull_mode: None,
            front_face: wgpu::FrontFace::Ccw,
            scissor_enable: true,
        });

        // Scissor extends outside the render target; it should be clamped to a 1x1 rect at (3,3).
        rt.set_scissor(Some(ScissorRect {
            x: 3,
            y: 3,
            width: 4,
            height: 4,
        }));

        rt.draw(3, 1, 0, 0).unwrap();
        rt.poll_wait();

        let pixels = rt.read_texture_rgba8(RTEX).await.unwrap();
        assert_eq!(pixels.len(), 4 * 4 * 4);
        for y in 0..4usize {
            for x in 0..4usize {
                let idx = (y * 4 + x) * 4;
                let px = &pixels[idx..idx + 4];
                if x == 3 && y == 3 {
                    assert_eq!(px, &[255, 0, 0, 255], "pixel ({x},{y})");
                } else {
                    assert_eq!(px, &[0, 0, 0, 255], "pixel ({x},{y})");
                }
            }
        }
    });
}

#[test]
fn aerogpu_cmd_runtime_scissor_out_of_bounds_draws_nothing() {
    pollster::block_on(async {
        let mut rt = match AerogpuCmdRuntime::new_for_tests().await {
            Ok(rt) => rt,
            Err(err) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({err:#})"));
                return;
            }
        };

        const VS: u32 = 1;
        const PS: u32 = 2;
        const IL: u32 = 3;
        const VB: u32 = 4;
        const RTEX: u32 = 5;

        rt.create_shader_dxbc(VS, DXBC_VS_PASSTHROUGH).unwrap();
        rt.create_shader_dxbc(PS, DXBC_PS_PASSTHROUGH).unwrap();
        rt.create_input_layout(IL, ILAY_POS3_COLOR).unwrap();

        let vertices: [Vertex; 3] = [
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

        rt.create_buffer(
            VB,
            std::mem::size_of_val(&vertices) as u64,
            wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        );
        rt.write_buffer(VB, 0, bytemuck::bytes_of(&vertices))
            .unwrap();

        rt.create_texture2d(
            RTEX,
            4,
            4,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );

        let mut colors = [None; 8];
        colors[0] = Some(RTEX);
        rt.set_render_targets(&colors, None);

        rt.bind_shaders(Some(VS), None, Some(PS));
        rt.set_input_layout(Some(IL));
        rt.set_vertex_buffers(
            0,
            &[VertexBufferBinding {
                buffer: VB,
                stride: std::mem::size_of::<Vertex>() as u32,
                offset: 0,
            }],
        );
        rt.set_primitive_topology(PrimitiveTopology::TriangleList);
        rt.set_rasterizer_state(RasterizerState {
            cull_mode: None,
            front_face: wgpu::FrontFace::Ccw,
            scissor_enable: true,
        });

        // Scissor rect lies completely outside the render target -> draw should be a no-op.
        rt.set_scissor(Some(ScissorRect {
            x: 10,
            y: 0,
            width: 1,
            height: 1,
        }));

        rt.draw(3, 1, 0, 0).unwrap();
        rt.poll_wait();

        let pixels = rt.read_texture_rgba8(RTEX).await.unwrap();
        assert_eq!(pixels.len(), 4 * 4 * 4);
        for (i, px) in pixels.chunks_exact(4).enumerate() {
            assert_eq!(px, &[0, 0, 0, 255], "pixel index {i}");
        }
    });
}
