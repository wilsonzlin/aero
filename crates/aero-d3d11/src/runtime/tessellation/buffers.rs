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
use crate::binding_model::EXPANDED_VERTEX_MAX_VARYINGS;
use crate::runtime::indirect_args::DrawIndexedIndirectArgs;

const REGISTER_STRIDE_BYTES: u64 = 16;

// Per-patch metadata layout is defined by `runtime::tessellation::TessellationLayoutPatchMeta`
// (written by the GPU layout pass). Keep sizing helpers in sync with that layout and the WGSL
// `PatchMeta` struct in `tessellation/layout_pass.rs`.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TessellationSizingError {
    InvalidParam(&'static str),
    CountTooLarge {
        what: &'static str,
        count: u64,
        max: u64,
    },
    Overflow(&'static str),
}

impl core::fmt::Display for TessellationSizingError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            TessellationSizingError::InvalidParam(msg) => write!(f, "invalid param: {msg}"),
            TessellationSizingError::CountTooLarge { what, count, max } => {
                write!(f, "{what} too large (count={count} max_supported={max})")
            }
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
            super::MAX_TESS_FACTOR_SUPPORTED,
            ds_output_register_count,
        )
    }
}

/// Computes the number of patches for a patchlist draw given the number of control points and
/// elements (vertices or indices).
///
/// D3D11 patchlist topologies define patches as fixed-size groups of `control_points` elements.
/// Both indexed and non-indexed draws follow the same rule; callers should pass:
/// - `element_count = vertex_count` for non-indexed draws
/// - `element_count = index_count` for indexed draws
pub fn patch_count_total_from_element_count(
    element_count: u32,
    control_points: u32,
) -> Result<u32, TessellationSizingError> {
    if control_points == 0 {
        return Err(TessellationSizingError::InvalidParam(
            "control_points must be > 0",
        ));
    }
    if control_points > 32 {
        return Err(TessellationSizingError::InvalidParam(
            "control_points must be <= 32",
        ));
    }
    // D3D11-style primitive count computation: floor division (excess elements are ignored).
    Ok(element_count / control_points)
}

fn checked_mul_u64(a: u64, b: u64, what: &'static str) -> Result<u64, TessellationSizingError> {
    a.checked_mul(b)
        .ok_or(TessellationSizingError::Overflow(what))
}

