//! Scratch buffer layouts + worst-case sizing helpers for tessellation expansion.
//!
//! Tessellation emulation expands patch lists into:
//! - intermediate buffers for stage IO (VS→HS, HS outputs, HS patch constants),
//! - per-patch metadata (counts and offsets),
//! - and final expanded vertex/index buffers + indirect draw arguments.
//!
//! This module contains only CPU-side sizing and allocation metadata; it does not implement any
//! GPU shader logic.

use super::super::expansion_scratch::ExpansionScratchAlloc;
use crate::runtime::indirect_args::DrawIndexedIndirectArgs;

const REGISTER_STRIDE_BYTES: u64 = 16;

/// Per-patch metadata consumed by tessellation expansion compute passes.
///
/// The metadata stores counts and offsets for each patch within the expanded vertex + index
/// buffers. Offsets are in elements (vertices/indices), not bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TessellationPatchMetadata {
    pub vertex_offset: u32,
    pub vertex_count: u32,
    pub index_offset: u32,
    pub index_count: u32,
}

impl TessellationPatchMetadata {
    pub const fn layout() -> (u64, u64) {
        (
            core::mem::size_of::<Self>() as u64,
            core::mem::align_of::<Self>() as u64,
        )
    }
}

// Compile-time layout validation.
const _: [(); 16] = [(); core::mem::size_of::<TessellationPatchMetadata>()];
const _: [(); 4] = [(); core::mem::align_of::<TessellationPatchMetadata>()];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TessellationSizingError {
    InvalidParam(&'static str),
    Overflow(&'static str),
}

impl core::fmt::Display for TessellationSizingError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            TessellationSizingError::InvalidParam(msg) => write!(f, "invalid param: {msg}"),
            TessellationSizingError::Overflow(msg) => write!(f, "size overflow: {msg}"),
        }
    }
}

impl std::error::Error for TessellationSizingError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TessellationSizingParams {
    pub patch_count_total: u32,
    pub control_points: u32,
    pub max_tess_factor: u32,
    pub ds_output_register_count: u32,
}

impl TessellationSizingParams {
    pub const fn new(
        patch_count_total: u32,
        control_points: u32,
        max_tess_factor: u32,
        ds_output_register_count: u32,
    ) -> Self {
        Self {
            patch_count_total,
            control_points,
            max_tess_factor,
            ds_output_register_count,
        }
    }

    pub const fn new_with_max_tess_factor(
        patch_count_total: u32,
        control_points: u32,
        ds_output_register_count: u32,
    ) -> Self {
        Self::new(
            patch_count_total,
            control_points,
            super::MAX_TESS_FACTOR,
            ds_output_register_count,
        )
    }
}

fn checked_mul_u64(a: u64, b: u64, what: &'static str) -> Result<u64, TessellationSizingError> {
    a.checked_mul(b)
        .ok_or(TessellationSizingError::Overflow(what))
}

/// Returns the byte stride for a shader output payload with `register_count` 4-float registers.
pub fn payload_stride_bytes(register_count: u32) -> Result<u64, TessellationSizingError> {
    if register_count == 0 {
        return Err(TessellationSizingError::InvalidParam(
            "register_count must be > 0",
        ));
    }
    checked_mul_u64(
        register_count as u64,
        REGISTER_STRIDE_BYTES,
        "payload stride bytes",
    )
}

/// Conservative worst-case tessellator vertex count per patch.
///
/// This assumes a quad domain and triangle output topology:
/// - vertices: `(factor + 1)^2`
pub fn worst_case_vertices_per_patch(max_tess_factor: u32) -> Result<u64, TessellationSizingError> {
    if max_tess_factor == 0 {
        return Err(TessellationSizingError::InvalidParam(
            "max_tess_factor must be > 0",
        ));
    }
    let n = max_tess_factor as u64;
    let np1 = n.checked_add(1).ok_or(TessellationSizingError::Overflow(
        "max_tess_factor + 1 overflows",
    ))?;
    checked_mul_u64(np1, np1, "vertices per patch")
}

