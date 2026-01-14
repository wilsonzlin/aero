mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuShaderResourceBufferBinding, AerogpuShaderStage, AerogpuUnorderedAccessBufferBinding,
    AEROGPU_RESOURCE_USAGE_STORAGE,
};
use aero_protocol::aerogpu::aerogpu_ring::AerogpuAllocEntry;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

const CS_STORE_UAV_RAW_DXBC: &[u8] = include_bytes!("fixtures/cs_store_uav_raw.dxbc");
const CS_COPY_RAW_SRV_TO_UAV_DXBC: &[u8] = include_bytes!("fixtures/cs_copy_raw_srv_to_uav.dxbc");
const CS_COPY_STRUCTURED_SRV_TO_UAV_DXBC: &[u8] =
    include_bytes!("fixtures/cs_copy_structured_srv_to_uav.dxbc");

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

#[test]
fn aerogpu_cmd_dispatch_copy_raw_srv_to_uav_writeback_smoke() {
    pollster::block_on(async {
        const TEST_NAME: &str = concat!(
            module_path!(),
            "::aerogpu_cmd_dispatch_copy_raw_srv_to_uav_writeback_smoke"
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

        // Include values whose raw bits correspond to non-negative integer floats (e.g. 1.0). The
        // translator must preserve raw bits across `ld_raw`/`store_raw` copies, not reinterpret them
        // as numeric integers.
        let src_words: [u32; 4] = [
            0x3f80_0000, // 1.0f32
            0x4000_0000, // 2.0f32
            0x4040_0000, // 3.0f32
            0x0000_0001, // u32=1 (tiny subnormal if interpreted as f32)
        ];
        let src_bytes: &[u8] = bytemuck::cast_slice(&src_words);

        const SRV: u32 = 1;
        const UAV: u32 = 2;
        const READBACK: u32 = 3;
        const CS: u32 = 4;
        const ALLOC_ID: u32 = 1;

        let alloc = AerogpuAllocEntry {
            alloc_id: ALLOC_ID,
            flags: 0,
            gpa: 0,
            size_bytes: src_bytes.len() as u64,
            reserved0: 0,
        };
        let allocs = [alloc];
        let mut guest_mem = VecGuestMemory::new(src_bytes.len());

        let mut writer = AerogpuCmdWriter::new();
        writer.create_buffer(
            SRV,
            AEROGPU_RESOURCE_USAGE_STORAGE,
            src_bytes.len() as u64,
            0,
            0,
        );
        writer.create_buffer(
            UAV,
            AEROGPU_RESOURCE_USAGE_STORAGE,
            src_bytes.len() as u64,
            0,
            0,
        );
        writer.create_buffer(READBACK, 0, src_bytes.len() as u64, ALLOC_ID, 0);
        writer.upload_resource(SRV, 0, src_bytes);
        writer.upload_resource(UAV, 0, &vec![0u8; src_bytes.len()]);

        writer.create_shader_dxbc(CS, AerogpuShaderStage::Compute, CS_COPY_RAW_SRV_TO_UAV_DXBC);
        writer.bind_shaders(0, 0, CS);
        writer.set_shader_resource_buffers(
            AerogpuShaderStage::Compute,
            0,
            &[AerogpuShaderResourceBufferBinding {
                buffer: SRV,
                offset_bytes: 0,
                size_bytes: 0,
                reserved0: 0,
            }],
        );
        writer.set_unordered_access_buffers(
            AerogpuShaderStage::Compute,
            0,
            &[AerogpuUnorderedAccessBufferBinding {
                buffer: UAV,
                offset_bytes: 0,
                size_bytes: 0,
                initial_count: 0,
            }],
        );
        writer.dispatch(1, 1, 1);
        writer.copy_buffer_writeback_dst(READBACK, UAV, 0, 0, src_bytes.len() as u64);
        let stream = writer.finish();

        exec.execute_cmd_stream_async(&stream, Some(&allocs), &mut guest_mem)
            .await
            .expect("execute_cmd_stream_async should succeed");
        exec.poll_wait();

        assert_eq!(
            guest_mem.as_slice(),
            src_bytes,
            "expected UAV buffer to match SRV buffer after compute copy"
        );
    });
}

#[test]
fn aerogpu_cmd_dispatch_copy_structured_srv_to_uav_writeback_smoke() {
    pollster::block_on(async {
        const TEST_NAME: &str = concat!(
            module_path!(),
            "::aerogpu_cmd_dispatch_copy_structured_srv_to_uav_writeback_smoke"
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

        // Two 16-byte elements (8 u32s). We'll read element 1 and write it into element 0.
        let src_words: [u32; 8] = [
            0,
            1,
            2,
            3,           // element 0
            0x3f80_0000, // 1.0f32
            0x4000_0000, // 2.0f32
            0x4040_0000, // 3.0f32
            0x4080_0000, // 4.0f32
        ];
        let expected_words: [u32; 8] = [
            0x3f80_0000,
            0x4000_0000,
            0x4040_0000,
            0x4080_0000, // element 0 overwritten with element 1
            0,
            0,
            0,
            0, // element 1 untouched (was initialized to 0)
        ];
        let src_bytes: &[u8] = bytemuck::cast_slice(&src_words);
        let expected_bytes: &[u8] = bytemuck::cast_slice(&expected_words);

        const SRV: u32 = 1;
        const UAV: u32 = 2;
        const READBACK: u32 = 3;
        const CS: u32 = 4;
        const ALLOC_ID: u32 = 1;

        let alloc = AerogpuAllocEntry {
            alloc_id: ALLOC_ID,
            flags: 0,
            gpa: 0,
            size_bytes: src_bytes.len() as u64,
            reserved0: 0,
        };
        let allocs = [alloc];
        let mut guest_mem = VecGuestMemory::new(src_bytes.len());

        let mut writer = AerogpuCmdWriter::new();
        writer.create_buffer(
            SRV,
            AEROGPU_RESOURCE_USAGE_STORAGE,
            src_bytes.len() as u64,
            0,
            0,
        );
        writer.create_buffer(
            UAV,
            AEROGPU_RESOURCE_USAGE_STORAGE,
            src_bytes.len() as u64,
            0,
            0,
        );
        writer.create_buffer(READBACK, 0, src_bytes.len() as u64, ALLOC_ID, 0);
        writer.upload_resource(SRV, 0, src_bytes);
        writer.upload_resource(UAV, 0, &vec![0u8; src_bytes.len()]);

        writer.create_shader_dxbc(
            CS,
            AerogpuShaderStage::Compute,
            CS_COPY_STRUCTURED_SRV_TO_UAV_DXBC,
        );
        writer.bind_shaders(0, 0, CS);
        writer.set_shader_resource_buffers(
            AerogpuShaderStage::Compute,
            0,
            &[AerogpuShaderResourceBufferBinding {
                buffer: SRV,
                offset_bytes: 0,
                size_bytes: 0,
                reserved0: 0,
            }],
        );
        writer.set_unordered_access_buffers(
            AerogpuShaderStage::Compute,
            0,
            &[AerogpuUnorderedAccessBufferBinding {
                buffer: UAV,
                offset_bytes: 0,
                size_bytes: 0,
                initial_count: 0,
            }],
        );
        writer.dispatch(1, 1, 1);
        writer.copy_buffer_writeback_dst(READBACK, UAV, 0, 0, src_bytes.len() as u64);
        let stream = writer.finish();

        exec.execute_cmd_stream_async(&stream, Some(&allocs), &mut guest_mem)
            .await
            .expect("execute_cmd_stream_async should succeed");
        exec.poll_wait();

        assert_eq!(
            guest_mem.as_slice(),
            expected_bytes,
            "expected structured UAV buffer write to match fixture behavior"
        );
    });
}
