mod common;

use aero_d3d11::runtime::aerogpu_resources::AerogpuResourceManager;
use aero_protocol::aerogpu::aerogpu_cmd::AerogpuShaderStage;

const DXBC_GS_EMIT_STREAM1: &[u8] = include_bytes!("fixtures/gs_emit_stream1.dxbc");

#[test]
fn aerogpu_resources_rejects_nonzero_emit_stream_index_for_ignored_geometry_shader() {
    pollster::block_on(async {
        let (device, queue, _supports_compute) = match common::wgpu::create_device_queue(
            "aero-d3d11 aerogpu_resources gs stream policy test device",
        )
        .await
        {
            Ok(v) => v,
            Err(err) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({err:#})"));
                return;
            }
        };

        let mut mgr = AerogpuResourceManager::new(device, queue);
        let err = mgr
            .create_shader_dxbc(
                1,
                AerogpuShaderStage::Geometry as u32,
                DXBC_GS_EMIT_STREAM1,
            )
            .expect_err("expected CreateShaderDxbc to reject non-zero stream index");
        let msg = err.to_string();
        assert!(
            msg.contains("emit_stream") && msg.contains("stream") && msg.contains("1"),
            "unexpected error: {err:#}"
        );
    });
}

