mod common;

use aero_d3d11::runtime::aerogpu_execute::AerogpuCmdRuntime;

const DXBC_GS_EMIT_STREAM1: &[u8] = include_bytes!("fixtures/gs_emit_stream1.dxbc");

#[test]
fn aerogpu_cmd_runtime_rejects_nonzero_emit_stream_index() {
    pollster::block_on(async {
        let mut rt = match AerogpuCmdRuntime::new_for_tests().await {
            Ok(rt) => rt,
            Err(err) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({err:#})"));
                return;
            }
        };

        let err = rt
            .create_shader_dxbc(1, DXBC_GS_EMIT_STREAM1)
            .expect_err("expected create_shader_dxbc to reject non-zero stream index");
        let msg = err.to_string();
        assert!(
            msg.contains("emit_stream") && msg.contains("stream") && msg.contains("1"),
            "unexpected error: {err:#}"
        );
    });
}

