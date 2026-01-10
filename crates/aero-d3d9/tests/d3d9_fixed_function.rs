use aero_d3d9::fixed_function::fvf::Fvf;
use aero_d3d9::fixed_function::shader_gen::{FixedFunctionGlobals, FixedFunctionShaderDesc};
use aero_d3d9::fixed_function::tss::{
    AlphaTestState, CompareFunc, FogState, TextureArg, TextureOp, TextureStageState,
};
use aero_d3d9::fixed_function::FixedFunctionShaderCache;

use bytemuck::{Pod, Zeroable};
use std::sync::Arc;
use wgpu::util::DeviceExt;

fn request_device() -> Option<(wgpu::Device, wgpu::Queue)> {
    // `AERO_REQUIRE_WEBGPU=1` means WebGPU is a hard requirement; anything else
    // (including `0`/unset) means tests should skip when no adapter/device is available.
    let require_webgpu = matches!(std::env::var("AERO_REQUIRE_WEBGPU").as_deref(), Ok("1"));

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::PRIMARY,
        ..Default::default()
    });

    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        compatible_surface: None,
        force_fallback_adapter: true,
    }));

    let adapter = match adapter {
        Some(adapter) => adapter,
        None => {
            if require_webgpu {
                panic!("AERO_REQUIRE_WEBGPU=1 but wgpu request_adapter returned None");
            }
            eprintln!("skipping WebGPU-dependent test: no suitable adapter");
            return None;
        }
    };

    match pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("aero-d3d9-fixed-function-tests"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
        },
        None,
    )) {
        Ok(device) => Some(device),
        Err(err) => {
            if require_webgpu {
                panic!("AERO_REQUIRE_WEBGPU=1 but request_device failed: {err:?}");
            }
            eprintln!("skipping WebGPU-dependent test: request_device failed: {err:?}");
            None
        }
    }
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
    let padded_bytes_per_row = ((unpadded_bytes_per_row + 255) / 256) * 256;
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

#[test]
fn shader_cache_hits_on_identical_state() {
    let mut cache = FixedFunctionShaderCache::new();
    let desc = FixedFunctionShaderDesc {
        fvf: Fvf(Fvf::XYZ | (1 << 8)),
        stage0: TextureStageState {
            color_op: TextureOp::Modulate,
            color_arg1: TextureArg::Texture,
            color_arg2: TextureArg::Diffuse,
            alpha_op: TextureOp::SelectArg1,
            alpha_arg1: TextureArg::Diffuse,
            alpha_arg2: TextureArg::Current,
        },
        alpha_test: AlphaTestState::default(),
        fog: FogState::default(),
    };

    let a = cache.get_or_create(&desc);
    let b = cache.get_or_create(&desc);
    assert!(Arc::ptr_eq(&a, &b));
    assert_eq!(cache.hits(), 1);
    assert_eq!(cache.misses(), 1);
}

