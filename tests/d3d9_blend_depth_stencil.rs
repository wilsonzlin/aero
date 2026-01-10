use aero_d3d9::state::tracker::{
    BlendFactor, BlendOp, ColorWriteMask, CompareFunc, CullMode, RasterizerState, StencilOp,
};
use aero_d3d9::state::{
    translate_pipeline_state, D3DPrimitiveType, PipelineCache, ShaderKey, StateTracker,
    VertexAttributeKey, VertexBufferLayoutKey,
};
use bytemuck::{Pod, Zeroable};
use std::borrow::Cow;
use wgpu::util::DeviceExt;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Vertex {
    pos: [f32; 3],
    color: [f32; 4],
}

fn quad_vertices(z: f32, color: [f32; 4]) -> [Vertex; 6] {
    let p0 = [-1.0, -1.0, z];
    let p1 = [1.0, -1.0, z];
    let p2 = [-1.0, 1.0, z];
    let p3 = [1.0, 1.0, z];

    [
        Vertex { pos: p0, color },
        Vertex { pos: p1, color },
        Vertex { pos: p2, color },
        Vertex { pos: p2, color },
        Vertex { pos: p1, color },
        Vertex { pos: p3, color },
    ]
}

fn left_half_triangle(z: f32, color: [f32; 4]) -> [Vertex; 3] {
    [
        Vertex {
            pos: [-1.0, -1.0, z],
            color,
        },
        Vertex {
            pos: [0.0, -1.0, z],
            color,
        },
        Vertex {
            pos: [-1.0, 1.0, z],
            color,
        },
    ]
}

fn create_device() -> Option<(wgpu::Device, wgpu::Queue)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let needs_runtime_dir = std::env::var("XDG_RUNTIME_DIR")
            .ok()
            .map(|v| v.is_empty())
            .unwrap_or(true);

        if needs_runtime_dir {
            let dir = std::env::temp_dir().join(format!(
                "aero-d3d9-xdg-runtime-{}-blend-depth-stencil",
                std::process::id()
            ));
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
            std::env::set_var("XDG_RUNTIME_DIR", &dir);
        }
    }

    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        compatible_surface: None,
        force_fallback_adapter: true,
    }))
    .or_else(|| {
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
    })?;

    pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("aero-d3d9 blend/depth/stencil tests"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
        },
        None,
    ))
    .ok()
}

fn create_shader_modules(device: &wgpu::Device) -> (wgpu::ShaderModule, wgpu::ShaderModule) {
    const SHADER: &str = r#"
struct VsOut {
  @builtin(position) pos: vec4<f32>,
  @location(0) color: vec4<f32>,
}

@vertex
fn vs_main(@location(0) pos: vec3<f32>, @location(1) color: vec4<f32>) -> VsOut {
  var out: VsOut;
  out.pos = vec4<f32>(pos, 1.0);
  out.color = color;
  return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
  return in.color;
}
"#;

    let vs = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("vs"),
        source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(SHADER)),
    });
    let fs = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("fs"),
        source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(SHADER)),
    });
    (vs, fs)
}

fn vertex_layout() -> Vec<VertexBufferLayoutKey> {
    vec![VertexBufferLayoutKey {
        array_stride: std::mem::size_of::<Vertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: vec![
            VertexAttributeKey {
                format: wgpu::VertexFormat::Float32x3,
                offset: 0,
                shader_location: 0,
            },
            VertexAttributeKey {
                format: wgpu::VertexFormat::Float32x4,
                offset: 12,
                shader_location: 1,
            },
        ],
    }]
}

fn build_wgpu_vertex_layouts<'a>(
    layout_keys: &[VertexBufferLayoutKey],
    attribute_storage: &'a mut Vec<Vec<wgpu::VertexAttribute>>,
) -> Vec<wgpu::VertexBufferLayout<'a>> {
    attribute_storage.clear();
    attribute_storage.extend(layout_keys.iter().map(|layout| {
        layout
            .attributes
            .iter()
            .map(|attr| wgpu::VertexAttribute {
                format: attr.format,
                offset: attr.offset,
                shader_location: attr.shader_location,
            })
            .collect::<Vec<_>>()
    }));

    layout_keys
        .iter()
        .zip(attribute_storage.iter())
        .map(|(layout, attrs)| wgpu::VertexBufferLayout {
            array_stride: layout.array_stride,
            step_mode: layout.step_mode,
            attributes: attrs.as_slice(),
        })
        .collect()
}

