mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCullMode, AerogpuFillMode, AerogpuIndexFormat, AerogpuPrimitiveTopology,
    AerogpuShaderStage, AerogpuVertexBufferBinding, AEROGPU_CLEAR_COLOR,
    AEROGPU_RESOURCE_USAGE_INDEX_BUFFER, AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
    AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;
use anyhow::{Context, Result};
use wgpu::Features;

const WIDTH: u32 = 64;
const HEIGHT: u32 = 64;

const VS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/vs_passthrough.dxbc");
const PS_PRIMITIVE_ID: &[u8] = include_bytes!("fixtures/ps_primitive_id.dxbc");
const ILAY_POS3_COLOR: &[u8] = include_bytes!("fixtures/ilay_pos3_color.bin");

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct VertexPos3Color4 {
    pos: [f32; 3],
    color: [f32; 4],
}

fn pixel_rgba8(buf: &[u8], x: u32, y: u32) -> [u8; 4] {
    let idx = ((y * WIDTH + x) * 4) as usize;
    buf[idx..idx + 4].try_into().expect("pixel slice")
}

#[test]
fn aerogpu_cmd_primitive_restart_preserves_sv_primitive_id() -> Result<()> {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::aerogpu_cmd_primitive_restart_preserves_sv_primitive_id"
        );
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(test_name, &format!("wgpu unavailable ({e:#})"));
                return Ok(());
            }
        };
        if !exec
            .device()
            .features()
            .contains(Features::SHADER_PRIMITIVE_INDEX)
        {
            common::skip_or_panic(
                test_name,
                "wgpu adapter does not support SHADER_PRIMITIVE_INDEX (SV_PrimitiveID)",
            );
            return Ok(());
        }

        const RT: u32 = 1;
        const VB: u32 = 2;
        const IB: u32 = 3;
        const VS: u32 = 4;
        const PS: u32 = 5;
        const IL: u32 = 6;

        // Two disconnected triangles with a primitive-restart cut between them.
        //
        // The pixel shader uses `SV_PrimitiveID` to output:
        // - primitive 0 => black
        // - primitive 1 => red
        //
        // If primitive restart emulation splits the draw into multiple draw calls, the primitive
        // ID would reset to 0 for the second draw, making both triangles black.
        //
        // Make the u16 restart index (0xFFFF) be in-bounds if primitive restart is disabled. This
        // keeps the gap check deterministic by ensuring the strip stitches through vertex 65535.
        let mut vertices = vec![
            VertexPos3Color4 {
                pos: [0.0; 3],
                color: [0.0; 4],
            };
            65_536
        ];
        let white = [1.0, 1.0, 1.0, 1.0];
        // Left triangle.
        vertices[0] = VertexPos3Color4 {
            pos: [-1.0, -1.0, 0.0],
            color: white,
        };
        vertices[1] = VertexPos3Color4 {
            pos: [-1.0, 1.0, 0.0],
            color: white,
        };
        vertices[2] = VertexPos3Color4 {
            pos: [-0.2, 0.0, 0.0],
            color: white,
        };
        // Right triangle.
        vertices[3] = VertexPos3Color4 {
            pos: [0.2, -1.0, 0.0],
            color: white,
        };
        vertices[4] = VertexPos3Color4 {
            pos: [1.0, 1.0, 0.0],
            color: white,
        };
        vertices[5] = VertexPos3Color4 {
            pos: [1.0, -1.0, 0.0],
            color: white,
        };
        // Vertex referenced by the restart index value when restart is disabled.
        vertices[65_535] = VertexPos3Color4 {
            pos: [0.0, 0.0, 0.0],
            color: white,
        };

        // Triangle strip indices with a primitive-restart value between strips.
        // Include one extra u16 so the upload size is 4-byte aligned.
        let indices: [u16; 8] = [0, 1, 2, 0xFFFF, 3, 4, 5, 0];

        let mut writer = AerogpuCmdWriter::new();
        writer.create_texture2d(
            RT,
            AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
            AerogpuFormat::R8G8B8A8Unorm as u32,
            WIDTH,
            HEIGHT,
            1,
            1,
            0,
            0,
            0,
        );

        writer.create_buffer(
            VB,
            AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
            core::mem::size_of_val(vertices.as_slice()) as u64,
            0,
            0,
        );
        writer.upload_resource(VB, 0, bytemuck::cast_slice(vertices.as_slice()));

        writer.create_buffer(
            IB,
            AEROGPU_RESOURCE_USAGE_INDEX_BUFFER,
            core::mem::size_of_val(&indices) as u64,
            0,
            0,
        );
        writer.upload_resource(IB, 0, bytemuck::cast_slice(&indices));

        writer.set_render_targets(&[RT], 0);
        // Clear to blue so the primitive 0 "black" triangle is distinguishable.
        writer.clear(AEROGPU_CLEAR_COLOR, [0.0, 0.0, 1.0, 1.0], 1.0, 0);

        // Disable culling so the test isn't sensitive to strip winding.
        writer.set_rasterizer_state(
            AerogpuFillMode::Solid,
            AerogpuCullMode::None,
            false,
            false,
            0,
            0,
        );

        writer.create_shader_dxbc(VS, AerogpuShaderStage::Vertex, VS_PASSTHROUGH);
        writer.create_shader_dxbc(PS, AerogpuShaderStage::Pixel, PS_PRIMITIVE_ID);
        writer.bind_shaders(VS, PS, 0);

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
        writer.set_index_buffer(IB, AerogpuIndexFormat::Uint16, 0);
        writer.set_primitive_topology(AerogpuPrimitiveTopology::TriangleStrip);

        writer.draw_indexed(7, 1, 0, 0, 0);

        let stream = writer.finish();
        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .context("execute_cmd_stream")?;
        exec.poll_wait();

        let pixels = exec
            .read_texture_rgba8(RT)
            .await
            .context("readback render target")?;
        assert_eq!(pixels.len(), (WIDTH * HEIGHT * 4) as usize);

        let bg = [0u8, 0u8, 255u8, 255u8];
        let prim0 = [0u8, 0u8, 0u8, 255u8];
        let prim1 = [255u8, 0u8, 0u8, 255u8];

        assert_eq!(
            pixel_rgba8(&pixels, 8, 32),
            prim0,
            "left triangle should be primitive 0 (black)"
        );
        assert_eq!(
            pixel_rgba8(&pixels, 60, 32),
            prim1,
            "right triangle should be primitive 1 (red)"
        );
        assert_eq!(
            pixel_rgba8(&pixels, 32, 32),
            bg,
            "gap pixel should remain background (primitive restart must reset strip assembly)"
        );

        Ok(())
    })
}
