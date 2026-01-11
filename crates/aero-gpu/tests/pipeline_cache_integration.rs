use aero_gpu::pipeline_cache::{PipelineCache, PipelineCacheConfig};
use aero_gpu::pipeline_key::{ColorTargetKey, ComputePipelineKey, PipelineLayoutKey, RenderPipelineKey, ShaderStage};
use aero_gpu::{GpuCapabilities, GpuError};

fn create_test_device() -> Option<(wgpu::Device, wgpu::Queue)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let needs_runtime_dir = std::env::var("XDG_RUNTIME_DIR")
            .ok()
            .map(|v| v.is_empty())
            .unwrap_or(true);

        if needs_runtime_dir {
            let dir = std::env::temp_dir()
                .join(format!("aero-gpu-xdg-runtime-{}-pipeline-cache", std::process::id()));
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
    }))?;

    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("aero-gpu integration test device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
        },
        None,
    ))
    .ok()?;

    Some((device, queue))
}

#[test]
fn render_pipeline_is_cached() {
    let Some((device, _queue)) = create_test_device() else {
        // Some environments (e.g. CI without software adapters) cannot initialize wgpu.
        // The cache itself is covered by unit tests; skip this integration test in that case.
        return;
    };

    let mut cache = PipelineCache::new(PipelineCacheConfig::default(), GpuCapabilities::from_device(&device));

    const VS: &str = r#"
        @vertex
        fn vs_main(@builtin(vertex_index) idx: u32) -> @builtin(position) vec4<f32> {
            var pos = array<vec2<f32>, 3>(
                vec2<f32>(-1.0, -1.0),
                vec2<f32>( 3.0, -1.0),
                vec2<f32>(-1.0,  3.0),
            );
            return vec4<f32>(pos[idx], 0.0, 1.0);
        }
    "#;

    const FS: &str = r#"
        @fragment
        fn fs_main() -> @location(0) vec4<f32> {
            return vec4<f32>(1.0, 0.0, 0.0, 1.0);
        }
    "#;

    let (vs_hash, _vs_module) =
        cache.get_or_create_shader_module(&device, ShaderStage::Vertex, VS, Some("vs"));
    let (fs_hash, _fs_module) =
        cache.get_or_create_shader_module(&device, ShaderStage::Fragment, FS, Some("fs"));

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("empty layout"),
        bind_group_layouts: &[],
        push_constant_ranges: &[],
    });

    let key = RenderPipelineKey {
        vertex_shader: vs_hash,
        fragment_shader: fs_hash,
        color_targets: vec![ColorTargetKey {
            format: wgpu::TextureFormat::Rgba8Unorm,
            blend: None,
            write_mask: wgpu::ColorWrites::ALL,
        }],
        depth_stencil: None,
        primitive_topology: wgpu::PrimitiveTopology::TriangleList,
        cull_mode: None,
        front_face: wgpu::FrontFace::Ccw,
        scissor_enabled: false,
        vertex_buffers: vec![],
        sample_count: 1,
        layout: PipelineLayoutKey::empty(),
    };

    let p1_ptr = {
        let p1 = cache
            .get_or_create_render_pipeline(&device, key.clone(), |device, vs, fs| {
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("solid color pipeline"),
                    layout: Some(&pipeline_layout),
                    vertex: wgpu::VertexState {
                        module: vs,
                        entry_point: "vs_main",
                        buffers: &[],
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                    },
                    fragment: Some(wgpu::FragmentState {
                        module: fs,
                        entry_point: "fs_main",
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
                    multisample: wgpu::MultisampleState {
                        count: 1,
                        ..Default::default()
                    },
                    multiview: None,
                })
            })
            .unwrap();
        p1 as *const wgpu::RenderPipeline
    };

    let stats_after_first = cache.stats();
    assert_eq!(stats_after_first.render_pipeline_misses, 1);
    assert_eq!(stats_after_first.render_pipeline_hits, 0);

    let p2_ptr = {
        let p2 = cache
            .get_or_create_render_pipeline(&device, key, |_device, _vs, _fs| {
                panic!("pipeline should have been cached")
            })
            .unwrap();
        p2 as *const wgpu::RenderPipeline
    };

    assert_eq!(p1_ptr, p2_ptr);
    let stats_after_second = cache.stats();
    assert_eq!(stats_after_second.render_pipeline_hits, 1);
}

#[test]
fn compute_pipeline_is_gated_by_capabilities() {
    let Some((device, _queue)) = create_test_device() else {
        return;
    };

    // Simulate WebGL2: compute is not supported.
    let mut cache = PipelineCache::new(
        PipelineCacheConfig::default(),
        GpuCapabilities {
            supports_compute: false,
            ..GpuCapabilities::from_device(&device)
        },
    );

    let key = ComputePipelineKey {
        shader: 0x1234,
        layout: PipelineLayoutKey::empty(),
    };

    let err = cache
        .get_or_create_compute_pipeline(&device, key, |_device, _cs| {
            panic!("compute pipeline creation must be gated before calling into wgpu")
        })
        .unwrap_err();

    assert_eq!(err, GpuError::Unsupported("compute"));
}
