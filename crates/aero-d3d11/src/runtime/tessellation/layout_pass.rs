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
