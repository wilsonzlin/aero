use aero_gpu::{GpuCapabilities, UploadRingBuffer, UploadRingBufferDescriptor};
use bytemuck::{Pod, Zeroable};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Vertex {
    pos: [f32; 2],
}

fn try_create_device() -> Option<(wgpu::Device, wgpu::Queue)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let needs_runtime_dir = std::env::var("XDG_RUNTIME_DIR")
            .ok()
            .map(|v| v.is_empty())
            .unwrap_or(true);

        if needs_runtime_dir {
            let dir = std::env::temp_dir()
                .join(format!("aero-gpu-xdg-runtime-{}-upload", std::process::id()));
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
            std::env::set_var("XDG_RUNTIME_DIR", &dir);
        }
    }

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        // Prefer "native" backends; this avoids noisy platform warnings from
        // initializing GL/WAYLAND stacks in headless CI environments.
        backends: wgpu::Backends::PRIMARY,
        ..Default::default()
    });

    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        compatible_surface: None,
        force_fallback_adapter: false,
    }))?;

    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("aero-gpu upload integration test"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
        },
        None,
    ))
    .ok()?;

    Some((device, queue))
}

#[test]
fn upload_each_frame_without_validation_errors() {
    let Some((device, queue)) = try_create_device() else {
        // CI environments without a usable adapter should not fail this crate.
        return;
    };

    let caps = GpuCapabilities::from_device(&device);
    let mut uploads = UploadRingBuffer::new(
        &device,
        caps,
        UploadRingBufferDescriptor {
            per_frame_size: 64 * 1024,
            frames_in_flight: 3,
            small_write_threshold: 0, // Force staging path.
            ..Default::default()
        },
    )
    .unwrap();

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("triangle shader"),
        source: wgpu::ShaderSource::Wgsl(
            r#"
            struct VsOut {
                @builtin(position) pos: vec4<f32>,
            };

            @vertex
            fn vs_main(@location(0) pos: vec2<f32>) -> VsOut {
                var out: VsOut;
                out.pos = vec4<f32>(pos, 0.0, 1.0);
                return out;
            }

            @fragment
            fn fs_main() -> @location(0) vec4<f32> {
                return vec4<f32>(1.0, 0.0, 0.0, 1.0);
            }
        "#
            .into(),
        ),
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("pipeline layout"),
        bind_group_layouts: &[],
        push_constant_ranges: &[],
    });

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: "vs_main",
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<Vertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &[wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x2,
                    offset: 0,
                    shader_location: 0,
                }],
            }],
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: "fs_main",
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::Rgba8Unorm,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
    });

    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("render target"),
        size: wgpu::Extent3d {
            width: 4,
            height: 4,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());

    for frame in 0..20 {
        uploads.begin_frame();

        // Upload a changing triangle to stress the allocator.
        let shift = frame as f32 * 0.001;
        let verts = [
            Vertex {
                pos: [-0.5 + shift, -0.5],
            },
            Vertex {
                pos: [0.0 + shift, 0.5],
            },
            Vertex {
                pos: [0.5 + shift, -0.5],
            },
        ];
        let vb = uploads.write_slice(&device, &queue, &verts).unwrap();

        let flush_cmd = uploads.flush_staged_writes();

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("render encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("render pass"),
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
            pass.set_vertex_buffer(0, vb.slice());
            pass.draw(0..3, 0..1);
        }

        let render_cmd = encoder.finish();

        if let Some(flush_cmd) = flush_cmd {
            queue.submit([flush_cmd, render_cmd]);
        } else {
            queue.submit([render_cmd]);
        }

        device.poll(wgpu::Maintain::Poll);
        uploads.recall();
    }
}
