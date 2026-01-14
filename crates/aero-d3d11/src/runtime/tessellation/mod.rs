//! Tessellation (HS/DS) expansion runtime.
//!
//! WebGPU does not expose hull/domain shader stages. Aero emulates tessellation by running
//! compute-based prepasses that:
//! - execute the relevant stages,
//! - expand patch lists into a flat vertex + index buffer,
//! - and write an indirect draw argument buffer for the final render pass.
//!
//! This module contains allocation plumbing + sizing helpers (CPU-side), along with WGSL templates
//! for the compute passes used by tessellation emulation.
//!
//! Note: low-level tessellator math helpers (currently triangle-domain integer partitioning) live
//! in [`crate::runtime::tessellator`]. This module owns per-draw scratch allocations and (future)
//! compute pipeline state for HS/DS emulation.

pub mod buffers;
pub mod layout_pass;
pub mod pipeline;
pub mod tessellator;
pub mod vs_as_compute;

use super::expansion_scratch::{ExpansionScratchAllocator, ExpansionScratchError};

/// Maximum tessellation factor supported by D3D11.
///
/// The runtime uses this value when computing conservative scratch buffer sizes and when deriving
/// per-patch tess levels in the GPU layout pass.
pub const MAX_TESS_FACTOR: u32 = super::tessellator::MAX_TESS_FACTOR;

/// Uniform payload for the GPU tessellation *layout pass*.
///
/// Layout matches the WGSL `LayoutParams` struct in [`layout_pass`], and is padded to 16 bytes so
/// it can be bound as a WebGPU uniform buffer.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TessellationLayoutParams {
    /// Number of patches to process.
    pub patch_count: u32,
    /// Capacity of the downstream expanded-vertex buffer, in vertices.
    pub max_vertices: u32,
    /// Capacity of the downstream expanded-index buffer, in indices.
    pub max_indices: u32,
    pub _pad0: u32,
}

impl TessellationLayoutParams {
    pub const fn layout() -> (u64, u64) {
        (
            core::mem::size_of::<Self>() as u64,
            core::mem::align_of::<Self>() as u64,
        )
    }

    /// Serializes this struct into little-endian bytes suitable for `Queue::write_buffer`.
    pub fn to_le_bytes(self) -> [u8; 16] {
        let mut out = [0u8; 16];
        out[0..4].copy_from_slice(&self.patch_count.to_le_bytes());
        out[4..8].copy_from_slice(&self.max_vertices.to_le_bytes());
        out[8..12].copy_from_slice(&self.max_indices.to_le_bytes());
        out[12..16].copy_from_slice(&self._pad0.to_le_bytes());
        out
    }
}

/// Per-patch metadata produced by the GPU tessellation *layout pass*.
///
/// This is the layout written by [`layout_pass::wgsl_tessellation_layout_pass`]. Offsets are in
/// elements (vertices/indices), not bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TessellationLayoutPatchMeta {
    pub tess_level: u32,
    pub vertex_base: u32,
    pub index_base: u32,
    pub vertex_count: u32,
    pub index_count: u32,
}

impl TessellationLayoutPatchMeta {
    pub const fn layout() -> (u64, u64) {
        (
            core::mem::size_of::<Self>() as u64,
            core::mem::align_of::<Self>() as u64,
        )
    }
}

// Compile-time layout validation (matches WGSL).
const _: [(); 16] = [(); core::mem::size_of::<TessellationLayoutParams>()];
const _: [(); 4] = [(); core::mem::align_of::<TessellationLayoutParams>()];
const _: [(); 20] = [(); core::mem::size_of::<TessellationLayoutPatchMeta>()];
const _: [(); 4] = [(); core::mem::align_of::<TessellationLayoutPatchMeta>()];

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
                    eprintln!(
                        "skipping tessellation scratch allocation test: wgpu unavailable ({e:#})"
                    );
                    return;
                }
            };
            if !exec.caps().supports_compute || !exec.caps().supports_indirect_execution {
                eprintln!(
                    "skipping tessellation scratch allocation test: backend lacks compute/indirect execution"
                );
                return;
            }

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
            assert_eq!(
                draw.expanded_vertices.size,
                draw.sizes.expanded_vertex_bytes
            );
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