/// Conservative worst-case tessellator index count per patch.
///
/// This assumes a quad domain and triangle output topology:
/// - indices: `6 * factor^2` (2 triangles per grid cell).
pub fn worst_case_indices_per_patch(max_tess_factor: u32) -> Result<u64, TessellationSizingError> {
    if max_tess_factor == 0 {
        return Err(TessellationSizingError::InvalidParam(
            "max_tess_factor must be > 0",
        ));
    }
    let n = max_tess_factor as u64;
    let n2 = checked_mul_u64(n, n, "max_tess_factor^2")?;
    checked_mul_u64(6, n2, "indices per patch")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TessellationDrawScratchSizes {
    pub ds_output_stride_bytes: u64,
    pub control_point_count_total: u64,
    pub expanded_vertex_count_total: u64,
    pub expanded_index_count_total: u64,

    pub vs_out_bytes: u64,
    pub hs_out_bytes: u64,
    pub hs_patch_constants_bytes: u64,
    pub tess_metadata_bytes: u64,
    pub expanded_vertex_bytes: u64,
    pub expanded_index_bytes: u64,
    pub indirect_args_bytes: u64,
}

impl TessellationDrawScratchSizes {
    pub fn new(params: TessellationSizingParams) -> Result<Self, TessellationSizingError> {
        if params.patch_count_total == 0 {
            return Err(TessellationSizingError::InvalidParam(
                "patch_count_total must be > 0",
            ));
        }
        if params.control_points == 0 {
            return Err(TessellationSizingError::InvalidParam(
                "control_points must be > 0",
            ));
        }

        let ds_output_stride_bytes = payload_stride_bytes(params.ds_output_register_count)?;

        let patch_count_total = params.patch_count_total as u64;
        let control_points = params.control_points as u64;
        let control_point_count_total =
            checked_mul_u64(patch_count_total, control_points, "patch_count_total * control_points")?;

        // Intermediate stage IO: per-control-point payloads.
        let vs_out_bytes = checked_mul_u64(
            control_point_count_total,
            ds_output_stride_bytes,
            "vs_out bytes",
        )?;
        let hs_out_bytes = checked_mul_u64(
            control_point_count_total,
            ds_output_stride_bytes,
            "hs_out bytes",
        )?;

        // Patch constants are per patch.
        let hs_patch_constants_bytes = checked_mul_u64(
            patch_count_total,
            ds_output_stride_bytes,
            "hs_patch_constants bytes",
        )?;

        let (metadata_size, _metadata_align) = TessellationPatchMetadata::layout();
        let tess_metadata_bytes = checked_mul_u64(patch_count_total, metadata_size, "metadata bytes")?;

        let max_vertices_per_patch = worst_case_vertices_per_patch(params.max_tess_factor)?;
        let expanded_vertex_count_total = checked_mul_u64(
            patch_count_total,
            max_vertices_per_patch,
            "expanded vertex count total",
        )?;
        let expanded_vertex_bytes = checked_mul_u64(
            expanded_vertex_count_total,
            ds_output_stride_bytes,
            "expanded vertex bytes",
        )?;

        let max_indices_per_patch = worst_case_indices_per_patch(params.max_tess_factor)?;
        let expanded_index_count_total = checked_mul_u64(
            patch_count_total,
            max_indices_per_patch,
            "expanded index count total",
        )?;
        let expanded_index_bytes =
            checked_mul_u64(expanded_index_count_total, 4, "expanded index bytes")?;

        let (indirect_args_bytes, _indirect_align) = DrawIndexedIndirectArgs::layout();

        Ok(Self {
            ds_output_stride_bytes,
            control_point_count_total,
            expanded_vertex_count_total,
            expanded_index_count_total,
            vs_out_bytes,
            hs_out_bytes,
            hs_patch_constants_bytes,
            tess_metadata_bytes,
            expanded_vertex_bytes,
            expanded_index_bytes,
            indirect_args_bytes,
        })
    }
}

#[derive(Debug, Clone)]
pub struct TessellationDrawScratch {
    pub vs_out: ExpansionScratchAlloc,
    pub hs_out: ExpansionScratchAlloc,
    pub hs_patch_constants: ExpansionScratchAlloc,
    pub tess_metadata: ExpansionScratchAlloc,
    pub expanded_vertices: ExpansionScratchAlloc,
    pub expanded_indices: ExpansionScratchAlloc,
    pub indirect_args: ExpansionScratchAlloc,
    pub sizes: TessellationDrawScratchSizes,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worst_case_counts_for_max_tess_factor() {
        assert_eq!(worst_case_vertices_per_patch(1).unwrap(), 4);
        assert_eq!(worst_case_indices_per_patch(1).unwrap(), 6);

        assert_eq!(worst_case_vertices_per_patch(64).unwrap(), 4225);
        assert_eq!(worst_case_indices_per_patch(64).unwrap(), 24576);
    }

    #[test]
    fn computes_expected_sizes_for_small_draw() {
        let params = TessellationSizingParams::new(2, 3, 4, 2);
        let sizes = TessellationDrawScratchSizes::new(params).unwrap();

        assert_eq!(sizes.ds_output_stride_bytes, 32);
        assert_eq!(sizes.control_point_count_total, 6);
        assert_eq!(sizes.vs_out_bytes, 192);
        assert_eq!(sizes.hs_out_bytes, 192);
        assert_eq!(sizes.hs_patch_constants_bytes, 64);
        assert_eq!(sizes.tess_metadata_bytes, 32);
        assert_eq!(sizes.expanded_vertex_count_total, 50);
        assert_eq!(sizes.expanded_vertex_bytes, 1600);
        assert_eq!(sizes.expanded_index_count_total, 192);
        assert_eq!(sizes.expanded_index_bytes, 768);
        assert_eq!(sizes.indirect_args_bytes, 20);
    }

    #[test]
    fn rejects_zero_parameters() {
        assert!(matches!(
            TessellationDrawScratchSizes::new(TessellationSizingParams::new(0, 1, 1, 1)),
            Err(TessellationSizingError::InvalidParam(_))
        ));
        assert!(matches!(
            TessellationDrawScratchSizes::new(TessellationSizingParams::new(1, 0, 1, 1)),
            Err(TessellationSizingError::InvalidParam(_))
        ));
        assert!(matches!(
            TessellationDrawScratchSizes::new(TessellationSizingParams::new(1, 1, 0, 1)),
            Err(TessellationSizingError::InvalidParam(_))
        ));
        assert!(matches!(
            TessellationDrawScratchSizes::new(TessellationSizingParams::new(1, 1, 1, 0)),
            Err(TessellationSizingError::InvalidParam(_))
        ));
    }

    #[test]
    fn detects_overflow() {
        let params = TessellationSizingParams::new(u32::MAX, 32, 64, u32::MAX);
        assert!(matches!(
            TessellationDrawScratchSizes::new(params),
            Err(TessellationSizingError::Overflow(_))
        ));
    }
}

