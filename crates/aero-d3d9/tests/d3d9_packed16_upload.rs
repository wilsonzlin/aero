use aero_d3d9::resources::*;

fn require_webgpu() -> bool {
    std::env::var("AERO_REQUIRE_WEBGPU")
        .ok()
        .map(|raw| {
            let v = raw.trim();
            v == "1"
                || v.eq_ignore_ascii_case("true")
                || v.eq_ignore_ascii_case("yes")
                || v.eq_ignore_ascii_case("on")
        })
        .unwrap_or(false)
}

fn skip_or_panic(test_name: &str, reason: &str) {
    if require_webgpu() {
        panic!("AERO_REQUIRE_WEBGPU is enabled but {test_name} cannot run: {reason}");
    }
    eprintln!("skipping {test_name}: {reason}");
}

fn ensure_xdg_runtime_dir() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let needs_runtime_dir = std::env::var("XDG_RUNTIME_DIR")
            .ok()
            .map(|v| v.is_empty())
            .unwrap_or(true);
        if !needs_runtime_dir {
            return;
        }

        let dir = std::env::temp_dir().join(format!(
            "aero-d3d9-xdg-runtime-{}-packed16-upload",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        std::env::set_var("XDG_RUNTIME_DIR", &dir);
    }
}

async fn request_device() -> Option<(wgpu::Device, wgpu::Queue)> {
    ensure_xdg_runtime_dir();

    // Prefer GL on Linux CI to avoid crashes in some Vulkan software adapters.
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: if cfg!(target_os = "linux") {
            wgpu::Backends::GL
        } else {
            wgpu::Backends::all()
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
    }?;

    adapter
        .request_device(
            &wgpu::DeviceDescriptor {
                label: Some("aero-d3d9 packed16 upload test device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults(),
            },
            None,
        )
        .await
        .ok()
}

fn readback_rgba8(
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

    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback-buffer"),
        size: buffer_size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("readback-encoder"),
    });

    encoder.copy_texture_to_buffer(
        wgpu::ImageCopyTexture {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::ImageCopyBuffer {
            buffer: &buffer,
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

    let slice = buffer.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        tx.send(result).ok();
    });
    #[cfg(not(target_arch = "wasm32"))]
    device.poll(wgpu::Maintain::Wait);
    rx.recv()
        .expect("map_async callback dropped")
        .expect("map_async failed");

    let data = slice.get_mapped_range();
    let mut out = vec![0u8; (width * height * bytes_per_pixel) as usize];
    for y in 0..height as usize {
        let src_start = y * padded_bytes_per_row as usize;
        let dst_start = y * unpadded_bytes_per_row as usize;
        out[dst_start..dst_start + unpadded_bytes_per_row as usize]
            .copy_from_slice(&data[src_start..src_start + unpadded_bytes_per_row as usize]);
    }
    drop(data);
    buffer.unmap();

    out
}

fn pixel_at_rgba(pixels: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let idx = ((y * width + x) * 4) as usize;
    [
        pixels[idx],
        pixels[idx + 1],
        pixels[idx + 2],
        pixels[idx + 3],
    ]
}

fn assert_rgba_approx(actual: [u8; 4], expected: [u8; 4], tolerance: u8) {
    for (a, e) in actual.into_iter().zip(expected) {
        let diff = a.abs_diff(e);
        assert!(
            diff <= tolerance,
            "component mismatch: actual={actual:?} expected={expected:?} tolerance={tolerance}",
        );
    }
}

const SAMPLE_SHADER: &str = r#"
struct VsOut {
    @builtin(position) pos: vec4<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) idx: u32) -> VsOut {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0),
        vec2<f32>(-1.0,  3.0),
    );
    let p = positions[idx];
    var out: VsOut;
    out.pos = vec4<f32>(p, 0.0, 1.0);
    return out;
}

@group(0) @binding(0) var t: texture_2d<f32>;

// Copy texel (x,y) to the output pixel (x,y).
@fragment
fn fs_main(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let x = i32(pos.x);
    let y = i32(pos.y);
    return textureLoad(t, vec2<i32>(x, y), 0);
}
"#;

const SAMPLE_CUBE_SHADER: &str = r#"
struct VsOut {
    @builtin(position) pos: vec4<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) idx: u32) -> VsOut {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0),
        vec2<f32>(-1.0,  3.0),
    );
    let p = positions[idx];
    var out: VsOut;
    out.pos = vec4<f32>(p, 0.0, 1.0);
    return out;
}

