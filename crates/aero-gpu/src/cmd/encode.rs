use super::{
    BindGroupId, BufferId, Color, GpuCmd, IndexFormat, LoadOp, Operations, PipelineId,
    RenderPassColorAttachmentDesc, RenderPassDepthStencilAttachmentDesc, RenderPassDesc, StoreOp,
    TextureViewId,
};

use std::borrow::Cow;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EncodeMetrics {
    pub commands_in: usize,
    pub render_passes: u32,
    pub draw_calls: u32,
    pub pipeline_switches: u32,
    pub bind_group_changes: u32,
    pub encode_time: Duration,
}

#[derive(Debug)]
pub struct EncodeResult {
    pub command_buffer: wgpu::CommandBuffer,
    pub metrics: EncodeMetrics,
}

#[derive(Debug)]
pub enum EncodeError {
    MissingPipeline(PipelineId),
    MissingBindGroup(BindGroupId),
    MissingBuffer(BufferId),
    MissingTextureView(TextureViewId),
    ArithmeticOverflow(&'static str),
    UnexpectedEndRenderPass,
    UnexpectedCommandOutsideRenderPass,
    UnterminatedRenderPass,
}

impl std::fmt::Display for EncodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EncodeError::MissingPipeline(id) => write!(f, "missing render pipeline {id:?}"),
            EncodeError::MissingBindGroup(id) => write!(f, "missing bind group {id:?}"),
            EncodeError::MissingBuffer(id) => write!(f, "missing buffer {id:?}"),
            EncodeError::MissingTextureView(id) => write!(f, "missing texture view {id:?}"),
            EncodeError::ArithmeticOverflow(ctx) => {
                write!(f, "arithmetic overflow while encoding {ctx}")
            }
            EncodeError::UnexpectedEndRenderPass => write!(f, "unexpected EndRenderPass"),
            EncodeError::UnexpectedCommandOutsideRenderPass => {
                write!(f, "unexpected command outside render pass")
            }
            EncodeError::UnterminatedRenderPass => write!(f, "unterminated render pass"),
        }
    }
}

impl std::error::Error for EncodeError {}

fn range_end_u32(start: u32, count: u32, context: &'static str) -> Result<u32, EncodeError> {
    start
        .checked_add(count)
        .ok_or(EncodeError::ArithmeticOverflow(context))
}

fn range_end_u64(start: u64, size: u64, context: &'static str) -> Result<u64, EncodeError> {
    start
        .checked_add(size)
        .ok_or(EncodeError::ArithmeticOverflow(context))
}

/// Lookup interface that maps lightweight IDs in [`GpuCmd`] to wgpu resources.
pub trait ResourceProvider {
    fn pipeline(&self, id: PipelineId) -> Option<&wgpu::RenderPipeline>;
    fn bind_group(&self, id: BindGroupId) -> Option<&wgpu::BindGroup>;
    fn buffer(&self, id: BufferId) -> Option<&wgpu::Buffer>;
    fn texture_view(&self, id: TextureViewId) -> Option<&wgpu::TextureView>;
}

/// Encoder that turns an optimized [`GpuCmd`] stream into a wgpu command buffer.
pub struct Encoder<'a, R: ResourceProvider> {
    device: &'a wgpu::Device,
    resources: &'a R,
}

impl<'a, R: ResourceProvider> Encoder<'a, R> {
    pub fn new(device: &'a wgpu::Device, resources: &'a R) -> Self {
        Self { device, resources }
    }

