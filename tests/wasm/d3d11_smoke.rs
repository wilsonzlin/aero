use crate::common;
use aero_d3d11::runtime::execute::D3D11Runtime;
use aero_gpu::protocol_d3d11::{
    BindingDesc, BindingType, BufferUsage, CmdWriter, DxgiFormat, PipelineKind, PrimitiveTopology,
    RenderPipelineDesc, ShaderStageFlags, Texture2dDesc, Texture2dUpdate, TextureUsage,
    VertexAttributeDesc, VertexBufferLayoutDesc, VertexFormat, VertexStepMode,
};

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    pos: [f32; 2],
    uv: [f32; 2],
}

async fn new_runtime(test_name: &str) -> Option<D3D11Runtime> {
    if !common::skip_unless_webgpu(test_name) {
        return None;
    }

    match D3D11Runtime::new_for_tests().await {
        Ok(rt) => Some(rt),
        Err(e) => {
            common::skip_or_panic(test_name, &format!("wgpu unavailable ({e:#})"));
            None
        }
    }
}

// ---- Compute shader execution tests (stage-scoped binding model) ----
//
// The Aero D3D11 translator uses a stage-scoped bind group scheme:
//   @group(0) = VS, @group(1) = PS, @group(2) = CS.
//
// The `protocol_d3d11` command-stream runtime (`D3D11Runtime`) follows the same convention for
// compute: resources are bound at `@group(2)`, with empty bind groups 0/1 to satisfy WebGPU's
// requirement that pipeline layouts include all groups up to the maximum used index.
//
// For these smoke tests we use a minimal wgpu harness so the WGSL snippets can bind resources at
// group 2 directly without coupling to any implicit command-stream binding behavior.

async fn read_mapped_buffer(device: &wgpu::Device, buffer: &wgpu::Buffer) -> Vec<u8> {
    let slice = buffer.slice(..);
    let (sender, receiver) = futures_intrusive::channel::shared::oneshot_channel();
    slice.map_async(wgpu::MapMode::Read, move |v| {
        sender.send(v).ok();
    });
    #[cfg(not(target_arch = "wasm32"))]
    device.poll(wgpu::Maintain::Wait);
    #[cfg(target_arch = "wasm32")]
    device.poll(wgpu::Maintain::Poll);

    receiver
        .receive()
        .await
        .expect("map_async dropped")
        .expect("map_async failed");

    let data = slice.get_mapped_range().to_vec();
    buffer.unmap();
    data
}

