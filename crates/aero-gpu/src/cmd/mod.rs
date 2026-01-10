//! Internal, backend-agnostic GPU command stream representation plus
//! optimization and encoding layers.

mod encode;
mod optimize;

pub use encode::{EncodeError, EncodeMetrics, EncodeResult, Encoder, ResourceProvider};
pub use optimize::{CommandOptimizer, OptimizeMetrics, OptimizeResult};

use std::ops::Range;

/// Lightweight handle into an internal pipeline cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PipelineId(pub u32);

/// Lightweight handle into an internal bind-group cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BindGroupId(pub u32);

/// Lightweight handle into an internal buffer cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BufferId(pub u32);

/// Lightweight handle into an internal texture-view cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TextureViewId(pub u32);

/// Backend-agnostic index format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IndexFormat {
    Uint16,
    Uint32,
}

/// Backend-agnostic color type matching WebGPU semantics.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}

impl Color {
    pub const TRANSPARENT_BLACK: Self = Self {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.0,
    };
}

/// Backend-agnostic load operation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LoadOp<T> {
    Load,
    Clear(T),
}

/// Backend-agnostic store operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreOp {
    Store,
    Discard,
}

/// Backend-agnostic load+store operations.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Operations<T> {
    pub load: LoadOp<T>,
    pub store: StoreOp,
}

/// Backend-agnostic render pass descriptor.
#[derive(Clone, Debug, PartialEq)]
pub struct RenderPassDesc {
    pub label: Option<String>,
    pub color_attachments: Vec<RenderPassColorAttachmentDesc>,
    pub depth_stencil_attachment: Option<RenderPassDepthStencilAttachmentDesc>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RenderPassColorAttachmentDesc {
    pub view: TextureViewId,
    pub resolve_target: Option<TextureViewId>,
    pub ops: Operations<Color>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RenderPassDepthStencilAttachmentDesc {
    pub view: TextureViewId,
    pub depth_ops: Option<Operations<f32>>,
    pub stencil_ops: Option<Operations<u32>>,
}

/// Backend-agnostic GPU command stream (internal to the GPU worker).
///
/// This format is not guest-visible. It exists to decouple command decoding from
/// backend encoding and enable CPU-side optimization (redundant state elision,
/// draw coalescing, ...).
#[derive(Clone, Debug, PartialEq)]
pub enum GpuCmd {
    BeginRenderPass(RenderPassDesc),
    EndRenderPass,

    SetPipeline(PipelineId),
    SetBindGroup {
        slot: u32,
        bind_group: BindGroupId,
        dynamic_offsets: Vec<u32>,
    },
    SetVertexBuffer {
        slot: u32,
        buffer: BufferId,
        offset: u64,
        size: Option<u64>,
    },
    SetIndexBuffer {
        buffer: BufferId,
        format: IndexFormat,
        offset: u64,
        size: Option<u64>,
    },

    Draw {
        vertex_count: u32,
        instance_count: u32,
        first_vertex: u32,
        first_instance: u32,
    },
    DrawIndexed {
        index_count: u32,
        instance_count: u32,
        first_index: u32,
        base_vertex: i32,
        first_instance: u32,
    },
}

impl GpuCmd {
    pub fn draw(vertices: Range<u32>, instances: Range<u32>) -> Self {
        Self::Draw {
            vertex_count: vertices.end - vertices.start,
            instance_count: instances.end - instances.start,
            first_vertex: vertices.start,
            first_instance: instances.start,
        }
    }

    pub fn draw_indexed(indices: Range<u32>, base_vertex: i32, instances: Range<u32>) -> Self {
        Self::DrawIndexed {
            index_count: indices.end - indices.start,
            instance_count: instances.end - instances.start,
            first_index: indices.start,
            base_vertex,
            first_instance: instances.start,
        }
    }
}
