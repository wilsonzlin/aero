mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_d3d11::runtime::bindings::{BoundTexture, ShaderStage};
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::AerogpuShaderStageEx;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

#[test]
fn aerogpu_cmd_stage_ex_compute_is_accepted_for_binding_packets() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        let mut writer = AerogpuCmdWriter::new();
        // Exercise the stage_ex ABI encoding for compute (`reserved0 = AerogpuShaderStageEx::Compute = 5`).
        writer.set_texture_ex(AerogpuShaderStageEx::Compute, 0, 0xCAFE_BABE);

        let stream = writer.finish();
        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("command stream should execute (stage_ex compute must be accepted)");

        assert_eq!(
            exec.binding_state().stage(ShaderStage::Compute).texture(0),
            Some(BoundTexture {
                texture: 0xCAFE_BABE
            })
        );
    });
}
