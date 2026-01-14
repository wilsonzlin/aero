use aero_d3d9::fixed_function::fvf::Fvf;
use aero_d3d9::fixed_function::shader_gen::{FixedFunctionGlobals, FixedFunctionShaderDesc};
use aero_d3d9::fixed_function::tss::{
    AlphaTestState, CompareFunc, FogState, LightingState, TextureArg, TextureOp,
    TextureResultTarget, TextureStageState,
};
use aero_d3d9::fixed_function::FixedFunctionShaderCache;

use bytemuck::{Pod, Zeroable};
use std::sync::Arc;
use wgpu::util::DeviceExt;

fn request_device() -> Option<(wgpu::Device, wgpu::Queue)> {
    // `AERO_REQUIRE_WEBGPU=1` means WebGPU is a hard requirement; anything else
    // (including `0`/unset) means tests should skip when no adapter/device is available.
    let require_webgpu = std::env::var("AERO_REQUIRE_WEBGPU")
        .ok()
        .map(|raw| {
            let v = raw.trim();
            v == "1"
                || v.eq_ignore_ascii_case("true")
                || v.eq_ignore_ascii_case("yes")
                || v.eq_ignore_ascii_case("on")
        })
        .unwrap_or(false);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let needs_runtime_dir = std::env::var("XDG_RUNTIME_DIR")
            .ok()
            .map(|v| v.is_empty())
            .unwrap_or(true);
        if needs_runtime_dir {
            let dir = std::env::temp_dir().join(format!(
                "aero-d3d9-xdg-runtime-{}-fixed-function",
                std::process::id()
            ));
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
            std::env::set_var("XDG_RUNTIME_DIR", &dir);
        }
    }

    // Prefer GL on Linux CI to avoid crashes in some Vulkan software adapters (lavapipe/llvmpipe).
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: if cfg!(target_os = "linux") {
            wgpu::Backends::GL
        } else {
            wgpu::Backends::PRIMARY
        },
        ..Default::default()
    });

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
    });

    let adapter = match adapter {
        Some(adapter) => adapter,
        None => {
            if require_webgpu {
                panic!("AERO_REQUIRE_WEBGPU is enabled but wgpu request_adapter returned None");
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
                panic!("AERO_REQUIRE_WEBGPU is enabled but request_device failed: {err:?}");
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

    #[cfg(target_arch = "wasm32")]
    device.poll(wgpu::Maintain::Poll);
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
            color_arg0: TextureArg::Current,
            color_arg1: TextureArg::Texture,
            color_arg2: TextureArg::Diffuse,
            alpha_op: TextureOp::SelectArg1,
            alpha_arg0: TextureArg::Current,
            alpha_arg1: TextureArg::Diffuse,
            alpha_arg2: TextureArg::Current,
            result_target: TextureResultTarget::Current,
        },
        stage1: TextureStageState::default(),
        alpha_test: AlphaTestState::default(),
        fog: FogState::default(),
        lighting: LightingState::default(),
    };

    let a = cache.get_or_create(&desc);
    let b = cache.get_or_create(&desc);
    assert!(Arc::ptr_eq(&a, &b));
    assert_eq!(cache.hits(), 1);
    assert_eq!(cache.misses(), 1);
}