@group(0) @binding(0) var t: texture_cube<f32>;
@group(0) @binding(1) var s: sampler;

// Output 6 pixels in a row, each sampling one face at its axis direction.
// Cube layer order is +X, -X, +Y, -Y, +Z, -Z (WebGPU spec).
@fragment
fn fs_main(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let x = i32(pos.x);
    var dir: vec3<f32>;
    switch x {
        case 0: { dir = vec3<f32>( 1.0,  0.0,  0.0); }
        case 1: { dir = vec3<f32>(-1.0,  0.0,  0.0); }
        case 2: { dir = vec3<f32>( 0.0,  1.0,  0.0); }
        case 3: { dir = vec3<f32>( 0.0, -1.0,  0.0); }
        case 4: { dir = vec3<f32>( 0.0,  0.0,  1.0); }
        default: { dir = vec3<f32>( 0.0,  0.0, -1.0); }
    }
    return textureSampleLevel(t, s, dir, 0.0);
}
"#;

#[test]
fn d3d9_packed16_upload_and_sample() {
    let (device, queue) = match pollster::block_on(request_device()) {
        Some(device) => device,
        None => {
            skip_or_panic(module_path!(), "wgpu adapter/device not available");
            return;
        }
    };

    let mut rm = ResourceManager::new(device, queue, ResourceManagerOptions::default());
    rm.begin_frame();

    let shader = rm
        .device()
        .create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("packed16 sample shader"),
            source: wgpu::ShaderSource::Wgsl(SAMPLE_SHADER.into()),
        });

    let bgl = rm
        .device()
        .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                },
                count: None,
            }],
        });

    let pipeline_layout = rm
        .device()
        .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pl"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });

    let pipeline = rm
        .device()
        .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("sample pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });

    struct Case {
        fmt: D3DFormat,
        pixels: [u16; 4],
        expected: [[u8; 4]; 4],
    }

    // Pixel order is row-major, top-to-bottom:
    //   (0,0) (1,0)
    //   (0,1) (1,1)
    let cases = [
        Case {
            fmt: D3DFormat::R5G6B5,
            // red, green, blue, white
            pixels: [0xF800, 0x07E0, 0x001F, 0xFFFF],
            expected: [
                [255, 0, 0, 255],
                [0, 255, 0, 255],
                [0, 0, 255, 255],
                [255, 255, 255, 255],
            ],
        },
        Case {
            fmt: D3DFormat::A1R5G5B5,
            // Opaque/transparent mix:
            // - top-left:   opaque red
            // - top-right:  transparent green
            // - bottom-left opaque blue
            // - bottom-right transparent white
            pixels: [
                0xFC00, // A=1 R=31 G=0 B=0
                0x03E0, // A=0 R=0  G=31 B=0
                0x801F, // A=1 R=0  G=0  B=31
                0x7FFF, // A=0 R=31 G=31 B=31
            ],
            expected: [
                [255, 0, 0, 255],
                [0, 255, 0, 0],
                [0, 0, 255, 255],
                [255, 255, 255, 0],
            ],
        },
        Case {
            fmt: D3DFormat::X1R5G5B5,
            // Same colors as above, but with the unused top bit deliberately varied. Alpha must
            // still be treated as 1.0.
            pixels: [0xFC00, 0x83E0, 0x801F, 0xFFFF],
            expected: [
                [255, 0, 0, 255],
                [0, 255, 0, 255],
                [0, 0, 255, 255],
                [255, 255, 255, 255],
            ],
        },
        Case {
            fmt: D3DFormat::A4R4G4B4,
            // Exercise 4-bit alpha expansion:
            // - opaque red
            // - alpha=0x8 green (-> 0x88)
            // - alpha=0x4 blue (-> 0x44)
            // - transparent white
            pixels: [0xFF00, 0x80F0, 0x400F, 0x0FFF],
            expected: [
                [255, 0, 0, 255],
                [0, 255, 0, 0x88],
                [0, 0, 255, 0x44],
                [255, 255, 255, 0],
            ],
        },
    ];

    for (i, case) in cases.into_iter().enumerate() {
        let id = 100 + i as u32;
        rm.create_texture(
            id,
            TextureDesc {
                kind: TextureKind::Texture2D {
                    width: 2,
                    height: 2,
                    levels: 1,
                },
                format: case.fmt,
                pool: D3DPool::Default,
                usage: TextureUsageKind::Sampled,
            },
        )
        .unwrap();

        let mut packed = Vec::with_capacity(case.pixels.len() * 2);
        for p in case.pixels {
            packed.extend_from_slice(&p.to_le_bytes());
        }

        {
            let locked = rm.lock_texture_rect(id, 0, 0, LockFlags::empty()).unwrap();
            assert_eq!(locked.pitch, 4); // 2px * 2 bytes/px
            assert_eq!(locked.data.len(), packed.len());
            locked.data.copy_from_slice(&packed);
        }
        rm.unlock_texture_rect(id).unwrap();

        // Acquire the texture view now (requires &mut ResourceManager) so we can build GPU state
        // without holding a long-lived borrow of `rm`.
        let view = rm.texture_view(id).unwrap();

        let bg = rm.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bg"),
            layout: &bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            }],
        });

        let out_tex = rm.device().create_texture(&wgpu::TextureDescriptor {
            label: Some("out"),
            size: wgpu::Extent3d {
                width: 2,
                height: 2,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let out_view = out_tex.create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = rm
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        rm.encode_uploads(&mut encoder);

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &out_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.draw(0..3, 0..1);
        }

        rm.submit(encoder);

        let pixels = readback_rgba8(rm.device(), rm.queue(), &out_tex, 2, 2);

        for (j, expected) in case.expected.into_iter().enumerate() {
            let x = (j % 2) as u32;
            let y = (j / 2) as u32;
            assert_rgba_approx(pixel_at_rgba(&pixels, 2, x, y), expected, 2);
        }

        assert!(rm.destroy_texture(id));
    }
}

