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

use super::MAX_TESS_FACTOR;

/// Returns a WGSL compute shader implementing the tessellation layout pass.
///
/// Bindings:
/// - `params_binding`: uniform [`super::TessellationLayoutParams`]
/// - `hs_tess_factors_binding`: storage `array<vec4<f32>>` (per patch: edge0, edge1, edge2, inside)
/// - `out_patch_meta_binding`: storage `array<PatchMeta>`
/// - `out_indirect_args_binding`: storage `DrawIndexedIndirectArgs`
/// - `out_debug_binding`: storage `DebugOut` (debug flag: non-zero if any patch was clamped/skipped)
pub fn wgsl_tessellation_layout_pass(
    group: u32,
    params_binding: u32,
    hs_tess_factors_binding: u32,
    out_patch_meta_binding: u32,
    out_indirect_args_binding: u32,
    out_debug_binding: u32,
) -> String {
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

struct DebugOut {{
    flag: u32,
}};

@group({group}) @binding({params_binding})
var<uniform> params: LayoutParams;

// Output of HS patch-constant phase:
// per patch: (edge0, edge1, edge2, inside).
@group({group}) @binding({hs_tess_factors_binding})
var<storage, read> hs_tess_factors: array<vec4<f32>>;

@group({group}) @binding({out_patch_meta_binding})
var<storage, read_write> out_patch_meta: array<PatchMeta>;

@group({group}) @binding({out_indirect_args_binding})
var<storage, read_write> out_indirect: DrawIndexedIndirectArgs;

@group({group}) @binding({out_debug_binding})
var<storage, read_write> out_debug: DebugOut;

const MAX_TESS: u32 = {MAX_TESS_FACTOR}u;

fn safe_round_tess_factor(x: f32) -> u32 {{
    // Defensive against NaN/Inf; treat as 0.
    if (isNan(x) || isInf(x)) {{
        return 0u;
    }}

    // Clamp before rounding to avoid undefined float→int conversions.
    let clamped = clamp(x, 0.0, f32(MAX_TESS));
    let r = round(clamped);
    // `round` may produce -0.0; clamp again just in case.
    return u32(clamp(r, 0.0, f32(MAX_TESS)));
}}

fn derive_tess_level(factors: vec4<f32>) -> u32 {{
    let e0 = safe_round_tess_factor(factors.x);
    let e1 = safe_round_tess_factor(factors.y);
    let e2 = safe_round_tess_factor(factors.z);
    let inside = safe_round_tess_factor(factors.w);
    let m = max(max(e0, e1), max(e2, inside));
    return max(1u, min(m, MAX_TESS));
}}

fn vertex_count_for_tess_level(level: u32) -> u32 {{
    // Triangle domain vertex count:
    // vertices = (n+1)(n+2)/2
    let a = level + 1u;
    let b = level + 2u;
    return (a * b) / 2u;
}}

fn index_count_for_tess_level(level: u32) -> u32 {{
    // Triangle domain index count (triangle list):
    // triangles = n^2, indices = 3*n^2
    return 3u * level * level;
}}

@compute @workgroup_size(1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {{
    if (gid.x != 0u) {{
        return;
    }}

    out_debug.flag = 0u;

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
            let v = vertex_count_for_tess_level(level);
            let i = index_count_for_tess_level(level);
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
}}
"#
    )
}