    pub fn encode(&self, cmds: &[GpuCmd]) -> Result<EncodeResult, EncodeError> {
        let start = Instant::now();
        let mut metrics = EncodeMetrics {
            commands_in: cmds.len(),
            ..EncodeMetrics::default()
        };
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });

        let mut i = 0;
        while i < cmds.len() {
            match &cmds[i] {
                GpuCmd::BeginRenderPass(desc) => {
                    i = self.encode_render_pass(&mut encoder, desc, cmds, i + 1, &mut metrics)?;
                }
                GpuCmd::EndRenderPass => return Err(EncodeError::UnexpectedEndRenderPass),
                _ => return Err(EncodeError::UnexpectedCommandOutsideRenderPass),
            }
        }

        let command_buffer = encoder.finish();
        metrics.encode_time = start.elapsed();
        Ok(EncodeResult {
            command_buffer,
            metrics,
        })
    }

    fn encode_render_pass(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        desc: &RenderPassDesc,
        cmds: &[GpuCmd],
        mut i: usize,
        metrics: &mut EncodeMetrics,
    ) -> Result<usize, EncodeError> {
        metrics.render_passes = metrics.render_passes.saturating_add(1);
        let mut color_attachments: Vec<Option<wgpu::RenderPassColorAttachment<'_>>> =
            Vec::with_capacity(desc.color_attachments.len());
        for ca in &desc.color_attachments {
            color_attachments.push(Some(self.encode_color_attachment(ca)?));
        }

        let depth_stencil_attachment: Option<wgpu::RenderPassDepthStencilAttachment<'_>> =
            match &desc.depth_stencil_attachment {
                Some(ds) => Some(self.encode_depth_stencil_attachment(ds)?),
                None => None,
            };

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: desc.label.as_deref(),
            color_attachments: &color_attachments,
            depth_stencil_attachment,
            occlusion_query_set: None,
            timestamp_writes: None,
        });

        let mut last_pipeline: Option<PipelineId> = None;
        let mut last_bind_groups: Vec<Option<(BindGroupId, &[u32])>> = Vec::new();

        while i < cmds.len() {
            match &cmds[i] {
                GpuCmd::EndRenderPass => return Ok(i + 1),
                GpuCmd::SetPipeline(id) => {
                    let pipeline = self
                        .resources
                        .pipeline(*id)
                        .ok_or(EncodeError::MissingPipeline(*id))?;
                    if last_pipeline != Some(*id) {
                        metrics.pipeline_switches = metrics.pipeline_switches.saturating_add(1);
                        last_pipeline = Some(*id);
                    }
                    pass.set_pipeline(pipeline);
                }
                GpuCmd::SetBindGroup {
                    slot,
                    bind_group,
                    dynamic_offsets,
                } => {
                    let bg = self
                        .resources
                        .bind_group(*bind_group)
                        .ok_or(EncodeError::MissingBindGroup(*bind_group))?;

                    let slot_idx = *slot as usize;
                    if slot_idx >= last_bind_groups.len() {
                        last_bind_groups.resize_with(slot_idx + 1, || None);
                    }
                    let offsets = dynamic_offsets.as_slice();
                    let is_same = last_bind_groups[slot_idx]
                        .as_ref()
                        .is_some_and(|(id, last_offsets)| id == bind_group && *last_offsets == offsets);
                    if !is_same {
                        metrics.bind_group_changes = metrics.bind_group_changes.saturating_add(1);
                        last_bind_groups[slot_idx] = Some((*bind_group, offsets));
                    }

                    pass.set_bind_group(*slot, bg, dynamic_offsets);
                }
                GpuCmd::SetVertexBuffer {
                    slot,
                    buffer,
                    offset,
                    size,
                } => {
                    let buf = self
                        .resources
                        .buffer(*buffer)
                        .ok_or(EncodeError::MissingBuffer(*buffer))?;
                    let slice = match size {
                        Some(size) => {
                            let end = range_end_u64(*offset, *size, "vertex buffer range")?;
                            buf.slice(*offset..end)
                        }
                        None => buf.slice(*offset..),
                    };
                    pass.set_vertex_buffer(*slot, slice);
                }
                GpuCmd::SetIndexBuffer {
                    buffer,
                    format,
                    offset,
                    size,
                } => {
                    let buf = self
                        .resources
                        .buffer(*buffer)
                        .ok_or(EncodeError::MissingBuffer(*buffer))?;
                    let slice = match size {
                        Some(size) => {
                            let end = range_end_u64(*offset, *size, "index buffer range")?;
                            buf.slice(*offset..end)
                        }
                        None => buf.slice(*offset..),
                    };
                    pass.set_index_buffer(slice, (*format).into());
                }
                GpuCmd::Draw {
                    vertex_count,
                    instance_count,
                    first_vertex,
                    first_instance,
                } => {
                    let v_end = range_end_u32(*first_vertex, *vertex_count, "draw vertex range")?;
                    let i_end =
                        range_end_u32(*first_instance, *instance_count, "draw instance range")?;
                    pass.draw(*first_vertex..v_end, *first_instance..i_end)
                }
                GpuCmd::DrawIndexed {
                    index_count,
                    instance_count,
                    first_index,
                    base_vertex,
                    first_instance,
                } => {
                    let idx_end = range_end_u32(*first_index, *index_count, "draw indexed range")?;
                    let inst_end =
                        range_end_u32(*first_instance, *instance_count, "draw instance range")?;
                    pass.draw_indexed(
                        *first_index..idx_end,
                        *base_vertex,
                        *first_instance..inst_end,
                    )
                }
                GpuCmd::BeginRenderPass(_) => return Err(EncodeError::UnterminatedRenderPass),
            }

            if matches!(&cmds[i], GpuCmd::Draw { .. } | GpuCmd::DrawIndexed { .. }) {
                metrics.draw_calls = metrics.draw_calls.saturating_add(1);
            }

            i += 1;
        }

        Err(EncodeError::UnterminatedRenderPass)
    }

    fn encode_color_attachment(
        &self,
        desc: &RenderPassColorAttachmentDesc,
    ) -> Result<wgpu::RenderPassColorAttachment<'_>, EncodeError> {
        let view = self
            .resources
            .texture_view(desc.view)
            .ok_or(EncodeError::MissingTextureView(desc.view))?;
        let resolve_target = match desc.resolve_target {
            Some(id) => Some(
                self.resources
                    .texture_view(id)
                    .ok_or(EncodeError::MissingTextureView(id))?,
            ),
            None => None,
        };

        Ok(wgpu::RenderPassColorAttachment {
            view,
            resolve_target,
            ops: desc.ops.into(),
        })
    }

    fn encode_depth_stencil_attachment(
        &self,
        desc: &RenderPassDepthStencilAttachmentDesc,
    ) -> Result<wgpu::RenderPassDepthStencilAttachment<'_>, EncodeError> {
        let view = self
            .resources
            .texture_view(desc.view)
            .ok_or(EncodeError::MissingTextureView(desc.view))?;
        Ok(wgpu::RenderPassDepthStencilAttachment {
            view,
            depth_ops: desc.depth_ops.map(|ops| ops.into()),
            stencil_ops: desc.stencil_ops.map(|ops| ops.into()),
        })
    }
}

