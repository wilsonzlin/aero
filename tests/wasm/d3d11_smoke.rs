use aero_d3d11::runtime::execute::D3D11Runtime;
use aero_gpu::protocol_d3d11::{
    BindingDesc, BindingType, BufferUsage, CmdWriter, DxgiFormat, PipelineKind, PrimitiveTopology,
    ShaderStageFlags, TextureUsage, VertexAttributeDesc, VertexBufferLayoutDesc, VertexFormat,
    VertexStepMode,
};

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    pos: [f32; 2],
    uv: [f32; 2],
}

#[test]
fn d3d11_render_textured_quad() {
    pollster::block_on(async {
        let mut rt = match D3D11Runtime::new_for_tests().await {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!("wgpu unavailable ({e:#}); skipping render smoke test");
                return;
            }
        };

        const VB: u32 = 1;
        const TEX: u32 = 2;
        const TEX_VIEW: u32 = 3;
        const SAMP: u32 = 4;
        const RT: u32 = 5;
        const RT_VIEW: u32 = 6;
        const SHADER: u32 = 7;
        const PIPE: u32 = 8;

        let vertices: [Vertex; 6] = [
            Vertex {
                pos: [-1.0, -1.0],
                uv: [0.0, 1.0],
            },
            Vertex {
                pos: [1.0, -1.0],
                uv: [1.0, 1.0],
            },
            Vertex {
                pos: [-1.0, 1.0],
                uv: [0.0, 0.0],
            },
            Vertex {
                pos: [-1.0, 1.0],
                uv: [0.0, 0.0],
            },
            Vertex {
                pos: [1.0, -1.0],
                uv: [1.0, 1.0],
            },
            Vertex {
                pos: [1.0, 1.0],
                uv: [1.0, 0.0],
            },
        ];

        let wgsl = r#"
struct VertexInput {
    @location(0) pos: vec2<f32>,
    @location(1) uv: vec2<f32>,
}

struct VertexOutput {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.pos = vec4<f32>(input.pos, 0.0, 1.0);
    out.uv = input.uv;
    return out;
}

@group(0) @binding(0) var t0: texture_2d<f32>;
@group(0) @binding(1) var s0: sampler;

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(t0, s0, input.uv);
}
"#;

        let vb_size = (std::mem::size_of_val(&vertices)) as u64;

        let attrs = [
            VertexAttributeDesc {
                shader_location: 0,
                offset: 0,
                format: VertexFormat::Float32x2,
            },
            VertexAttributeDesc {
                shader_location: 1,
                offset: 8,
                format: VertexFormat::Float32x2,
            },
        ];
        let vbs = [VertexBufferLayoutDesc {
            array_stride: std::mem::size_of::<Vertex>() as u32,
            step_mode: VertexStepMode::Vertex,
            attributes: &attrs,
        }];
        let bindings = [
            BindingDesc {
                binding: 0,
                ty: BindingType::Texture2D,
                visibility: ShaderStageFlags::FRAGMENT,
                storage_texture_format: None,
            },
            BindingDesc {
                binding: 1,
                ty: BindingType::Sampler,
                visibility: ShaderStageFlags::FRAGMENT,
                storage_texture_format: None,
            },
        ];

        let mut w = CmdWriter::new();
        w.create_buffer(VB, vb_size, BufferUsage::VERTEX | BufferUsage::COPY_DST);
        w.update_buffer(VB, 0, bytemuck::bytes_of(&vertices));
        w.create_shader_module_wgsl(SHADER, wgsl);
        w.create_texture2d(
            TEX,
            1,
            1,
            1,
            1,
            DxgiFormat::R8G8B8A8Unorm,
            TextureUsage::TEXTURE_BINDING | TextureUsage::COPY_DST,
        );
        w.create_texture_view(TEX_VIEW, TEX, 0, 1, 0, 1);
        w.update_texture2d(TEX, 0, 0, 1, 1, 4, &[255, 0, 0, 255]);
        w.create_sampler(SAMP, 0);
        w.create_texture2d(
            RT,
            4,
            4,
            1,
            1,
            DxgiFormat::R8G8B8A8Unorm,
            TextureUsage::RENDER_ATTACHMENT | TextureUsage::COPY_SRC,
        );
        w.create_texture_view(RT_VIEW, RT, 0, 1, 0, 1);
        w.create_render_pipeline(
            PIPE,
            SHADER,
            SHADER,
            DxgiFormat::R8G8B8A8Unorm,
            DxgiFormat::Unknown,
            PrimitiveTopology::TriangleList,
            &vbs,
            &bindings,
        );
        w.begin_render_pass(RT_VIEW, [0.0, 0.0, 0.0, 1.0], None, 1.0, 0);
        w.set_pipeline(PipelineKind::Render, PIPE);
        w.set_vertex_buffer(0, VB, 0);
        w.set_bind_texture_view(0, TEX_VIEW);
        w.set_bind_sampler(1, SAMP);
        w.draw(6, 1, 0, 0);
        w.end_render_pass();

        rt.execute(&w.finish()).unwrap();
        rt.poll_wait();

        let pixels = rt.read_texture_rgba8(RT).await.unwrap();
        assert_eq!(pixels.len(), 4 * 4 * 4);
        assert_eq!(&pixels[0..4], &[255, 0, 0, 255]);
        assert_eq!(&pixels[4 * 3..4 * 4], &[255, 0, 0, 255]);
    });
}

#[test]
fn d3d11_compute_writes_storage_buffer() {
    pollster::block_on(async {
        let mut rt = match D3D11Runtime::new_for_tests().await {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!("wgpu unavailable ({e:#}); skipping compute smoke test");
                return;
            }
        };

        const OUT: u32 = 101;
        const READBACK: u32 = 102;
        const SHADER: u32 = 103;
        const PIPE: u32 = 104;

        let wgsl = r#"
struct Output {
    values: array<u32>,
}

@group(0) @binding(0) var<storage, read_write> out_buf: Output;

@compute @workgroup_size(64)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if (idx < 16u) {
        out_buf.values[idx] = idx * 2u + 1u;
    }
}
"#;

        let bindings = [BindingDesc {
            binding: 0,
            ty: BindingType::StorageBufferReadWrite,
            visibility: ShaderStageFlags::COMPUTE,
            storage_texture_format: None,
        }];

        let size = 16u64 * 4;
        let mut w = CmdWriter::new();
        w.create_buffer(
            OUT,
            size,
            BufferUsage::STORAGE | BufferUsage::COPY_SRC | BufferUsage::COPY_DST,
        );
        w.create_buffer(
            READBACK,
            size,
            BufferUsage::MAP_READ | BufferUsage::COPY_DST,
        );
        w.create_shader_module_wgsl(SHADER, wgsl);
        w.create_compute_pipeline(PIPE, SHADER, &bindings);
        w.begin_compute_pass();
        w.set_pipeline(PipelineKind::Compute, PIPE);
        w.set_bind_buffer(0, OUT, 0, 0);
        w.dispatch(1, 1, 1);
        w.end_compute_pass();
        w.copy_buffer_to_buffer(OUT, 0, READBACK, 0, size);

        rt.execute(&w.finish()).unwrap();
        rt.poll_wait();

        let bytes = rt.read_buffer(READBACK, 0, size).await.unwrap();
        let words: Vec<u32> = bytes
            .chunks_exact(4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        assert_eq!(words.len(), 16);
        for (i, v) in words.iter().enumerate() {
            assert_eq!(*v, i as u32 * 2 + 1);
        }
    });
}