fn read_texture_rgba8(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    width: u32,
    height: u32,
) -> Vec<u8> {
    let bytes_per_pixel = 4;
    let bytes_per_row = width * bytes_per_pixel;
    assert_eq!(bytes_per_row % wgpu::COPY_BYTES_PER_ROW_ALIGNMENT, 0);

    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback buffer"),
        size: (bytes_per_row * height) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("readback encoder"),
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
                bytes_per_row: Some(bytes_per_row),
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
    slice.map_async(wgpu::MapMode::Read, move |res| tx.send(res).unwrap());
    device.poll(wgpu::Maintain::Wait);
    rx.recv().unwrap().unwrap();

    let data = slice.get_mapped_range().to_vec();
    buffer.unmap();
    data
}

fn assert_rgba_near(actual: [u8; 4], expected: [u8; 4], tolerance: u8) {
    for i in 0..4 {
        let a = actual[i] as i16;
        let e = expected[i] as i16;
        let diff = (a - e).abs() as u8;
        assert!(
            diff <= tolerance,
            "channel {}: actual={:?} expected={:?} tolerance={}",
            i,
            actual,
            expected,
            tolerance
        );
    }
}

#[test]
fn render_blend_mode_correctness() {
    let Some((device, queue)) = create_device() else {
        // Some environments (e.g. CI containers without software adapters) cannot initialize wgpu.
        // The state translation is covered by unit tests; skip these integration tests in that case.
        return;
    };
    let (vs, fs) = create_shader_modules(&device);

    let width = 64;
    let height = 64;

    let color = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("color"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let color_view = color.create_view(&wgpu::TextureViewDescriptor::default());

    let layout_keys = vertex_layout();
    let mut tracker = StateTracker::default();
    tracker.set_vertex_shader(Some(ShaderKey(1)));
    tracker.set_pixel_shader(Some(ShaderKey(2)));
    tracker.set_vertex_layouts(layout_keys.clone());
    tracker.set_render_targets(vec![wgpu::TextureFormat::Rgba8Unorm], None);
    tracker.set_primitive_type(D3DPrimitiveType::TriangleList);
    tracker.rasterizer = RasterizerState {
        cull_mode: CullMode::None,
        ..RasterizerState::default()
    };

    let mut cache = PipelineCache::new(8);
    let mut attribute_storage: Vec<Vec<wgpu::VertexAttribute>> = Vec::new();

    let red_vertices = quad_vertices(0.0, [1.0, 0.0, 0.0, 1.0]);
    let green_vertices = quad_vertices(0.0, [0.0, 1.0, 0.0, 0.5]);

    let red_vb = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("red vb"),
        contents: bytemuck::cast_slice(&red_vertices),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let green_vb = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("green vb"),
        contents: bytemuck::cast_slice(&green_vertices),
        usage: wgpu::BufferUsages::VERTEX,
    });

    // Pipelines must outlive the render pass that references them.
    //
    // Declare/create them *before* `begin_render_pass`, otherwise Rust will drop
    // them before the pass ends (reverse declaration order).
    tracker.blend.alpha_blend_enable = false;
    let (_, translated_opaque, dynamic_opaque) =
        translate_pipeline_state(&tracker).expect("incomplete state");
    let key_opaque = tracker.pipeline_key().unwrap();
    let opaque_pipeline = cache.get_or_create(key_opaque.clone(), || {
        let wgpu_layouts = build_wgpu_vertex_layouts(&layout_keys, &mut attribute_storage);
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("opaque"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &vs,
                entry_point: "vs_main",
                buffers: &wgpu_layouts,
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &fs,
                entry_point: "fs_main",
                targets: &translated_opaque.targets,
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: translated_opaque.primitive,
            depth_stencil: translated_opaque.depth_stencil,
            multisample: translated_opaque.multisample,
            multiview: None,
        })
    });
    let _ = cache.get_or_create(key_opaque, || panic!("expected pipeline cache hit"));

    tracker.blend.alpha_blend_enable = true;
    tracker.blend.src_blend = BlendFactor::SrcAlpha;
    tracker.blend.dst_blend = BlendFactor::InvSrcAlpha;
    tracker.blend.blend_op = BlendOp::Add;
    let (_, translated_alpha, dynamic_alpha) =
        translate_pipeline_state(&tracker).expect("incomplete state");
    let key_alpha = tracker.pipeline_key().unwrap();
    let alpha_pipeline = cache.get_or_create(key_alpha.clone(), || {
        let wgpu_layouts = build_wgpu_vertex_layouts(&layout_keys, &mut attribute_storage);
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("alpha"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &vs,
                entry_point: "vs_main",
                buffers: &wgpu_layouts,
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &fs,
                entry_point: "fs_main",
                targets: &translated_alpha.targets,
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: translated_alpha.primitive,
            depth_stencil: translated_alpha.depth_stencil,
            multisample: translated_alpha.multisample,
            multiview: None,
        })
    });
    let _ = cache.get_or_create(key_alpha, || panic!("expected pipeline cache hit"));

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("blend encoder"),
    });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("blend pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &color_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            occlusion_query_set: None,
            timestamp_writes: None,
        });

        // Draw opaque red.
        pass.set_pipeline(&opaque_pipeline);
        pass.set_vertex_buffer(0, red_vb.slice(..));
        pass.set_blend_constant(dynamic_opaque.blend_constant);
        pass.draw(0..6, 0..1);

        // Draw alpha-blended green over red.
        pass.set_pipeline(&alpha_pipeline);
        pass.set_vertex_buffer(0, green_vb.slice(..));
        pass.set_blend_constant(dynamic_alpha.blend_constant);
        pass.draw(0..6, 0..1);
    }
    queue.submit([encoder.finish()]);

    // Ensure the cache actually hit for repeated pipeline requests.
    let stats = cache.stats();
    assert!(stats.hits >= 2, "stats={stats:?}");
    assert!(stats.misses >= 2, "stats={stats:?}");

    let data = read_texture_rgba8(&device, &queue, &color, width, height);
    let x = 32;
    let y = 32;
    let idx = (y * width * 4 + x * 4) as usize;
    let actual = [data[idx], data[idx + 1], data[idx + 2], data[idx + 3]];

    // Expected ~= (0.5, 0.5, 0.0, 0.75) in UNORM.
    assert_rgba_near(actual, [128, 128, 0, 191], 2);
}

