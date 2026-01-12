mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;

#[test]
fn new_for_tests_prefers_gl_backend_on_linux() {
    pollster::block_on(async {
        let exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(err) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({err:#})"));
                return;
            }
        };

        if cfg!(target_os = "linux") {
            assert_eq!(
                exec.backend(),
                wgpu::Backend::Gl,
                "expected new_for_tests() to select the GL backend on Linux"
            );
        }
    })
}
