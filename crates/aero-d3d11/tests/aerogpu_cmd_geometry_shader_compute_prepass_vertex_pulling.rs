mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::guest_memory::VecGuestMemory;
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

/// Force the executor down the GS/HS/DS compute-prepass path while still using a real input layout
/// + vertex buffers.
///
/// This exercises the vertex pulling bind-group plumbing used by the eventual VS-as-compute
/// implementation.
#[test]
fn aerogpu_cmd_geometry_shader_compute_prepass_vertex_pulling_smoke() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::aerogpu_cmd_geometry_shader_compute_prepass_vertex_pulling_smoke"
        );
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(test_name, &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        if !common::require_gs_prepass_or_skip(&exec, test_name) {
            return;
        }

        let max_storage_buffers = exec.device().limits().max_storage_buffers_per_shader_stage;
        // The placeholder vertex-pulling prepass binds:
        // - 4 storage buffers for emulation outputs (expanded verts/indices/indirect/counter)
        // - 1+ storage buffers for IA vertex pulling
        if max_storage_buffers < 5 {
            common::skip_or_panic(
                test_name,
                &format!(
                    "requires >=5 storage buffers per shader stage for vertex pulling (got {max_storage_buffers})"
                ),
            );
            return;
        }

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

        // Force the compute-prepass path by using an adjacency topology (not supported by WebGPU
        // render pipelines). Request a single primitive so the placeholder prepass emits its
        // fullscreen triangle.
        writer.set_primitive_topology(AerogpuPrimitiveTopology::LineListAdj);
        writer.bind_shaders(VS, PS, 0);
        writer.draw(4, 1, 0, 0);

        let stream = writer.finish();
        let mut guest_mem = VecGuestMemory::new(0);
        let report = match exec.execute_cmd_stream(&stream, None, &mut guest_mem) {
            Ok(report) => report,
            Err(err) => {
                // Regression check: this test runs with `Limits::downlevel_defaults()` which includes
                // `max_storage_buffers_per_shader_stage = 4`. The placeholder compute prepass must be
                // able to run vertex pulling without exceeding that limit.
                let msg = err.to_string();
                if msg.contains("max_storage_buffers_per_shader_stage")
                    || msg.contains("Too many StorageBuffers")
                    || msg.contains("too many storage buffers")
                {
                    panic!("compute-prepass pipeline exceeded storage buffer limit: {err:#}");
                }
                if common::skip_if_compute_or_indirect_unsupported(test_name, &err) {
                    return;
                }
                panic!("execute_cmd_stream failed: {err:#}");
            }
        };
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
    });
}
