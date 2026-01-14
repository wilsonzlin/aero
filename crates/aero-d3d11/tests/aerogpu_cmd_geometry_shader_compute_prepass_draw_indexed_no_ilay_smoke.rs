mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuIndexFormat, AerogpuShaderStage, AEROGPU_CLEAR_COLOR,
    AEROGPU_RESOURCE_USAGE_INDEX_BUFFER, AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

const VS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/vs_passthrough.dxbc");
const PS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/ps_passthrough.dxbc");

#[test]
fn aerogpu_cmd_geometry_shader_compute_prepass_draw_indexed_no_ilay_smoke() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::aerogpu_cmd_geometry_shader_compute_prepass_draw_indexed_no_ilay_smoke"
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
        const IB: u32 = 2;
        const VS: u32 = 3;
        const PS: u32 = 4;

        // The placeholder compute prepass shader writes a fixed triangle and does not consume the
        // index buffer yet, but `DRAW_INDEXED` still requires one to be bound. This test ensures we
        // can bind an index buffer *without* an input layout and still execute the prepass path
        // (important for future VS-as-compute work where shaders use only SV_VertexID).
        let indices: [u32; 3] = [0, 1, 2];

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

        writer.create_buffer(
            IB,
            AEROGPU_RESOURCE_USAGE_INDEX_BUFFER,
            core::mem::size_of_val(&indices) as u64,
            0,
            0,
        );
        writer.upload_resource(IB, 0, bytemuck::cast_slice(&indices));
        writer.set_index_buffer(IB, AerogpuIndexFormat::Uint32, 0);

        writer.create_shader_dxbc(VS, AerogpuShaderStage::Vertex, VS_PASSTHROUGH);
        writer.create_shader_dxbc(PS, AerogpuShaderStage::Pixel, PS_PASSTHROUGH);

        // Bind a dummy GS handle to force the compute-prepass path; no actual GS shader is needed
        // for this smoke test.
        writer.bind_shaders_ex(VS, PS, 0, 0xCAFE_BABE, 0, 0);
        writer.draw_indexed(3, 1, 0, 0, 0);

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
