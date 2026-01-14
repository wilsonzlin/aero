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

const WGSL_PASSTHROUGH_WHITE: &str = r#"
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

const WGSL_PASSTHROUGH_WHITE_INSTANCE_Y_OFFSET: &str = r#"
struct VertexInput {
    @location(0) pos: vec2<f32>,
}

struct VertexOutput {
    @builtin(position) pos: vec4<f32>,
}

@vertex
fn vs_main(input: VertexInput, @builtin(instance_index) instance: u32) -> VertexOutput {
    // Draw two copies of the same geometry by applying a vertical translation based on
    // `instance_index`. This lets the test reuse a single vertex buffer while still validating
    // two separate strip primitive restart cases (Uint16 and Uint32) in one render pass.
    let y_offset = f32(instance) - 0.5;

    var out: VertexOutput;
    out.pos = vec4<f32>(input.pos + vec2<f32>(0.0, y_offset), 0.0, 1.0);
    return out;
}

@fragment
fn fs_main(_input: VertexOutput) -> @location(0) vec4<f32> {
    return vec4<f32>(1.0, 1.0, 1.0, 1.0);
}
"#;

const WGSL_PASSTHROUGH_WHITE_OFFSET_HALF_PIXEL: &str = r#"
struct VertexInput {
    @location(0) pos: vec2<f32>,
}

struct VertexOutput {
    @builtin(position) pos: vec4<f32>,
}

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    // Offset by half a pixel (in NDC) so an out-of-bounds zeroed vertex fetch lands on a
    // deterministic pixel. This makes the line-strip restart tests stable without needing a
    // 65k-vertex buffer.
    let offset = vec2<f32>(0.015625, -0.015625); // (+1/64, -1/64) for a 64x64 render target.

    var out: VertexOutput;
    out.pos = vec4<f32>(input.pos + offset, 0.0, 1.0);
    return out;
}

@fragment
fn fs_main(_input: VertexOutput) -> @location(0) vec4<f32> {
    return vec4<f32>(1.0, 1.0, 1.0, 1.0);
}
"#;

async fn run_triangle_strip_restart_test<TIndex: bytemuck::Pod>(
    test_name: &str,
    index_format: IndexFormat,
    vertices: &[VertexPos2],
    indices: &[TIndex],
    draw_index_count: u32,
    index_buffer_offset_bytes: u64,
    first_index: u32,
) {
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
        std::mem::size_of_val(vertices) as u64,
        BufferUsage::VERTEX | BufferUsage::COPY_DST,
    );
    w.update_buffer(VB, 0, bytemuck::cast_slice(vertices));

    w.create_buffer(
        IB,
        std::mem::size_of_val(indices) as u64,
        BufferUsage::INDEX | BufferUsage::COPY_DST,
    );
    w.update_buffer(IB, 0, bytemuck::cast_slice(indices));

    w.create_shader_module_wgsl(SHADER, WGSL_PASSTHROUGH_WHITE);
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
    w.set_index_buffer(IB, index_format, index_buffer_offset_bytes);
    w.draw_indexed(draw_index_count, 1, first_index, 0, 0);
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
}

async fn run_line_strip_restart_test<TIndex: bytemuck::Pod>(
    test_name: &str,
    index_format: IndexFormat,
    vertices: &[VertexPos2],
    indices: &[TIndex],
    draw_index_count: u32,
) {
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
        std::mem::size_of_val(vertices) as u64,
        BufferUsage::VERTEX | BufferUsage::COPY_DST,
    );
    w.update_buffer(VB, 0, bytemuck::cast_slice(vertices));

    w.create_buffer(
        IB,
        std::mem::size_of_val(indices) as u64,
        BufferUsage::INDEX | BufferUsage::COPY_DST,
    );
    w.update_buffer(IB, 0, bytemuck::cast_slice(indices));

    w.create_shader_module_wgsl(SHADER, WGSL_PASSTHROUGH_WHITE_OFFSET_HALF_PIXEL);
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
            topology: PrimitiveTopology::LineStrip,
            vertex_buffers: &vbs,
            bindings: &[],
        },
    );
    w.begin_render_pass(RT_VIEW, [0.0, 0.0, 0.0, 1.0], None, 1.0, 0);
    w.set_pipeline(PipelineKind::Render, PIPE);
    w.set_vertex_buffer(0, VB, 0);
    w.set_index_buffer(IB, index_format, 0);
    w.draw_indexed(draw_index_count, 1, 0, 0, 0);
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
        "left line segment should render"
    );
    assert_eq!(
        pixel_rgba8(&pixels, 48, 32),
        fg,
        "right line segment should render"
    );
    assert_eq!(
        pixel_rgba8(&pixels, 32, 32),
        bg,
        "gap pixel should remain background (primitive restart must reset strip assembly)"
    );
}

