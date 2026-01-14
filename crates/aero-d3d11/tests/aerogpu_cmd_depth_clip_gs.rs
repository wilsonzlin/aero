mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCullMode, AerogpuFillMode, AerogpuPrimitiveTopology, AerogpuShaderStage,
    AerogpuVertexBufferBinding, AEROGPU_CLEAR_COLOR, AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
    AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

const DXBC_VS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/vs_passthrough.dxbc");
const DXBC_PS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/ps_passthrough.dxbc");

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct GsVertex {
    // Clip-space position (post-VS/GS).
    pos: [f32; 4],
    // User varying payload (post-VS/GS).
    color: [f32; 4],
}

fn assert_all_pixels_eq(pixels: &[u8], expected: [u8; 4]) {
    assert_eq!(pixels.len() % 4, 0, "pixel buffer must be RGBA8");
    for (i, px) in pixels.chunks_exact(4).enumerate() {
        assert_eq!(px, expected, "pixel mismatch at index {i}");
    }
}

#[test]
fn aerogpu_cmd_depth_clip_toggle_clamps_z_when_disabled_with_gs_emulation() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const RT: u32 = 1;
        const VB: u32 = 2;
        const VS: u32 = 10;
        const PS: u32 = 11;

        // Fullscreen triangle in clip-space, but with z outside D3D's legal (0..w) depth range.
        let verts = [
            GsVertex {
                pos: [-1.0, -1.0, 2.0, 1.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            GsVertex {
                pos: [3.0, -1.0, 2.0, 1.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            GsVertex {
                pos: [-1.0, 3.0, 2.0, 1.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
        ];
        let vb_bytes = bytemuck::bytes_of(&verts);

        let mut setup = AerogpuCmdWriter::new();
        setup.create_buffer(
            VB,
            AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
            vb_bytes.len() as u64,
            0,
            0,
        );
        setup.upload_resource(VB, 0, vb_bytes);

        setup.create_texture2d(
            RT,
            AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
            AerogpuFormat::R8G8B8A8Unorm as u32,
            4,
            4,
            1,
            1,
            0,
            0,
            0,
        );

        setup.create_shader_dxbc(VS, AerogpuShaderStage::Vertex, DXBC_VS_PASSTHROUGH);
        setup.create_shader_dxbc(PS, AerogpuShaderStage::Pixel, DXBC_PS_PASSTHROUGH);

        let setup_stream = setup.finish();
        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&setup_stream, None, &mut guest_mem)
            .expect("setup cmd stream should succeed");
        exec.poll_wait();

        let draw_once = |depth_clip_disable: bool| -> Vec<u8> {
            let mut writer = AerogpuCmdWriter::new();
            // Bind a non-zero CS handle to activate GS emulation in the executor. (There is no
            // explicit GS stage in the command stream yet.)
            writer.bind_shaders(VS, PS, 999);
            writer.set_render_targets(&[RT], 0);
            writer.set_viewport(0.0, 0.0, 4.0, 4.0, 0.0, 1.0);
            writer.clear(AEROGPU_CLEAR_COLOR, [0.0, 0.0, 0.0, 1.0], 1.0, 0);
            writer.set_vertex_buffers(
                0,
                &[AerogpuVertexBufferBinding {
                    buffer: VB,
                    stride_bytes: core::mem::size_of::<GsVertex>() as u32,
                    offset_bytes: 0,
                    reserved0: 0,
                }],
            );
            writer.set_primitive_topology(AerogpuPrimitiveTopology::TriangleList);
            writer.set_rasterizer_state_ext(
                AerogpuFillMode::Solid,
                AerogpuCullMode::None,
                false,
                false,
                0,
                depth_clip_disable,
            );
            writer.draw(3, 1, 0, 0);
            writer.finish()
        };

        // Depth clip enabled (default): triangle should be clipped away.
        let stream_enabled = draw_once(false);
        if let Err(err) = exec.execute_cmd_stream(&stream_enabled, None, &mut guest_mem) {
            if common::skip_if_compute_or_indirect_unsupported(module_path!(), &err) {
                return;
            }
            panic!("execute_cmd_stream failed: {err:#}");
        }
        exec.poll_wait();
        let pixels = exec
            .read_texture_rgba8(RT)
            .await
            .expect("readback should succeed");
        assert_all_pixels_eq(&pixels, [0, 0, 0, 255]);

        // Depth clip disabled: emulate DepthClipEnable=FALSE by clamping z in the post-GS VS.
        let stream_disabled = draw_once(true);
        exec.execute_cmd_stream(&stream_disabled, None, &mut guest_mem)
            .expect("draw (depth clip disabled) should succeed");
        exec.poll_wait();
        let pixels = exec
            .read_texture_rgba8(RT)
            .await
            .expect("readback should succeed");
        assert_all_pixels_eq(&pixels, [255, 0, 0, 255]);
    });
}
