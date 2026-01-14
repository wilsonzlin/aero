//! WGSL template for the tessellation *layout pass*.
//!
//! The layout pass runs after the HS patch-constant phase has produced tess factors (`hs_tess_factors`).
//! It performs a deterministic serial scan over patches to compute:
//! - a single derived tess level per patch,
//! - per-patch vertex/index counts (triangle-domain),
//! - prefix-sum base offsets,
//! - and the final `DrawIndexedIndirectArgs` for the expanded draw.
//!
//! This pass intentionally runs with `@workgroup_size(1)` and only the `global_invocation_id.x==0`
//! lane active so ordering is deterministic without atomics.

use super::{tessellator::wgsl_tri_tessellator_lib, MAX_TESS_FACTOR_SUPPORTED};

/// Whether the layout pass includes debug counters in its `DebugOut` buffer.
///
/// This is enabled in unit tests so they can assert on the computed total vertex/index counts.
pub(crate) const INCLUDE_DEBUG_COUNTERS: bool =
    cfg!(any(test, feature = "tessellation_debug_counters"));

/// Size in bytes of the `DebugOut` storage buffer written by [`wgsl_tessellation_layout_pass`].
///
/// This must match the WGSL `DebugOut` struct layout:
/// - 4 bytes when counters are disabled (`flag: u32`)
/// - 16 bytes when counters are enabled (`flag + 2x counters + padding`)
pub(crate) const DEBUG_OUT_SIZE_BYTES: u64 = if INCLUDE_DEBUG_COUNTERS { 16 } else { 4 };