#[test]
fn d3d9_packed16_cube_upload_and_sample() {
    let (device, queue) = match pollster::block_on(request_device()) {
        Some(device) => device,
        None => {
            skip_or_panic(module_path!(), "wgpu adapter/device not available");
            return;
        }
    };

    let mut rm = ResourceManager::new(device, queue, ResourceManagerOptions::default());
    rm.begin_frame();

    let shader = rm
        .device()
        .create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("packed16 cube sample shader"),
            source: wgpu::ShaderSource::Wgsl(SAMPLE_CUBE_SHADER.into()),
        });

    let sampler = rm.device().create_sampler(&wgpu::SamplerDescriptor {
        label: Some("cube sampler"),
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        mipmap_filter: wgpu::FilterMode::Nearest,
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        ..Default::default()
    });

    let bgl = rm
        .device()
        .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("cube bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        multisampled: false,
                        view_dimension: wgpu::TextureViewDimension::Cube,
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
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

    let pipeline_layout = rm
        .device()
        .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("cube pipeline layout"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });

    let pipeline = rm
        .device()
        .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("cube sample pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });

    const TEX: GuestResourceId = 100;
    rm.create_texture(
        TEX,
        TextureDesc {
            kind: TextureKind::Cube { size: 1, levels: 1 },
            format: D3DFormat::A1R5G5B5,
            pool: D3DPool::Default,
            usage: TextureUsageKind::Sampled,
        },
    )
    .unwrap();

    // Fill cube faces (array layers) in WebGPU order: +X, -X, +Y, -Y, +Z, -Z.
    // Use distinct colors + alpha to validate A1 handling as well.
    let faces: [(u16, [u8; 4]); 6] = [
        (0xFC00, [255, 0, 0, 255]),   // +X: opaque red
        (0x03E0, [0, 255, 0, 0]),     // -X: transparent green
        (0x801F, [0, 0, 255, 255]),   // +Y: opaque blue
        (0x7FFF, [255, 255, 255, 0]), // -Y: transparent white
        (0x8000, [0, 0, 0, 255]),     // +Z: opaque black
        (0x7C1F, [255, 0, 255, 0]),   // -Z: transparent magenta
    ];

    for (layer, (pixel, _expected)) in faces.iter().enumerate() {
        let packed = pixel.to_le_bytes();
        let locked = rm
            .lock_texture_rect(TEX, 0, layer as u32, LockFlags::empty())
            .unwrap();
        assert_eq!(locked.pitch, 2);
        assert_eq!(locked.data.len(), 2);
        locked.data.copy_from_slice(&packed);
        rm.unlock_texture_rect(TEX).unwrap();
    }

    let view = rm.texture_view(TEX).unwrap();

    let bg = rm.device().create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("cube bg"),
        layout: &bgl,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
        ],
    });

    let out_tex = rm.device().create_texture(&wgpu::TextureDescriptor {
        label: Some("out cube"),
        size: wgpu::Extent3d {
            width: 6,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let out_view = out_tex.create_view(&wgpu::TextureViewDescriptor::default());

    let mut encoder = rm
        .device()
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    rm.encode_uploads(&mut encoder);

    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("cube pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &out_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            occlusion_query_set: None,
            timestamp_writes: None,
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bg, &[]);
        pass.draw(0..3, 0..1);
    }

    rm.submit(encoder);

    let pixels = readback_rgba8(rm.device(), rm.queue(), &out_tex, 6, 1);
    for (i, (_pixel, expected)) in faces.into_iter().enumerate() {
        assert_rgba_approx(pixel_at_rgba(&pixels, 6, i as u32, 0), expected, 2);
    }

    assert!(rm.destroy_texture(TEX));
}

#[test]
fn d3d9_packed16_managed_eviction_reuploads_shadow_data() {
    let (device, queue) = match pollster::block_on(request_device()) {
        Some(device) => device,
        None => {
            skip_or_panic(module_path!(), "wgpu adapter/device not available");
            return;
        }
    };

    let mut rm = ResourceManager::new(device, queue, ResourceManagerOptions::default());
    rm.begin_frame();

    let shader = rm
        .device()
        .create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("packed16 sample shader (managed eviction)"),
            source: wgpu::ShaderSource::Wgsl(SAMPLE_SHADER.into()),
        });

    let bgl = rm
        .device()
        .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                },
                count: None,
            }],
        });

    let pipeline_layout = rm
        .device()
        .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pl"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });

    let pipeline = rm
        .device()
        .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("sample pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });

    const TEX: GuestResourceId = 1234;
    rm.create_texture(
        TEX,
        TextureDesc {
            kind: TextureKind::Texture2D {
                width: 2,
                height: 2,
                levels: 1,
            },
            format: D3DFormat::R5G6B5,
            pool: D3DPool::Managed,
            usage: TextureUsageKind::Sampled,
        },
    )
    .unwrap();

    // Upload red, green, blue, white.
    let pixels: [u16; 4] = [0xF800, 0x07E0, 0x001F, 0xFFFF];
    let mut packed = Vec::with_capacity(pixels.len() * 2);
    for p in pixels {
        packed.extend_from_slice(&p.to_le_bytes());
    }
    {
        let locked = rm.lock_texture_rect(TEX, 0, 0, LockFlags::empty()).unwrap();
        assert_eq!(locked.pitch, 4);
        locked.data.copy_from_slice(&packed);
    }
    rm.unlock_texture_rect(TEX).unwrap();

    // Flush the initial upload so we don't have stale upload ops referencing the soon-to-be-evicted
    // GPU texture.
    {
        let mut encoder = rm
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        rm.encode_uploads(&mut encoder);
        rm.submit(encoder);
    }

    // Evict the GPU texture backing; this must keep the shadow copy for managed textures.
    assert!(rm.texture_mut(TEX).unwrap().evict_gpu());

    // Re-acquire view (recreates texture + reuploads from shadow).
    let view = rm.texture_view(TEX).unwrap();
    let bg = rm.device().create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("bg"),
        layout: &bgl,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::TextureView(&view),
        }],
    });

    let out_tex = rm.device().create_texture(&wgpu::TextureDescriptor {
        label: Some("out"),
        size: wgpu::Extent3d {
            width: 2,
            height: 2,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let out_view = out_tex.create_view(&wgpu::TextureViewDescriptor::default());

    let mut encoder = rm
        .device()
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    rm.encode_uploads(&mut encoder);

    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &out_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            occlusion_query_set: None,
            timestamp_writes: None,
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bg, &[]);
        pass.draw(0..3, 0..1);
    }

    rm.submit(encoder);

    let pixels = readback_rgba8(rm.device(), rm.queue(), &out_tex, 2, 2);
    // Allow a tiny tolerance; main correctness is that the content survives eviction via shadow
    // reupload.
    assert_rgba_approx(pixel_at_rgba(&pixels, 2, 0, 0), [255, 0, 0, 255], 2);
    assert_rgba_approx(pixel_at_rgba(&pixels, 2, 1, 0), [0, 255, 0, 255], 2);
    assert_rgba_approx(pixel_at_rgba(&pixels, 2, 0, 1), [0, 0, 255, 255], 2);
    assert_rgba_approx(pixel_at_rgba(&pixels, 2, 1, 1), [255, 255, 255, 255], 2);
}
