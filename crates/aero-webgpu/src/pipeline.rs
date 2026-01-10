use std::collections::HashMap;
use std::hash::Hash;

/// A tiny cache helper for GPU objects keyed by a hashable descriptor key.
///
/// This is intentionally generic so higher layers can use it for render pipelines,
/// bind group layouts, samplers, etc.
#[derive(Default)]
pub struct Cache<K, V> {
    map: HashMap<K, V>,
}

impl<K, V> Cache<K, V>
where
    K: Eq + Hash,
{
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn clear(&mut self) {
        self.map.clear();
    }

    pub fn get_or_create<F>(&mut self, key: K, create: F) -> &V
    where
        F: FnOnce() -> V,
    {
        self.map.entry(key).or_insert_with(create)
    }
}

/// Create a simple fullscreen-triangle render pipeline.
///
/// The shader module must provide:
/// - `@vertex fn vs_main(@builtin(vertex_index) ...) -> @builtin(position) ...`
/// - a fragment entry point specified by `fragment_entry_point`.
pub fn create_fullscreen_triangle_pipeline(
    device: &wgpu::Device,
    bind_group_layout: &wgpu::BindGroupLayout,
    shader: &wgpu::ShaderModule,
    fragment_entry_point: &str,
    color_format: wgpu::TextureFormat,
    label: Option<&str>,
) -> wgpu::RenderPipeline {
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label,
        bind_group_layouts: &[bind_group_layout],
        push_constant_ranges: &[],
    });

    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label,
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: "vs_main",
            buffers: &[],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: fragment_entry_point,
            targets: &[Some(wgpu::ColorTargetState {
                format: color_format,
                blend: Some(wgpu::BlendState::REPLACE),
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
    })
}