async fn read_texture_rgba8(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    width: u32,
    height: u32,
) -> Vec<u8> {
    let bytes_per_pixel = 4u32;
    let unpadded_bytes_per_row = width * bytes_per_pixel;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(align) * align;
    let buffer_size = padded_bytes_per_row as u64 * height as u64;

    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("d3d11_smoke read_texture_rgba8 staging"),
        size: buffer_size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("d3d11_smoke read_texture_rgba8 encoder"),
    });
    encoder.copy_texture_to_buffer(
        wgpu::ImageCopyTexture {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::ImageCopyBuffer {
            buffer: &staging,
            layout: wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(padded_bytes_per_row),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);

    let slice = staging.slice(..);
    let (sender, receiver) = futures_intrusive::channel::shared::oneshot_channel();
    slice.map_async(wgpu::MapMode::Read, move |v| {
        sender.send(v).ok();
    });
    #[cfg(not(target_arch = "wasm32"))]
    device.poll(wgpu::Maintain::Wait);
    #[cfg(target_arch = "wasm32")]
    device.poll(wgpu::Maintain::Poll);

    receiver
        .receive()
        .await
        .expect("map_async dropped")
        .expect("map_async failed");

    let mapped = slice.get_mapped_range();
    let mut out = Vec::with_capacity((unpadded_bytes_per_row * height) as usize);
    for row in 0..height as usize {
        let start = row * padded_bytes_per_row as usize;
        out.extend_from_slice(&mapped[start..start + unpadded_bytes_per_row as usize]);
    }
    drop(mapped);
    staging.unmap();
    out
}

#[test]
fn d3d11_render_textured_quad() {
    pollster::block_on(async {
        let test_name = concat!(module_path!(), "::d3d11_render_textured_quad");
        let Some(mut rt) = new_runtime(test_name).await else {
            return;
        };

        const VB: u32 = 1;
        const TEX_RED: u32 = 2;
        const TEX_RED_VIEW: u32 = 3;
        const SAMP: u32 = 4;
        const SAMP_DUP: u32 = 11;
        const RT: u32 = 5;
        const RT_VIEW: u32 = 6;
        const SHADER: u32 = 7;
        const PIPE: u32 = 8;
        const TEX_GREEN: u32 = 9;
        const TEX_GREEN_VIEW: u32 = 10;

        // Two quads side-by-side so we can change bindings between draws.
        let vertices: [Vertex; 12] = [
            Vertex {
                pos: [-1.0, -1.0],
                uv: [0.0, 1.0],
            },
            Vertex {
                pos: [0.0, -1.0],
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
                pos: [0.0, -1.0],
                uv: [1.0, 1.0],
            },
            Vertex {
                pos: [0.0, 1.0],
                uv: [1.0, 0.0],
            },
            Vertex {
                pos: [0.0, -1.0],
                uv: [0.0, 1.0],
            },
            Vertex {
                pos: [1.0, -1.0],
                uv: [1.0, 1.0],
            },
            Vertex {
                pos: [0.0, 1.0],
                uv: [0.0, 0.0],
            },
            Vertex {
                pos: [0.0, 1.0],
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
            TEX_RED,
            Texture2dDesc {
                width: 1,
                height: 1,
                array_layers: 1,
                mip_level_count: 1,
                format: DxgiFormat::R8G8B8A8Unorm,
                usage: TextureUsage::TEXTURE_BINDING | TextureUsage::COPY_DST,
            },
        );
        w.create_texture_view(TEX_RED_VIEW, TEX_RED, 0, 1, 0, 1);
        w.update_texture2d(
            TEX_RED,
            Texture2dUpdate {
                mip_level: 0,
                array_layer: 0,
                width: 1,
                height: 1,
                bytes_per_row: 4,
                data: &[255, 0, 0, 255],
            },
        );

        w.create_texture2d(
            TEX_GREEN,
            Texture2dDesc {
                width: 1,
                height: 1,
                array_layers: 1,
                mip_level_count: 1,
                format: DxgiFormat::R8G8B8A8Unorm,
                usage: TextureUsage::TEXTURE_BINDING | TextureUsage::COPY_DST,
            },
        );
        w.create_texture_view(TEX_GREEN_VIEW, TEX_GREEN, 0, 1, 0, 1);
        w.update_texture2d(
            TEX_GREEN,
            Texture2dUpdate {
                mip_level: 0,
                array_layer: 0,
                width: 1,
                height: 1,
                bytes_per_row: 4,
                data: &[0, 255, 0, 255],
            },
        );

        // Create two identical sampler objects. The runtime should deduplicate the underlying
        // `wgpu::Sampler` and treat both IDs as equivalent for bind-group caching.
        w.create_sampler(SAMP, 0);
        w.create_sampler(SAMP_DUP, 0);
        w.create_texture2d(
            RT,
            Texture2dDesc {
                width: 2,
                height: 1,
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
                topology: PrimitiveTopology::TriangleList,
                vertex_buffers: &vbs,
                bindings: &bindings,
            },
        );
        w.begin_render_pass(RT_VIEW, [0.0, 0.0, 0.0, 1.0], None, 1.0, 0);
        w.set_pipeline(PipelineKind::Render, PIPE);
        w.set_vertex_buffer(0, VB, 0);
        w.set_bind_sampler(1, SAMP);
        w.set_bind_texture_view(0, TEX_RED_VIEW);
        w.draw(6, 1, 0, 0);
        w.set_bind_texture_view(0, TEX_GREEN_VIEW);
        w.draw(6, 1, 6, 0);
        // Repeat the same bind groups to verify caching/hit behavior.
        w.set_bind_sampler(1, SAMP_DUP);
        w.set_bind_texture_view(0, TEX_RED_VIEW);
        w.draw(6, 1, 0, 0);
        w.set_bind_texture_view(0, TEX_GREEN_VIEW);
        w.draw(6, 1, 6, 0);
        w.end_render_pass();

        rt.execute(&w.finish()).unwrap();
        rt.poll_wait();

        let stats = rt.cache_stats();
        assert_eq!(stats.samplers.misses, 1);
        assert_eq!(stats.samplers.hits, 1);
        assert_eq!(stats.samplers.entries, 1);
        assert_eq!(stats.bind_group_layouts.misses, 1);
        assert_eq!(stats.bind_groups.misses, 2);
        assert_eq!(stats.bind_groups.hits, 2);
        assert_eq!(stats.bind_groups.entries, 2);

        let pixels = rt.read_texture_rgba8(RT).await.unwrap();
        assert_eq!(pixels.len(), 2 * 4);
        assert_eq!(&pixels[0..4], &[255, 0, 0, 255]);
        assert_eq!(&pixels[4..8], &[0, 255, 0, 255]);
    });
}

#[test]
fn d3d11_compute_writes_storage_buffer() {
    pollster::block_on(async {
        let test_name = concat!(module_path!(), "::d3d11_compute_writes_storage_buffer");
        let Some(rt) = new_runtime(test_name).await else {
            return;
        };
        if !rt.supports_compute() {
            common::skip_or_panic(test_name, "compute unsupported");
            return;
        }

        let wgsl = r#"
struct Output {
    values: array<u32>,
}

@group(2) @binding(0) var<storage, read_write> out_buf: Output;

@compute @workgroup_size(64)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if (idx < 16u) {
        out_buf.values[idx] = idx * 2u + 1u;
    }
}
"#;

        let second_dispatch_offset = 256u64;
        let size = second_dispatch_offset + 16u64 * 4;
        let device = rt.device();
        let queue = rt.queue();

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("d3d11_smoke cs storage buffer shader"),
            source: wgpu::ShaderSource::Wgsl(wgsl.into()),
        });

        let empty_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("d3d11_smoke empty bind group layout"),
            entries: &[],
        });
        let group2_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("d3d11_smoke cs bind group layout (@group(2))"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: false },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("d3d11_smoke cs pipeline layout (@group(2))"),
            bind_group_layouts: &[&empty_layout, &empty_layout, &group2_layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("d3d11_smoke cs pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: "cs_main",
            compilation_options: Default::default(),
        });

        let out = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("d3d11_smoke cs out buffer"),
            size,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("d3d11_smoke cs readback buffer"),
            size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let binding_size = wgpu::BufferSize::new(16 * 4).expect("binding size");
        let bg_first = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("d3d11_smoke cs bind group (offset 0)"),
            layout: &group2_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &out,
                    offset: 0,
                    size: Some(binding_size),
                }),
            }],
        });
        let bg_second = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("d3d11_smoke cs bind group (offset 256)"),
            layout: &group2_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &out,
                    offset: second_dispatch_offset,
                    size: Some(binding_size),
                }),
            }],
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("d3d11_smoke cs encoder"),
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("d3d11_smoke cs pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(2, &bg_first, &[]);
            pass.dispatch_workgroups(1, 1, 1);
            pass.set_bind_group(2, &bg_second, &[]);
            pass.dispatch_workgroups(1, 1, 1);
        }
        encoder.copy_buffer_to_buffer(&out, 0, &readback, 0, size);
        queue.submit([encoder.finish()]);

        let bytes = read_mapped_buffer(device, &readback).await;
        let words: Vec<u32> = bytes
            .chunks_exact(4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        assert_eq!(words.len(), (size / 4) as usize);
        for i in 0..16 {
            assert_eq!(words[i], i as u32 * 2 + 1);
            assert_eq!(
                words[(second_dispatch_offset as usize / 4) + i],
                i as u32 * 2 + 1
            );
        }
    });
}

