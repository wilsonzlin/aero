mod common;

use aero_d3d9::sm3::ShaderStage;
use aero_d3d9::{shader, shader_translate};
use aero_dxbc::{test_utils as dxbc_test_utils, FourCC as DxbcFourCC};
use aero_gpu::aerogpu_d3d9::{D3d9ShaderCache, D3d9ShaderCacheError, ShaderPayloadFormat};
use aero_gpu::{readback_rgba8, TextureRegion};

fn create_test_device() -> Option<(wgpu::Device, wgpu::Queue)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let needs_runtime_dir = std::env::var("XDG_RUNTIME_DIR")
            .ok()
            .map(|v| v.is_empty())
            .unwrap_or(true);

        if needs_runtime_dir {
            let dir = std::env::temp_dir().join(format!(
                "aero-gpu-xdg-runtime-{}-d3d9-shader",
                std::process::id()
            ));
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
            std::env::set_var("XDG_RUNTIME_DIR", &dir);
        }
    }

    // Prefer wgpu's GL backend on Linux CI for stability. Vulkan software adapters have been a
    // recurring source of flakes/crashes in headless sandboxes.
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: if cfg!(target_os = "linux") {
            wgpu::Backends::GL
        } else {
            wgpu::Backends::all()
        },
        ..Default::default()
    });

    let adapter = match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        compatible_surface: None,
        force_fallback_adapter: true,
    })) {
        Some(adapter) => Some(adapter),
        None => pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: false,
        })),
    }?;

    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("aero-gpu d3d9 shader test device"),
            required_features: wgpu::Features::empty(),
            required_limits: {
                let mut limits = wgpu::Limits::downlevel_defaults();
                // Match `aero-gpu`'s D3D9 executor constants buffer size.
                limits.max_uniform_buffer_binding_size =
                    limits.max_uniform_buffer_binding_size.max(18432);
                limits
            },
        },
        None,
    ))
    .ok()?;

    Some((device, queue))
}

fn enc_reg_type(ty: u8) -> u32 {
    let low = (ty & 0x7) as u32;
    let high = (ty & 0x18) as u32;
    (low << 28) | (high << 8)
}

fn enc_src(reg_type: u8, reg_num: u16, swizzle: u8) -> u32 {
    enc_reg_type(reg_type) | (reg_num as u32) | ((swizzle as u32) << 16)
}

fn enc_dst(reg_type: u8, reg_num: u16, mask: u8) -> u32 {
    enc_reg_type(reg_type) | (reg_num as u32) | ((mask as u32) << 16)
}

fn enc_inst(opcode: u16, params: &[u32]) -> Vec<u32> {
    // The minimal translator only consumes opcode + "length" in bits 24..27.
    let token = (opcode as u32) | (((params.len() as u32) + 1) << 24);
    let mut v = vec![token];
    v.extend_from_slice(params);
    v
}

fn to_bytes(words: &[u32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(words.len() * 4);
    for w in words {
        bytes.extend_from_slice(&w.to_le_bytes());
    }
    bytes
}

fn assemble_vs_fullscreen_pos_tex_color() -> Vec<u8> {
    // vs_2_0
    let mut out = vec![0xFFFE0200];
    // mov oPos, v0
    out.extend(enc_inst(0x0001, &[enc_dst(4, 0, 0xF), enc_src(1, 0, 0xE4)]));
    // mov oT0, v1
    out.extend(enc_inst(0x0001, &[enc_dst(6, 0, 0xF), enc_src(1, 1, 0xE4)]));
    // mov oD0, v2
    out.extend(enc_inst(0x0001, &[enc_dst(5, 0, 0xF), enc_src(1, 2, 0xE4)]));
    // end
    out.push(0x0000FFFF);
    to_bytes(&out)
}

fn assemble_vs_fullscreen_pos_only() -> Vec<u8> {
    // vs_2_0
    let mut out = vec![0xFFFE0200];
    // mov oPos, v0
    out.extend(enc_inst(0x0001, &[enc_dst(4, 0, 0xF), enc_src(1, 0, 0xE4)]));
    // end
    out.push(0x0000FFFF);
    to_bytes(&out)
}

fn assemble_ps_tex_mad() -> Vec<u8> {
    // ps_2_0
    let mut out = vec![0xFFFF0200];
    // texld r0, t0, s0
    out.extend(enc_inst(
        0x0042,
        &[
            enc_dst(0, 0, 0xF),   // r0
            enc_src(3, 0, 0xE4),  // t0
            enc_src(10, 0, 0xE4), // s0
        ],
    ));
    // mad r0, r0, v0, c0
    out.extend(enc_inst(
        0x0004,
        &[
            enc_dst(0, 0, 0xF),
            enc_src(0, 0, 0xE4), // r0
            enc_src(1, 0, 0xE4), // v0 (color)
            enc_src(2, 0, 0xE4), // c0
        ],
    ));
    // mov oC0, r0
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 0, 0xE4)]));
    out.push(0x0000FFFF);
    to_bytes(&out)
}

