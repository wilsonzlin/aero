mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCullMode, AerogpuFillMode, AerogpuPrimitiveTopology, AerogpuShaderStage,
    AEROGPU_CLEAR_COLOR, AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

const VS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/vs_passthrough.dxbc");
const PS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/ps_passthrough.dxbc");

/// Same as `aerogpu_cmd_geometry_shader_compute_prepass_forced_by_adjacency_topology_legacy_bind_shaders`,
/// but emits the append-only `BIND_SHADERS` ABI extension (`bind_shaders_ex`) to ensure extended
/// shader-binding packets interoperate correctly with topology-forced emulation.
#[test]
fn aerogpu_cmd_geometry_shader_compute_prepass_forced_by_adjacency_topology_bind_shaders_ex() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::aerogpu_cmd_geometry_shader_compute_prepass_forced_by_adjacency_topology_bind_shaders_ex"
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
        writer.clear(AEROGPU_CLEAR_COLOR, [0.0, 0.0, 0.0, 1.0], 1.0, 0);

        writer.create_shader_dxbc(VS, AerogpuShaderStage::Vertex, VS_PASSTHROUGH);
        writer.create_shader_dxbc(PS, AerogpuShaderStage::Pixel, PS_PASSTHROUGH);

        // Make the test independent of triangle winding conventions.
        writer.set_rasterizer_state_ext(
            AerogpuFillMode::Solid,
            AerogpuCullMode::None,
            /*front_counter_clockwise=*/ false,
            /*depth_clip_disable=*/ false,
            /*depth_bias=*/ 0,
            /*multisample=*/ false,
        );

        // Force the compute-prepass path via an adjacency topology (D3D11-only). A line-list-adj
        // primitive consumes 4 vertices. Use `bind_shaders_ex` even though `{gs,hs,ds}=0` to ensure
        // the extended encoding does not interfere with topology-based emulation.
        writer.bind_shaders_ex(VS, PS, 0, 0, 0, 0);
        writer.set_primitive_topology(AerogpuPrimitiveTopology::LineListAdj);
        writer.draw(4, 1, 0, 0);

        let stream = writer.finish();
        let mut guest_mem = VecGuestMemory::new(0);
        if let Err(err) = exec.execute_cmd_stream(&stream, None, &mut guest_mem) {
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
        let idx = ((8usize / 2) * 8usize + (8usize / 2)) * 4;
        let center = &pixels[idx..idx + 4];
        assert_eq!(center, &[255, 0, 0, 255]);
    });
}