#[test]
fn d3d11_runtime_triangle_strip_draw_indexed_supports_primitive_restart_u16() {
    pollster::block_on(async {
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

        run_triangle_strip_restart_test(
            concat!(
                module_path!(),
                "::d3d11_runtime_triangle_strip_draw_indexed_supports_primitive_restart_u16"
            ),
            IndexFormat::Uint16,
            &vertices,
            &indices,
            7,
            0,
            0,
        )
        .await;
    });
}

#[test]
fn d3d11_runtime_triangle_strip_draw_indexed_supports_primitive_restart_u32() {
    pollster::block_on(async {
        let vertices = [
            VertexPos2 { pos: [-0.9, -0.5] },
            VertexPos2 { pos: [-0.1, -0.5] },
            VertexPos2 { pos: [-0.5, 0.5] },
            VertexPos2 { pos: [0.1, -0.5] },
            VertexPos2 { pos: [0.9, -0.5] },
            VertexPos2 { pos: [0.5, 0.5] },
        ];

        // Triangle strip indices with a primitive-restart value between strips.
        //
        // We can't practically allocate enough vertices for 0xFFFF_FFFF to be in-bounds. This
        // relies on WebGPU/wgpu's robust buffer access behavior: if primitive restart is *not*
        // enabled, the out-of-bounds vertex fetch will produce a zeroed position at the origin
        // (center), which will cause the strip to stitch and fill the "gap pixel".
        let indices: [u32; 7] = [0, 1, 2, 0xFFFF_FFFF, 3, 4, 5];

        run_triangle_strip_restart_test(
            concat!(
                module_path!(),
                "::d3d11_runtime_triangle_strip_draw_indexed_supports_primitive_restart_u32"
            ),
            IndexFormat::Uint32,
            &vertices,
            &indices,
            7,
            0,
            0,
        )
        .await;
    });
}

#[test]
fn d3d11_runtime_triangle_strip_draw_indexed_restart_respects_index_buffer_offset_and_first_index_u16(
) {
    pollster::block_on(async {
        // Same setup as the baseline u16 restart test, but bind the index buffer with a non-zero
        // byte offset and draw with a non-zero `first_index`.
        //
        // This exercises the runtime's restart emulation slice math:
        // `index_buffer_offset_bytes + first_index * index_stride`.
        let mut vertices = vec![VertexPos2 { pos: [0.0; 2] }; 65_536];
        vertices[0] = VertexPos2 { pos: [-0.9, -0.5] };
        vertices[1] = VertexPos2 { pos: [-0.1, -0.5] };
        vertices[2] = VertexPos2 { pos: [-0.5, 0.5] };
        vertices[3] = VertexPos2 { pos: [0.1, -0.5] };
        vertices[4] = VertexPos2 { pos: [0.9, -0.5] };
        vertices[5] = VertexPos2 { pos: [0.5, 0.5] };
        vertices[65_535] = VertexPos2 { pos: [0.0, 0.0] };

        // Two u16 padding values at the start (4 bytes) so we can bind the IB with a non-zero
        // offset while keeping `UpdateBuffer` 4-byte aligned.
        //
        // After applying `index_buffer_offset_bytes=4` and `first_index=1`, the draw sees:
        // [0, 1, 2, 0xFFFF, 3, 4, 5].
        let indices: [u16; 10] = [10, 11, 123, 0, 1, 2, 0xFFFF, 3, 4, 5];

        run_triangle_strip_restart_test(
            concat!(
                module_path!(),
                "::d3d11_runtime_triangle_strip_draw_indexed_restart_respects_index_buffer_offset_and_first_index_u16"
            ),
            IndexFormat::Uint16,
            &vertices,
            &indices,
            7,
            4,
            1,
        )
        .await;
    });
}

#[test]
fn d3d11_runtime_line_strip_draw_indexed_supports_primitive_restart_u16() {
    pollster::block_on(async {
        let vertices = [
            VertexPos2 { pos: [-0.9, 0.0] },
            VertexPos2 { pos: [-0.1, 0.0] },
            VertexPos2 { pos: [0.1, 0.0] },
            VertexPos2 { pos: [0.9, 0.0] },
        ];
        // Include one extra u16 so the upload size is 4-byte aligned.
        let indices: [u16; 6] = [0, 1, 0xFFFF, 2, 3, 0];

        run_line_strip_restart_test(
            concat!(
                module_path!(),
                "::d3d11_runtime_line_strip_draw_indexed_supports_primitive_restart_u16"
            ),
            IndexFormat::Uint16,
            &vertices,
            &indices,
            5,
        )
        .await;
    });
}

