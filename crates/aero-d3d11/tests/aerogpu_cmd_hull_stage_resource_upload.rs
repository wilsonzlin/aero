mod common;

use aero_d3d11::binding_model::BINDING_BASE_CBUFFER;
use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_d3d11::runtime::bindings::ShaderStage;
use aero_d3d11::{Binding, BindingKind};
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuConstantBufferBinding, AerogpuShaderStageEx, AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_ring::AerogpuAllocEntry;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

#[test]
fn aerogpu_cmd_hull_stage_group3_upload_and_scratch_use_hull_bucket() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const ALLOC_ID: u32 = 1;
        const ALLOC_GPA: u64 = 0x100;
        const CB_HANDLE: u32 = 2;
        const CB_SIZE: u64 = 64;

        let mut guest_mem = VecGuestMemory::new(0x2000);
        let init_bytes: Vec<u8> = (0..CB_SIZE as u8).collect();
        guest_mem.write(ALLOC_GPA, &init_bytes).unwrap();

        let allocs = [AerogpuAllocEntry {
            alloc_id: ALLOC_ID,
            flags: 0,
            gpa: ALLOC_GPA,
            size_bytes: CB_SIZE,
            reserved0: 0,
        }];

        // Create a guest-backed constant buffer and bind it to the hull-stage bucket (stage_ex).
        // Use an unaligned offset so the executor must perform a scratch copy.
        let mut writer = AerogpuCmdWriter::new();
        writer.create_buffer(
            CB_HANDLE,
            AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER,
            CB_SIZE,
            ALLOC_ID,
            0,
        );
        writer.set_constant_buffers_ex(
            AerogpuShaderStageEx::Hull,
            1, // start_slot
            &[AerogpuConstantBufferBinding {
                buffer: CB_HANDLE,
                offset_bytes: 4, // not aligned to min_uniform_buffer_offset_alignment
                size_bytes: 16,
                reserved0: 0,
            }],
        );

        let stream = writer.finish();
        exec.execute_cmd_stream(&stream, Some(&allocs), &mut guest_mem)
            .expect("command stream should execute");

        // Reset debug counters so we only observe the explicit ensure/upload path below.
        let _ = exec.take_resource_upload_debug_stats();

        // Simulate a hull-stage compute pipeline that declares its resources in @group(3)
        // (per the stage_ex binding model).
        let group_bindings = vec![
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec![Binding {
                group: 3,
                binding: BINDING_BASE_CBUFFER + 1,
                visibility: wgpu::ShaderStages::COMPUTE,
                kind: BindingKind::ConstantBuffer {
                    slot: 1,
                    reg_count: 1,
                },
            }],
        ];

        exec.ensure_group_bindings_resources_uploaded_for_tests(
            &group_bindings,
            &[(3, ShaderStage::Hull)],
            Some(&allocs),
            &mut guest_mem,
        )
        .expect("resource upload should succeed");

        let stats = exec.take_resource_upload_debug_stats();
        assert_eq!(
            stats.geometry_stage_bucket_lookups, 0,
            "resource upload must not consult the Geometry stage bucket for a hull-stage pipeline"
        );
        assert_eq!(
            stats.hull_stage_bucket_lookups, 1,
            "resource upload must consult the Hull stage bucket exactly once"
        );
        assert_eq!(
            stats.implicit_buffer_uploads, 1,
            "hull constant buffer must be implicitly uploaded from guest memory"
        );
        assert_eq!(
            stats.constant_buffer_scratch_copies, 1,
            "hull constant buffer with unaligned offset must trigger a scratch copy"
        );
    });
}

#[test]
fn aerogpu_cmd_domain_stage_group3_upload_and_scratch_use_domain_bucket() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const ALLOC_ID: u32 = 1;
        const ALLOC_GPA: u64 = 0x100;
        const CB_HANDLE: u32 = 2;
        const CB_SIZE: u64 = 64;

        let mut guest_mem = VecGuestMemory::new(0x2000);
        let init_bytes: Vec<u8> = (0..CB_SIZE as u8).collect();
        guest_mem.write(ALLOC_GPA, &init_bytes).unwrap();

        let allocs = [AerogpuAllocEntry {
            alloc_id: ALLOC_ID,
            flags: 0,
            gpa: ALLOC_GPA,
            size_bytes: CB_SIZE,
            reserved0: 0,
        }];

        // Create a guest-backed constant buffer and bind it to the domain-stage bucket (stage_ex).
        // Use an unaligned offset so the executor must perform a scratch copy.
        let mut writer = AerogpuCmdWriter::new();
        writer.create_buffer(
            CB_HANDLE,
            AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER,
            CB_SIZE,
            ALLOC_ID,
            0,
        );
        writer.set_constant_buffers_ex(
            AerogpuShaderStageEx::Domain,
            1, // start_slot
            &[AerogpuConstantBufferBinding {
                buffer: CB_HANDLE,
                offset_bytes: 4, // not aligned to min_uniform_buffer_offset_alignment
                size_bytes: 16,
                reserved0: 0,
            }],
        );

        let stream = writer.finish();
        exec.execute_cmd_stream(&stream, Some(&allocs), &mut guest_mem)
            .expect("command stream should execute");

        // Reset debug counters so we only observe the explicit ensure/upload path below.
        let _ = exec.take_resource_upload_debug_stats();

        // Simulate a domain-stage compute pipeline that declares its resources in @group(3)
        // (per the stage_ex binding model).
        let group_bindings = vec![
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec![Binding {
                group: 3,
                binding: BINDING_BASE_CBUFFER + 1,
                visibility: wgpu::ShaderStages::COMPUTE,
                kind: BindingKind::ConstantBuffer { slot: 1, reg_count: 1 },
            }],
        ];

        exec.ensure_group_bindings_resources_uploaded_for_tests(
            &group_bindings,
            &[(3, ShaderStage::Domain)],
            Some(&allocs),
            &mut guest_mem,
        )
        .expect("resource upload should succeed");

        let stats = exec.take_resource_upload_debug_stats();
        assert_eq!(
            stats.geometry_stage_bucket_lookups, 0,
            "resource upload must not consult the Geometry stage bucket for a domain-stage pipeline"
        );
        assert_eq!(
            stats.domain_stage_bucket_lookups, 1,
            "resource upload must consult the Domain stage bucket exactly once"
        );
        assert_eq!(
            stats.implicit_buffer_uploads, 1,
            "domain constant buffer must be implicitly uploaded from guest memory"
        );
        assert_eq!(
            stats.constant_buffer_scratch_copies, 1,
            "domain constant buffer with unaligned offset must trigger a scratch copy"
        );
    });
}
