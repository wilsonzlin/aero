mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCompareFunc, AerogpuPrimitiveTopology, AerogpuShaderStage, AerogpuVertexBufferBinding,
    AEROGPU_CLEAR_COLOR, AEROGPU_CLEAR_DEPTH, AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL,
    AEROGPU_RESOURCE_USAGE_RENDER_TARGET, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

const DXBC_VS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/vs_passthrough.dxbc");
const DXBC_PS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/ps_passthrough.dxbc");
const ILAY_POS3_COLOR: &[u8] = include_bytes!("fixtures/ilay_pos3_color.bin");

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    pos: [f32; 3],
    color: [f32; 4],
}

#[test]
fn aerogpu_cmd_depth_only_with_all_null_rtv_slots_writes_depth() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const VB: u32 = 1;
        const C: u32 = 2;
        const D: u32 = 3;
        const VS: u32 = 10;
        const PS: u32 = 11;
        const IL: u32 = 20;

        // Fullscreen triangle (two copies at different depths):
        // - Pass 1: z = 0.5 (depth-only)
        // - Pass 2: z = 0.6, red output, depth test LESS (should be rejected)
        let vertices = [
            // Pass 1
            Vertex {
                pos: [-1.0, -1.0, 0.5],
                color: [1.0, 1.0, 1.0, 1.0],
            },
            Vertex {
                pos: [-1.0, 3.0, 0.5],
                color: [1.0, 1.0, 1.0, 1.0],
            },
            Vertex {
                pos: [3.0, -1.0, 0.5],
                color: [1.0, 1.0, 1.0, 1.0],
            },
            // Pass 2
            Vertex {
                pos: [-1.0, -1.0, 0.6],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [-1.0, 3.0, 0.6],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [3.0, -1.0, 0.6],
                color: [1.0, 0.0, 0.0, 1.0],
            },
        ];
        let vb_bytes = bytemuck::bytes_of(&vertices);

        let mut writer = AerogpuCmdWriter::new();
        writer.create_buffer(
            VB,
            AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
            vb_bytes.len() as u64,
            0,
            0,
        );
        writer.upload_resource(VB, 0, vb_bytes);

        writer.create_texture2d(
            C,
            AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
            AerogpuFormat::R8G8B8A8Unorm as u32,
            4,
            4,
            1,
            1,
            0,
            0,
            0,
        );
        writer.create_texture2d(
            D,
            AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL,
            AerogpuFormat::D32Float as u32,
            4,
            4,
            1,
            1,
            0,
            0,
            0,
        );

        writer.create_shader_dxbc(VS, AerogpuShaderStage::Vertex, DXBC_VS_PASSTHROUGH);
        writer.create_shader_dxbc(PS, AerogpuShaderStage::Pixel, DXBC_PS_PASSTHROUGH);
        writer.bind_shaders(VS, PS, 0);

        writer.create_input_layout(IL, ILAY_POS3_COLOR);
        writer.set_input_layout(IL);
        writer.set_vertex_buffers(
            0,
            &[AerogpuVertexBufferBinding {
                buffer: VB,
                stride_bytes: core::mem::size_of::<Vertex>() as u32,
                offset_bytes: 0,
                reserved0: 0,
            }],
        );
        writer.set_primitive_topology(AerogpuPrimitiveTopology::TriangleList);
        writer.set_viewport(0.0, 0.0, 4.0, 4.0, 0.0, 1.0);
        writer.set_depth_stencil_state(true, true, AerogpuCompareFunc::Less, false, 0, 0);

        // Pass 1: depth-only, but expressed as 8 RTV slots all set to NULL.
        let null_rtvs = [0u32; 8];
        writer.set_render_targets(&null_rtvs, D);
        writer.clear(AEROGPU_CLEAR_DEPTH, [0.0, 0.0, 0.0, 0.0], 1.0, 0);
        writer.draw(3, 1, 0, 0);

        // Pass 2: bind color+depth, clear color to black, draw red at z=0.6 (should fail depth test).
        writer.set_render_targets(&[C], D);
        writer.clear(AEROGPU_CLEAR_COLOR, [0.0, 0.0, 0.0, 1.0], 1.0, 0);
        writer.draw(3, 1, 3, 0);

        writer.present(0, 0);

        let stream = writer.finish();
        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let pixels = exec
            .read_texture_rgba8(C)
            .await
            .expect("readback should succeed");
        for px in pixels.chunks_exact(4) {
            assert_eq!(
                px,
                &[0, 0, 0, 255],
                "color draw should be rejected by depth"
            );
        }
    });
}