fn assemble_ps_unknown_opcode_fallback() -> Vec<u8> {
    // ps_2_0
    let mut out = vec![0xFFFF0200];
    // Unknown opcode that is rejected by the strict SM3 pipeline but skipped by the legacy
    // translator.
    out.extend(enc_inst(0x1234, &[]));
    // mov oC0, c0
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(2, 0, 0xE4)]));
    // end
    out.push(0x0000FFFF);
    to_bytes(&out)
}

#[test]
fn d3d9_shader_cache_rejects_oversized_payloads() {
    let Some((device, _queue)) = create_test_device() else {
        common::skip_or_panic(module_path!(), "no wgpu adapter available");
        return;
    };

    let mut cache = D3d9ShaderCache::new();
    let bytes = vec![0u8; aero_gpu::aerogpu_d3d9::MAX_D3D9_SHADER_BLOB_BYTES + 1];

    let err = cache
        .create_shader(&device, 1, ShaderStage::Vertex, &bytes)
        .unwrap_err();
    match err {
        D3d9ShaderCacheError::PayloadTooLarge { len, max } => {
            assert_eq!(len, aero_gpu::aerogpu_d3d9::MAX_D3D9_SHADER_BLOB_BYTES + 1);
            assert_eq!(max, aero_gpu::aerogpu_d3d9::MAX_D3D9_SHADER_BLOB_BYTES);
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn d3d9_token_stream_shaders_render_fullscreen_triangle() {
    let Some((device, queue)) = create_test_device() else {
        common::skip_or_panic(module_path!(), "no wgpu adapter available");
        return;
    };

    let mut cache = D3d9ShaderCache::new();
    let vs_bytes = assemble_vs_fullscreen_pos_tex_color();
    let ps_bytes = assemble_ps_tex_mad();

    cache
        .create_shader(&device, 1, ShaderStage::Vertex, &vs_bytes)
        .unwrap();
    cache
        .create_shader(&device, 2, ShaderStage::Pixel, &ps_bytes)
        .unwrap();

    let vs = cache.get(1).unwrap();
    let ps = cache.get(2).unwrap();

    // Constants buffer: float + int + bool constant banks for VS+PS.
    //
    // This must match the `Constants` WGSL layout used by the shader translators, and the upload
    // layout used by the D3D9 executor.
    //
    // Byte layout:
    // - Float constants: 512 * vec4<f32> (VS then PS)
    // - Int constants:   512 * vec4<i32> (VS then PS)
    // - Bool constants:  512 * u32 (VS then PS)
    //
    // Note: bools are packed in WGSL as `array<vec4<u32>, 128>`, but the underlying uniform bytes
    // are still a linear `u32[512]` region.
    const CONSTANTS_FLOAT_BANK_SIZE_BYTES: u64 = 512 * 16;
    const CONSTANTS_INT_BANK_SIZE_BYTES: u64 = 512 * 16;
    const CONSTANTS_BOOL_BANK_SIZE_BYTES: u64 = 512 * 4;
    const CONSTANTS_FLOATS_OFFSET_BYTES: u64 = 0;
    const CONSTANTS_BUFFER_SIZE_BYTES: u64 = CONSTANTS_FLOAT_BANK_SIZE_BYTES
        + CONSTANTS_INT_BANK_SIZE_BYTES
        + CONSTANTS_BOOL_BANK_SIZE_BYTES;
    let constants = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("d3d9 constants"),
        size: CONSTANTS_BUFFER_SIZE_BYTES,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    // Initialize c0 to 0 so `mad r0, r0, v0, c0` becomes a pure multiply.
    let ps_c0_offset = 256u64 * 16;
    queue.write_buffer(
        &constants,
        CONSTANTS_FLOATS_OFFSET_BYTES + ps_c0_offset,
        &[0u8; 16],
    );

    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("sample tex"),
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
            texture: &texture,
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
    let texture_view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("sample sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        mipmap_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
    });

    // Match `aero-d3d9` token stream shader translation binding contract:
    // - group(0): constants (binding 0)
    // - group(1): VS texture/sampler bindings
    // - group(2): PS texture/sampler bindings
    //
    // Binding numbers are derived from sampler register index:
    //   texture binding = 2*s
    //   sampler binding = 2*s + 1
    let constants_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("d3d9 constants bgl"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: wgpu::BufferSize::new(CONSTANTS_BUFFER_SIZE_BYTES),
            },
            count: None,
        }],
    });
    let vs_samplers_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("d3d9 vs samplers bgl (empty)"),
        entries: &[],
    });
    let ps_samplers_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("d3d9 ps samplers bgl"),
        entries: &[
            // s0 texture.
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
            // s0 sampler.
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });

    let constants_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("d3d9 constants bg"),
        layout: &constants_bgl,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: constants.as_entire_binding(),
        }],
    });
    let vs_samplers_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("d3d9 vs samplers bg (empty)"),
        layout: &vs_samplers_bgl,
        entries: &[],
    });
    let ps_samplers_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("d3d9 ps samplers bg"),
        layout: &ps_samplers_bgl,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&texture_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
        ],
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("d3d9 pipeline layout"),
        bind_group_layouts: &[&constants_bgl, &vs_samplers_bgl, &ps_samplers_bgl],
        push_constant_ranges: &[],
    });

    #[repr(C)]
    #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
    struct Vertex {
        pos: [f32; 4],
        tex: [f32; 4],
        color: [f32; 4],
    }

    let white = [1.0, 1.0, 1.0, 1.0];
    let verts = [
        Vertex {
            pos: [-1.0, -1.0, 0.0, 1.0],
            tex: [0.0, 0.0, 0.0, 0.0],
            color: white,
        },
        Vertex {
            pos: [3.0, -1.0, 0.0, 1.0],
            tex: [0.0, 0.0, 0.0, 0.0],
            color: white,
        },
        Vertex {
            pos: [-1.0, 3.0, 0.0, 1.0],
            tex: [0.0, 0.0, 0.0, 0.0],
            color: white,
        },
    ];

    let vb = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("vb"),
        size: (std::mem::size_of_val(&verts)) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&vb, 0, bytemuck::cast_slice(&verts));

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("d3d9 pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &vs.module,
            entry_point: vs.entry_point,
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<Vertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &[
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x4,
                        offset: 0,
                        shader_location: 0,
                    },
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x4,
                        offset: 16,
                        shader_location: 1,
                    },
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x4,
                        offset: 32,
                        shader_location: 2,
                    },
                ],
            }],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &ps.module,
            entry_point: ps.entry_point,
            targets: &[Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::Rgba8Unorm,
                blend: None,
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

    let rt = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("rt"),
        size: wgpu::Extent3d {
            width: 16,
            height: 16,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let rt_view = rt.create_view(&wgpu::TextureViewDescriptor::default());

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("encode"),
    });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &rt_view,
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
        pass.set_bind_group(0, &constants_bg, &[]);
        pass.set_bind_group(1, &vs_samplers_bg, &[]);
        pass.set_bind_group(2, &ps_samplers_bg, &[]);
        pass.set_vertex_buffer(0, vb.slice(..));
        pass.draw(0..3, 0..1);
    }
    queue.submit([encoder.finish()]);

    let rgba = pollster::block_on(readback_rgba8(
        &device,
        &queue,
        &rt,
        TextureRegion {
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            size: wgpu::Extent3d {
                width: 16,
                height: 16,
                depth_or_array_layers: 1,
            },
        },
    ));
    let hash = blake3::hash(&rgba);
    assert_eq!(
        hash.to_hex().as_str(),
        "19090216277806527f728c6d47abb2566ebbc60c0d243f9198cf1f84b862b34a"
    );
    assert_eq!(
        &rgba[(8 * 16 + 8) * 4..(8 * 16 + 8) * 4 + 4],
        &[255, 0, 0, 255]
    );
}