#[test]
fn d3d11_runtime_line_strip_draw_indexed_supports_primitive_restart_u32() {
    pollster::block_on(async {
        let vertices = [
            VertexPos2 { pos: [-0.9, 0.0] },
            VertexPos2 { pos: [-0.1, 0.0] },
            VertexPos2 { pos: [0.1, 0.0] },
            VertexPos2 { pos: [0.9, 0.0] },
        ];
        let indices: [u32; 5] = [0, 1, 0xFFFF_FFFF, 2, 3];

        run_line_strip_restart_test(
            concat!(
                module_path!(),
                "::d3d11_runtime_line_strip_draw_indexed_supports_primitive_restart_u32"
            ),
            IndexFormat::Uint32,
            &vertices,
            &indices,
            5,
        )
        .await;
    });
}

#[test]
fn d3d11_runtime_triangle_strip_switches_restart_pipeline_between_u16_and_u32() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::d3d11_runtime_triangle_strip_switches_restart_pipeline_between_u16_and_u32"
        );

        let mut rt = match D3D11Runtime::new_for_tests().await {
            Ok(rt) => rt,
            Err(err) => {
                common::skip_or_panic(test_name, &format!("wgpu unavailable ({err:#})"));
                return;
            }
        };

        const VB: u32 = 1;
        const IB16: u32 = 2;
        const IB32: u32 = 3;
        const RT: u32 = 4;
        const RT_VIEW: u32 = 5;
        const SHADER: u32 = 6;
        const PIPE: u32 = 7;

        // One base geometry buffer shared across both draws.
        //
        // Allocate enough vertices so the primitive-restart index for Uint16 (0xFFFF) is
        // in-bounds when restart is *not* enabled. Vertex 65535 stays at the origin, so the
        // vertex shader's instance-based translation moves it onto the expected "gap pixel" for
        // each draw.
        let mut vertices = vec![VertexPos2 { pos: [0.0; 2] }; 65_536];
        vertices[0] = VertexPos2 { pos: [-0.9, -0.4] };
        vertices[1] = VertexPos2 { pos: [-0.1, -0.4] };
        vertices[2] = VertexPos2 { pos: [-0.5, 0.4] };
        vertices[3] = VertexPos2 { pos: [0.1, -0.4] };
        vertices[4] = VertexPos2 { pos: [0.9, -0.4] };
        vertices[5] = VertexPos2 { pos: [0.5, 0.4] };

        // Triangle strip indices with a primitive-restart value between strips.
        // Include one extra u16 so the upload size is 4-byte aligned.
        let indices_u16: [u16; 8] = [0, 1, 2, 0xFFFF, 3, 4, 5, 0];
        let indices_u32: [u32; 7] = [0, 1, 2, 0xFFFF_FFFF, 3, 4, 5];

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
            IB16,
            std::mem::size_of_val(&indices_u16) as u64,
            BufferUsage::INDEX | BufferUsage::COPY_DST,
        );
        w.update_buffer(IB16, 0, bytemuck::cast_slice(&indices_u16));

        w.create_buffer(
            IB32,
            std::mem::size_of_val(&indices_u32) as u64,
            BufferUsage::INDEX | BufferUsage::COPY_DST,
        );
        w.update_buffer(IB32, 0, bytemuck::cast_slice(&indices_u32));

        w.create_shader_module_wgsl(SHADER, WGSL_PASSTHROUGH_WHITE_INSTANCE_Y_OFFSET);
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

        // Draw 0: Uint16 indices. Instance 0 -> y_offset=-0.5 (bottom half).
        w.set_index_buffer(IB16, IndexFormat::Uint16, 0);
        w.draw_indexed(7, 1, 0, 0, 0);

        // Draw 1: Uint32 indices. Instance 1 -> y_offset=+0.5 (top half).
        w.set_index_buffer(IB32, IndexFormat::Uint32, 0);
        w.draw_indexed(7, 1, 0, 0, 1);

        w.end_render_pass();

        rt.execute(&w.finish()).unwrap();
        rt.poll_wait();

        let pixels = rt.read_texture_rgba8(RT).await.unwrap();
        assert_eq!(pixels.len(), (WIDTH * HEIGHT * 4) as usize);

        let bg = [0u8, 0u8, 0u8, 255u8];
        let fg = [255u8, 255u8, 255u8, 255u8];

        // Bottom (Uint16): y=-0.5 -> y=48
        assert_eq!(
            pixel_rgba8(&pixels, 16, 48),
            fg,
            "bottom left triangle should render"
        );
        assert_eq!(
            pixel_rgba8(&pixels, 48, 48),
            fg,
            "bottom right triangle should render"
        );
        assert_eq!(
            pixel_rgba8(&pixels, 32, 48),
            bg,
            "bottom gap pixel should remain background"
        );

        // Top (Uint32): y=+0.5 -> y=16
        assert_eq!(
            pixel_rgba8(&pixels, 16, 16),
            fg,
            "top left triangle should render"
        );
        assert_eq!(
            pixel_rgba8(&pixels, 48, 16),
            fg,
            "top right triangle should render"
        );
        assert_eq!(
            pixel_rgba8(&pixels, 32, 16),
            bg,
            "top gap pixel should remain background"
        );
    });
}
