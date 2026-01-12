mod common;

use aero_d3d11::runtime::aerogpu_execute::AerogpuCmdRuntime;
use aero_d3d11::runtime::aerogpu_state::{BlendState, PrimitiveTopology, RasterizerState, VertexBufferBinding};

const DXBC_VS_PASSTHROUGH_TEXCOORD: &[u8] = include_bytes!("fixtures/vs_passthrough_texcoord.dxbc");
const DXBC_PS_SAMPLE: &[u8] = include_bytes!("fixtures/ps_sample.dxbc");
const ILAY_POS3_TEX2: &[u8] = include_bytes!("fixtures/ilay_pos3_tex2.bin");

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct VertexPos3Tex2 {
    pos: [f32; 3],
    tex: [f32; 2],
}

#[test]
fn aerogpu_cmd_runtime_reuses_pipeline_layout_across_pipeline_misses() {
    pollster::block_on(async {
        let mut rt = match AerogpuCmdRuntime::new_for_tests().await {
            Ok(rt) => rt,
            Err(err) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({err:#})"));
                return;
            }
        };

        // Use a PS that declares reflection bindings (texture+sampler) so the PipelineLayoutKey is
        // non-empty. Then force two distinct render pipeline keys by toggling blending between
        // draws; the pipeline layout should still be reused via the PipelineLayoutKey cache.
        const VS: u32 = 1;
        const PS: u32 = 2;
        const IL: u32 = 3;
        const VB: u32 = 4;
        const TEX: u32 = 5;
        const RTEX: u32 = 6;

        rt.create_shader_dxbc(VS, DXBC_VS_PASSTHROUGH_TEXCOORD)
            .unwrap();
        rt.create_shader_dxbc(PS, DXBC_PS_SAMPLE).unwrap();
        rt.create_input_layout(IL, ILAY_POS3_TEX2).unwrap();

        let vertices: [VertexPos3Tex2; 3] = [
            VertexPos3Tex2 {
                pos: [-1.0, -1.0, 0.0],
                tex: [0.0, 0.0],
            },
            VertexPos3Tex2 {
                pos: [3.0, -1.0, 0.0],
                tex: [2.0, 0.0],
            },
            VertexPos3Tex2 {
                pos: [-1.0, 3.0, 0.0],
                tex: [0.0, 2.0],
            },
        ];
        rt.create_buffer(
            VB,
            std::mem::size_of_val(&vertices) as u64,
            wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        );
        rt.write_buffer(VB, 0, bytemuck::bytes_of(&vertices))
            .unwrap();

        // Minimal 1x1 texture so the PS has a real binding (though the cache behavior is keyed by
        // reflection, not by bound resources).
        rt.create_texture2d(
            TEX,
            1,
            1,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        );
        rt.write_texture_rgba8(TEX, 1, 1, 4, &[255, 0, 0, 255]).unwrap();
        rt.set_ps_texture(0, Some(TEX));

        rt.create_texture2d(
            RTEX,
            1,
            1,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );

        let mut colors = [None; 8];
        colors[0] = Some(RTEX);
        rt.set_render_targets(&colors, None);

        rt.bind_shaders(Some(VS), Some(PS));
        rt.set_input_layout(Some(IL));
        rt.set_vertex_buffers(
            0,
            &[VertexBufferBinding {
                buffer: VB,
                stride: std::mem::size_of::<VertexPos3Tex2>() as u32,
                offset: 0,
            }],
        );
        rt.set_primitive_topology(PrimitiveTopology::TriangleList);
        rt.set_rasterizer_state(RasterizerState {
            cull_mode: None,
            front_face: wgpu::FrontFace::Ccw,
            scissor_enable: false,
        });

        // Draw #1: blending disabled (default).
        rt.set_blend_state(BlendState::default());
        rt.draw(3, 1, 0, 0).unwrap();

        // Draw #2: blending enabled (but factors preserve output).
        rt.set_blend_state(BlendState {
            blend: Some(wgpu::BlendState {
                color: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::One,
                    dst_factor: wgpu::BlendFactor::Zero,
                    operation: wgpu::BlendOperation::Add,
                },
                alpha: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::One,
                    dst_factor: wgpu::BlendFactor::Zero,
                    operation: wgpu::BlendOperation::Add,
                },
            }),
            write_mask: wgpu::ColorWrites::ALL,
        });
        rt.draw(3, 1, 0, 0).unwrap();

        rt.poll_wait();

        let pipeline_stats = rt.pipeline_cache_stats();
        assert_eq!(pipeline_stats.render_pipeline_misses, 2);
        assert_eq!(pipeline_stats.render_pipeline_hits, 0);

        let layout_stats = rt.pipeline_layout_cache_stats();
        assert_eq!(layout_stats.misses, 1);
        assert_eq!(layout_stats.hits, 1);
        assert_eq!(layout_stats.entries, 1);
    });
}

