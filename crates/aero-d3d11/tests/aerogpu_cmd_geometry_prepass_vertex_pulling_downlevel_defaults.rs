mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_gpu::GpuCapabilities;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuPrimitiveTopology, AerogpuShaderStage, AerogpuVertexBufferBinding, AEROGPU_CLEAR_COLOR,
    AEROGPU_RESOURCE_USAGE_RENDER_TARGET, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

const VS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/vs_passthrough.dxbc");
const PS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/ps_passthrough.dxbc");
const ILAY_POS3_COLOR: &[u8] = include_bytes!("fixtures/ilay_pos3_color.bin");

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct VertexPos3Color4 {
    pos: [f32; 3],
    color: [f32; 4],
}

/// Regression test for PREPASS-STORAGE-BUDGET-011:
///
/// - Create a device with `wgpu::Limits::downlevel_defaults()` (notably
///   `max_storage_buffers_per_shader_stage = 4`).
/// - Force the executor down the placeholder GS/HS/DS compute-prepass path while vertex pulling is
///   enabled (input layout + vertex buffers bound).
/// - Assert the executor does not hit a wgpu validation panic. The implementation may either fall
///   back to a reduced-binding placeholder variant or return a normal `Err` with clear
///   diagnostics.
#[test]
fn aerogpu_cmd_geometry_prepass_vertex_pulling_downlevel_defaults_no_panic() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::aerogpu_cmd_geometry_prepass_vertex_pulling_downlevel_defaults_no_panic"
        );

        let (device, queue, downlevel, backend) =
            match common::wgpu::create_device_queue_with_downlevel_backend(
                "aero-d3d11 geometry prepass downlevel device",
            )
            .await
            {
                Ok(v) => v,
                Err(err) => {
                    common::skip_or_panic(test_name, &format!("wgpu unavailable ({err:#})"));
                    return;
                }
            };

        let supports_compute = downlevel
            .flags
            .contains(wgpu::DownlevelFlags::COMPUTE_SHADERS);
        let supports_indirect = downlevel
            .flags
            .contains(wgpu::DownlevelFlags::INDIRECT_EXECUTION);
        if !supports_compute {
            common::skip_or_panic(test_name, "compute unsupported");
            return;
        }
        if !supports_indirect {
            common::skip_or_panic(test_name, "indirect unsupported");
            return;
        }

        let max_storage_buffers = device.limits().max_storage_buffers_per_shader_stage;
        assert!(
            max_storage_buffers <= wgpu::Limits::downlevel_defaults().max_storage_buffers_per_shader_stage,
            "expected downlevel-default storage buffer limits (got max_storage_buffers_per_shader_stage={max_storage_buffers})"
        );

        let caps = GpuCapabilities::from_device(&device).with_downlevel_flags(downlevel.flags);
        let mut exec =
            AerogpuD3d11Executor::new_with_caps(device, queue, backend, caps, supports_indirect);

        const VB: u32 = 1;
        const RT: u32 = 2;
        const VS: u32 = 3;
        const PS: u32 = 4;
        const IL: u32 = 6;

        let verts = [
            VertexPos3Color4 {
                pos: [-1.0, -1.0, 0.0],
                color: [1.0, 1.0, 1.0, 1.0],
            },
            VertexPos3Color4 {
                pos: [-1.0, 1.0, 0.0],
                color: [1.0, 1.0, 1.0, 1.0],
            },
            VertexPos3Color4 {
                pos: [1.0, -1.0, 0.0],
                color: [1.0, 1.0, 1.0, 1.0],
            },
        ];

        let mut writer = AerogpuCmdWriter::new();
        writer.create_buffer(
            VB,
            AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
            core::mem::size_of_val(&verts) as u64,
            0,
            0,
        );
        writer.upload_resource(VB, 0, bytemuck::cast_slice(&verts));

        let (w, h) = (8u32, 8u32);
        writer.create_texture2d(
            RT,
            AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
            AerogpuFormat::R8G8B8A8Unorm as u32,
            w,
            h,
            1,
            1,
            0,
            0,
            0,
        );
        writer.set_render_targets(&[RT], 0);
        writer.clear(AEROGPU_CLEAR_COLOR, [0.0, 0.0, 0.0, 1.0], 1.0, 0);

        writer.create_shader_dxbc(VS, AerogpuShaderStage::Vertex, VS_PASSTHROUGH);
        writer.create_shader_dxbc(PS, AerogpuShaderStage::Pixel, PS_PASSTHROUGH);

        writer.create_input_layout(IL, ILAY_POS3_COLOR);
        writer.set_input_layout(IL);
        writer.set_vertex_buffers(
            0,
            &[AerogpuVertexBufferBinding {
                buffer: VB,
                stride_bytes: core::mem::size_of::<VertexPos3Color4>() as u32,
                offset_bytes: 0,
                reserved0: 0,
            }],
        );

        // Force the placeholder compute-prepass path with an adjacency topology (unsupported by
        // WebGPU render pipelines), while still having vertex pulling enabled via the input layout.
        writer.set_primitive_topology(AerogpuPrimitiveTopology::LineListAdj);
        writer.bind_shaders(VS, PS, 0);
        writer.draw(4, 1, 0, 0);

        let stream = writer.finish();
        let mut guest_mem = VecGuestMemory::new(0);
        match exec.execute_cmd_stream(&stream, None, &mut guest_mem) {
            Ok(report) => {
                exec.poll_wait();
                assert!(
                    report.presents.is_empty(),
                    "this test does not use PRESENT and should not report any presents"
                );

                let pixels = exec
                    .read_texture_rgba8(RT)
                    .await
                    .expect("readback should succeed");
                for px in pixels.chunks_exact(4) {
                    assert_eq!(px, &[255, 0, 0, 255]);
                }
            }
            Err(err) => {
                // The important regression is "no panic". If the implementation cannot fall back
                // it should return a normal error with clear per-stage storage buffer diagnostics.
                let msg = err.to_string();
                assert!(
                    msg.contains("max_storage_buffers_per_shader_stage"),
                    "expected a storage-buffer limit error, got: {err:#}"
                );
            }
        }
    });
}
