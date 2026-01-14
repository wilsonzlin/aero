mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

#[test]
fn aerogpu_cmd_tracks_extended_bind_shaders_payload() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        let mut writer = AerogpuCmdWriter::new();
        writer.bind_shaders_ex(10, 20, 30, 40, 50, 60);
        let stream = writer.finish();

        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("execute_cmd_stream should succeed");

        let bound = exec.bound_shader_handles();
        assert_eq!(bound.vs, Some(10));
        assert_eq!(bound.ps, Some(20));
        assert_eq!(bound.cs, Some(30));
        assert_eq!(bound.gs, Some(40));
        assert_eq!(bound.hs, Some(50));
        assert_eq!(bound.ds, Some(60));
    });
}