#[test]
fn shader_includes_lighting_branch_only_when_enabled() {
    let base = FixedFunctionShaderDesc {
        fvf: Fvf(Fvf::XYZ | Fvf::NORMAL),
        stage0: TextureStageState::default(),
        stage1: TextureStageState::default(),
        alpha_test: AlphaTestState::default(),
        fog: FogState::default(),
        lighting: LightingState { enabled: false },
    };
    let unlit = aero_d3d9::fixed_function::shader_gen::generate_fixed_function_shaders(&base);
    assert!(
        !unlit.vertex_wgsl.contains("let lambert"),
        "unexpected lighting code in unlit shader:\n{}",
        unlit.vertex_wgsl
    );

    let lit = FixedFunctionShaderDesc {
        fvf: base.fvf,
        stage0: base.stage0,
        stage1: base.stage1,
        alpha_test: base.alpha_test,
        fog: base.fog,
        lighting: LightingState { enabled: true },
    };
    let lit = aero_d3d9::fixed_function::shader_gen::generate_fixed_function_shaders(&lit);
    assert!(
        lit.vertex_wgsl.contains("let lambert"),
        "missing lighting code in lit shader:\n{}",
        lit.vertex_wgsl
    );
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
            color_arg0: TextureArg::Current,
            color_arg1: TextureArg::Texture,
            color_arg2: TextureArg::Diffuse,
            alpha_op: TextureOp::SelectArg1,
            alpha_arg0: TextureArg::Current,
            alpha_arg1: TextureArg::Texture,
            alpha_arg2: TextureArg::Current,
            result_target: TextureResultTarget::Current,
        },
        stage1: TextureStageState::default(),
        alpha_test: AlphaTestState::default(),
        fog: FogState::default(),
        lighting: LightingState::default(),
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
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 3,
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
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(&tex_view),
            },
            wgpu::BindGroupEntry {
                binding: 3,
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
            color_arg0: TextureArg::Current,
            color_arg1: TextureArg::Texture,
            color_arg2: TextureArg::Diffuse,
            alpha_op: TextureOp::Modulate,
            alpha_arg0: TextureArg::Current,
            alpha_arg1: TextureArg::Texture,
            alpha_arg2: TextureArg::Diffuse,
            result_target: TextureResultTarget::Current,
        },
        stage1: TextureStageState::default(),
        alpha_test: AlphaTestState::default(),
        fog: FogState::default(),
        lighting: LightingState::default(),
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
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 3,
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
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(&tex_view),
            },
            wgpu::BindGroupEntry {
                binding: 3,
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
            color_arg0: TextureArg::Current,
            color_arg1: TextureArg::Diffuse,
            color_arg2: TextureArg::Current,
            alpha_op: TextureOp::SelectArg1,
            alpha_arg0: TextureArg::Current,
            alpha_arg1: TextureArg::Diffuse,
            alpha_arg2: TextureArg::Current,
            result_target: TextureResultTarget::Current,
        },
        stage1: TextureStageState::default(),
        alpha_test: AlphaTestState {
            enabled: true,
            func: CompareFunc::Greater,
        },
        fog: FogState::default(),
        lighting: LightingState::default(),
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
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 3,
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
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(&tex_view),
            },
            wgpu::BindGroupEntry {
                binding: 3,
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


#[test]
fn render_directional_lighting_diffuse_only() {
    let Some((device, queue)) = request_device() else {
        return;
    };

    #[repr(C)]
    #[derive(Clone, Copy, Pod, Zeroable)]
    struct Vertex {
        pos: [f32; 3],
        normal: [f32; 3],
    }

    let width = 16;
    let height = 16;

    let make_target = |label: &str| {
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
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
        let view = tex.create_view(&Default::default());
        (tex, view)
    };

    let (target_unlit, view_unlit) = make_target("target-unlit");
    let (target_lit, view_lit) = make_target("target-lit");

    // Dummy texture/sampler bindings (fragment shader always declares them).
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

    let stage0 = TextureStageState {
        color_op: TextureOp::SelectArg1,
        color_arg0: TextureArg::Current,
        color_arg1: TextureArg::Diffuse,
        color_arg2: TextureArg::Current,
        alpha_op: TextureOp::SelectArg1,
        alpha_arg0: TextureArg::Current,
        alpha_arg1: TextureArg::Diffuse,
        alpha_arg2: TextureArg::Current,
        result_target: TextureResultTarget::Current,
    };

    let desc_unlit = FixedFunctionShaderDesc {
        fvf: Fvf(Fvf::XYZ | Fvf::NORMAL),
        stage0,
        stage1: TextureStageState::default(),
        alpha_test: AlphaTestState::default(),
        fog: FogState::default(),
        lighting: LightingState { enabled: false },
    };
    let desc_lit = FixedFunctionShaderDesc {
        fvf: desc_unlit.fvf,
        stage0: desc_unlit.stage0,
        stage1: desc_unlit.stage1,
        alpha_test: desc_unlit.alpha_test,
        fog: desc_unlit.fog,
        lighting: LightingState { enabled: true },
    };

    let shaders_unlit = aero_d3d9::fixed_function::shader_gen::generate_fixed_function_shaders(&desc_unlit);
    let shaders_lit = aero_d3d9::fixed_function::shader_gen::generate_fixed_function_shaders(&desc_lit);

    let vs_unlit = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("vs-unlit"),
        source: wgpu::ShaderSource::Wgsl(shaders_unlit.vertex_wgsl.clone().into()),
    });
    let fs_unlit = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("fs-unlit"),
        source: wgpu::ShaderSource::Wgsl(shaders_unlit.fragment_wgsl.clone().into()),
    });

    let vs_lit = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("vs-lit"),
        source: wgpu::ShaderSource::Wgsl(shaders_lit.vertex_wgsl.clone().into()),
    });
    let fs_lit = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("fs-lit"),
        source: wgpu::ShaderSource::Wgsl(shaders_lit.fragment_wgsl.clone().into()),
    });

    let globals = FixedFunctionGlobals {
        world_view_proj: FixedFunctionGlobals::identity().world_view_proj,
        viewport: [0.0, 0.0, width as f32, height as f32],
        material_diffuse: [1.0, 1.0, 1.0, 1.0],
        material_ambient: [0.0, 0.0, 0.0, 0.0],
        light_dir: [0.0, 0.0, -1.0, 0.0],
        light_color: [1.0, 0.0, 0.0, 1.0],
        lighting_flags: [1, 1, 0, 0],
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
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 3,
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
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(&tex_view),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
        ],
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("pipeline-layout"),
        bind_group_layouts: &[&globals_bgl, &tex_bgl],
        push_constant_ranges: &[],
    });

    let pipeline_unlit = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("pipeline-unlit"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &vs_unlit,
            entry_point: "vs_main",
            buffers: &[shaders_unlit.vertex_buffer_layout()],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &fs_unlit,
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

    let pipeline_lit = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("pipeline-lit"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &vs_lit,
            entry_point: "vs_main",
            buffers: &[shaders_lit.vertex_buffer_layout()],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &fs_lit,
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

    // Full-screen triangle with a known normal.
    let verts = [
        Vertex {
            pos: [-1.0, -1.0, 0.0],
            normal: [0.0, 0.0, 1.0],
        },
        Vertex {
            pos: [3.0, -1.0, 0.0],
            normal: [0.0, 0.0, 1.0],
        },
        Vertex {
            pos: [-1.0, 3.0, 0.0],
            normal: [0.0, 0.0, 1.0],
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
            label: Some("pass-unlit"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view_unlit,
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

        pass.set_pipeline(&pipeline_unlit);
        pass.set_bind_group(0, &globals_bg, &[]);
        pass.set_bind_group(1, &tex_bg, &[]);
        pass.set_vertex_buffer(0, vb.slice(..));
        pass.draw(0..3, 0..1);
    }

    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("pass-lit"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view_lit,
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

        pass.set_pipeline(&pipeline_lit);
        pass.set_bind_group(0, &globals_bg, &[]);
        pass.set_bind_group(1, &tex_bg, &[]);
        pass.set_vertex_buffer(0, vb.slice(..));
        pass.draw(0..3, 0..1);
    }

    queue.submit([encoder.finish()]);

    let pixels_unlit = readback_rgba8(&device, &queue, &target_unlit, width, height);
    let pixels_lit = readback_rgba8(&device, &queue, &target_lit, width, height);

    assert_rgba_approx(pixel_at_rgba(&pixels_unlit, width, 8, 8), [255, 255, 255, 255], 2);
    assert_rgba_approx(pixel_at_rgba(&pixels_lit, width, 8, 8), [255, 0, 0, 255], 2);
}

#[test]
fn render_two_stage_texture_ops_modulate_add_subtract() {
    let Some((device, queue)) = request_device() else {
        return;
    };

    #[repr(C)]
    #[derive(Clone, Copy, Pod, Zeroable)]
    struct Vertex {
        pos: [f32; 3],
        tex0: [f32; 2],
        tex1: [f32; 2],
    }

    let width = 4;
    let height = 4;

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

    let make_tex = |label: &str, rgba: [u8; 4]| {
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
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
            &rgba,
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
        let view = tex.create_view(&Default::default());
        (tex, view)
    };

    let (_tex0, tex0_view) = make_tex("tex0", [128, 64, 32, 255]);
    let (_tex1, tex1_view) = make_tex("tex1", [64, 128, 255, 255]);

    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("nearest"),
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
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
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 3,
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
                resource: wgpu::BindingResource::TextureView(&tex0_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(&tex1_view),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
        ],
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("pipeline-layout"),
        bind_group_layouts: &[&globals_bgl, &tex_bgl],
        push_constant_ranges: &[],
    });

    let verts = [
        Vertex {
            pos: [-1.0, -1.0, 0.0],
            tex0: [0.0, 1.0],
            tex1: [0.0, 1.0],
        },
        Vertex {
            pos: [-1.0, 1.0, 0.0],
            tex0: [0.0, 0.0],
            tex1: [0.0, 0.0],
        },
        Vertex {
            pos: [1.0, 1.0, 0.0],
            tex0: [1.0, 0.0],
            tex1: [1.0, 0.0],
        },
        Vertex {
            pos: [-1.0, -1.0, 0.0],
            tex0: [0.0, 1.0],
            tex1: [0.0, 1.0],
        },
        Vertex {
            pos: [1.0, 1.0, 0.0],
            tex0: [1.0, 0.0],
            tex1: [1.0, 0.0],
        },
        Vertex {
            pos: [1.0, -1.0, 0.0],
            tex0: [1.0, 1.0],
            tex1: [1.0, 1.0],
        },
    ];

    let vb = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("vb"),
        contents: bytemuck::cast_slice(&verts),
        usage: wgpu::BufferUsages::VERTEX,
    });

    let cases = [
        (TextureOp::Modulate, [32, 32, 32, 255]),
        (TextureOp::Modulate2x, [65, 65, 64, 255]),
        (TextureOp::Modulate4x, [129, 129, 128, 255]),
        (TextureOp::Add, [192, 192, 255, 255]),
        (TextureOp::AddSigned, [65, 65, 160, 255]),
        (TextureOp::AddSigned2x, [129, 129, 255, 255]),
        (TextureOp::AddSmooth, [160, 160, 255, 255]),
        (TextureOp::MultiplyAdd, [160, 96, 64, 255]),
        (TextureOp::Subtract, [64, 0, 0, 255]),
    ];

    for (op, expected) in cases {
        let desc = FixedFunctionShaderDesc {
            fvf: Fvf(Fvf::XYZ | (2 << 8)),
            stage0: TextureStageState {
                color_op: TextureOp::SelectArg1,
                color_arg0: TextureArg::Current,
                color_arg1: TextureArg::Texture,
                color_arg2: TextureArg::Current,
                alpha_op: TextureOp::SelectArg1,
                alpha_arg0: TextureArg::Current,
                alpha_arg1: TextureArg::Texture,
                alpha_arg2: TextureArg::Current,
                result_target: TextureResultTarget::Current,
            },
            stage1: TextureStageState {
                color_op: op,
                color_arg0: TextureArg::Current,
                color_arg1: TextureArg::Current,
                color_arg2: TextureArg::Texture,
                alpha_op: TextureOp::SelectArg1,
                alpha_arg0: TextureArg::Current,
                alpha_arg1: TextureArg::Current,
                alpha_arg2: TextureArg::Current,
                result_target: TextureResultTarget::Current,
            },
            alpha_test: AlphaTestState::default(),
            fog: FogState::default(),
            lighting: LightingState::default(),
        };

        let shaders = aero_d3d9::fixed_function::shader_gen::generate_fixed_function_shaders(&desc);
        let vs = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vs"),
            source: wgpu::ShaderSource::Wgsl(shaders.vertex_wgsl.clone().into()),
        });
        let fs = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("fs"),
            source: wgpu::ShaderSource::Wgsl(shaders.fragment_wgsl.clone().into()),
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
        assert_rgba_approx(pixel_at_rgba(&pixels, width, 1, 1), expected, 3);
    }
}