#[test]
fn render_depth_test_correctness() {
    let Some((device, queue)) = create_device() else {
        return;
    };
    let (vs, fs) = create_shader_modules(&device);

    let width = 64;
    let height = 64;

    let color = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("color"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let color_view = color.create_view(&wgpu::TextureViewDescriptor::default());

    let depth = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("depth"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth32Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let depth_view = depth.create_view(&wgpu::TextureViewDescriptor::default());

    let layout_keys = vertex_layout();
    let mut tracker = StateTracker::default();
    tracker.set_vertex_shader(Some(ShaderKey(1)));
    tracker.set_pixel_shader(Some(ShaderKey(2)));
    tracker.set_vertex_layouts(layout_keys.clone());
    tracker.set_render_targets(
        vec![wgpu::TextureFormat::Rgba8Unorm],
        Some(wgpu::TextureFormat::Depth32Float),
    );
    tracker.set_primitive_type(D3DPrimitiveType::TriangleList);
    tracker.rasterizer = RasterizerState {
        cull_mode: CullMode::None,
        ..RasterizerState::default()
    };

    tracker.depth_stencil.depth_enable = true;
    tracker.depth_stencil.depth_write_enable = true;
    tracker.depth_stencil.depth_func = CompareFunc::Less;

    let mut cache = PipelineCache::new(8);
    let mut attribute_storage: Vec<Vec<wgpu::VertexAttribute>> = Vec::new();

    let near_vertices = quad_vertices(0.1, [0.0, 1.0, 0.0, 1.0]);
    let far_vertices = quad_vertices(0.9, [1.0, 0.0, 0.0, 1.0]);

    let near_vb = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("near vb"),
        contents: bytemuck::cast_slice(&near_vertices),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let far_vb = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("far vb"),
        contents: bytemuck::cast_slice(&far_vertices),
        usage: wgpu::BufferUsages::VERTEX,
    });

    let (_, translated, dynamic) = translate_pipeline_state(&tracker).expect("incomplete state");
    let key = tracker.pipeline_key().unwrap();
    let pipeline = cache.get_or_create(key.clone(), || {
        let wgpu_layouts = build_wgpu_vertex_layouts(&layout_keys, &mut attribute_storage);
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("depth"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &vs,
                entry_point: "vs_main",
                buffers: &wgpu_layouts,
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &fs,
                entry_point: "fs_main",
                targets: &translated.targets,
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: translated.primitive,
            depth_stencil: translated.depth_stencil,
            multisample: translated.multisample,
            multiview: None,
        })
    });
    let _ = cache.get_or_create(key, || panic!("expected pipeline cache hit"));

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("depth encoder"),
    });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("depth pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &color_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &depth_view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            occlusion_query_set: None,
            timestamp_writes: None,
        });

        pass.set_pipeline(&pipeline);
        pass.set_blend_constant(dynamic.blend_constant);

        // Near first (writes depth 0.1).
        pass.set_vertex_buffer(0, near_vb.slice(..));
        pass.draw(0..6, 0..1);

        // Far second should fail depth and not overwrite green.
        pass.set_vertex_buffer(0, far_vb.slice(..));
        pass.draw(0..6, 0..1);
    }
    queue.submit([encoder.finish()]);

    let data = read_texture_rgba8(&device, &queue, &color, width, height);
    let x = 32;
    let y = 32;
    let idx = (y * width * 4 + x * 4) as usize;
    let actual = [data[idx], data[idx + 1], data[idx + 2], data[idx + 3]];

    assert_rgba_near(actual, [0, 255, 0, 255], 0);
}

