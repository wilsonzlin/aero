mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCullMode, AerogpuFillMode, AerogpuPrimitiveTopology, AerogpuShaderStage,
    AerogpuShaderStageEx, AerogpuVertexBufferBinding, AEROGPU_CLEAR_COLOR,
    AEROGPU_RESOURCE_USAGE_RENDER_TARGET, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

// Validates that the translated GS compute prepass is used for triangle-list draws (not just
// point-list). The GS fixture is a passthrough triangle GS that only writes position (o0). With the
// translated prepass, the drawn triangle should land in the top-right corner and the pixel shader
// will observe the missing color varying (v1 = 0). If the placeholder prepass runs, the fullscreen
// placeholder triangle will fill the render target red.

const VS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/vs_passthrough.dxbc");
const GS_PASSTHROUGH_TRIANGLE: &[u8] = include_bytes!("fixtures/gs_passthrough.dxbc");
const PS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/ps_passthrough.dxbc");
const ILAY_POS3_COLOR: &[u8] = include_bytes!("fixtures/ilay_pos3_color.bin");

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct VertexPos3Color4 {
    pos: [f32; 3],
    color: [f32; 4],
}

#[test]
fn aerogpu_cmd_geometry_shader_trianglelist_smoke() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::aerogpu_cmd_geometry_shader_trianglelist_smoke"
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

        const VB: u32 = 1;
        const RT: u32 = 2;
        const VS: u32 = 3;
        const GS: u32 = 4;
        const PS: u32 = 5;
        const IL: u32 = 6;

        // A triangle in the top-right quadrant that should NOT cover the center pixel.
        let vertices = [
            VertexPos3Color4 {
                pos: [0.2, 0.2, 0.0],
                color: [1.0, 1.0, 1.0, 1.0],
            },
            VertexPos3Color4 {
                pos: [0.9, 0.2, 0.0],
                color: [1.0, 1.0, 1.0, 1.0],
            },
            VertexPos3Color4 {
                pos: [0.2, 0.9, 0.0],
                color: [1.0, 1.0, 1.0, 1.0],
            },
        ];

        let mut writer = AerogpuCmdWriter::new();
        writer.create_buffer(
            VB,
            AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
            core::mem::size_of_val(&vertices) as u64,
            0,
            0,
        );
        writer.upload_resource(VB, 0, bytemuck::cast_slice(&vertices));

        let w = 64u32;
        let h = 64u32;
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
        writer.set_viewport(0.0, 0.0, w as f32, h as f32, 0.0, 1.0);

        writer.create_shader_dxbc(VS, AerogpuShaderStage::Vertex, VS_PASSTHROUGH);
        writer.create_shader_dxbc_ex(GS, AerogpuShaderStageEx::Geometry, GS_PASSTHROUGH_TRIANGLE);
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
        writer.set_primitive_topology(AerogpuPrimitiveTopology::TriangleList);

        writer.bind_shaders_ex(VS, PS, 0, GS, 0, 0);
        // Disable face culling so the test does not depend on backend-specific winding conventions.
        writer.set_rasterizer_state_ext(
            AerogpuFillMode::Solid,
            AerogpuCullMode::None,
            false,
            false,
            0,
            false,
        );

        // Clear to blue; if the placeholder prepass runs, it fills the whole RT red.
        writer.clear(AEROGPU_CLEAR_COLOR, [0.0, 0.0, 1.0, 1.0], 1.0, 0);
        writer.draw(3, 1, 0, 0);
        writer.present(0, 0);
        let stream = writer.finish();

        let mut guest_mem = VecGuestMemory::new(0);
        let report = match exec.execute_cmd_stream(&stream, None, &mut guest_mem) {
            Ok(report) => report,
            Err(err) => {
                if common::skip_if_compute_or_indirect_unsupported(test_name, &err) {
                    return;
                }
                panic!("execute_cmd_stream failed: {err:#}");
            }
        };
        exec.poll_wait();

        let render_target = report
            .presents
            .last()
            .and_then(|p| p.presented_render_target)
            .expect("stream should present a render target");
        assert_eq!(render_target, RT);

        let pixels = exec
            .read_texture_rgba8(render_target)
            .await
            .expect("readback should succeed");
        assert_eq!(pixels.len(), (w * h * 4) as usize);

        let px = |x: u32, y: u32| -> [u8; 4] {
            let idx = ((y * w + x) * 4) as usize;
            pixels[idx..idx + 4].try_into().unwrap()
        };

        // Pixel inside the triangle should be black (missing color varying => v1 = 0).
        assert_eq!(px(48, 16), [0, 0, 0, 0]);
        // Center pixel should remain the blue clear color; the triangle is confined to the
        // top-right quadrant.
        assert_eq!(px(w / 2, h / 2), [0, 0, 255, 255]);
    });
}