#[test]
fn d3d11_compute_writes_storage_texture() {
    pollster::block_on(async {
        let test_name = concat!(module_path!(), "::d3d11_compute_writes_storage_texture");
        let Some(rt) = new_runtime(test_name).await else {
            return;
        };
        if !rt.supports_compute() {
            common::skip_or_panic(test_name, "compute unsupported");
            return;
        }

        let wgsl = r#"
@group(2) @binding(0) var out_tex: texture_storage_2d<rgba8unorm, write>;

@compute @workgroup_size(1, 1, 1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x < 4u && gid.y < 4u) {
        textureStore(out_tex, vec2<i32>(i32(gid.x), i32(gid.y)), vec4<f32>(0.0, 0.0, 1.0, 1.0));
    }
}
"#;

        let device = rt.device();
        let queue = rt.queue();

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("d3d11_smoke cs storage texture shader"),
            source: wgpu::ShaderSource::Wgsl(wgsl.into()),
        });

        let empty_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("d3d11_smoke empty bind group layout"),
            entries: &[],
        });
        let group2_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("d3d11_smoke cs storage texture layout (@group(2))"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::StorageTexture {
                    access: wgpu::StorageTextureAccess::WriteOnly,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    view_dimension: wgpu::TextureViewDimension::D2,
                },
                count: None,
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("d3d11_smoke cs storage texture pipeline layout"),
            bind_group_layouts: &[&empty_layout, &empty_layout, &group2_layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("d3d11_smoke cs storage texture pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: "cs_main",
            compilation_options: Default::default(),
        });

        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("d3d11_smoke cs storage texture output"),
            size: wgpu::Extent3d {
                width: 4,
                height: 4,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());

        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("d3d11_smoke cs storage texture bind group"),
            layout: &group2_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            }],
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("d3d11_smoke cs storage texture encoder"),
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("d3d11_smoke cs storage texture pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(2, &bg, &[]);
            pass.dispatch_workgroups(4, 4, 1);
        }
        queue.submit([encoder.finish()]);

        let pixels = read_texture_rgba8(device, queue, &tex, 4, 4).await;
        assert_eq!(pixels.len(), 4 * 4 * 4);
        assert_eq!(&pixels[0..4], &[0, 0, 255, 255]);
        assert_eq!(&pixels[(3 * 4 + 3) * 4..(3 * 4 + 4) * 4], &[0, 0, 255, 255]);
    });
}

