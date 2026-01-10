use std::collections::HashMap;

use aero_gpu::protocol_d3d11::ResourceId;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum PipelineBinding {
    Render(ResourceId),
    Compute(ResourceId),
}

#[derive(Debug, Copy, Clone)]
pub struct BoundVertexBuffer {
    pub buffer: ResourceId,
    pub offset: u64,
}

#[derive(Debug, Copy, Clone)]
pub struct BoundIndexBuffer {
    pub buffer: ResourceId,
    pub format: wgpu::IndexFormat,
    pub offset: u64,
}

#[derive(Debug, Clone)]
pub enum BoundResource {
    Buffer {
        buffer: ResourceId,
        offset: u64,
        size: Option<u64>,
    },
    Sampler {
        sampler: ResourceId,
    },
    TextureView {
        view: ResourceId,
    },
}

#[derive(Debug, Default)]
pub struct D3D11State {
    pub current_pipeline: Option<PipelineBinding>,
    pub vertex_buffers: Vec<Option<BoundVertexBuffer>>,
    pub index_buffer: Option<BoundIndexBuffer>,
    pub bindings: HashMap<u32, BoundResource>,
}

impl D3D11State {
    pub fn new() -> Self {
        Self {
            current_pipeline: None,
            vertex_buffers: vec![None; 16],
            index_buffer: None,
            bindings: HashMap::new(),
        }
    }
}