#[test]
fn render_stencil_correctness() {
    let Some((device, queue)) = create_device() else {
        return;
    };
    let (vs, fs) = create_shader_modules(&device);

    let width = 64;
    let height = 64;

    let color = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("color"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let color_view = color.create_view(&wgpu::TextureViewDescriptor::default());

    let ds = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("depth-stencil"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth24PlusStencil8,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let ds_view = ds.create_view(&wgpu::TextureViewDescriptor::default());

    let layout_keys = vertex_layout();
    let mut tracker = StateTracker::default();
    tracker.set_vertex_shader(Some(ShaderKey(1)));
    tracker.set_pixel_shader(Some(ShaderKey(2)));
    tracker.set_vertex_layouts(layout_keys.clone());
    tracker.set_render_targets(
        vec![wgpu::TextureFormat::Rgba8Unorm],
        Some(wgpu::TextureFormat::Depth24PlusStencil8),
    );
    tracker.set_primitive_type(D3DPrimitiveType::TriangleList);
    tracker.rasterizer = RasterizerState {
        cull_mode: CullMode::None,
        ..RasterizerState::default()
    };

    tracker.depth_stencil.depth_enable = false;
    tracker.depth_stencil.depth_write_enable = false;
    tracker.depth_stencil.stencil_enable = true;
    tracker.depth_stencil.stencil_ref = 1;
    tracker.depth_stencil.stencil_read_mask = 0xFF;
    tracker.depth_stencil.stencil_write_mask = 0xFF;
    tracker.depth_stencil.stencil_func = CompareFunc::Always;
    tracker.depth_stencil.stencil_fail = StencilOp::Keep;
    tracker.depth_stencil.stencil_zfail = StencilOp::Keep;
    tracker.depth_stencil.stencil_pass = StencilOp::Replace;

    // First pass writes stencil only.
    tracker.set_color_write_mask(0, ColorWriteMask::NONE);

    let mut cache = PipelineCache::new(8);
    let mut attribute_storage: Vec<Vec<wgpu::VertexAttribute>> = Vec::new();

    let tri_vertices = left_half_triangle(0.0, [1.0, 0.0, 0.0, 1.0]);
    let tri_vb = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("tri vb"),
        contents: bytemuck::cast_slice(&tri_vertices),
        usage: wgpu::BufferUsages::VERTEX,
    });

    let (_, translated, dynamic) = translate_pipeline_state(&tracker).expect("incomplete state");
    let key = tracker.pipeline_key().unwrap();
    let stencil_write_pipeline = cache.get_or_create(key.clone(), || {
        let wgpu_layouts = build_wgpu_vertex_layouts(&layout_keys, &mut attribute_storage);
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("stencil write"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &vs,
                entry_point: "vs_main",
                buffers: &wgpu_layouts,
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &fs,
                entry_point: "fs_main",
                targets: &translated.targets,
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: translated.primitive,
            depth_stencil: translated.depth_stencil,
            multisample: translated.multisample,
            multiview: None,
        })
    });
    let _ = cache.get_or_create(key, || panic!("expected pipeline cache hit"));

    // Second pass draws green only where stencil==1.
    tracker.depth_stencil.stencil_func = CompareFunc::Equal;
    tracker.depth_stencil.stencil_pass = StencilOp::Keep;
    tracker.depth_stencil.stencil_write_mask = 0;
    tracker.set_color_write_mask(0, ColorWriteMask::RGBA);

    let full_vertices = quad_vertices(0.0, [0.0, 1.0, 0.0, 1.0]);
    let full_vb = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("full vb"),
        contents: bytemuck::cast_slice(&full_vertices),
        usage: wgpu::BufferUsages::VERTEX,
    });

    let (_, translated, dynamic2) = translate_pipeline_state(&tracker).expect("incomplete state");
    let key = tracker.pipeline_key().unwrap();
    let stencil_test_pipeline = cache.get_or_create(key.clone(), || {
        let wgpu_layouts = build_wgpu_vertex_layouts(&layout_keys, &mut attribute_storage);
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("stencil test"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &vs,
                entry_point: "vs_main",
                buffers: &wgpu_layouts,
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &fs,
                entry_point: "fs_main",
                targets: &translated.targets,
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: translated.primitive,
            depth_stencil: translated.depth_stencil,
            multisample: translated.multisample,
            multiview: None,
        })
    });
    let _ = cache.get_or_create(key, || panic!("expected pipeline cache hit"));

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("stencil encoder"),
    });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("stencil pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &color_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &ds_view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(0),
                    store: wgpu::StoreOp::Store,
                }),
            }),
            occlusion_query_set: None,
            timestamp_writes: None,
        });

        // Write stencil in left half.
        pass.set_pipeline(&stencil_write_pipeline);
        pass.set_vertex_buffer(0, tri_vb.slice(..));
        pass.set_blend_constant(dynamic.blend_constant);
        pass.set_stencil_reference(dynamic.stencil_reference);
        pass.draw(0..3, 0..1);

        // Draw green where stencil==1.
        pass.set_pipeline(&stencil_test_pipeline);
        pass.set_vertex_buffer(0, full_vb.slice(..));
        pass.set_blend_constant(dynamic2.blend_constant);
        pass.set_stencil_reference(dynamic2.stencil_reference);
        pass.draw(0..6, 0..1);
    }
    queue.submit([encoder.finish()]);

    let data = read_texture_rgba8(&device, &queue, &color, width, height);

    let sample = |x: u32, y: u32| -> [u8; 4] {
        let idx = (y * width * 4 + x * 4) as usize;
        [data[idx], data[idx + 1], data[idx + 2], data[idx + 3]]
    };

    // Ensure the "inside" sample is well within the left-side triangle used to
    // stamp the stencil buffer.
    let left = sample(8, 32);
    let right = sample(48, 32);

    assert_rgba_near(left, [0, 255, 0, 255], 0);
    assert_rgba_near(right, [0, 0, 0, 0], 0);
}
