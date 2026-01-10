use super::{BindGroupId, BufferId, GpuCmd, PipelineId};

use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OptimizeMetrics {
    pub commands_in: usize,
    pub commands_out: usize,
    pub redundant_state_sets_removed: usize,
    pub draw_calls_coalesced: usize,
    pub optimize_time: Duration,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandOptimizer {
    /// If enabled, the optimizer will attempt to merge *consecutive* draws into a
    /// single draw when it is provably safe (contiguous vertex/index ranges and
    /// identical instance parameters).
    pub enable_draw_coalescing: bool,
}

impl CommandOptimizer {
    pub fn new() -> Self {
        Self {
            enable_draw_coalescing: true,
        }
    }

    pub fn optimize(&self, cmds: Vec<GpuCmd>) -> OptimizeResult {
        let start = Instant::now();
        let mut metrics = OptimizeMetrics {
            commands_in: cmds.len(),
            ..OptimizeMetrics::default()
        };

        let mut out = Vec::with_capacity(cmds.len());
        let mut in_render_pass = false;
        let mut state = RenderState::default();

        for cmd in cmds {
            match cmd {
                GpuCmd::BeginRenderPass(desc) => {
                    in_render_pass = true;
                    state.reset();
                    out.push(GpuCmd::BeginRenderPass(desc));
                }
                GpuCmd::EndRenderPass => {
                    in_render_pass = false;
                    state.reset();
                    out.push(GpuCmd::EndRenderPass);
                }
                GpuCmd::SetPipeline(id) if in_render_pass => {
                    if state.pipeline == Some(id) {
                        metrics.redundant_state_sets_removed += 1;
                        continue;
                    }
                    state.pipeline = Some(id);
                    out.push(GpuCmd::SetPipeline(id));
                }
                GpuCmd::SetBindGroup {
                    slot,
                    bind_group,
                    dynamic_offsets,
                } if in_render_pass => {
                    let is_redundant = {
                        let existing = state.bind_group(slot);
                        existing.as_ref().is_some_and(|s| {
                            s.id == bind_group && s.dynamic_offsets == dynamic_offsets
                        })
                    };
                    if is_redundant {
                        metrics.redundant_state_sets_removed += 1;
                        continue;
                    }
                    state.set_bind_group(
                        slot,
                        BindGroupState {
                            id: bind_group,
                            dynamic_offsets: dynamic_offsets.clone(),
                        },
                    );
                    out.push(GpuCmd::SetBindGroup {
                        slot,
                        bind_group,
                        dynamic_offsets,
                    });
                }
                GpuCmd::SetVertexBuffer {
                    slot,
                    buffer,
                    offset,
                    size,
                } if in_render_pass => {
                    let next = VertexBufferState {
                        id: buffer,
                        offset,
                        size,
                    };
                    let is_redundant = {
                        let existing = state.vertex_buffer(slot);
                        existing.as_ref().is_some_and(|s| s == &next)
                    };
                    if is_redundant {
                        metrics.redundant_state_sets_removed += 1;
                        continue;
                    }
                    state.set_vertex_buffer(slot, next);
                    out.push(GpuCmd::SetVertexBuffer {
                        slot,
                        buffer,
                        offset,
                        size,
                    });
                }
                GpuCmd::SetIndexBuffer {
                    buffer,
                    format,
                    offset,
                    size,
                } if in_render_pass => {
                    let next = IndexBufferState {
                        id: buffer,
                        format,
                        offset,
                        size,
                    };
                    if state.index_buffer.as_ref().is_some_and(|s| s == &next) {
                        metrics.redundant_state_sets_removed += 1;
                        continue;
                    }
                    state.index_buffer = Some(next);
                    out.push(GpuCmd::SetIndexBuffer {
                        buffer,
                        format,
                        offset,
                        size,
                    });
                }
                cmd @ (GpuCmd::Draw { .. } | GpuCmd::DrawIndexed { .. }) if in_render_pass => {
                    if self.enable_draw_coalescing && try_coalesce_draw(&mut out, &cmd) {
                        metrics.draw_calls_coalesced += 1;
                        continue;
                    }
                    out.push(cmd);
                }
                other => {
                    // For now, treat anything else as opaque and preserve it. We also reset
                    // cached state when leaving a render pass boundary (handled above).
                    //
                    // Note: Commands outside a render pass (or additional pass types) may be
                    // added later; keeping them unoptimized is always correct.
                    out.push(other);
                }
            }
        }

        metrics.commands_out = out.len();
        metrics.optimize_time = start.elapsed();

        OptimizeResult { cmds: out, metrics }
    }
}

impl Default for CommandOptimizer {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug)]
pub struct OptimizeResult {
    pub cmds: Vec<GpuCmd>,
    pub metrics: OptimizeMetrics,
}

