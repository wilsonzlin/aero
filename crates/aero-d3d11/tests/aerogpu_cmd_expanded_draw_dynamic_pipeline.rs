mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuIndexFormat, AerogpuShaderStage, AEROGPU_CLEAR_COLOR, AEROGPU_RESOURCE_USAGE_INDEX_BUFFER,
    AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

const VS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/vs_passthrough.dxbc");
const PS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/ps_passthrough.dxbc");

#[test]
fn aerogpu_cmd_expanded_draw_dynamic_pipeline() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const RT: u32 = 1;
        const DUMMY_IB: u32 = 2;
        const VS: u32 = 3;
        const PS: u32 = 4;

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

        // Bind a dummy index buffer so `DRAW_INDEXED` is accepted (the expanded path generates and
        // uses its own index buffer internally).
        writer.create_buffer(DUMMY_IB, AEROGPU_RESOURCE_USAGE_INDEX_BUFFER, 16, 0, 0);
        writer.set_index_buffer(DUMMY_IB, AerogpuIndexFormat::Uint16, 0);

        writer.create_shader_dxbc(VS, AerogpuShaderStage::Vertex, VS_PASSTHROUGH);
        writer.create_shader_dxbc(PS, AerogpuShaderStage::Pixel, PS_PASSTHROUGH);

        // Force the compute-prepass expanded-vertex path by binding a dummy GS handle.
        writer.bind_shaders_with_gs(VS, 0xCAFE_BABE, PS, 0);

        // The executor replaces this with `draw_indexed_indirect` using the generated indirect args
        // and index buffer.
        writer.draw_indexed(3, 1, 0, 0, 0);

        let stream = writer.finish();

        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("execute_cmd_stream should succeed");
        exec.poll_wait();

        let (width, height) = exec.texture_size(RT).expect("render target should exist");
        assert_eq!((width, height), (8, 8));

        let pixels = exec
            .read_texture_rgba8(RT)
            .await
            .expect("readback should succeed");
        let idx = ((height as usize / 2) * width as usize + (width as usize / 2)) * 4;
        let center = &pixels[idx..idx + 4];
        assert_eq!(center, &[255, 0, 0, 255]);
    });
}