#[test]
fn d3d9_token_stream_legacy_fallback_shader_uses_executor_constants_layout() {
    let Some((device, queue)) = create_test_device() else {
        common::skip_or_panic(module_path!(), "no wgpu adapter available");
        return;
    };

    let mut cache = D3d9ShaderCache::new();
    let vs_bytes = assemble_vs_fullscreen_pos_only();
    let ps_bytes = assemble_ps_unknown_opcode_fallback();

    // Sanity-check that this shader actually exercises the SM3->legacy fallback path. This keeps
    // the test meaningful if the strict SM3 pipeline gains support for this opcode in the future.
    let translated = shader_translate::translate_d3d9_shader_to_wgsl(&ps_bytes, shader::WgslOptions::default())
        .expect("shader translation succeeds");
    assert_eq!(
        translated.backend,
        shader_translate::ShaderTranslateBackend::LegacyFallback
    );

    cache
        .create_shader(&device, 1, ShaderStage::Vertex, &vs_bytes)
        .unwrap();
    cache
        .create_shader(&device, 2, ShaderStage::Pixel, &ps_bytes)
        .unwrap();

    let vs = cache.get(1).unwrap();
    let ps = cache.get(2).unwrap();

    // Constants buffer: float + int + bool constant banks for VS+PS.
    //
    // This must match the `Constants` WGSL layout used by the shader translators, and the upload
    // layout used by the D3D9 executor.
    const CONSTANTS_FLOAT_BANK_SIZE_BYTES: u64 = 512 * 16;
    const CONSTANTS_INT_BANK_SIZE_BYTES: u64 = 512 * 16;
    const CONSTANTS_BOOL_BANK_SIZE_BYTES: u64 = 512 * 4;
    const CONSTANTS_BUFFER_SIZE_BYTES: u64 = CONSTANTS_FLOAT_BANK_SIZE_BYTES
        + CONSTANTS_INT_BANK_SIZE_BYTES
        + CONSTANTS_BOOL_BANK_SIZE_BYTES;
    let constants = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("d3d9 constants"),
        size: CONSTANTS_BUFFER_SIZE_BYTES,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    // Pixel shader c0 lives at index 256 in the packed register file.
    let ps_c0_offset_bytes = 256u64 * 16;
    let green = [0.0f32, 1.0, 0.0, 1.0];
    queue.write_buffer(
        &constants,
        ps_c0_offset_bytes,
        bytemuck::cast_slice(&green),
    );

    let constants_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("d3d9 constants bgl"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: wgpu::BufferSize::new(CONSTANTS_BUFFER_SIZE_BYTES),
            },
            count: None,
        }],
    });
    let vs_samplers_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("d3d9 vs samplers bgl (empty)"),
        entries: &[],
    });
    let ps_samplers_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("d3d9 ps samplers bgl (empty)"),
        entries: &[],
    });

    let constants_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("d3d9 constants bg"),
        layout: &constants_bgl,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: constants.as_entire_binding(),
        }],
    });
    let vs_samplers_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("d3d9 vs samplers bg (empty)"),
        layout: &vs_samplers_bgl,
        entries: &[],
    });
    let ps_samplers_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("d3d9 ps samplers bg (empty)"),
        layout: &ps_samplers_bgl,
        entries: &[],
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("d3d9 pipeline layout"),
        bind_group_layouts: &[&constants_bgl, &vs_samplers_bgl, &ps_samplers_bgl],
        push_constant_ranges: &[],
    });

    #[repr(C)]
    #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
    struct Vertex {
        pos: [f32; 4],
    }

    let verts = [
        Vertex {
            pos: [-1.0, -1.0, 0.0, 1.0],
        },
        Vertex {
            pos: [3.0, -1.0, 0.0, 1.0],
        },
        Vertex {
            pos: [-1.0, 3.0, 0.0, 1.0],
        },
    ];

    let vb = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("vb"),
        size: (std::mem::size_of_val(&verts)) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&vb, 0, bytemuck::cast_slice(&verts));

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("d3d9 legacy fallback pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &vs.module,
            entry_point: vs.entry_point,
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<Vertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &[wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x4,
                    offset: 0,
                    shader_location: 0,
                }],
            }],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &ps.module,
            entry_point: ps.entry_point,
            targets: &[Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::Rgba8Unorm,
                blend: None,
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

    let rt = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("rt"),
        size: wgpu::Extent3d {
            width: 16,
            height: 16,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let rt_view = rt.create_view(&wgpu::TextureViewDescriptor::default());

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("encode"),
    });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &rt_view,
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
        pass.set_bind_group(0, &constants_bg, &[]);
        pass.set_bind_group(1, &vs_samplers_bg, &[]);
        pass.set_bind_group(2, &ps_samplers_bg, &[]);
        pass.set_vertex_buffer(0, vb.slice(..));
        pass.draw(0..3, 0..1);
    }
    queue.submit([encoder.finish()]);

    let rgba = pollster::block_on(readback_rgba8(
        &device,
        &queue,
        &rt,
        TextureRegion {
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            size: wgpu::Extent3d {
                width: 16,
                height: 16,
                depth_or_array_layers: 1,
            },
        },
    ));
    // Sample the center pixel.
    assert_eq!(
        &rgba[(8 * 16 + 8) * 4..(8 * 16 + 8) * 4 + 4],
        &[0, 255, 0, 255]
    );
}