#[test]
fn render_two_stage_dotproduct3() {
    let Some((device, queue)) = request_device() else {
        return;
    };

    #[repr(C)]
    #[derive(Clone, Copy, Pod, Zeroable)]
    struct Vertex {
        pos: [f32; 3],
        tex0: [f32; 2],
        tex1: [f32; 2],
    }

    let width = 4;
    let height = 4;

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

    let make_tex = |label: &str, rgba: [u8; 4]| {
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
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
            &rgba,
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
        let view = tex.create_view(&Default::default());
        (tex, view)
    };

    // Chosen so DOTPRODUCT3 result stays in-range and distinguishes the +0.5 bias:
    // arg=(0.75-ish, 0.5-ish, 0.5-ish) produces ~0.75 output after the DP3 scale+bias.
    let (_tex0, tex0_view) = make_tex("tex0", [192, 128, 128, 255]);
    let (_tex1, tex1_view) = make_tex("tex1", [192, 128, 128, 255]);

    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("nearest"),
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
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
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 3,
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
                resource: wgpu::BindingResource::TextureView(&tex0_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(&tex1_view),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
        ],
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("pipeline-layout"),
        bind_group_layouts: &[&globals_bgl, &tex_bgl],
        push_constant_ranges: &[],
    });

    let verts = [
        Vertex {
            pos: [-1.0, -1.0, 0.0],
            tex0: [0.0, 1.0],
            tex1: [0.0, 1.0],
        },
        Vertex {
            pos: [-1.0, 1.0, 0.0],
            tex0: [0.0, 0.0],
            tex1: [0.0, 0.0],
        },
        Vertex {
            pos: [1.0, 1.0, 0.0],
            tex0: [1.0, 0.0],
            tex1: [1.0, 0.0],
        },
        Vertex {
            pos: [-1.0, -1.0, 0.0],
            tex0: [0.0, 1.0],
            tex1: [0.0, 1.0],
        },
        Vertex {
            pos: [1.0, 1.0, 0.0],
            tex0: [1.0, 0.0],
            tex1: [1.0, 0.0],
        },
        Vertex {
            pos: [1.0, -1.0, 0.0],
            tex0: [1.0, 1.0],
            tex1: [1.0, 1.0],
        },
    ];

    let vb = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("vb"),
        contents: bytemuck::cast_slice(&verts),
        usage: wgpu::BufferUsages::VERTEX,
    });

    let desc = FixedFunctionShaderDesc {
        fvf: Fvf(Fvf::XYZ | (2 << 8)),
        stage0: TextureStageState {
            color_op: TextureOp::SelectArg1,
            color_arg0: TextureArg::Current,
            color_arg1: TextureArg::Texture,
            color_arg2: TextureArg::Current,
            alpha_op: TextureOp::SelectArg1,
            alpha_arg0: TextureArg::Current,
            alpha_arg1: TextureArg::Texture,
            alpha_arg2: TextureArg::Current,
            result_target: TextureResultTarget::Current,
        },
        stage1: TextureStageState {
            color_op: TextureOp::DotProduct3,
            color_arg0: TextureArg::Current,
            color_arg1: TextureArg::Current,
            color_arg2: TextureArg::Texture,
            alpha_op: TextureOp::SelectArg1,
            alpha_arg0: TextureArg::Current,
            alpha_arg1: TextureArg::Current,
            alpha_arg2: TextureArg::Current,
            result_target: TextureResultTarget::Current,
        },
        alpha_test: AlphaTestState::default(),
        fog: FogState::default(),
        lighting: LightingState::default(),
    };

    let shaders = aero_d3d9::fixed_function::shader_gen::generate_fixed_function_shaders(&desc);
    let vs = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("vs"),
        source: wgpu::ShaderSource::Wgsl(shaders.vertex_wgsl.clone().into()),
    });
    let fs = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("fs"),
        source: wgpu::ShaderSource::Wgsl(shaders.fragment_wgsl.clone().into()),
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
    assert_rgba_approx(pixel_at_rgba(&pixels, width, 1, 1), [193, 193, 193, 255], 4);
}