#[derive(Clone, Debug, Default)]
struct RenderState {
    pipeline: Option<PipelineId>,
    bind_groups: Vec<Option<BindGroupState>>,
    vertex_buffers: Vec<Option<VertexBufferState>>,
    index_buffer: Option<IndexBufferState>,
}

impl RenderState {
    fn reset(&mut self) {
        *self = Self::default();
    }

    fn bind_group(&mut self, slot: u32) -> &mut Option<BindGroupState> {
        let slot = slot as usize;
        if self.bind_groups.len() <= slot {
            self.bind_groups.resize_with(slot + 1, || None);
        }
        &mut self.bind_groups[slot]
    }

    fn set_bind_group(&mut self, slot: u32, state: BindGroupState) {
        *self.bind_group(slot) = Some(state);
    }

    fn vertex_buffer(&mut self, slot: u32) -> &mut Option<VertexBufferState> {
        let slot = slot as usize;
        if self.vertex_buffers.len() <= slot {
            self.vertex_buffers.resize_with(slot + 1, || None);
        }
        &mut self.vertex_buffers[slot]
    }

    fn set_vertex_buffer(&mut self, slot: u32, state: VertexBufferState) {
        *self.vertex_buffer(slot) = Some(state);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct BindGroupState {
    id: BindGroupId,
    dynamic_offsets: Vec<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct VertexBufferState {
    id: BufferId,
    offset: u64,
    size: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct IndexBufferState {
    id: BufferId,
    format: super::IndexFormat,
    offset: u64,
    size: Option<u64>,
}

fn try_coalesce_draw(out: &mut [GpuCmd], next: &GpuCmd) -> bool {
    let Some(prev) = out.last_mut() else {
        return false;
    };

    match (prev, next) {
        (
            GpuCmd::Draw {
                vertex_count: a_count,
                instance_count: a_instances,
                first_vertex: a_first,
                first_instance: a_first_instance,
            },
            GpuCmd::Draw {
                vertex_count: b_count,
                instance_count: b_instances,
                first_vertex: b_first,
                first_instance: b_first_instance,
            },
        ) => {
            if a_instances != b_instances || a_first_instance != b_first_instance {
                return false;
            }

            let Some(a_end) = a_first.checked_add(*a_count) else {
                return false;
            };
            if a_end != *b_first {
                return false;
            }
            let Some(new_count) = a_count.checked_add(*b_count) else {
                return false;
            };
            *a_count = new_count;
            true
        }
        (
            GpuCmd::DrawIndexed {
                index_count: a_count,
                instance_count: a_instances,
                first_index: a_first,
                base_vertex: a_base_vertex,
                first_instance: a_first_instance,
            },
            GpuCmd::DrawIndexed {
                index_count: b_count,
                instance_count: b_instances,
                first_index: b_first,
                base_vertex: b_base_vertex,
                first_instance: b_first_instance,
            },
        ) => {
            if a_instances != b_instances
                || a_first_instance != b_first_instance
                || a_base_vertex != b_base_vertex
            {
                return false;
            }

            let Some(a_end) = a_first.checked_add(*a_count) else {
                return false;
            };
            if a_end != *b_first {
                return false;
            }
            let Some(new_count) = a_count.checked_add(*b_count) else {
                return false;
            };
            *a_count = new_count;
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::{BindGroupId, GpuCmd, PipelineId, RenderPassDesc};

    fn pass() -> RenderPassDesc {
        RenderPassDesc {
            label: None,
            color_attachments: Vec::new(),
            depth_stencil_attachment: None,
        }
    }

    #[test]
    fn removes_redundant_pipeline_sets_within_pass() {
        let input = vec![
            GpuCmd::BeginRenderPass(pass()),
            GpuCmd::SetPipeline(PipelineId(1)),
            GpuCmd::SetPipeline(PipelineId(1)),
            GpuCmd::Draw {
                vertex_count: 3,
                instance_count: 1,
                first_vertex: 0,
                first_instance: 0,
            },
            GpuCmd::EndRenderPass,
        ];

        let optimizer = CommandOptimizer::default();
        let result = optimizer.optimize(input);

        assert_eq!(
            result.cmds,
            vec![
                GpuCmd::BeginRenderPass(pass()),
                GpuCmd::SetPipeline(PipelineId(1)),
                GpuCmd::Draw {
                    vertex_count: 3,
                    instance_count: 1,
                    first_vertex: 0,
                    first_instance: 0,
                },
                GpuCmd::EndRenderPass,
            ]
        );
        assert_eq!(result.metrics.redundant_state_sets_removed, 1);
        assert_eq!(result.metrics.commands_in, 5);
        assert_eq!(result.metrics.commands_out, 4);
    }

    #[test]
    fn does_not_elide_state_across_render_pass_boundaries() {
        let input = vec![
            GpuCmd::BeginRenderPass(pass()),
            GpuCmd::SetPipeline(PipelineId(1)),
            GpuCmd::EndRenderPass,
            GpuCmd::BeginRenderPass(pass()),
            GpuCmd::SetPipeline(PipelineId(1)),
            GpuCmd::EndRenderPass,
        ];

        let optimizer = CommandOptimizer::default();
        let result = optimizer.optimize(input);

        assert_eq!(result.metrics.redundant_state_sets_removed, 0);
        assert_eq!(result.metrics.commands_in, result.metrics.commands_out);
    }

    #[test]
    fn preserves_dynamic_offset_semantics() {
        let input = vec![
            GpuCmd::BeginRenderPass(pass()),
            GpuCmd::SetBindGroup {
                slot: 0,
                bind_group: BindGroupId(2),
                dynamic_offsets: vec![0],
            },
            GpuCmd::SetBindGroup {
                slot: 0,
                bind_group: BindGroupId(2),
                dynamic_offsets: vec![4],
            },
            GpuCmd::EndRenderPass,
        ];

        let optimizer = CommandOptimizer::default();
        let result = optimizer.optimize(input.clone());

        assert_eq!(result.cmds, input);
        assert_eq!(result.metrics.redundant_state_sets_removed, 0);
    }

    #[test]
    fn coalesces_consecutive_draws_with_contiguous_ranges() {
        let input = vec![
            GpuCmd::BeginRenderPass(pass()),
            GpuCmd::SetPipeline(PipelineId(1)),
            GpuCmd::Draw {
                vertex_count: 3,
                instance_count: 1,
                first_vertex: 0,
                first_instance: 0,
            },
            GpuCmd::Draw {
                vertex_count: 2,
                instance_count: 1,
                first_vertex: 3,
                first_instance: 0,
            },
            GpuCmd::EndRenderPass,
        ];

        let optimizer = CommandOptimizer::default();
        let result = optimizer.optimize(input);

        assert_eq!(
            result.cmds,
            vec![
                GpuCmd::BeginRenderPass(pass()),
                GpuCmd::SetPipeline(PipelineId(1)),
                GpuCmd::Draw {
                    vertex_count: 5,
                    instance_count: 1,
                    first_vertex: 0,
                    first_instance: 0,
                },
                GpuCmd::EndRenderPass,
            ]
        );
        assert_eq!(result.metrics.draw_calls_coalesced, 1);
    }
}
