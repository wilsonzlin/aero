use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use aero_gpu::bindings::layout_cache::CachedBindGroupLayout;
use aero_gpu::bindings::samplers::SamplerId;
use aero_gpu::protocol_d3d11::ResourceId;

pub struct BufferResource {
    pub buffer: wgpu::Buffer,
    pub size: u64,
    pub shadow: Option<Vec<u8>>,
}

impl fmt::Debug for BufferResource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BufferResource")
            .field("buffer", &self.buffer)
            .field("size", &self.size)
            .field("shadow_len", &self.shadow.as_ref().map(|s| s.len()))
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct Texture2dDesc {
    pub width: u32,
    pub height: u32,
    pub array_layers: u32,
    pub mip_level_count: u32,
    pub format: wgpu::TextureFormat,
}

#[derive(Debug)]
pub struct TextureResource {
    pub texture: wgpu::Texture,
    pub desc: Texture2dDesc,
}

#[derive(Debug)]
pub struct TextureViewResource {
    pub view: wgpu::TextureView,
}

#[derive(Debug)]
pub struct SamplerResource {
    pub id: SamplerId,
    pub sampler: Arc<wgpu::Sampler>,
}

#[derive(Debug)]
pub struct ShaderModuleResource {
    pub module: wgpu::ShaderModule,
    pub wgsl: String,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum BindingKind {
    UniformBuffer,
    StorageBuffer { read_only: bool },
    Sampler,
    Texture2D,
    StorageTexture2DWriteOnly { format: wgpu::TextureFormat },
}

#[derive(Debug, Copy, Clone)]
pub struct BindingDef {
    pub binding: u32,
    pub visibility: wgpu::ShaderStages,
    pub kind: BindingKind,
}

#[derive(Debug)]
pub enum RenderPipelineVariants {
    NonStrip(wgpu::RenderPipeline),
    Strip {
        non_indexed: wgpu::RenderPipeline,
        u16: wgpu::RenderPipeline,
        u32: wgpu::RenderPipeline,
    },
}

impl RenderPipelineVariants {
    pub fn get(&self, strip_index_format: Option<wgpu::IndexFormat>) -> &wgpu::RenderPipeline {
        match self {
            Self::NonStrip(p) => p,
            Self::Strip {
                non_indexed,
                u16,
                u32,
            } => match strip_index_format {
                Some(wgpu::IndexFormat::Uint16) => u16,
                Some(wgpu::IndexFormat::Uint32) => u32,
                None => non_indexed,
            },
        }
    }

    pub fn uses_strip_index_format(&self) -> bool {
        matches!(self, Self::Strip { .. })
    }
}

#[derive(Debug)]
pub struct RenderPipelineResource {
    pub pipelines: RenderPipelineVariants,
    /// Optional list-topology variant used for strip primitive restart emulation.
    pub strip_to_list_pipeline: Option<wgpu::RenderPipeline>,
    pub topology: wgpu::PrimitiveTopology,
    pub bind_group_layout: CachedBindGroupLayout,
    pub bindings: Vec<BindingDef>,
}

#[derive(Debug)]
pub struct ComputePipelineResource {
    pub pipeline: wgpu::ComputePipeline,
    pub bind_group_layout: CachedBindGroupLayout,
    pub bindings: Vec<BindingDef>,
}

#[derive(Debug, Default)]
pub struct D3D11Resources {
    pub buffers: HashMap<ResourceId, BufferResource>,
    pub textures: HashMap<ResourceId, TextureResource>,
    pub texture_views: HashMap<ResourceId, TextureViewResource>,
    pub samplers: HashMap<ResourceId, SamplerResource>,
    pub shaders: HashMap<ResourceId, ShaderModuleResource>,
    pub render_pipelines: HashMap<ResourceId, RenderPipelineResource>,
    pub compute_pipelines: HashMap<ResourceId, ComputePipelineResource>,
}
