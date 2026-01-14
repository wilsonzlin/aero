mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuIndexFormat, AerogpuPrimitiveTopology, AerogpuShaderStage, AerogpuVertexBufferBinding,
    AEROGPU_CLEAR_COLOR, AEROGPU_RESOURCE_USAGE_INDEX_BUFFER, AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
    AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
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

/// Smoke test for the placeholder compute prepass pipeline's storage-buffer budget handling.
///
/// This forces the executor down the compute-prepass path (adjacency topology), while using
/// vertex pulling + index pulling bindings. On backends with very low
/// `max_storage_buffers_per_shader_stage` limits, this should now fail with a skip-safe error
/// (rather than a wgpu validation panic).
#[test]
fn aerogpu_cmd_geometry_shader_compute_prepass_storage_budget_gate() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::aerogpu_cmd_geometry_shader_compute_prepass_storage_budget_gate"
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

        const RT: u32 = 1;
        const VB: u32 = 2;
        const IB: u32 = 3;
        const VS: u32 = 4;
        const PS: u32 = 5;
        const IL: u32 = 6;

        // LineListAdj primitive count is element_count / 4; use a single primitive.
        let vertices = [
            VertexPos3Color4 {
                pos: [-1.0, -1.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            VertexPos3Color4 {
                pos: [-1.0, 1.0, 0.0],
                color: [0.0, 1.0, 0.0, 1.0],
            },
            VertexPos3Color4 {
                pos: [1.0, -1.0, 0.0],
                color: [0.0, 0.0, 1.0, 1.0],
            },
            VertexPos3Color4 {
                pos: [1.0, 1.0, 0.0],
                color: [1.0, 1.0, 1.0, 1.0],
            },
        ];
        let indices: [u32; 4] = [0, 1, 2, 3];

        let mut writer = AerogpuCmdWriter::new();

        writer.create_texture2d(
            RT,
            AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
            AerogpuFormat::R8G8B8A8Unorm as u32,
            8,
            8,
            1,
            1,
            0,
            0,
            0,
        );
        writer.set_render_targets(&[RT], 0);
        writer.clear(AEROGPU_CLEAR_COLOR, [0.0, 0.0, 0.0, 1.0], 1.0, 0);

        writer.create_buffer(
            VB,
            AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
            core::mem::size_of_val(&vertices) as u64,
            0,
            0,
        );
        writer.upload_resource(VB, 0, bytemuck::cast_slice(&vertices));

        writer.create_buffer(
            IB,
            AEROGPU_RESOURCE_USAGE_INDEX_BUFFER,
            core::mem::size_of_val(&indices) as u64,
            0,
            0,
        );
        writer.upload_resource(IB, 0, bytemuck::cast_slice(&indices));

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
        writer.set_index_buffer(IB, AerogpuIndexFormat::Uint32, 0);

        writer.bind_shaders(VS, PS, 0);
        // Adjacency topology forces the compute-prepass path while still allowing vertex pulling.
        writer.set_primitive_topology(AerogpuPrimitiveTopology::LineListAdj);
        writer.draw_indexed(4, 1, 0, 0, 0);

        let stream = writer.finish();
        let mut guest_mem = VecGuestMemory::new(0);
        if let Err(err) = exec.execute_cmd_stream(&stream, None, &mut guest_mem) {
            if common::skip_if_compute_or_indirect_unsupported(test_name, &err) {
                return;
            }
            panic!("execute_cmd_stream failed: {err:#}");
        }
        exec.poll_wait();

        let pixels = exec
            .read_texture_rgba8(RT)
            .await
            .expect("readback should succeed");
        for px in pixels.chunks_exact(4) {
            assert_eq!(px, &[255, 0, 0, 255]);
        }
    });
}
