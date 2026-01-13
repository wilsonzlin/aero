mod common;

use aero_d3d11::binding_model::EXPANDED_VERTEX_MAX_VARYINGS;
use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuPrimitiveTopology, AerogpuShaderStage, AEROGPU_CLEAR_COLOR,
    AEROGPU_RESOURCE_USAGE_RENDER_TARGET, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

const VS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/vs_passthrough.dxbc");
const PS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/ps_passthrough.dxbc");

fn push_f32(out: &mut Vec<u8>, v: f32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_vec4(out: &mut Vec<u8>, v: [f32; 4]) {
    for f in v {
        push_f32(out, f);
    }
}

fn push_expanded_vertex(out: &mut Vec<u8>, pos: [f32; 4], color_loc1: [f32; 4]) {
    // Matches `runtime/wgsl_link.rs` `ExpandedVertex`:
    //   pos: vec4<f32>
    //   varyings: array<vec4<f32>, 32>
    push_vec4(out, pos);
    for loc in 0..EXPANDED_VERTEX_MAX_VARYINGS {
        let v = if loc == 1 { color_loc1 } else { [0.0; 4] };
        push_vec4(out, v);
    }
}

#[test]
fn aerogpu_cmd_can_render_from_expanded_vertex_buffer_via_passthrough_vs() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const EXPANDED_VB: u32 = 1;
        const RT: u32 = 2;
        const VS: u32 = 3;
        const PS: u32 = 4;

        let mut expanded = Vec::new();
        // Fullscreen triangle in clip-space.
        push_expanded_vertex(
            &mut expanded,
            [-1.0, -1.0, 0.0, 1.0],
            [1.0, 0.0, 0.0, 1.0],
        );
        push_expanded_vertex(
            &mut expanded,
            [-1.0, 3.0, 0.0, 1.0],
            [1.0, 0.0, 0.0, 1.0],
        );
        push_expanded_vertex(
            &mut expanded,
            [3.0, -1.0, 0.0, 1.0],
            [1.0, 0.0, 0.0, 1.0],
        );

        let w = 16u32;
        let h = 16u32;

        let mut writer = AerogpuCmdWriter::new();
        writer.create_buffer(
            EXPANDED_VB,
            AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
            expanded.len() as u64,
            0,
            0,
        );
        writer.upload_resource(EXPANDED_VB, 0, &expanded);

        writer.create_texture2d(
            RT,
            AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
            AerogpuFormat::B8G8R8A8Unorm as u32,
            w,
            h,
            1,
            1,
            0,
            0,
            0,
        );
        writer.set_render_targets(&[RT], 0);
        writer.set_viewport(0.0, 0.0, w as f32, h as f32, 0.0, 1.0);

        writer.create_shader_dxbc(VS, AerogpuShaderStage::Vertex, VS_PASSTHROUGH);
        writer.create_shader_dxbc(PS, AerogpuShaderStage::Pixel, PS_PASSTHROUGH);
        writer.bind_shaders(VS, PS, 0);
        writer.set_primitive_topology(AerogpuPrimitiveTopology::TriangleList);

        let stream1 = writer.finish();
        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream1, None, &mut guest_mem)
            .expect("initial setup stream should succeed");

        // Enable the emulated vertex-pulling path and point it at our expanded vertex buffer.
        exec.set_emulated_expanded_vertex_buffer(Some(EXPANDED_VB));

        let mut writer = AerogpuCmdWriter::new();
        writer.clear(AEROGPU_CLEAR_COLOR, [0.0, 0.0, 0.0, 1.0], 1.0, 0);
        writer.draw(3, 1, 0, 0);
        writer.present(0, 0);
        let stream2 = writer.finish();

        let report = exec
            .execute_cmd_stream(&stream2, None, &mut guest_mem)
            .expect("draw stream should succeed");
        exec.poll_wait();

        let render_target = report
            .presents
            .last()
            .and_then(|p| p.presented_render_target)
            .expect("stream should present a render target");
        let pixels = exec
            .read_texture_rgba8(render_target)
            .await
            .expect("readback should succeed");
        assert_eq!(pixels.len(), (w * h * 4) as usize);
        for (i, px) in pixels.chunks_exact(4).enumerate() {
            assert_eq!(px, &[255, 0, 0, 255], "pixel {i}");
        }
    });
}

