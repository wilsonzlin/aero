use std::sync::mpsc;

use aero_webgpu::WebGpuContext;

// Smoke test: render a solid red fullscreen triangle into an offscreen texture and read back a pixel.
//
// The test is written to gracefully skip on environments that don't expose a usable adapter
// (e.g. CI containers without Vulkan/GL drivers).
#[test]
fn headless_webgpu_triangle_renders() {
    pollster::block_on(async {
        let ctx = match WebGpuContext::request_headless(Default::default()).await {
            Ok(ctx) => ctx,
            Err(err) => {
                eprintln!("skipping WebGPU smoke test: {err}");
                return;
            }
        };

        let device = ctx.device();
        let queue = ctx.queue();

        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let size = 64u32;
        let extent = wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        };

        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("smoke target"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("smoke shader"),
            source: wgpu::ShaderSource::Wgsl(SMOKE_WGSL.into()),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("smoke pipeline layout"),
            bind_group_layouts: &[],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("smoke pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });

        let bytes_per_row = size * 4; // 64*4 == 256, already aligned.
        assert_eq!(bytes_per_row % wgpu::COPY_BYTES_PER_ROW_ALIGNMENT, 0);

        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("smoke readback"),
            size: (bytes_per_row * size) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("smoke encoder"),
        });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("smoke pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&pipeline);
            pass.draw(0..3, 0..1);
        }

        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture: &target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &readback,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(bytes_per_row),
                    rows_per_image: Some(size),
                },
            },
            extent,
        );

        queue.submit(Some(encoder.finish()));

        // Map and wait.
        let slice = readback.slice(..);
        let (tx, rx) = mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |res| {
            tx.send(res).ok();
        });
        device.poll(wgpu::Maintain::Wait);
        rx.recv().expect("map callback").expect("map ok");

        let data = slice.get_mapped_range();
        let x = size / 2;
        let y = size / 2;
        let idx = (y * bytes_per_row + x * 4) as usize;
        let px = &data[idx..idx + 4];

        // Expect opaque red (sRGB render target, but 1.0 stays 255).
        assert!(px[0] >= 250, "R channel too low: {}", px[0]);
        assert!(px[1] <= 5, "G channel too high: {}", px[1]);
        assert!(px[2] <= 5, "B channel too high: {}", px[2]);
        assert_eq!(px[3], 255, "A channel not opaque: {}", px[3]);

        drop(data);
        readback.unmap();
    });
}

const SMOKE_WGSL: &str = r#"
@vertex
fn vs_main(@builtin(vertex_index) idx: u32) -> @builtin(position) vec4<f32> {
    var pos = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0),
        vec2<f32>(-1.0,  3.0),
    );
    return vec4<f32>(pos[idx], 0.0, 1.0);
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return vec4<f32>(1.0, 0.0, 0.0, 1.0);
}
"#;
