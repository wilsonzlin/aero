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
