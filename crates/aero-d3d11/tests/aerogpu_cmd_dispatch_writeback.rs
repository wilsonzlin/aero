mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuShaderStage, AerogpuUnorderedAccessBufferBinding, AEROGPU_RESOURCE_USAGE_STORAGE,
};
use aero_protocol::aerogpu::aerogpu_ring::AerogpuAllocEntry;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

const CS_STORE_UAV_RAW_DXBC: &[u8] = include_bytes!("fixtures/cs_store_uav_raw.dxbc");

#[test]
fn aerogpu_cmd_dispatch_writeback_smoke() {
    pollster::block_on(async {
        const TEST_NAME: &str = concat!(module_path!(), "::aerogpu_cmd_dispatch_writeback_smoke");

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

        const OUT: u32 = 1;
        const READBACK: u32 = 2;
        const CS: u32 = 3;
        const ALLOC_ID: u32 = 1;

        let alloc = AerogpuAllocEntry {
            alloc_id: ALLOC_ID,
            flags: 0,
            gpa: 0,
            size_bytes: 4,
            reserved0: 0,
        };
        let allocs = [alloc];

        let mut guest_mem = VecGuestMemory::new(alloc.size_bytes as usize);

        let mut writer = AerogpuCmdWriter::new();
        writer.create_buffer(OUT, AEROGPU_RESOURCE_USAGE_STORAGE, 4, 0, 0);
        writer.create_buffer(READBACK, 0, 4, ALLOC_ID, 0);
        writer.create_shader_dxbc(CS, AerogpuShaderStage::Compute, CS_STORE_UAV_RAW_DXBC);
        writer.bind_shaders(0, 0, CS);
        writer.set_unordered_access_buffers(
            AerogpuShaderStage::Compute,
            0,
            &[AerogpuUnorderedAccessBufferBinding {
                buffer: OUT,
                offset_bytes: 0,
                size_bytes: 0,
                initial_count: 0,
            }],
        );
        writer.dispatch(1, 1, 1);
        writer.copy_buffer_writeback_dst(READBACK, OUT, 0, 0, 4);
        let stream = writer.finish();

        exec.execute_cmd_stream_async(&stream, Some(&allocs), &mut guest_mem)
            .await
            .expect("execute_cmd_stream_async should succeed");
        exec.poll_wait();

        let got = u32::from_le_bytes(
            guest_mem.as_slice()[..4]
                .try_into()
                .expect("guest mem read 4 bytes"),
        );
        assert_eq!(got, 0x1234_5678);
    });
}