#[test]
fn render_two_stage_blend_alpha_ops() {
    let Some((device, queue)) = request_device() else {
        return;
    };

    #[repr(C)]
    #[derive(Clone, Copy, Pod, Zeroable)]
    struct Vertex {
        pos: [f32; 3],
        tex0: [f32; 2],
        tex1: [f32; 2],
    }

    let width = 4;
    let height = 4;

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

    let make_tex = |label: &str, rgba: [u8; 4]| {
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
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
            &rgba,
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
        let view = tex.create_view(&Default::default());
        (tex, view)
    };

    // Stage0: red with alpha=0.25, Stage1: green with alpha=0.75.
    // This makes BLENDTEXTUREALPHA and BLENDCURRENTALPHA distinguishable:
    // - BLENDTEXTUREALPHA uses stage1 texture alpha => ~ (63, 192, 0)
    // - BLENDCURRENTALPHA uses current alpha (from stage0) => ~ (191, 64, 0)
    let (_tex0, tex0_view) = make_tex("tex0", [255, 0, 0, 64]);
    let (_tex1, tex1_view) = make_tex("tex1", [0, 255, 0, 192]);

    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("nearest"),
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
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
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 3,
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
                resource: wgpu::BindingResource::TextureView(&tex0_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(&tex1_view),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
        ],
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("pipeline-layout"),
        bind_group_layouts: &[&globals_bgl, &tex_bgl],
        push_constant_ranges: &[],
    });

    let verts = [
        Vertex {
            pos: [-1.0, -1.0, 0.0],
            tex0: [0.0, 1.0],
            tex1: [0.0, 1.0],
        },
        Vertex {
            pos: [-1.0, 1.0, 0.0],
            tex0: [0.0, 0.0],
            tex1: [0.0, 0.0],
        },
        Vertex {
            pos: [1.0, 1.0, 0.0],
            tex0: [1.0, 0.0],
            tex1: [1.0, 0.0],
        },
        Vertex {
            pos: [-1.0, -1.0, 0.0],
            tex0: [0.0, 1.0],
            tex1: [0.0, 1.0],
        },
        Vertex {
            pos: [1.0, 1.0, 0.0],
            tex0: [1.0, 0.0],
            tex1: [1.0, 0.0],
        },
        Vertex {
            pos: [1.0, -1.0, 0.0],
            tex0: [1.0, 1.0],
            tex1: [1.0, 1.0],
        },
    ];

    let vb = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("vb"),
        contents: bytemuck::cast_slice(&verts),
        usage: wgpu::BufferUsages::VERTEX,
    });

    let cases = [
        (TextureOp::BlendTextureAlpha, [63, 192, 0, 64]),
        (TextureOp::BlendCurrentAlpha, [191, 64, 0, 64]),
    ];

    for (op, expected) in cases {
        let desc = FixedFunctionShaderDesc {
            fvf: Fvf(Fvf::XYZ | (2 << 8)),
            stage0: TextureStageState {
                color_op: TextureOp::SelectArg1,
                color_arg0: TextureArg::Current,
                color_arg1: TextureArg::Texture,
                color_arg2: TextureArg::Current,
                alpha_op: TextureOp::SelectArg1,
                alpha_arg0: TextureArg::Current,
                alpha_arg1: TextureArg::Texture,
                alpha_arg2: TextureArg::Current,
                result_target: TextureResultTarget::Current,
            },
            stage1: TextureStageState {
                color_op: op,
                color_arg0: TextureArg::Current,
                color_arg1: TextureArg::Texture,
                color_arg2: TextureArg::Current,
                alpha_op: TextureOp::SelectArg1,
                alpha_arg0: TextureArg::Current,
                alpha_arg1: TextureArg::Current,
                alpha_arg2: TextureArg::Current,
                result_target: TextureResultTarget::Current,
            },
            alpha_test: AlphaTestState::default(),
            fog: FogState::default(),
            lighting: LightingState::default(),
        };

        let shaders = aero_d3d9::fixed_function::shader_gen::generate_fixed_function_shaders(&desc);
        let vs = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vs"),
            source: wgpu::ShaderSource::Wgsl(shaders.vertex_wgsl.clone().into()),
        });
        let fs = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("fs"),
            source: wgpu::ShaderSource::Wgsl(shaders.fragment_wgsl.clone().into()),
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
        assert_rgba_approx(pixel_at_rgba(&pixels, width, 1, 1), expected, 3);
    }
}