#[test]
fn render_transformed_textured_quad() {
    let Some((device, queue)) = request_device() else {
        return;
    };

    #[repr(C)]
    #[derive(Clone, Copy, Pod, Zeroable)]
    struct Vertex {
        pos: [f32; 3],
        tex: [f32; 2],
    }

    let width = 16;
    let height = 16;

    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("target"),
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
    let target_view = target.create_view(&Default::default());

    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("tex0"),
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::ImageCopyTexture {
            texture: &tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &[255, 0, 0, 255],
        wgpu::ImageDataLayout {
            offset: 0,
            bytes_per_row: Some(4),
            rows_per_image: Some(1),
        },
        wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
    );
    let tex_view = tex.create_view(&Default::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("nearest"),
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
    });

    let desc = FixedFunctionShaderDesc {
        fvf: Fvf(Fvf::XYZ | (1 << 8)),
        stage0: TextureStageState {
            color_op: TextureOp::Modulate,
            color_arg1: TextureArg::Texture,
            color_arg2: TextureArg::Diffuse,
            alpha_op: TextureOp::SelectArg1,
            alpha_arg1: TextureArg::Texture,
            alpha_arg2: TextureArg::Current,
        },
        alpha_test: AlphaTestState::default(),
        fog: FogState::default(),
    };

    let shaders = aero_d3d9::fixed_function::shader_gen::generate_fixed_function_shaders(&desc);
    let vertex_wgsl = shaders.vertex_wgsl.clone();
    let fragment_wgsl = shaders.fragment_wgsl.clone();
    let vs = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("vs"),
        source: wgpu::ShaderSource::Wgsl(vertex_wgsl.into()),
    });
    let fs = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("fs"),
        source: wgpu::ShaderSource::Wgsl(fragment_wgsl.into()),
    });

    let globals = FixedFunctionGlobals {
        world_view_proj: [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.5, 0.0, 0.0, 1.0],
        ],
        viewport: [0.0, 0.0, width as f32, height as f32],
        ..FixedFunctionGlobals::identity()
    };

    let globals_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("globals"),
        contents: globals.as_bytes(),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    });

    let globals_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("globals-bgl"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });
    let globals_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("globals-bg"),
        layout: &globals_bgl,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: globals_buf.as_entire_binding(),
        }],
    });

    let tex_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("tex-bgl"),
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
    let tex_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("tex-bg"),
        layout: &tex_bgl,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&tex_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
        ],
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("pipeline-layout"),
        bind_group_layouts: &[&globals_bgl, &tex_bgl],
        push_constant_ranges: &[],
    });

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &vs,
            entry_point: "vs_main",
            buffers: &[shaders.vertex_buffer_layout()],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &fs,
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

    // Quad covers x=[-0.5,0.5], then translated by +0.5 => x=[0.0,1.0] (right half).
    let verts = [
        Vertex {
            pos: [-0.5, -0.5, 0.0],
            tex: [0.0, 1.0],
        },
        Vertex {
            pos: [-0.5, 0.5, 0.0],
            tex: [0.0, 0.0],
        },
        Vertex {
            pos: [0.5, 0.5, 0.0],
            tex: [1.0, 0.0],
        },
        Vertex {
            pos: [-0.5, -0.5, 0.0],
            tex: [0.0, 1.0],
        },
        Vertex {
            pos: [0.5, 0.5, 0.0],
            tex: [1.0, 0.0],
        },
        Vertex {
            pos: [0.5, -0.5, 0.0],
            tex: [1.0, 1.0],
        },
    ];
    let vb = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("vb"),
        contents: bytemuck::cast_slice(&verts),
        usage: wgpu::BufferUsages::VERTEX,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("render-encoder"),
    });

    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("render-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target_view,
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
        pass.set_bind_group(0, &globals_bg, &[]);
        pass.set_bind_group(1, &tex_bg, &[]);
        pass.set_vertex_buffer(0, vb.slice(..));
        pass.draw(0..6, 0..1);
    }

    queue.submit([encoder.finish()]);

    let pixels = readback_rgba8(&device, &queue, &target, width, height);

    // Left side should remain black.
    assert_rgba_approx(pixel_at_rgba(&pixels, width, 2, 8), [0, 0, 0, 255], 2);
    // Right side should be red from the texture.
    assert_rgba_approx(pixel_at_rgba(&pixels, width, 12, 8), [255, 0, 0, 255], 2);
}

