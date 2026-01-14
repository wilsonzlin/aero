mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{AerogpuCmdDispatch, AerogpuCmdStreamHeader};
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

#[test]
fn aerogpu_cmd_dispatch_stage_ex_vertex_value_is_rejected() {
    pollster::block_on(async {
        const TEST_NAME: &str = concat!(
            module_path!(),
            "::aerogpu_cmd_dispatch_stage_ex_vertex_value_is_rejected"
        );

        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(TEST_NAME, &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        if !exec.capabilities().supports_compute {
            common::skip_or_panic(TEST_NAME, "compute unsupported");
            return;
        }

        let mut writer = AerogpuCmdWriter::new();
        writer.dispatch(1, 1, 1);
        let mut stream = writer.finish();

        // `stage_ex == 1` is the DXBC program type for Vertex, but it is intentionally invalid in
        // the AeroGPU stage_ex encoding (0 is reserved for legacy/default compute; Vertex must be
        // encoded via the legacy shader_stage value).
        let dispatch_reserved0_offset = AerogpuCmdStreamHeader::SIZE_BYTES
            + core::mem::offset_of!(AerogpuCmdDispatch, reserved0);
        stream[dispatch_reserved0_offset..dispatch_reserved0_offset + 4]
            .copy_from_slice(&1u32.to_le_bytes());

        let mut guest_mem = VecGuestMemory::new(0);
        let err = exec
            .execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect_err("stage_ex=1 must be rejected for DISPATCH");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("stage_ex=1"),
            "error should mention invalid stage_ex=1, got: {msg}"
        );
    });
}
