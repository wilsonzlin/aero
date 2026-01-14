mod common;

use std::fs;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::AerogpuShaderStageEx;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

fn load_fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    fs::read(&path).unwrap_or_else(|e| panic!("failed to read {path}: {e}"))
}

#[test]
fn rejects_nonzero_emit_stream_index() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        // This fixture is a minimal SM5 geometry-shader token stream containing `emit_stream(1)`.
        let dxbc = load_fixture("gs_emit_stream1.dxbc");

        let mut writer = AerogpuCmdWriter::new();
        writer.create_shader_dxbc_ex(1, AerogpuShaderStageEx::Geometry, &dxbc);
        let stream = writer.finish();

        let mut guest_mem = VecGuestMemory::new(0);
        let err = exec
            .execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect_err("expected CREATE_SHADER_DXBC to reject non-zero stream index");
        let msg = err.to_string();
        assert!(
            msg.contains("emit_stream") && msg.contains("stream") && msg.contains("1"),
            "unexpected error: {err:#}"
        );
    });
}