#[test]
fn render_vertex_color_modulation() {
    let Some((device, queue)) = request_device() else {
        return;
    };

    #[repr(C)]
    #[derive(Clone, Copy, Pod, Zeroable)]
    struct Vertex {
        pos: [f32; 3],
        diffuse: u32,
        tex: [f32; 2],
    }

    let width = 16;
    let height = 16;

    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("target"),
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
    let target_view = target.create_view(&Default::default());

    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("tex0"),
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::ImageCopyTexture {
            texture: &tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &[255, 255, 255, 255],
        wgpu::ImageDataLayout {
            offset: 0,
            bytes_per_row: Some(4),
            rows_per_image: Some(1),
        },
        wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
    );
    let tex_view = tex.create_view(&Default::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("nearest"),
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
    });

    let desc = FixedFunctionShaderDesc {
        fvf: Fvf(Fvf::XYZ | Fvf::DIFFUSE | (1 << 8)),
        stage0: TextureStageState {
            color_op: TextureOp::Modulate,
            color_arg1: TextureArg::Texture,
            color_arg2: TextureArg::Diffuse,
            alpha_op: TextureOp::Modulate,
            alpha_arg1: TextureArg::Texture,
            alpha_arg2: TextureArg::Diffuse,
        },
        alpha_test: AlphaTestState::default(),
        fog: FogState::default(),
    };

    let shaders = aero_d3d9::fixed_function::shader_gen::generate_fixed_function_shaders(&desc);
    let vertex_wgsl = shaders.vertex_wgsl.clone();
    let fragment_wgsl = shaders.fragment_wgsl.clone();
    let vs = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("vs"),
        source: wgpu::ShaderSource::Wgsl(vertex_wgsl.into()),
    });
    let fs = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("fs"),
        source: wgpu::ShaderSource::Wgsl(fragment_wgsl.into()),
    });

    let globals = FixedFunctionGlobals {
        world_view_proj: FixedFunctionGlobals::identity().world_view_proj,
        viewport: [0.0, 0.0, width as f32, height as f32],
        ..FixedFunctionGlobals::identity()
    };
    let globals_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("globals"),
        contents: globals.as_bytes(),
        usage: wgpu::BufferUsages::UNIFORM,
    });

    let globals_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("globals-bgl"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });
    let globals_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("globals-bg"),
        layout: &globals_bgl,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: globals_buf.as_entire_binding(),
        }],
    });

    let tex_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("tex-bgl"),
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
    let tex_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("tex-bg"),
        layout: &tex_bgl,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&tex_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
        ],
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("pipeline-layout"),
        bind_group_layouts: &[&globals_bgl, &tex_bgl],
        push_constant_ranges: &[],
    });

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &vs,
            entry_point: "vs_main",
            buffers: &[shaders.vertex_buffer_layout()],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &fs,
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

    // Fullscreen quad with corner colors.
    let verts = [
        Vertex {
            pos: [-1.0, -1.0, 0.0],
            diffuse: 0xFFFF0000, // red
            tex: [0.0, 1.0],
        },
        Vertex {
            pos: [-1.0, 1.0, 0.0],
            diffuse: 0xFF00FF00, // green
            tex: [0.0, 0.0],
        },
        Vertex {
            pos: [1.0, 1.0, 0.0],
            diffuse: 0xFF0000FF, // blue
            tex: [1.0, 0.0],
        },
        Vertex {
            pos: [-1.0, -1.0, 0.0],
            diffuse: 0xFFFF0000,
            tex: [0.0, 1.0],
        },
        Vertex {
            pos: [1.0, 1.0, 0.0],
            diffuse: 0xFF0000FF,
            tex: [1.0, 0.0],
        },
        Vertex {
            pos: [1.0, -1.0, 0.0],
            diffuse: 0xFFFFFFFF, // white
            tex: [1.0, 1.0],
        },
    ];

    let vb = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("vb"),
        contents: bytemuck::cast_slice(&verts),
        usage: wgpu::BufferUsages::VERTEX,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("render-encoder"),
    });

    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("render-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target_view,
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
        pass.set_bind_group(0, &globals_bg, &[]);
        pass.set_bind_group(1, &tex_bg, &[]);
        pass.set_vertex_buffer(0, vb.slice(..));
        pass.draw(0..6, 0..1);
    }

    queue.submit([encoder.finish()]);

    let pixels = readback_rgba8(&device, &queue, &target, width, height);

    // Sample near each corner to avoid rasterization edge rules.
    assert_rgba_approx(pixel_at_rgba(&pixels, width, 0, 15), [255, 0, 0, 255], 32);
    assert_rgba_approx(pixel_at_rgba(&pixels, width, 0, 0), [0, 255, 0, 255], 32);
    assert_rgba_approx(pixel_at_rgba(&pixels, width, 15, 0), [0, 0, 255, 255], 32);
    assert_rgba_approx(
        pixel_at_rgba(&pixels, width, 15, 15),
        [255, 255, 255, 255],
        32,
    );
}

