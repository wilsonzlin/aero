mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCullMode, AerogpuFillMode, AerogpuPrimitiveTopology, AerogpuShaderStage,
    AEROGPU_CLEAR_COLOR, AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

const DXBC_VS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/vs_passthrough.dxbc");
const DXBC_PS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/ps_passthrough.dxbc");

fn assert_all_pixels_eq(pixels: &[u8], expected: [u8; 4]) {
    assert_eq!(pixels.len() % 4, 0, "pixel buffer must be RGBA8");
    for (i, px) in pixels.chunks_exact(4).enumerate() {
        assert_eq!(px, expected, "pixel mismatch at index {i}");
    }
}

#[test]
fn aerogpu_cmd_depth_clip_toggle_clamps_z_when_disabled_with_gs_emulation() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::aerogpu_cmd_depth_clip_toggle_clamps_z_when_disabled_with_gs_emulation"
        );
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(test_name, &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        if !common::require_gs_prepass_or_skip(&exec, test_name) {
            return;
        }

        const RT: u32 = 1;
        const VS: u32 = 10;
        const PS: u32 = 11;
        const GS: u32 = 0xCAFE_BABE;

        let mut setup = AerogpuCmdWriter::new();
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
            // The compute-prepass path uses a placeholder compute shader that reads z from the
            // compute-stage legacy constants buffer (register 0). Set it to `z=2` so the generated
            // triangle is outside D3D's legal clip-space depth range (0..w) and therefore only
            // renders when depth clip is disabled (z is clamped in the passthrough VS).
            writer.set_shader_constants_f(AerogpuShaderStage::Compute, 0, &[2.0, 0.0, 0.0, 0.0]);

            writer.bind_shaders_with_gs(VS, GS, PS, 0);
            writer.set_render_targets(&[RT], 0);
            writer.set_viewport(0.0, 0.0, 4.0, 4.0, 0.0, 1.0);
            writer.clear(AEROGPU_CLEAR_COLOR, [0.0, 0.0, 0.0, 1.0], 1.0, 0);
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
            if common::skip_if_compute_or_indirect_unsupported(test_name, &err) {
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
