mod common;

use aero_d3d11::runtime::aerogpu_execute::AerogpuCmdRuntime;
use aero_d3d11::runtime::aerogpu_state::{
    PrimitiveTopology, RasterizerState, VertexBufferBinding, Viewport,
};

const DXBC_VS_PASSTHROUGH_TEXCOORD: &[u8] = include_bytes!("fixtures/vs_passthrough_texcoord.dxbc");
const DXBC_PS_SAMPLE: &[u8] = include_bytes!("fixtures/ps_sample.dxbc");
const ILAY_POS3_TEX2: &[u8] = include_bytes!("fixtures/ilay_pos3_tex2.bin");

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct VertexPos3Tex2 {
    pos: [f32; 3],
    uv: [f32; 2],
}

#[test]
fn aerogpu_cmd_runtime_bind_group_cache_does_not_reuse_stale_texture_on_handle_reuse() {
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
        const TEX: u32 = 5;
        const RTEX: u32 = 6;

        rt.create_shader_dxbc(VS, DXBC_VS_PASSTHROUGH_TEXCOORD)
            .unwrap();
        rt.create_shader_dxbc(PS, DXBC_PS_SAMPLE).unwrap();
        rt.create_input_layout(IL, ILAY_POS3_TEX2).unwrap();

        let vertices: [VertexPos3Tex2; 3] = [
            VertexPos3Tex2 {
                pos: [-1.0, -1.0, 0.0],
                uv: [0.0, 0.0],
            },
            VertexPos3Tex2 {
                pos: [3.0, -1.0, 0.0],
                uv: [0.0, 0.0],
            },
            VertexPos3Tex2 {
                pos: [-1.0, 3.0, 0.0],
                uv: [0.0, 0.0],
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
            1,
            1,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );

        rt.create_texture2d(
            TEX,
            1,
            1,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        );

        let red: [u8; 4] = [255, 0, 0, 255];
        rt.write_texture_rgba8(TEX, 1, 1, 4, &red).unwrap();

        let mut colors = [None; 8];
        colors[0] = Some(RTEX);
        rt.set_render_targets(&colors, None);

        rt.bind_shaders(Some(VS), None, Some(PS));
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
        rt.set_viewport(Some(Viewport {
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
            min_depth: 0.0,
            max_depth: 1.0,
        }));

        rt.set_ps_texture(0, Some(TEX));
        rt.draw(3, 1, 0, 0).unwrap();
        rt.poll_wait();

        let pixels1 = rt.read_texture_rgba8(RTEX).await.unwrap();
        assert_eq!(pixels1, vec![255, 0, 0, 255]);

        // Recreate the texture using the same handle, then draw again. If bind groups are cached
        // by handle rather than by a per-resource unique ID, the cached bind group can continue to
        // reference the old texture view and sample the stale pixel.
        rt.create_texture2d(
            TEX,
            1,
            1,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        );
        let blue: [u8; 4] = [0, 0, 255, 255];
        rt.write_texture_rgba8(TEX, 1, 1, 4, &blue).unwrap();

        rt.set_ps_texture(0, Some(TEX));
        rt.draw(3, 1, 0, 0).unwrap();
        rt.poll_wait();

        let pixels2 = rt.read_texture_rgba8(RTEX).await.unwrap();
        assert_eq!(pixels2, vec![0, 0, 255, 255]);
    });
}