#[test]
fn render_alpha_test_with_blending() {
    let Some((device, queue)) = request_device() else {
        return;
    };

    #[repr(C)]
    #[derive(Clone, Copy, Pod, Zeroable)]
    struct Vertex {
        pos: [f32; 3],
        diffuse: u32,
        tex: [f32; 2],
    }

    let width = 16;
    let height = 16;

    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("target"),
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
    let target_view = target.create_view(&Default::default());

    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("tex0"),
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::ImageCopyTexture {
            texture: &tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &[255, 255, 255, 255],
        wgpu::ImageDataLayout {
            offset: 0,
            bytes_per_row: Some(4),
            rows_per_image: Some(1),
        },
        wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
    );
    let tex_view = tex.create_view(&Default::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("nearest"),
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
    });

    let desc = FixedFunctionShaderDesc {
        fvf: Fvf(Fvf::XYZ | Fvf::DIFFUSE | (1 << 8)),
        stage0: TextureStageState {
            color_op: TextureOp::SelectArg1,
            color_arg1: TextureArg::Diffuse,
            color_arg2: TextureArg::Current,
            alpha_op: TextureOp::SelectArg1,
            alpha_arg1: TextureArg::Diffuse,
            alpha_arg2: TextureArg::Current,
        },
        alpha_test: AlphaTestState {
            enabled: true,
            func: CompareFunc::Greater,
        },
        fog: FogState::default(),
    };

    let shaders = aero_d3d9::fixed_function::shader_gen::generate_fixed_function_shaders(&desc);
    let vertex_wgsl = shaders.vertex_wgsl.clone();
    let fragment_wgsl = shaders.fragment_wgsl.clone();
    let vs = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("vs"),
        source: wgpu::ShaderSource::Wgsl(vertex_wgsl.into()),
    });
    let fs = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("fs"),
        source: wgpu::ShaderSource::Wgsl(fragment_wgsl.into()),
    });

    let globals = FixedFunctionGlobals {
        world_view_proj: FixedFunctionGlobals::identity().world_view_proj,
        viewport: [0.0, 0.0, width as f32, height as f32],
        alpha_test: [0.5, 0.0, 0.0, 0.0],
        ..FixedFunctionGlobals::identity()
    };
    let globals_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("globals"),
        contents: globals.as_bytes(),
        usage: wgpu::BufferUsages::UNIFORM,
    });

    let globals_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("globals-bgl"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });
    let globals_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("globals-bg"),
        layout: &globals_bgl,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: globals_buf.as_entire_binding(),
        }],
    });

    let tex_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("tex-bgl"),
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
    let tex_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("tex-bg"),
        layout: &tex_bgl,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&tex_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
        ],
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("pipeline-layout"),
        bind_group_layouts: &[&globals_bgl, &tex_bgl],
        push_constant_ranges: &[],
    });

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &vs,
            entry_point: "vs_main",
            buffers: &[shaders.vertex_buffer_layout()],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &fs,
            entry_point: "fs_main",
            targets: &[Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::Rgba8Unorm,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
    });

    // Left side alpha=0.0 -> fails alpha test and is discarded. Right side alpha=0.6 -> blends.
    let red = 0x00FF0000u32;
    let left_alpha = 0x00u32;
    let right_alpha = 0x99u32; // 153/255 ~= 0.6

    let verts = [
        Vertex {
            pos: [-1.0, -1.0, 0.0],
            diffuse: (left_alpha << 24) | red,
            tex: [0.0, 1.0],
        },
        Vertex {
            pos: [-1.0, 1.0, 0.0],
            diffuse: (left_alpha << 24) | red,
            tex: [0.0, 0.0],
        },
        Vertex {
            pos: [1.0, 1.0, 0.0],
            diffuse: (right_alpha << 24) | red,
            tex: [1.0, 0.0],
        },
        Vertex {
            pos: [-1.0, -1.0, 0.0],
            diffuse: (left_alpha << 24) | red,
            tex: [0.0, 1.0],
        },
        Vertex {
            pos: [1.0, 1.0, 0.0],
            diffuse: (right_alpha << 24) | red,
            tex: [1.0, 0.0],
        },
        Vertex {
            pos: [1.0, -1.0, 0.0],
            diffuse: (right_alpha << 24) | red,
            tex: [1.0, 1.0],
        },
    ];

    let vb = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("vb"),
        contents: bytemuck::cast_slice(&verts),
        usage: wgpu::BufferUsages::VERTEX,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("render-encoder"),
    });

    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("render-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLUE),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            occlusion_query_set: None,
            timestamp_writes: None,
        });

        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &globals_bg, &[]);
        pass.set_bind_group(1, &tex_bg, &[]);
        pass.set_vertex_buffer(0, vb.slice(..));
        pass.draw(0..6, 0..1);
    }

    queue.submit([encoder.finish()]);

    let pixels = readback_rgba8(&device, &queue, &target, width, height);

    // Left should remain background blue.
    assert_rgba_approx(pixel_at_rgba(&pixels, width, 1, 8), [0, 0, 255, 255], 3);

    // Right should be blended red over blue. The alpha varies across X, so compute the expected
    // blend for the chosen sample position.
    let sample_x = 15;
    let u = (sample_x as f32 + 0.5) / width as f32;
    let src_alpha = u * (right_alpha as f32 / 255.0);
    let expected_r = (255.0 * src_alpha).round() as u8;
    let expected_b = (255.0 * (1.0 - src_alpha)).round() as u8;
    assert_rgba_approx(
        pixel_at_rgba(&pixels, width, sample_x, 8),
        [expected_r, 0, expected_b, 255],
        12,
    );
}
