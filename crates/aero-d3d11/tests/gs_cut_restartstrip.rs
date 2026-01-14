mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_d3d11::{DxbcFile, ShaderStage, Sm4Program};
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

const WIDTH: u32 = 64;
const HEIGHT: u32 = 64;

const VS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/vs_passthrough.dxbc");
const PS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/ps_passthrough.dxbc");
const ILAY_POS3_COLOR: &[u8] = include_bytes!("fixtures/ilay_pos3_color.bin");

// This fixture is also used by `sm4_geometry_decode.rs` to validate that the SM4 decoder
// recognizes `emit` + `cut` instructions.
const GS_CUT_DXBC: &[u8] = include_bytes!("fixtures/gs_emit_cut.dxbc");

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
fn gs_cut_restartstrip_resets_triangle_strip_assembly_semantics() -> Result<()> {
    pollster::block_on(async {
        // Ensure our checked-in fixture is at least a valid geometry shader DXBC container and
        // actually contains the `cut` opcode token. This helps catch accidental fixture
        // corruption, even though the test below uses WGSL to emulate the expected behavior.
        let dxbc = DxbcFile::parse(GS_CUT_DXBC).context("parse gs_emit_cut.dxbc as DXBC")?;
        let program = Sm4Program::parse_from_dxbc(&dxbc).context("parse gs_emit_cut.dxbc as SM4")?;
        assert_eq!(
            program.stage,
            ShaderStage::Geometry,
            "gs_emit_cut.dxbc must be a geometry shader"
        );
        assert!(
            program
                .tokens
                .iter()
                .any(|t| (*t & aero_d3d11::sm4::opcode::OPCODE_MASK) == aero_d3d11::sm4::opcode::OPCODE_CUT),
            "gs_emit_cut.dxbc must contain a cut opcode (RestartStrip)"
        );

        let test_name =
            concat!(module_path!(), "::gs_cut_restartstrip_resets_triangle_strip_assembly_semantics");
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(test_name, &format!("wgpu unavailable ({e:#})"));
                return Ok(());
            }
        };

        const RT: u32 = 1;
        const VB: u32 = 2;
        const IB: u32 = 3;
        const VS: u32 = 4;
        const PS: u32 = 5;
        const IL: u32 = 6;

        // Build a vertex buffer large enough that the primitive-restart index (0xFFFF for Uint16)
        // is in-bounds if primitive restart is *not* enabled. This makes the test deterministic:
        // without primitive restart, the strip will stitch through vertex 65535 and cover the
        // center pixel.
        let mut vertices = vec![
            VertexPos3Color4 {
                pos: [0.0; 3],
                color: [0.0; 4],
            };
            65_536
        ];
        let white = [1.0, 1.0, 1.0, 1.0];
        vertices[0] = VertexPos3Color4 {
            pos: [-0.9, -0.5, 0.0],
            color: white,
        };
        vertices[1] = VertexPos3Color4 {
            pos: [-0.1, -0.5, 0.0],
            color: white,
        };
        vertices[2] = VertexPos3Color4 {
            pos: [-0.5, 0.5, 0.0],
            color: white,
        };
        vertices[3] = VertexPos3Color4 {
            pos: [0.1, -0.5, 0.0],
            color: white,
        };
        vertices[4] = VertexPos3Color4 {
            pos: [0.9, -0.5, 0.0],
            color: white,
        };
        vertices[5] = VertexPos3Color4 {
            pos: [0.5, 0.5, 0.0],
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
        writer.clear(AEROGPU_CLEAR_COLOR, [0.0, 0.0, 0.0, 1.0], 1.0, 0);

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
        writer.create_shader_dxbc(PS, AerogpuShaderStage::Pixel, PS_PASSTHROUGH);
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

        let bg = [0u8, 0u8, 0u8, 255u8];
        let fg = [255u8, 255u8, 255u8, 255u8];

        assert_eq!(pixel_rgba8(&pixels, 16, 32), fg, "left triangle should render");
        assert_eq!(pixel_rgba8(&pixels, 48, 32), fg, "right triangle should render");
        assert_eq!(
            pixel_rgba8(&pixels, 32, 32),
            bg,
            "gap pixel should remain background (primitive restart / RestartStrip must reset strip assembly)"
        );

        Ok(())
    })
}
