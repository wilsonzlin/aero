mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;

#[test]
fn new_for_tests_disables_texture_compression_on_gl_backend() {
    pollster::block_on(async {
        let exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(err) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({err:#})"));
                return;
            }
        };

        // `AerogpuD3d11Executor::new_for_tests` prefers the GL backend on Linux CI. wgpu's GL
        // backend has historically had shaky support for texture compression workflows, so the test
        // device is expected to have all compression features disabled.
        if cfg!(target_os = "linux") {
            let features = exec.device().features();
            assert!(
                !features.contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
                    && !features.contains(wgpu::Features::TEXTURE_COMPRESSION_ETC2)
                    && !features.contains(wgpu::Features::TEXTURE_COMPRESSION_ASTC_HDR),
                "expected texture compression features to be disabled on the GL test device (got {features:?})"
            );
        }
    })
}

