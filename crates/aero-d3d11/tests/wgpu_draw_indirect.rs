mod common;

use std::borrow::Cow;

use aero_d3d11::runtime::indirect_args::DrawIndirectArgs;
use anyhow::{anyhow, Context, Result};

async fn create_device_queue() -> Result<(wgpu::Device, wgpu::Queue, bool)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let needs_runtime_dir = std::env::var("XDG_RUNTIME_DIR")
            .ok()
            .map(|v| v.is_empty())
            .unwrap_or(true);

        if needs_runtime_dir {
            let dir =
                std::env::temp_dir().join(format!("aero-d3d11-xdg-runtime-{}", std::process::id()));
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
            std::env::set_var("XDG_RUNTIME_DIR", &dir);
        }
    }

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        // Prefer GL on Linux CI to avoid crashes in some Vulkan software adapters.
        backends: if cfg!(target_os = "linux") {
            wgpu::Backends::GL
        } else {
            // Prefer "native" backends; this avoids noisy platform warnings from
            // initializing GL/WAYLAND stacks in headless CI environments.
            wgpu::Backends::PRIMARY
        },
        ..Default::default()
    });

    let adapter = match instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: true,
        })
        .await
    {
        Some(adapter) => Some(adapter),
        None => {
            instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::LowPower,
                    compatible_surface: None,
                    force_fallback_adapter: false,
                })
                .await
        }
    }
    .ok_or_else(|| anyhow!("wgpu: no suitable adapter found"))?;

    let supports_compute = adapter
        .get_downlevel_capabilities()
        .flags
        .contains(wgpu::DownlevelFlags::COMPUTE_SHADERS);

    let (device, queue) = adapter
        .request_device(
            &wgpu::DeviceDescriptor {
                label: Some("aero-d3d11 draw_indirect test device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults(),
            },
            None,
        )
        .await
        .map_err(|e| anyhow!("wgpu: request_device failed: {e:?}"))?;

    Ok((device, queue, supports_compute))
}

async fn read_texture_rgba8(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    width: u32,
    height: u32,
) -> Result<Vec<u8>> {
    let bytes_per_pixel = 4u32;
    let unpadded_bytes_per_row = width
        .checked_mul(bytes_per_pixel)
        .ok_or_else(|| anyhow!("bytes_per_row overflow"))?;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(align) * align;
    let buffer_size = padded_bytes_per_row as u64 * height as u64;

    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("aero-d3d11 draw_indirect readback staging"),
        size: buffer_size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("aero-d3d11 draw_indirect readback encoder"),
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
        .ok_or_else(|| anyhow!("wgpu: map_async dropped"))?
        .context("wgpu: map_async failed")?;

    let mapped = slice.get_mapped_range();
    let mut out = Vec::with_capacity((unpadded_bytes_per_row * height) as usize);
    for row in 0..height as usize {
        let start = row * padded_bytes_per_row as usize;
        out.extend_from_slice(&mapped[start..start + unpadded_bytes_per_row as usize]);
    }
    drop(mapped);
    staging.unmap();
    Ok(out)
}

#[test]
fn wgpu_draw_indirect_uses_args_written_by_compute() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::wgpu_draw_indirect_uses_args_written_by_compute"
        );

        let (device, queue, supports_compute) = match create_device_queue().await {
            Ok(v) => v,
            Err(err) => {
                common::skip_or_panic(test_name, &format!("wgpu unavailable ({err:#})"));
                return;
            }
        };
        if !supports_compute {
            common::skip_or_panic(test_name, "compute unsupported");
            return;
        }

        let (args_size, args_align) = DrawIndirectArgs::layout();
        assert_eq!(args_size, 16);
        assert_eq!(args_align, 4);

        let args_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aero-d3d11 draw_indirect args buffer"),
            size: args_size,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::INDIRECT
                | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });

        let cs_wgsl = r#"
            struct DrawArgs {
                vertex_count: u32,
                instance_count: u32,
                first_vertex: u32,
                first_instance: u32,
            };

            @group(0) @binding(0)
            var<storage, read_write> args: DrawArgs;

            @compute @workgroup_size(1)
            fn main() {
                args.vertex_count = 3u;
                args.instance_count = 1u;
                args.first_vertex = 0u;
                args.first_instance = 0u;
            }
        "#;

        let cs_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("aero-d3d11 draw_indirect args compute"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(cs_wgsl)),
        });

        let cs_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("aero-d3d11 draw_indirect compute bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: false },
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(args_size),
                },
                count: None,
            }],
        });

        let cs_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("aero-d3d11 draw_indirect compute bg"),
            layout: &cs_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: args_buffer.as_entire_binding(),
            }],
        });

        let cs_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("aero-d3d11 draw_indirect compute pl"),
            bind_group_layouts: &[&cs_bgl],
            push_constant_ranges: &[],
        });

        let cs_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("aero-d3d11 draw_indirect compute pipeline"),
            layout: Some(&cs_pl),
            module: &cs_module,
            entry_point: "main",
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        });

        let rs_wgsl = r#"
            @vertex
            fn vs_main(@builtin(vertex_index) vid: u32) -> @builtin(position) vec4<f32> {
                var pos = array<vec2<f32>, 3>(
                    vec2<f32>(-1.0, -1.0),
                    vec2<f32>(3.0, -1.0),
                    vec2<f32>(-1.0, 3.0),
                );
                let p = pos[vid];
                return vec4<f32>(p, 0.0, 1.0);
            }

            @fragment
            fn fs_main() -> @location(0) vec4<f32> {
                return vec4<f32>(1.0, 0.0, 0.0, 1.0);
            }
        "#;

        let rs_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("aero-d3d11 draw_indirect render shader"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(rs_wgsl)),
        });

        let rt_format = wgpu::TextureFormat::Rgba8Unorm;
        let render_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("aero-d3d11 draw_indirect render pl"),
            bind_group_layouts: &[],
            push_constant_ranges: &[],
        });

        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("aero-d3d11 draw_indirect render pipeline"),
            layout: Some(&render_pl),
            vertex: wgpu::VertexState {
                module: &rs_module,
                entry_point: "vs_main",
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &rs_module,
                entry_point: "fs_main",
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: rt_format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });

        let (width, height) = (4u32, 4u32);
        let rt = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("aero-d3d11 draw_indirect rt"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: rt_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let rt_view = rt.create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("aero-d3d11 draw_indirect encoder"),
        });

        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("aero-d3d11 draw_indirect compute pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&cs_pipeline);
            pass.set_bind_group(0, &cs_bg, &[]);
            pass.dispatch_workgroups(1, 1, 1);
        }

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("aero-d3d11 draw_indirect render pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &rt_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.0,
                            g: 0.0,
                            b: 0.0,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&render_pipeline);
            pass.draw_indirect(&args_buffer, 0);
        }

        queue.submit([encoder.finish()]);

        let pixels = read_texture_rgba8(&device, &queue, &rt, width, height)
            .await
            .expect("read back render target");
        assert_eq!(pixels.len(), (width * height * 4) as usize);

        // Sample a center pixel. The full-screen triangle should cover the entire render target.
        let x = width / 2;
        let y = height / 2;
        let idx = ((y * width + x) * 4) as usize;
        assert_eq!(&pixels[idx..idx + 4], &[255, 0, 0, 255]);
    });
}