#[test]
fn dxbc_prefixed_payload_is_detected_and_translated() {
    let Some((device, _queue)) = create_test_device() else {
        common::skip_or_panic(module_path!(), "no wgpu adapter available");
        return;
    };

    let ps_token_stream = assemble_ps_tex_mad();
    let dxbc_blob = dxbc_test_utils::build_container(&[(DxbcFourCC(*b"SHDR"), &ps_token_stream)]);

    assert_eq!(
        ShaderPayloadFormat::detect(&dxbc_blob),
        ShaderPayloadFormat::Dxbc
    );

    let mut cache = D3d9ShaderCache::new();
    cache
        .create_shader(&device, 1, ShaderStage::Pixel, &dxbc_blob)
        .unwrap();

    let shader = cache.get(1).unwrap();
    assert_eq!(shader.payload_format, ShaderPayloadFormat::Dxbc);
    assert_eq!(shader.version.major, 2);
    assert!(shader.wgsl.contains("textureSample"));
}

#[test]
fn d3d9_shader_cache_legacy_fallback_allows_unknown_opcode() {
    let Some((device, _queue)) = create_test_device() else {
        common::skip_or_panic(module_path!(), "no wgpu adapter available");
        return;
    };

    let ps_bytes = assemble_ps_unknown_opcode_fallback();
    let mut cache = D3d9ShaderCache::new();

    // Should succeed via `aero_d3d9::shader_translate`'s legacy fallback path.
    cache
        .create_shader(&device, 1, ShaderStage::Pixel, &ps_bytes)
        .unwrap();
}
