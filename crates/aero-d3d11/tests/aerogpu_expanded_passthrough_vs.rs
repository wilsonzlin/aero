mod common;

use aero_d3d11::runtime::aerogpu_execute::AerogpuCmdRuntime;
use aero_d3d11::runtime::aerogpu_state::{PrimitiveTopology, RasterizerState};

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ExpandedVertexPosColor {
    pos: [f32; 4],
    color: [f32; 4],
}

const PS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/ps_passthrough.dxbc");

#[test]
fn expanded_draw_uses_generated_passthrough_vs() {
    pollster::block_on(async {
        let mut rt = match AerogpuCmdRuntime::new_for_tests().await {
            Ok(rt) => rt,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const PS: u32 = 1;
        const EXPANDED_VB: u32 = 2;
        const RTEX: u32 = 3;

        rt.create_shader_dxbc(PS, PS_PASSTHROUGH).unwrap();

        // Fullscreen triangle in clip space (CW winding).
        let verts: [ExpandedVertexPosColor; 3] = [
            ExpandedVertexPosColor {
                pos: [-1.0, -1.0, 0.0, 1.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            ExpandedVertexPosColor {
                pos: [-1.0, 3.0, 0.0, 1.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            ExpandedVertexPosColor {
                pos: [3.0, -1.0, 0.0, 1.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
        ];

        rt.create_buffer(
            EXPANDED_VB,
            std::mem::size_of_val(&verts) as u64,
            wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        );
        rt.write_buffer(EXPANDED_VB, 0, bytemuck::bytes_of(&verts))
            .unwrap();

        rt.create_texture2d(
            RTEX,
            16,
            16,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );

        let mut colors = [None; 8];
        colors[0] = Some(RTEX);
        rt.set_render_targets(&colors, None);
        rt.bind_shaders(None, None, Some(PS));
        rt.set_primitive_topology(PrimitiveTopology::TriangleList);
        rt.set_rasterizer_state(RasterizerState {
            cull_mode: None,
            front_face: wgpu::FrontFace::Ccw,
            scissor_enable: false,
        });

        // Draw twice: the second draw should hit the pipeline cache (and reuse the generated VS).
        rt.draw_expanded_passthrough(EXPANDED_VB, 3, 1, 0, 0)
            .unwrap();
        rt.draw_expanded_passthrough(EXPANDED_VB, 3, 1, 0, 0)
            .unwrap();

        rt.poll_wait();

        let stats = rt.pipeline_cache_stats();
        assert_eq!(
            stats.render_pipeline_misses, 1,
            "expected one pipeline miss"
        );
        assert_eq!(stats.render_pipeline_hits, 1, "expected one pipeline hit");

        let pixels = rt.read_texture_rgba8(RTEX).await.unwrap();
        assert_eq!(pixels.len(), (16 * 16 * 4) as usize);
        for (i, px) in pixels.chunks_exact(4).enumerate() {
            assert_eq!(px, &[255, 0, 0, 255], "pixel {i}");
        }
    });
}