#[test]
fn render_fixed_function_uniform_sources_and_flags() {
    let Some((device, queue)) = request_device() else {
        return;
    };

    #[repr(C)]
    #[derive(Clone, Copy, Pod, Zeroable)]
    struct Vertex {
        pos: [f32; 3],
        diffuse: u32,
        specular: u32,
        tex0: [f32; 2],
        tex1: [f32; 2],
    }

    let width = 4;
    let height = 4;

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

    let make_tex = |label: &str, rgba: [u8; 4]| {
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
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
            &rgba,
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
        let view = tex.create_view(&Default::default());
        (tex, view)
    };

    let (_tex0, tex0_view) = make_tex("tex0", [128, 64, 32, 255]);
    let (_tex1, tex1_view) = make_tex("tex1", [64, 128, 255, 255]);

    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("nearest"),
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
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
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 3,
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
                resource: wgpu::BindingResource::TextureView(&tex0_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(&tex1_view),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
        ],
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("pipeline-layout"),
        bind_group_layouts: &[&globals_bgl, &tex_bgl],
        push_constant_ranges: &[],
    });

    let verts = [
        Vertex {
            pos: [-1.0, -1.0, 0.5],
            diffuse: 0xFFFF_FFFF,
            specular: 0x4000_0000, // alpha = 64/255
            tex0: [0.0, 1.0],
            tex1: [0.0, 1.0],
        },
        Vertex {
            pos: [-1.0, 1.0, 0.5],
            diffuse: 0xFFFF_FFFF,
            specular: 0x4000_0000,
            tex0: [0.0, 0.0],
            tex1: [0.0, 0.0],
        },
        Vertex {
            pos: [1.0, 1.0, 0.5],
            diffuse: 0xFFFF_FFFF,
            specular: 0x4000_0000,
            tex0: [1.0, 0.0],
            tex1: [1.0, 0.0],
        },
        Vertex {
            pos: [-1.0, -1.0, 0.5],
            diffuse: 0xFFFF_FFFF,
            specular: 0x4000_0000,
            tex0: [0.0, 1.0],
            tex1: [0.0, 1.0],
        },
        Vertex {
            pos: [1.0, 1.0, 0.5],
            diffuse: 0xFFFF_FFFF,
            specular: 0x4000_0000,
            tex0: [1.0, 0.0],
            tex1: [1.0, 0.0],
        },
        Vertex {
            pos: [1.0, -1.0, 0.5],
            diffuse: 0xFFFF_FFFF,
            specular: 0x4000_0000,
            tex0: [1.0, 1.0],
            tex1: [1.0, 1.0],
        },
    ];
    let vb = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("vb"),
        contents: bytemuck::cast_slice(&verts),
        usage: wgpu::BufferUsages::VERTEX,
    });

    let fvf = Fvf(Fvf::XYZ | Fvf::DIFFUSE | Fvf::SPECULAR | (2 << 8));

    let desc_base = FixedFunctionShaderDesc {
        fvf,
        stage0: TextureStageState::default(),
        stage1: TextureStageState::default(),
        alpha_test: AlphaTestState::default(),
        fog: FogState::default(),
        lighting: LightingState::default(),
    };

    let stage0_select_tex = TextureStageState {
        color_op: TextureOp::SelectArg1,
        color_arg0: TextureArg::Current,
        color_arg1: TextureArg::Texture,
        color_arg2: TextureArg::Current,
        alpha_op: TextureOp::SelectArg1,
        alpha_arg0: TextureArg::Current,
        alpha_arg1: TextureArg::Texture,
        alpha_arg2: TextureArg::Current,
        result_target: TextureResultTarget::Current,
    };

    let mut globals_base = FixedFunctionGlobals::identity();
    globals_base.viewport = [0.0, 0.0, width as f32, height as f32];

    let mut globals_tf = globals_base;
    globals_tf.texture_factor = [0.5, 0.5, 0.5, 0.5];

    let mut globals_stage_const = globals_base;
    globals_stage_const.stage_constants[1] = [0.25, 0.25, 0.25, 0.25];

    let mut globals_fog = globals_base;
    globals_fog.fog_color = [1.0, 0.0, 0.0, 1.0];
    globals_fog.fog_params = [0.0, 1.0, 0.0, 0.0];

    let cases = [
        (
            "texture-factor",
            FixedFunctionShaderDesc {
                fvf,
                stage0: stage0_select_tex,
                stage1: TextureStageState {
                    color_op: TextureOp::Modulate,
                    color_arg0: TextureArg::Current,
                    color_arg1: TextureArg::Current,
                    color_arg2: TextureArg::TextureFactor,
                    alpha_op: TextureOp::Modulate,
                    alpha_arg0: TextureArg::Current,
                    alpha_arg1: TextureArg::Current,
                    alpha_arg2: TextureArg::TextureFactor,
                    result_target: TextureResultTarget::Current,
                },
                ..desc_base.clone()
            },
            globals_tf,
            [64, 32, 16, 128],
        ),
        (
            "stage-constant",
            FixedFunctionShaderDesc {
                fvf,
                stage0: stage0_select_tex,
                stage1: TextureStageState {
                    color_op: TextureOp::Add,
                    color_arg0: TextureArg::Current,
                    color_arg1: TextureArg::Current,
                    color_arg2: TextureArg::Factor,
                    alpha_op: TextureOp::SelectArg2,
                    alpha_arg0: TextureArg::Current,
                    alpha_arg1: TextureArg::Current,
                    alpha_arg2: TextureArg::Factor,
                    result_target: TextureResultTarget::Current,
                },
                ..desc_base.clone()
            },
            globals_stage_const,
            [192, 128, 96, 64],
        ),
        (
            "arg-flags",
            FixedFunctionShaderDesc {
                fvf,
                stage0: TextureStageState {
                    color_op: TextureOp::Modulate2x,
                    color_arg0: TextureArg::Current,
                    color_arg1: TextureArg::Texture.complement(),
                    color_arg2: TextureArg::Specular.alpha_replicate(),
                    alpha_op: TextureOp::SelectArg1,
                    alpha_arg0: TextureArg::Current,
                    alpha_arg1: TextureArg::Diffuse,
                    alpha_arg2: TextureArg::Current,
                    result_target: TextureResultTarget::Current,
                },
                ..desc_base.clone()
            },
            globals_base,
            [64, 96, 112, 255],
        ),
        (
            "fog",
            FixedFunctionShaderDesc {
                fog: FogState { enabled: true },
                ..desc_base.clone()
            },
            globals_fog,
            [255, 128, 128, 255],
        ),
    ];

    for (_label, desc, globals, expected) in cases {
        let globals_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("globals"),
            contents: globals.as_bytes(),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let globals_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("globals-bg"),
            layout: &globals_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals_buf.as_entire_binding(),
            }],
        });

        let shaders = aero_d3d9::fixed_function::shader_gen::generate_fixed_function_shaders(&desc);
        let vs = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vs"),
            source: wgpu::ShaderSource::Wgsl(shaders.vertex_wgsl.clone().into()),
        });
        let fs = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("fs"),
            source: wgpu::ShaderSource::Wgsl(shaders.fragment_wgsl.clone().into()),
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
        assert_rgba_approx(
            pixel_at_rgba(&pixels, width, 1, 1),
            expected,
            3,
        );
    }
}

