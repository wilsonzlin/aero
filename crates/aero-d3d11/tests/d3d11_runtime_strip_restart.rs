mod common;

use aero_d3d11::runtime::execute::D3D11Runtime;
use aero_gpu::protocol_d3d11::{
    BufferUsage, CmdWriter, DxgiFormat, IndexFormat, PipelineKind, PrimitiveTopology,
    RenderPipelineDesc, Texture2dDesc, TextureUsage, VertexAttributeDesc, VertexBufferLayoutDesc,
    VertexFormat, VertexStepMode,
};

const WIDTH: u32 = 64;
const HEIGHT: u32 = 64;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct VertexPos2 {
    pos: [f32; 2],
}

fn pixel_rgba8(buf: &[u8], x: u32, y: u32) -> [u8; 4] {
    let idx = ((y * WIDTH + x) * 4) as usize;
    buf[idx..idx + 4].try_into().expect("pixel slice")
}

#[test]
fn d3d11_runtime_triangle_strip_draw_indexed_supports_primitive_restart() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::d3d11_runtime_triangle_strip_draw_indexed_supports_primitive_restart"
        );

        let mut rt = match D3D11Runtime::new_for_tests().await {
            Ok(rt) => rt,
            Err(err) => {
                common::skip_or_panic(test_name, &format!("wgpu unavailable ({err:#})"));
                return;
            }
        };

        const VB: u32 = 1;
        const IB: u32 = 2;
        const RT: u32 = 3;
        const RT_VIEW: u32 = 4;
        const SHADER: u32 = 5;
        const PIPE: u32 = 6;

        // Build a vertex buffer large enough that the primitive-restart index (0xFFFF for Uint16)
        // is in-bounds if primitive restart is *not* enabled. This makes the test deterministic:
        // without primitive restart, the strip will stitch through vertex 65535 and cover the
        // center pixel.
        let mut vertices = vec![VertexPos2 { pos: [0.0; 2] }; 65_536];
        vertices[0] = VertexPos2 { pos: [-0.9, -0.5] };
        vertices[1] = VertexPos2 { pos: [-0.1, -0.5] };
        vertices[2] = VertexPos2 { pos: [-0.5, 0.5] };
        vertices[3] = VertexPos2 { pos: [0.1, -0.5] };
        vertices[4] = VertexPos2 { pos: [0.9, -0.5] };
        vertices[5] = VertexPos2 { pos: [0.5, 0.5] };
        // Vertex referenced by the restart index value when restart is disabled.
        vertices[65_535] = VertexPos2 { pos: [0.0, 0.0] };

        // Triangle strip indices with a primitive-restart value between strips.
        // Include one extra u16 so the upload size is 4-byte aligned.
        let indices: [u16; 8] = [0, 1, 2, 0xFFFF, 3, 4, 5, 0];

        let wgsl = r#"
struct VertexInput {
    @location(0) pos: vec2<f32>,
}

struct VertexOutput {
    @builtin(position) pos: vec4<f32>,
}

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.pos = vec4<f32>(input.pos, 0.0, 1.0);
    return out;
}

@fragment
fn fs_main(_input: VertexOutput) -> @location(0) vec4<f32> {
    return vec4<f32>(1.0, 1.0, 1.0, 1.0);
}
"#;

        let attrs = [VertexAttributeDesc {
            shader_location: 0,
            offset: 0,
            format: VertexFormat::Float32x2,
        }];
        let vbs = [VertexBufferLayoutDesc {
            array_stride: std::mem::size_of::<VertexPos2>() as u32,
            step_mode: VertexStepMode::Vertex,
            attributes: &attrs,
        }];

        let mut w = CmdWriter::new();
        w.create_buffer(
            VB,
            std::mem::size_of_val(vertices.as_slice()) as u64,
            BufferUsage::VERTEX | BufferUsage::COPY_DST,
        );
        w.update_buffer(VB, 0, bytemuck::cast_slice(vertices.as_slice()));

        w.create_buffer(
            IB,
            std::mem::size_of_val(&indices) as u64,
            BufferUsage::INDEX | BufferUsage::COPY_DST,
        );
        w.update_buffer(IB, 0, bytemuck::cast_slice(&indices));

        w.create_shader_module_wgsl(SHADER, wgsl);
        w.create_texture2d(
            RT,
            Texture2dDesc {
                width: WIDTH,
                height: HEIGHT,
                array_layers: 1,
                mip_level_count: 1,
                format: DxgiFormat::R8G8B8A8Unorm,
                usage: TextureUsage::RENDER_ATTACHMENT | TextureUsage::COPY_SRC,
            },
        );
        w.create_texture_view(RT_VIEW, RT, 0, 1, 0, 1);
        w.create_render_pipeline(
            PIPE,
            RenderPipelineDesc {
                vs_shader: SHADER,
                fs_shader: SHADER,
                color_format: DxgiFormat::R8G8B8A8Unorm,
                depth_format: DxgiFormat::Unknown,
                topology: PrimitiveTopology::TriangleStrip,
                vertex_buffers: &vbs,
                bindings: &[],
            },
        );
        w.begin_render_pass(RT_VIEW, [0.0, 0.0, 0.0, 1.0], None, 1.0, 0);
        w.set_pipeline(PipelineKind::Render, PIPE);
        w.set_vertex_buffer(0, VB, 0);
        w.set_index_buffer(IB, IndexFormat::Uint16, 0);
        w.draw_indexed(7, 1, 0, 0, 0);
        w.end_render_pass();

        rt.execute(&w.finish()).unwrap();
        rt.poll_wait();

        let pixels = rt.read_texture_rgba8(RT).await.unwrap();
        assert_eq!(pixels.len(), (WIDTH * HEIGHT * 4) as usize);

        let bg = [0u8, 0u8, 0u8, 255u8];
        let fg = [255u8, 255u8, 255u8, 255u8];

        assert_eq!(
            pixel_rgba8(&pixels, 16, 32),
            fg,
            "left triangle should render"
        );
        assert_eq!(
            pixel_rgba8(&pixels, 48, 32),
            fg,
            "right triangle should render"
        );
        assert_eq!(
            pixel_rgba8(&pixels, 32, 32),
            bg,
            "gap pixel should remain background (primitive restart must reset strip assembly)"
        );
    });
}
