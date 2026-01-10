use aero_gpu::cmd::{
    BindGroupId, BufferId, Color, CommandOptimizer, Encoder, GpuCmd, IndexFormat, LoadOp,
    Operations, PipelineId, RenderPassColorAttachmentDesc, RenderPassDesc, StoreOp, TextureViewId,
};

use std::borrow::Cow;
use wgpu::util::DeviceExt;

struct TestResources {
    pipelines: Vec<wgpu::RenderPipeline>,
    bind_groups: Vec<wgpu::BindGroup>,
    buffers: Vec<wgpu::Buffer>,
    textures: Vec<wgpu::Texture>,
    views: Vec<wgpu::TextureView>,
}

impl TestResources {
    fn new() -> Self {
        Self {
            pipelines: Vec::new(),
            bind_groups: Vec::new(),
            buffers: Vec::new(),
            textures: Vec::new(),
            views: Vec::new(),
        }
    }

    fn add_pipeline(&mut self, pipeline: wgpu::RenderPipeline) -> PipelineId {
        let id = PipelineId(self.pipelines.len() as u32);
        self.pipelines.push(pipeline);
        id
    }

    fn add_buffer(&mut self, buffer: wgpu::Buffer) -> BufferId {
        let id = BufferId(self.buffers.len() as u32);
        self.buffers.push(buffer);
        id
    }

    fn add_texture_view(
        &mut self,
        texture: wgpu::Texture,
        view: wgpu::TextureView,
    ) -> TextureViewId {
        let id = TextureViewId(self.views.len() as u32);
        self.textures.push(texture);
        self.views.push(view);
        id
    }
}

impl aero_gpu::cmd::ResourceProvider for TestResources {
    fn pipeline(&self, id: PipelineId) -> Option<&wgpu::RenderPipeline> {
        self.pipelines.get(id.0 as usize)
    }

    fn bind_group(&self, id: BindGroupId) -> Option<&wgpu::BindGroup> {
        self.bind_groups.get(id.0 as usize)
    }

    fn buffer(&self, id: BufferId) -> Option<&wgpu::Buffer> {
        self.buffers.get(id.0 as usize)
    }

    fn texture_view(&self, id: TextureViewId) -> Option<&wgpu::TextureView> {
        self.views.get(id.0 as usize)
    }
}

#[test]
fn encode_and_submit_command_list_without_validation_errors() {
    pollster::block_on(async {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let needs_runtime_dir = std::env::var("XDG_RUNTIME_DIR")
                .ok()
                .map(|v| v.is_empty())
                .unwrap_or(true);

            if needs_runtime_dir {
                let dir = std::env::temp_dir()
                    .join(format!("aero-gpu-xdg-runtime-{}", std::process::id()));
                std::fs::create_dir_all(&dir).expect("create XDG_RUNTIME_DIR");
                std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
                    .expect("chmod XDG_RUNTIME_DIR");
                std::env::set_var("XDG_RUNTIME_DIR", &dir);
            }
        }

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            dx12_shader_compiler: Default::default(),
            flags: wgpu::InstanceFlags::default(),
            gles_minor_version: wgpu::Gles3MinorVersion::Automatic,
        });

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: None,
                force_fallback_adapter: true,
            })
            .await;

        let Some(adapter) = adapter else {
            eprintln!("skipping encode_submit test: no wgpu adapter available");
            return;
        };

        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: None,
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::downlevel_defaults(),
                },
                None,
            )
            .await
            .expect("request_device");

        let format = wgpu::TextureFormat::Rgba8Unorm;
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: 4,
                height: 4,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: None,
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(
                r#"
struct VertexInput {
  @location(0) pos: vec2<f32>,
};

@vertex
fn vs_main(in: VertexInput) -> @builtin(position) vec4<f32> {
  return vec4<f32>(in.pos, 0.0, 1.0);
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
  return vec4<f32>(1.0, 0.0, 0.0, 1.0);
}
"#,
            )),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[],
            push_constant_ranges: &[],
        });

        const ATTRS: [wgpu::VertexAttribute; 1] = [wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x2,
            offset: 0,
            shader_location: 0,
        }];

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: None,
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<[f32; 2]>() as wgpu::BufferAddress,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &ATTRS,
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_main",
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });

        let vertices: [[f32; 2]; 6] = [
            [-0.5, -0.5],
            [0.5, -0.5],
            [0.0, 0.5],
            [-0.5, 0.5],
            [0.5, 0.5],
            [0.0, -0.5],
        ];

        let indices: [u16; 6] = [0, 1, 2, 3, 4, 5];

        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: bytemuck::cast_slice(&vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });

        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: bytemuck::cast_slice(&indices),
            usage: wgpu::BufferUsages::INDEX,
        });

        let mut resources = TestResources::new();
        let view_id = resources.add_texture_view(texture, view);
        let pipeline_id = resources.add_pipeline(pipeline);
        let vertex_buffer_id = resources.add_buffer(vertex_buffer);
        let index_buffer_id = resources.add_buffer(index_buffer);

        let cmd_list = vec![
            GpuCmd::BeginRenderPass(RenderPassDesc {
                label: None,
                color_attachments: vec![RenderPassColorAttachmentDesc {
                    view: view_id,
                    resolve_target: None,
                    ops: Operations {
                        load: LoadOp::Clear(Color::TRANSPARENT_BLACK),
                        store: StoreOp::Store,
                    },
                }],
                depth_stencil_attachment: None,
            }),
            GpuCmd::SetPipeline(pipeline_id),
            // Intentional redundancy to exercise the optimizer.
            GpuCmd::SetPipeline(pipeline_id),
            GpuCmd::SetVertexBuffer {
                slot: 0,
                buffer: vertex_buffer_id,
                offset: 0,
                size: None,
            },
            GpuCmd::SetIndexBuffer {
                buffer: index_buffer_id,
                format: IndexFormat::Uint16,
                offset: 0,
                size: None,
            },
            // Two contiguous draw calls that can be merged when draw coalescing is enabled.
            GpuCmd::DrawIndexed {
                index_count: 3,
                instance_count: 1,
                first_index: 0,
                base_vertex: 0,
                first_instance: 0,
            },
            GpuCmd::DrawIndexed {
                index_count: 3,
                instance_count: 1,
                first_index: 3,
                base_vertex: 0,
                first_instance: 0,
            },
            GpuCmd::EndRenderPass,
        ];

        let optimizer = CommandOptimizer::default();
        let optimized = optimizer.optimize(cmd_list);
        assert!(optimized.metrics.commands_out < optimized.metrics.commands_in);

        let encoder = Encoder::new(&device, &resources);

        device.push_error_scope(wgpu::ErrorFilter::Validation);
        let encoded = encoder.encode(&optimized.cmds).expect("encode");
        assert_eq!(encoded.metrics.render_passes, 1);
        assert_eq!(encoded.metrics.draw_calls, 1);
        assert_eq!(encoded.metrics.pipeline_switches, 1);
        assert_eq!(encoded.metrics.bind_group_changes, 0);
        queue.submit(Some(encoded.command_buffer));
        device.poll(wgpu::Maintain::Wait);
        let err = device.pop_error_scope().await;
        assert!(err.is_none(), "wgpu validation error: {err:?}");
    });
}