impl From<IndexFormat> for wgpu::IndexFormat {
    fn from(value: IndexFormat) -> Self {
        match value {
            IndexFormat::Uint16 => wgpu::IndexFormat::Uint16,
            IndexFormat::Uint32 => wgpu::IndexFormat::Uint32,
        }
    }
}

impl From<Color> for wgpu::Color {
    fn from(value: Color) -> Self {
        Self {
            r: value.r,
            g: value.g,
            b: value.b,
            a: value.a,
        }
    }
}

impl From<Operations<Color>> for wgpu::Operations<wgpu::Color> {
    fn from(value: Operations<Color>) -> Self {
        Self {
            load: match value.load {
                LoadOp::Load => wgpu::LoadOp::Load,
                LoadOp::Clear(color) => wgpu::LoadOp::Clear(color.into()),
            },
            store: value.store.into(),
        }
    }
}

impl<T> From<Operations<T>> for wgpu::Operations<T>
where
    T: Copy,
{
    fn from(value: Operations<T>) -> Self {
        Self {
            load: value.load.into(),
            store: value.store.into(),
        }
    }
}

impl<T> From<LoadOp<T>> for wgpu::LoadOp<T>
where
    T: Copy,
{
    fn from(value: LoadOp<T>) -> Self {
        match value {
            LoadOp::Load => wgpu::LoadOp::Load,
            LoadOp::Clear(v) => wgpu::LoadOp::Clear(v),
        }
    }
}

impl From<StoreOp> for wgpu::StoreOp {
    fn from(value: StoreOp) -> Self {
        match value {
            StoreOp::Store => wgpu::StoreOp::Store,
            StoreOp::Discard => wgpu::StoreOp::Discard,
        }
    }
}

// Make sure the WGSL shader source can be used in integration tests without
// pulling `std::borrow::Cow` into those modules.
#[allow(dead_code)]
pub(crate) fn wgsl(src: &'static str) -> wgpu::ShaderSource<'static> {
    wgpu::ShaderSource::Wgsl(Cow::Borrowed(src))
}