/// Returns a WGSL compute shader implementing the tessellation layout pass.
///
/// Bindings:
/// - `params_binding`: uniform [`super::TessellationLayoutParams`]
/// - `hs_tess_factors_binding`: storage `array<vec4<f32>>` (per patch: edge0, edge1, edge2, inside)
/// - `out_patch_meta_binding`: storage `array<PatchMeta>`
/// - `out_indirect_args_binding`: storage `DrawIndexedIndirectArgs`
/// - `out_debug_binding`: storage `DebugOut` (debug flag; and optional counters when enabled)
pub fn wgsl_tessellation_layout_pass(
    group: u32,
    params_binding: u32,
    hs_tess_factors_binding: u32,
    out_patch_meta_binding: u32,
    out_indirect_args_binding: u32,
    out_debug_binding: u32,
) -> String {
    let tri_lib = wgsl_tri_tessellator_lib(MAX_TESS_FACTOR_SUPPORTED);

    // When enabled, the layout pass writes counters that can be read back in tests.
    let include_counters = INCLUDE_DEBUG_COUNTERS;
    let (debug_struct, debug_init, debug_write) = if include_counters {
        (
            r#"
struct DebugOut {
    flag: u32,
    total_vertices_written: u32,
    total_indices_written: u32,
    _pad0: u32,
};
"#,
            r#"
    out_debug.flag = 0u;
    out_debug.total_vertices_written = 0u;
    out_debug.total_indices_written = 0u;
"#,
            r#"
    out_debug.total_vertices_written = total_vertices;
    out_debug.total_indices_written = total_indices;
"#,
        )
    } else {
        (
            r#"
struct DebugOut {
    flag: u32,
};
"#,
            r#"
    out_debug.flag = 0u;
"#,
            "",
        )
    };
    format!(
        r#"
// ---- Layouts (must match Rust `#[repr(C)]` structs) ----
struct LayoutParams {{
    patch_count: u32,
    max_vertices: u32,
    max_indices: u32,
    _pad0: u32,
}};

struct PatchMeta {{
    tess_level: u32,
    vertex_base: u32,
    index_base: u32,
    vertex_count: u32,
    index_count: u32,
}};

// WebGPU `DrawIndexedIndirectArgs` layout.
struct DrawIndexedIndirectArgs {{
    index_count: u32,
    instance_count: u32,
    first_index: u32,
    base_vertex: i32,
    first_instance: u32,
}};

{debug_struct}

{tri_lib}

@group({group}) @binding({params_binding})
var<uniform> params: LayoutParams;

// Output of HS patch-constant phase:
// per patch: (edge0, edge1, edge2, inside).
//
// Note: this buffer is logically read-only here, but it is allocated from shared scratch storage.
// wgpu treats `storage, read_write` usage as exclusive within a compute dispatch; binding different
// slices of the same underlying buffer as both read-only and read-write triggers validation errors.
// Bind as read_write to keep usage consistent within the dispatch.
@group({group}) @binding({hs_tess_factors_binding})
// NOTE: Bound as `read_write` even though the layout pass only reads it. wgpu tracks usage at the
// whole-buffer granularity, and the tessellation prepass allocates all scratch buffers from a
// single backing buffer. Treating scratch inputs as `read_write` avoids mixing read-only and
// read-write storage views of the same buffer in one dispatch.
var<storage, read_write> hs_tess_factors: array<vec4<f32>>;

@group({group}) @binding({out_patch_meta_binding})
var<storage, read_write> out_patch_meta: array<PatchMeta>;

@group({group}) @binding({out_indirect_args_binding})
var<storage, read_write> out_indirect: DrawIndexedIndirectArgs;

@group({group}) @binding({out_debug_binding})
var<storage, read_write> out_debug: DebugOut;

fn safe_round_tess_factor(x: f32) -> u32 {{
    // Defensive against NaN/Inf; treat as 0.
    //
    // wgpu 0.20's WGSL parser doesn't recognize `isNan`/`isInf`, so we
    // implement the check manually using IEEE-754 exponent bits.
    let bits: u32 = bitcast<u32>(x);
    let exp: u32 = (bits >> 23u) & 0xFFu;
    if (exp == 0xFFu) {{
        return 0u;
    }}

    // Clamp before rounding to avoid undefined float→int conversions.
    let clamped = clamp(x, 0.0, f32(MAX_TESS_FACTOR));
    let r = round(clamped);
    // `round` may produce -0.0; clamp again just in case.
    return u32(clamp(r, 0.0, f32(MAX_TESS_FACTOR)));
}}

fn derive_tess_level(factors: vec4<f32>) -> u32 {{
    let e0 = safe_round_tess_factor(factors.x);
    let e1 = safe_round_tess_factor(factors.y);
    let e2 = safe_round_tess_factor(factors.z);
    let inside = safe_round_tess_factor(factors.w);
    let m = max(max(e0, e1), max(e2, inside));
    return tri_clamp_level(m);
}}

@compute @workgroup_size(1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {{
    if (gid.x != 0u) {{
        return;
    }}

{debug_init}

    var total_vertices: u32 = 0u;
    var total_indices: u32 = 0u;

    var patch_id: u32 = 0u;
    loop {{
        if (patch_id >= params.patch_count) {{
            break;
        }}

        let factors = hs_tess_factors[patch_id];
        let initial_level = derive_tess_level(factors);

        // Remaining capacity.
        let rem_vertices = select(params.max_vertices - total_vertices, 0u, total_vertices >= params.max_vertices);
        let rem_indices = select(params.max_indices - total_indices, 0u, total_indices >= params.max_indices);

        var level = initial_level;
        var v_count: u32 = 0u;
        var i_count: u32 = 0u;

        // Clamp `level` down until this patch fits in the remaining space, or drop it (level=0).
        loop {{
            if (level == 0u) {{
                break;
            }}
            let v = tri_vertex_count(level);
            let i = tri_index_count(level);
            if (v <= rem_vertices && i <= rem_indices) {{
                v_count = v;
                i_count = i;
                break;
            }}
            level = level - 1u;
        }}

        if (level != initial_level) {{
            out_debug.flag = 1u;
        }}

        out_patch_meta[patch_id].tess_level = level;
        out_patch_meta[patch_id].vertex_base = total_vertices;
        out_patch_meta[patch_id].index_base = total_indices;
        out_patch_meta[patch_id].vertex_count = v_count;
        out_patch_meta[patch_id].index_count = i_count;

        total_vertices = total_vertices + v_count;
        total_indices = total_indices + i_count;

        patch_id = patch_id + 1u;
    }}

    // Final draw args (single instance; higher-level code can override instance_count if needed).
    out_indirect.index_count = total_indices;
    out_indirect.instance_count = 1u;
    out_indirect.first_index = 0u;
    out_indirect.base_vertex = 0;
    out_indirect.first_instance = 0u;

{debug_write}
}}
"#
    )
}

#[cfg(test)]
mod tests {
    use crate::runtime::tessellation::{
        tessellator, TessellationLayoutParams, TessellationLayoutPatchMeta,
        MAX_TESS_FACTOR_SUPPORTED,
    };

    fn safe_round_tess_factor_cpu(x: f32) -> u32 {
        if !x.is_finite() {
            return 0;
        }
        let max = MAX_TESS_FACTOR_SUPPORTED as f32;
        let clamped = x.clamp(0.0, max);
        let r = clamped.round();
        r.clamp(0.0, max) as u32
    }

    fn derive_tess_level_cpu(factors: [f32; 4]) -> u32 {
        let e0 = safe_round_tess_factor_cpu(factors[0]);
        let e1 = safe_round_tess_factor_cpu(factors[1]);
        let e2 = safe_round_tess_factor_cpu(factors[2]);
        let inside = safe_round_tess_factor_cpu(factors[3]);
        let m = e0.max(e1).max(e2).max(inside);
        // Mirror the WGSL `tri_clamp_level` from the generated tessellator lib used by the layout
        // pass (`MAX_TESS_FACTOR` is set to `MAX_TESS_FACTOR_SUPPORTED` there).
        m.clamp(1, MAX_TESS_FACTOR_SUPPORTED)
    }

    fn cpu_layout_pass_triangle_domain(
        hs_tess_factors: &[[f32; 4]],
        params: TessellationLayoutParams,
    ) -> (
        Vec<TessellationLayoutPatchMeta>,
        crate::runtime::indirect_args::DrawIndexedIndirectArgs,
        bool,
    ) {
        let mut out = Vec::with_capacity(params.patch_count as usize);
        let mut total_vertices: u32 = 0;
        let mut total_indices: u32 = 0;
        let mut clamped_any = false;

        for patch_id in 0..params.patch_count as usize {
            let factors = hs_tess_factors.get(patch_id).copied().unwrap_or([0.0; 4]);
            let initial_level = derive_tess_level_cpu(factors);

            let rem_vertices = params.max_vertices.saturating_sub(total_vertices);
            let rem_indices = params.max_indices.saturating_sub(total_indices);

            let mut level = initial_level;
            let mut v_count: u32 = 0;
            let mut i_count: u32 = 0;

            while level != 0 {
                let v = tessellator::tri_vertex_count(level.min(MAX_TESS_FACTOR_SUPPORTED));
                let i = tessellator::tri_index_count(level.min(MAX_TESS_FACTOR_SUPPORTED));
                if v <= rem_vertices && i <= rem_indices {
                    v_count = v;
                    i_count = i;
                    break;
                }
                level = level.saturating_sub(1);
            }

            if level != initial_level {
                clamped_any = true;
            }

            out.push(TessellationLayoutPatchMeta {
                tess_level: level,
                vertex_base: total_vertices,
                index_base: total_indices,
                vertex_count: v_count,
                index_count: i_count,
            });

            total_vertices = total_vertices.saturating_add(v_count);
            total_indices = total_indices.saturating_add(i_count);
        }

        (
            out,
            crate::runtime::indirect_args::DrawIndexedIndirectArgs {
                index_count: total_indices,
                instance_count: 1,
                first_index: 0,
                base_vertex: 0,
                first_instance: 0,
            },
            clamped_any,
        )
    }

    #[test]
    fn cpu_layout_matches_expected_prefix_sums() {
        let factors = vec![
            [1.0, 1.0, 1.0, 1.0],   // level 1
            [2.6, 1.0, 1.0, 0.0],   // round(2.6)=3
            [100.0, 1.0, 1.0, 1.0], // clamped to MAX_TESS_FACTOR_SUPPORTED
        ];

        let l0 = derive_tess_level_cpu(factors[0]);
        let l1 = derive_tess_level_cpu(factors[1]);
        let l2 = derive_tess_level_cpu(factors[2]);
        assert_eq!(l0, 1);
        assert_eq!(l1, 3);
        assert_eq!(l2, MAX_TESS_FACTOR_SUPPORTED);

        let v0 = tessellator::tri_vertex_count(l0);
        let i0 = tessellator::tri_index_count(l0);
        let v1 = tessellator::tri_vertex_count(l1);
        let i1 = tessellator::tri_index_count(l1);
        let v2 = tessellator::tri_vertex_count(l2);
        let i2 = tessellator::tri_index_count(l2);

        let params = TessellationLayoutParams {
            patch_count: 3,
            max_vertices: v0 + v1 + v2,
            max_indices: i0 + i1 + i2,
            _pad0: 0,
        };
        let (meta, indirect, clamped_any) = cpu_layout_pass_triangle_domain(&factors, params);
        assert!(!clamped_any, "expected no capacity-based clamping");

        assert_eq!(
            meta,
            vec![
                TessellationLayoutPatchMeta {
                    tess_level: l0,
                    vertex_base: 0,
                    index_base: 0,
                    vertex_count: v0,
                    index_count: i0,
                },
                TessellationLayoutPatchMeta {
                    tess_level: l1,
                    vertex_base: v0,
                    index_base: i0,
                    vertex_count: v1,
                    index_count: i1,
                },
                TessellationLayoutPatchMeta {
                    tess_level: l2,
                    vertex_base: v0 + v1,
                    index_base: i0 + i1,
                    vertex_count: v2,
                    index_count: i2,
                },
            ]
        );

        assert_eq!(indirect.index_count, i0 + i1 + i2);
        assert_eq!(indirect.instance_count, 1);
        assert_eq!(indirect.first_index, 0);
        assert_eq!(indirect.base_vertex, 0);
        assert_eq!(indirect.first_instance, 0);
    }

    #[test]
    fn cpu_layout_clamps_down_when_out_of_space() {
        let factors = vec![
            [1.0, 1.0, 1.0, 1.0],   // level 1
            [2.6, 1.0, 1.0, 0.0],   // level 3
            [100.0, 1.0, 1.0, 1.0], // level 16
        ];
        let l0 = derive_tess_level_cpu(factors[0]);
        let l1 = derive_tess_level_cpu(factors[1]);
        let v0 = tessellator::tri_vertex_count(l0);
        let i0 = tessellator::tri_index_count(l0);
        let v1 = tessellator::tri_vertex_count(l1);
        let i1 = tessellator::tri_index_count(l1);

        // Only enough space for the first two patches.
        let params = TessellationLayoutParams {
            patch_count: 3,
            max_vertices: v0 + v1,
            max_indices: i0 + i1,
            _pad0: 0,
        };
        let (meta, indirect, clamped_any) = cpu_layout_pass_triangle_domain(&factors, params);
        assert!(clamped_any, "expected capacity-based clamping to occur");
        assert_eq!(meta[2].tess_level, 0, "final patch should be dropped");
        assert_eq!(meta[2].vertex_count, 0);
        assert_eq!(meta[2].index_count, 0);
        assert_eq!(indirect.index_count, i0 + i1);
    }
}