fn require_u32_count(count: u64, what: &'static str) -> Result<(), TessellationSizingError> {
    if count > u32::MAX as u64 {
        return Err(TessellationSizingError::CountTooLarge {
            what,
            count,
            max: u32::MAX as u64,
        });
    }
    Ok(())
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
    let max_tess_factor = max_tess_factor.min(super::MAX_TESS_FACTOR_SUPPORTED);
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
    let max_tess_factor = max_tess_factor.min(super::MAX_TESS_FACTOR_SUPPORTED);
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
    /// HS-derived tess factors buffer (`hs_tess_factors`) consumed by the layout pass.
    ///
    /// Layout: `HS_TESS_FACTOR_VEC4S_PER_PATCH` `vec4<f32>` values per patch.
    pub hs_tess_factors_bytes: u64,
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
        // D3D11 patchlist topologies encode 1..=32 control points.
        if params.control_points > 32 {
            return Err(TessellationSizingError::InvalidParam(
                "control_points must be <= 32",
            ));
        }

        let ds_output_stride_bytes = payload_stride_bytes(params.ds_output_register_count)?;
        // Expanded draws are consumed by `runtime::wgsl_link::generate_passthrough_vs_wgsl`, which
        // expects the fixed `ExpandedVertex` record layout:
        //   { pos: vec4<f32>, varyings: array<vec4<f32>, EXPANDED_VERTEX_MAX_VARYINGS> }.
        //
        // Keep the expanded vertex stride in sync with that layout (pos + 32 varyings).
        let expanded_vertex_stride_bytes = payload_stride_bytes(1 + EXPANDED_VERTEX_MAX_VARYINGS)?;

        let patch_count_total = params.patch_count_total as u64;
        let control_points = params.control_points as u64;
        let control_point_count_total = checked_mul_u64(
            patch_count_total,
            control_points,
            "patch_count_total * control_points",
        )?;
        require_u32_count(control_point_count_total, "control_point_count_total")?;

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

        // Hull shader patch-constant output used by the layout pass:
        // `vec4<f32>` containing `{edge0, edge1, edge2, inside}` tess factors for triangle-domain
        // integer partitioning.
        let hs_patch_constants_bytes =
            checked_mul_u64(patch_count_total, 16, "hs_patch_constants bytes")?;

        let hs_tess_factors_bytes = checked_mul_u64(
            patch_count_total,
            super::HS_TESS_FACTOR_VEC4S_PER_PATCH as u64,
            "hs_tess_factors vec4 count",
        )
        .and_then(|vec4_count| checked_mul_u64(vec4_count, 16, "hs_tess_factors bytes"))?;

        let (metadata_size, _metadata_align) = super::TessellationLayoutPatchMeta::layout();
        let tess_metadata_bytes =
            checked_mul_u64(patch_count_total, metadata_size, "metadata bytes")?;

        let max_vertices_per_patch = worst_case_vertices_per_patch(params.max_tess_factor)?;
        let expanded_vertex_count_total = checked_mul_u64(
            patch_count_total,
            max_vertices_per_patch,
            "expanded vertex count total",
        )?;
        require_u32_count(expanded_vertex_count_total, "expanded_vertex_count_total")?;
        let expanded_vertex_bytes = checked_mul_u64(
            expanded_vertex_count_total,
            expanded_vertex_stride_bytes,
            "expanded vertex bytes",
        )?;

        let max_indices_per_patch = worst_case_indices_per_patch(params.max_tess_factor)?;
        let expanded_index_count_total = checked_mul_u64(
            patch_count_total,
            max_indices_per_patch,
            "expanded index count total",
        )?;
        require_u32_count(expanded_index_count_total, "expanded_index_count_total")?;
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
            hs_tess_factors_bytes,
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
    pub hs_tess_factors: ExpansionScratchAlloc,
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

        assert_eq!(
            worst_case_vertices_per_patch(crate::runtime::tessellation::MAX_TESS_FACTOR_SUPPORTED)
                .unwrap(),
            289
        );
        assert_eq!(
            worst_case_indices_per_patch(crate::runtime::tessellation::MAX_TESS_FACTOR_SUPPORTED)
                .unwrap(),
            1536
        );

        // The runtime clamps tessellation factors to MAX_TESS_FACTOR_SUPPORTED.
        assert_eq!(
            worst_case_vertices_per_patch(65).unwrap(),
            worst_case_vertices_per_patch(crate::runtime::tessellation::MAX_TESS_FACTOR_SUPPORTED)
                .unwrap()
        );
        assert_eq!(
            worst_case_indices_per_patch(65).unwrap(),
            worst_case_indices_per_patch(crate::runtime::tessellation::MAX_TESS_FACTOR_SUPPORTED)
                .unwrap()
        );
    }

    #[test]
    fn computes_expected_sizes_for_small_draw() {
        let params = TessellationSizingParams::new(2, 3, 4, 2);
        let sizes = TessellationDrawScratchSizes::new(params).unwrap();

        assert_eq!(sizes.ds_output_stride_bytes, 32);
        assert_eq!(sizes.control_point_count_total, 6);
        assert_eq!(sizes.vs_out_bytes, 192);
        assert_eq!(sizes.hs_out_bytes, 192);
        assert_eq!(sizes.hs_patch_constants_bytes, 32);
        assert_eq!(
            sizes.hs_tess_factors_bytes, 32,
            "one vec4<f32> tess factor payload per patch"
        );
        assert_eq!(
            sizes.tess_metadata_bytes, 40,
            "Patch meta is 5 u32s (tess_level + 4 offsets/counts)"
        );
        assert_eq!(sizes.expanded_vertex_count_total, 50);
        // Expanded vertices are written in the same layout consumed by the emulation passthrough VS:
        // `pos + 32 varyings` => 33 * 16 bytes per vertex.
        assert_eq!(sizes.expanded_vertex_bytes, 50 * 33 * 16);
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
    fn rejects_control_points_above_d3d11_limit() {
        assert!(matches!(
            TessellationDrawScratchSizes::new(TessellationSizingParams::new(1, 33, 1, 1)),
            Err(TessellationSizingError::InvalidParam(_))
        ));
    }

    #[test]
    fn patch_count_from_element_count_matches_expected() {
        assert_eq!(
            patch_count_total_from_element_count(12, 3).unwrap(),
            4,
            "12 vertices with 3 control points => 4 patches"
        );
        // D3D-style behavior uses floor division (extra elements are ignored).
        assert_eq!(patch_count_total_from_element_count(11, 3).unwrap(), 3);
        assert_eq!(patch_count_total_from_element_count(0, 3).unwrap(), 0);
        assert!(patch_count_total_from_element_count(3, 0).is_err());
        assert!(patch_count_total_from_element_count(3, 33).is_err());
    }

    #[test]
    fn detects_overflow() {
        // Keep counts within u32 so we validate u64 byte-size overflow paths.
        //
        // With tess factor clamping, the expanded vertex/index math stays well within u64.
        // Use an absurd per-control-point output payload (ds_output_register_count) together with a
        // maximal patch count to overflow the VS/HS stage-IO byte-size computation:
        //   control_point_count_total * ds_output_stride_bytes > u64::MAX
        // where `ds_output_stride_bytes = payload_stride_bytes(u32::MAX) = u32::MAX * 16`.
        let params = TessellationSizingParams::new(u32::MAX, 1, 1, u32::MAX);
        assert!(matches!(
            TessellationDrawScratchSizes::new(params),
            Err(TessellationSizingError::Overflow(_))
        ));
    }

    #[test]
    fn rejects_counts_that_exceed_u32() {
        // Chosen so the expanded index count exceeds u32::MAX without overflowing u64.
        let params = TessellationSizingParams::new(3_000_000, 1, 64, 1);
        let err = TessellationDrawScratchSizes::new(params).unwrap_err();
        assert!(matches!(
            err,
            TessellationSizingError::CountTooLarge {
                what: "expanded_index_count_total",
                ..
            }
        ));
    }

    #[test]
    fn rejects_control_point_count_that_exceeds_u32() {
        let params = TessellationSizingParams::new(200_000_000, 32, 1, 1);
        let err = TessellationDrawScratchSizes::new(params).unwrap_err();
        assert!(matches!(
            err,
            TessellationSizingError::CountTooLarge {
                what: "control_point_count_total",
                ..
            }
        ));
    }
}