#[test]
fn render_two_stage_result_to_temp_then_add() {
    let Some((device, queue)) = request_device() else {
        return;
    };

    #[repr(C)]
    #[derive(Clone, Copy, Pod, Zeroable)]
    struct Vertex {
        pos: [f32; 3],
        diffuse: u32,
        tex0: [f32; 2],
        tex1: [f32; 2],
    }

    let width = 4;
    let height = 4;

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

    let make_tex = |label: &str, rgba: [u8; 4]| {
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
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
            &rgba,
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
        let view = tex.create_view(&Default::default());
        (tex, view)
    };

    let (_tex0, tex0_view) = make_tex("tex0", [128, 64, 32, 255]);
    let (_tex1, tex1_view) = make_tex("tex1", [64, 128, 255, 255]);

    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("nearest"),
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
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
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 3,
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
                resource: wgpu::BindingResource::TextureView(&tex0_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(&tex1_view),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
        ],
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("pipeline-layout"),
        bind_group_layouts: &[&globals_bgl, &tex_bgl],
        push_constant_ranges: &[],
    });

    let diffuse = 0xFF40_4040u32; // RGBA = (0.25, 0.25, 0.25, 1.0)
    let verts = [
        Vertex {
            pos: [-1.0, -1.0, 0.0],
            diffuse,
            tex0: [0.0, 1.0],
            tex1: [0.0, 1.0],
        },
        Vertex {
            pos: [-1.0, 1.0, 0.0],
            diffuse,
            tex0: [0.0, 0.0],
            tex1: [0.0, 0.0],
        },
        Vertex {
            pos: [1.0, 1.0, 0.0],
            diffuse,
            tex0: [1.0, 0.0],
            tex1: [1.0, 0.0],
        },
        Vertex {
            pos: [-1.0, -1.0, 0.0],
            diffuse,
            tex0: [0.0, 1.0],
            tex1: [0.0, 1.0],
        },
        Vertex {
            pos: [1.0, 1.0, 0.0],
            diffuse,
            tex0: [1.0, 0.0],
            tex1: [1.0, 0.0],
        },
        Vertex {
            pos: [1.0, -1.0, 0.0],
            diffuse,
            tex0: [1.0, 1.0],
            tex1: [1.0, 1.0],
        },
    ];

    let vb = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("vb"),
        contents: bytemuck::cast_slice(&verts),
        usage: wgpu::BufferUsages::VERTEX,
    });

    let desc = FixedFunctionShaderDesc {
        fvf: Fvf(Fvf::XYZ | Fvf::DIFFUSE | (2 << 8)),
        stage0: TextureStageState {
            color_op: TextureOp::SelectArg1,
            color_arg0: TextureArg::Current,
            color_arg1: TextureArg::Texture,
            color_arg2: TextureArg::Current,
            alpha_op: TextureOp::SelectArg1,
            alpha_arg0: TextureArg::Current,
            alpha_arg1: TextureArg::Texture,
            alpha_arg2: TextureArg::Current,
            result_target: TextureResultTarget::Temp,
        },
        stage1: TextureStageState {
            color_op: TextureOp::Add,
            color_arg0: TextureArg::Current,
            color_arg1: TextureArg::Temp,
            color_arg2: TextureArg::Current,
            alpha_op: TextureOp::SelectArg1,
            alpha_arg0: TextureArg::Current,
            alpha_arg1: TextureArg::Current,
            alpha_arg2: TextureArg::Current,
            result_target: TextureResultTarget::Current,
        },
        alpha_test: AlphaTestState::default(),
        fog: FogState::default(),
        lighting: LightingState::default(),
    };

    let shaders = aero_d3d9::fixed_function::shader_gen::generate_fixed_function_shaders(&desc);
    let vs = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("vs"),
        source: wgpu::ShaderSource::Wgsl(shaders.vertex_wgsl.clone().into()),
    });
    let fs = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("fs"),
        source: wgpu::ShaderSource::Wgsl(shaders.fragment_wgsl.clone().into()),
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
    assert_rgba_approx(pixel_at_rgba(&pixels, width, 1, 1), [191, 128, 96, 255], 3);
}