#[test]
fn d3d11_update_texture2d_unaligned_bytes_per_row() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::d3d11_update_texture2d_unaligned_bytes_per_row"
        );
        let Some(mut rt) = new_runtime(test_name).await else {
            return;
        };

        const TEX: u32 = 301;

        // 3 * 4 = 12 bytes/row, which is not 256-aligned and height > 1, so the executor must
        // repack before calling into WebGPU.
        let data: [u8; 24] = [
            // Row 0: red, green, blue.
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, // Row 1: white, black, magenta.
            255, 255, 255, 255, 0, 0, 0, 255, 255, 0, 255, 255,
        ];

        let mut w = CmdWriter::new();
        w.create_texture2d(
            TEX,
            Texture2dDesc {
                width: 3,
                height: 2,
                array_layers: 1,
                mip_level_count: 1,
                format: DxgiFormat::R8G8B8A8Unorm,
                usage: TextureUsage::COPY_DST | TextureUsage::COPY_SRC,
            },
        );
        w.update_texture2d(
            TEX,
            Texture2dUpdate {
                mip_level: 0,
                array_layer: 0,
                width: 3,
                height: 2,
                bytes_per_row: 12,
                data: &data,
            },
        );

        rt.execute(&w.finish()).unwrap();
        rt.poll_wait();

        let pixels = rt.read_texture_rgba8(TEX).await.unwrap();
        assert_eq!(pixels.len(), 3 * 2 * 4);
        assert_eq!(&pixels[0..4], &[255, 0, 0, 255]);
        assert_eq!(&pixels[8..12], &[0, 0, 255, 255]);
        assert_eq!(&pixels[12..16], &[255, 255, 255, 255]);
        assert_eq!(&pixels[20..24], &[255, 0, 255, 255]);
    });
}
