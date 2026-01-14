//! Tessellation (HS/DS) expansion runtime.
//!
//! WebGPU does not expose hull/domain shader stages. Aero emulates tessellation by running
//! compute-based prepasses that:
//! - execute the relevant stages,
//! - expand patch lists into a flat vertex + index buffer,
//! - and write an indirect draw argument buffer for the final render pass.
//!
//! This module currently contains allocation plumbing and worst-case size helpers. Shader logic
//! (actual expansion compute pipelines) is intentionally out of scope for now.
//!
//! Note: low-level tessellator math helpers (currently triangle-domain integer partitioning) live
//! in [`crate::runtime::tessellator`]. This module owns per-draw scratch allocations and (future)
//! compute pipeline state for HS/DS emulation.

pub mod buffers;
pub mod pipeline;
pub mod tessellator;
pub mod vs_as_compute;

use super::expansion_scratch::{ExpansionScratchAllocator, ExpansionScratchError};

/// Maximum tessellation factor supported by D3D11.
///
/// The runtime uses this value when computing conservative scratch buffer sizes.
pub const MAX_TESS_FACTOR: u32 = super::tessellator::MAX_TESS_FACTOR;

#[derive(Debug, Default)]
pub struct TessellationRuntime {
    pipelines: pipeline::TessellationPipelines,
}

#[derive(Debug)]
pub enum TessellationRuntimeError {
    Sizing(buffers::TessellationSizingError),
    Scratch(ExpansionScratchError),
}

impl core::fmt::Display for TessellationRuntimeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            TessellationRuntimeError::Sizing(e) => write!(f, "tessellation sizing error: {e}"),
            TessellationRuntimeError::Scratch(e) => write!(f, "tessellation scratch error: {e}"),
        }
    }
}

impl std::error::Error for TessellationRuntimeError {}

impl TessellationRuntime {
    pub fn reset(&mut self) {
        self.pipelines.reset();
    }

    /// Allocate per-draw scratch buffers for tessellation expansion.
    ///
    /// The returned allocations are all subranges of the shared [`ExpansionScratchAllocator`]
    /// backing buffer.
    pub fn alloc_draw_scratch(
        &mut self,
        device: &wgpu::Device,
        scratch: &mut ExpansionScratchAllocator,
        params: buffers::TessellationSizingParams,
    ) -> Result<buffers::TessellationDrawScratch, TessellationRuntimeError> {
        let sizes = buffers::TessellationDrawScratchSizes::new(params)
            .map_err(TessellationRuntimeError::Sizing)?;

        // Intermediate stage outputs are modelled as storage buffers, but allocating them via the
        // "vertex output" path keeps alignment consistent with other stage-emulation scratch.
        let vs_out = scratch
            .alloc_vertex_output(device, sizes.vs_out_bytes)
            .map_err(TessellationRuntimeError::Scratch)?;
        let hs_out = scratch
            .alloc_vertex_output(device, sizes.hs_out_bytes)
            .map_err(TessellationRuntimeError::Scratch)?;
        let hs_patch_constants = scratch
            .alloc_vertex_output(device, sizes.hs_patch_constants_bytes)
            .map_err(TessellationRuntimeError::Scratch)?;

        let tess_metadata = scratch
            .alloc_metadata(device, sizes.tess_metadata_bytes, 16)
            .map_err(TessellationRuntimeError::Scratch)?;

        let expanded_vertices = scratch
            .alloc_vertex_output(device, sizes.expanded_vertex_bytes)
            .map_err(TessellationRuntimeError::Scratch)?;
        let expanded_indices = scratch
            .alloc_index_output(device, sizes.expanded_index_bytes)
            .map_err(TessellationRuntimeError::Scratch)?;

        let indirect_args = scratch
            .alloc_indirect_draw_indexed(device)
            .map_err(TessellationRuntimeError::Scratch)?;

        Ok(buffers::TessellationDrawScratch {
            vs_out,
            hs_out,
            hs_patch_constants,
            tess_metadata,
            expanded_vertices,
            expanded_indices,
            indirect_args,
            sizes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
    use std::sync::Arc;

    #[test]
    fn alloc_draw_scratch_allocates_expected_sizes() {
        pollster::block_on(async {
            let exec = match AerogpuD3d11Executor::new_for_tests().await {
                Ok(exec) => exec,
                Err(e) => {
                    eprintln!("skipping tessellation scratch allocation test: wgpu unavailable ({e:#})");
                    return;
                }
            };

            let mut scratch = ExpansionScratchAllocator::new(Default::default());
            let mut rt = TessellationRuntime::default();
            let params = buffers::TessellationSizingParams::new(2, 3, MAX_TESS_FACTOR, 2);
            let draw = rt
                .alloc_draw_scratch(exec.device(), &mut scratch, params)
                .expect("alloc_draw_scratch should succeed");

            assert_eq!(draw.vs_out.size, draw.sizes.vs_out_bytes);
            assert_eq!(draw.hs_out.size, draw.sizes.hs_out_bytes);
            assert_eq!(
                draw.hs_patch_constants.size,
                draw.sizes.hs_patch_constants_bytes
            );
            assert_eq!(draw.tess_metadata.size, draw.sizes.tess_metadata_bytes);
            assert_eq!(draw.expanded_vertices.size, draw.sizes.expanded_vertex_bytes);
            assert_eq!(draw.expanded_indices.size, draw.sizes.expanded_index_bytes);
            assert_eq!(draw.indirect_args.size, draw.sizes.indirect_args_bytes);

            // All allocations should share the same backing buffer when capacity is sufficient.
            assert!(Arc::ptr_eq(&draw.vs_out.buffer, &draw.hs_out.buffer));
            assert!(Arc::ptr_eq(
                &draw.vs_out.buffer,
                &draw.expanded_vertices.buffer
            ));
        });
    }
}
