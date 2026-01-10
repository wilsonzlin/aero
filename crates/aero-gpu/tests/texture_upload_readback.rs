use std::borrow::Cow;

use aero_gpu::{
    readback_rgba8, GpuCapabilities, SamplerDesc, TextureDesc, TextureFormat, TextureManager,
    TextureRegion, TextureViewDesc,
};

const BLIT_WGSL: &str = r#"
@group(0) @binding(0) var src_tex: texture_2d<f32>;
@group(0) @binding(1) var src_samp: sampler;

@vertex
fn vs_main(@builtin(vertex_index) idx: u32) -> @builtin(position) vec4<f32> {
    // Fullscreen triangle.
    var pos = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -3.0),
        vec2<f32>( 3.0,  1.0),
        vec2<f32>(-1.0,  1.0),
    );
    return vec4<f32>(pos[idx], 0.0, 1.0);
}

@fragment
fn fs_main(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    // `p.xy` is in pixel space with a +0.5 offset (sample center).
    let dims = vec2<f32>(textureDimensions(src_tex, 0u));
    let uv = p.xy / dims;
    return textureSampleLevel(src_tex, src_samp, uv, 0.0);
}
"#;

#[test]
fn upload_blit_readback_roundtrip() {
    pollster::block_on(async {
        #[cfg(target_os = "linux")]
        {
            // Some Vulkan loaders expect XDG_RUNTIME_DIR to exist and be usable. CI containers
            // commonly omit it, which can result in noisy-but-benign stderr output.
            if std::env::var_os("XDG_RUNTIME_DIR").is_none() {
                use std::os::unix::fs::PermissionsExt;

                let dir = std::env::temp_dir().join(format!(
                    "aero_gpu_xdg_runtime_{}_{}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos()
                ));
                let _ = std::fs::create_dir_all(&dir);
                let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
                std::env::set_var("XDG_RUNTIME_DIR", &dir);
            }
        }

        let instance = wgpu::Instance::default();
        let adapter = match instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await
        {
            Some(adapter) => adapter,
            None => {
                // Headless CI environments can legitimately have no usable adapter.
                return;
            }
        };

        let (device, queue) = match adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: None,
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::downlevel_defaults(),
                },
                None,
            )
            .await
        {
            Ok(pair) => pair,
            Err(_) => return,
        };

        // Force BC fallback to exercise CPU decompression + RGBA8 upload.
        let caps = GpuCapabilities {
            supports_bc_texture_compression: false,
            ..GpuCapabilities::from_device(&device)
        };
        let mut textures = TextureManager::new(&device, &queue, caps);

        let tex_key = 0x1234_u64;
        textures.create_texture(
            tex_key,
            TextureDesc::new_2d(
                4,
                4,
                TextureFormat::Bc1RgbaUnorm,
                wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            ),
        );

        // BC1 block: white (idx0) for top half, black (idx1) for bottom half.
        // color0=0xffff (white), color1=0x0000 (black), indices=0x55550000 LE.
        let bc1 = [
            0xff, 0xff, // color0
            0x00, 0x00, // color1
            0x00, 0x00, 0x55, 0x55, // indices
        ];
        textures.write_texture(tex_key, &bc1).unwrap();

        // Cache hits should be observable through stats.
        let _ = textures.view(tex_key, TextureViewDesc::default()).unwrap();
        let _ = textures.view(tex_key, TextureViewDesc::default()).unwrap();
        assert_eq!(textures.stats().views_created, 1);
        assert_eq!(textures.stats().view_cache_hits, 1);

        let samp_desc = SamplerDesc {
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..SamplerDesc::default()
        };
        let _ = textures.sampler(samp_desc);
        let _ = textures.sampler(samp_desc);
        assert_eq!(textures.stats().samplers_created, 1);
        assert_eq!(textures.stats().sampler_cache_hits, 1);

        let src_view = textures.view(tex_key, TextureViewDesc::default()).unwrap();
        let src_samp = textures.sampler(samp_desc);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("blit"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(BLIT_WGSL)),
        });

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blit.bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blit.bg"),
            layout: &bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&src_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&src_samp),
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("blit.pl"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });

        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("target"),
            size: wgpu::Extent3d {
                width: 4,
                height: 4,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("blit.pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_main",
                compilation_options: Default::default(),
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

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("blit.encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("blit.pass"),
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
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        queue.submit([encoder.finish()]);

        // Read back and validate.
        let rgba = readback_rgba8(
            &device,
            &queue,
            &target,
            TextureRegion::full(wgpu::Extent3d {
                width: 4,
                height: 4,
                depth_or_array_layers: 1,
            }),
        )
        .await;

        // Pixel (0,0) should be white.
        assert_eq!(&rgba[0..4], &[255, 255, 255, 255]);
        // Pixel (0,3) should be black (bottom-left).
        let bottom_left = (3 * 4 * 4) as usize;
        assert_eq!(&rgba[bottom_left..bottom_left + 4], &[0, 0, 0, 255]);
    });
}
