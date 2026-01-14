mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuPrimitiveTopology, AerogpuShaderStage, AEROGPU_CLEAR_COLOR,
    AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

const VS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/vs_passthrough.dxbc");
const PS_PRIMITIVE_ID: &[u8] = include_bytes!("fixtures/ps_primitive_id.dxbc");

#[test]
fn aerogpu_cmd_sv_primitive_id_compute_prepass_colors_primitives() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const RT: u32 = 1;
        const VS: u32 = 2;
        const PS: u32 = 3;

        let mut writer = AerogpuCmdWriter::new();
        writer.create_texture2d(
            RT,
            AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
            AerogpuFormat::R8G8B8A8Unorm as u32,
            8,
            8,
            1,
            1,
            0,
            0,
            0,
        );
        writer.set_render_targets(&[RT], 0);
        writer.clear(AEROGPU_CLEAR_COLOR, [0.0, 0.0, 1.0, 1.0], 1.0, 0);

        writer.create_shader_dxbc(VS, AerogpuShaderStage::Vertex, VS_PASSTHROUGH);
        writer.create_shader_dxbc(PS, AerogpuShaderStage::Pixel, PS_PRIMITIVE_ID);

        writer.set_primitive_topology(AerogpuPrimitiveTopology::TriangleList);

        // Bind a non-zero GS handle to force the GS/HS/DS compute-prepass path.
        writer.bind_shaders_with_gs(VS, 0xCAFE_BABE, PS, 0);
        // Request 2 primitives (triangle list with 6 vertices). The compute prepass expands each
        // primitive into a triangle, and the pixel shader uses SV_PrimitiveID to output:
        // - primitive 0: black
        // - primitive 1: red
        writer.draw(6, 1, 0, 0);

        let stream = writer.finish();
        let mut guest_mem = VecGuestMemory::new(0);
        if let Err(err) = exec.execute_cmd_stream(&stream, None, &mut guest_mem) {
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

        let px = |x: usize, y: usize| -> [u8; 4] {
            let idx = (y * 8 + x) * 4;
            pixels[idx..idx + 4].try_into().unwrap()
        };

        // Primitive 0 covers the left half and should be black.
        assert_eq!(px(8 / 4, 8 / 2), [0, 0, 0, 255]);
        // Primitive 1 covers the right half and should be red.
        assert_eq!(px(8 * 3 / 4, 8 / 2), [255, 0, 0, 255]);
    });
}
