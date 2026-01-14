//! Triangle-domain tessellation helpers (integer partitioning).
//!
//! This module provides:
//! - CPU-side helpers for sizing and debugging (counts + topology math).
//! - A reusable WGSL snippet implementing the same logic for compute-based tessellation expansion.
//!
//! The tessellation level `L` subdivides each patch edge into `L` segments, generating a triangular
//! grid of domain points:
//! - vertex_count = (L + 1)(L + 2) / 2
//! - triangle_count = L * L
//! - index_count = triangle_count * 3
//!
//! Vertex enumeration:
//! - rows `i = 0..=L`
//! - within a row, columns `j = 0..=(L - i)`
//! - barycentric domain location = `(i, j, L - i - j) / L`.

/// Default maximum tessellation factor for integer-partitioned triangle domains.
///
/// D3D11's fixed-function tessellator clamps to 64 for triangle domains.
pub const MAX_TESS_FACTOR: u32 = 64;

fn clamp_level(level: u32) -> u32 {
    level.clamp(1, MAX_TESS_FACTOR)
}

/// Number of vertices emitted by integer-partitioned triangle tessellation at `level`.
///
/// Formula: `(level + 1)(level + 2)/2`.
pub fn tri_vertex_count(level: u32) -> u32 {
    let level = clamp_level(level);
    (level + 1) * (level + 2) / 2
}

/// Number of indices (triangle list) emitted by integer-partitioned triangle tessellation at
/// `level`.
///
/// Formula: `level * level * 3`.
pub fn tri_index_count(level: u32) -> u32 {
    let level = clamp_level(level);
    level * level * 3
}

fn tri_triangle_count(level: u32) -> u32 {
    let level = clamp_level(level);
    level * level
}

fn ceil_sqrt_u32(v: u32) -> u32 {
    if v == 0 {
        return 0;
    }
    // Tessellation factors are clamped to `MAX_TESS_FACTOR` (64), so values are tiny (<= 4096).
    // Use a float sqrt + correction for a compact, robust ceil-sqrt implementation.
    let f = (v as f64).sqrt();
    let mut r = f.floor() as u32;
    if r.saturating_mul(r) < v {
        r += 1;
    }
    r
}

/// Converts `(row=i, col=j)` coordinates into a linear vertex index in the tessellator's canonical
/// enumeration.
///
/// `i` must be `<= level`, and `j` must be `<= level - i`.
fn tri_vertex_index(level: u32, i: u32, j: u32) -> u32 {
    // Row start:
    //   sum_{r=0..i-1} (level - r + 1)
    // = i*(level+1) - i*(i-1)/2
    // = i*(2*level + 3 - i)/2
    let start = i * (2 * level + 3 - i) / 2;
    start + j
}

/// Returns barycentric coordinates `(u, v, w)` for `local_vertex` in
/// `[0, tri_vertex_count(level))`.
pub fn tri_vertex_domain_location(level: u32, local_vertex: u32) -> [f32; 3] {
    let level = clamp_level(level);

    // Clamp for robustness (keeps behavior defined for invalid indices).
    let vtx_count = tri_vertex_count(level);
    let mut idx = local_vertex.min(vtx_count.saturating_sub(1));

    // Decode `idx` into `(i, j)` in the canonical row-major enumeration.
    let mut i: u32 = 0;
    while i <= level {
        let row_len = level - i + 1;
        if idx < row_len {
            break;
        }
        idx -= row_len;
        i += 1;
    }

    let j = idx;
    let k = level - i - j;
    let inv = 1.0 / level as f32;
    [i as f32 * inv, j as f32 * inv, k as f32 * inv]
}

/// Returns the three vertex indices for the `local_triangle`th triangle (triangle-list topology) in
/// `[0, level*level)`.
///
/// The ordering is deterministic but is not guaranteed to match any specific native tessellator
/// implementation; it is intended for internal use where vertices and indices are generated using
/// the same enumeration.
pub fn tri_index_to_vertex_indices(level: u32, local_triangle: u32) -> [u32; 3] {
    let level = clamp_level(level);

    // Clamp for robustness (keeps behavior defined for invalid indices).
    let tri_count = tri_triangle_count(level);
    let tri_id = local_triangle.min(tri_count.saturating_sub(1));

    // Closed-form row decoding:
    // - Total triangles: level^2
    // - Row `i` (0-based) contributes `2*(level-i)-1` triangles.
    //
    // Row starts are square prefix sums:
    //   row_base(i) = level^2 - (level - i)^2
    //
    // Solve for the row by inverting with ceil-sqrt.
    let remaining: u32 = tri_count - tri_id; // 1..=level^2
    let k: u32 = ceil_sqrt_u32(remaining); // 1..=level
    let i: u32 = level - k;
    let row_base: u32 = tri_count - k * k;
    let t: u32 = tri_id - row_base;

    let j: u32 = t / 2;
    let is_down: bool = (t & 1) == 1;

    if !is_down {
        // "Up" triangle: (i,j), (i+1,j), (i,j+1)
        let a = tri_vertex_index(level, i, j);
        let b = tri_vertex_index(level, i + 1, j);
        let c = tri_vertex_index(level, i, j + 1);
        [a, b, c]
    } else {
        // "Down" triangle: (i+1,j), (i+1,j+1), (i,j+1)
        let a = tri_vertex_index(level, i + 1, j);
        let b = tri_vertex_index(level, i + 1, j + 1);
        let c = tri_vertex_index(level, i, j + 1);
        [a, b, c]
    }
}

/// CW variant of [`tri_index_to_vertex_indices`].
///
/// This is equivalent to swapping the last two vertices of the triangle produced by
/// [`tri_index_to_vertex_indices`], and therefore inherits the same clamping behavior for
/// `level`/`local_triangle`.
pub fn tri_index_to_vertex_indices_cw(level: u32, local_triangle: u32) -> [u32; 3] {
    let [a, b, c] = tri_index_to_vertex_indices(level, local_triangle);
    [a, c, b]
}

/// Returns a WGSL snippet implementing triangle-domain tessellation helpers.
///
/// This snippet is pure math (no bindings) and can be concatenated into shader templates.
pub fn wgsl_tri_tessellator_lib(max_tess_factor: u32) -> String {
    let max_tess_factor = max_tess_factor.max(1);
    format!(
        r#"
// ---- Aero triangle-domain tessellation helpers (generated) ----
const MAX_TESS_FACTOR: u32 = {max_tess_factor}u;

fn tri_clamp_level(level: u32) -> u32 {{
  return clamp(level, 1u, MAX_TESS_FACTOR);
}}

fn tri_vertex_count(level: u32) -> u32 {{
  let l = tri_clamp_level(level);
  return (l + 1u) * (l + 2u) / 2u;
}}

fn tri_index_count(level: u32) -> u32 {{
  let l = tri_clamp_level(level);
  return l * l * 3u;
}}

fn tri_vertex_index(level: u32, i: u32, j: u32) -> u32 {{
  // start(i) = i*(2*level + 3 - i)/2
  let start = i * (2u * level + 3u - i) / 2u;
  return start + j;
}}

fn tri_vertex_domain_location(level: u32, local_vertex: u32) -> vec3<f32> {{
  let l = tri_clamp_level(level);
  let vtx_count = tri_vertex_count(l);
  let idx0 = min(local_vertex, vtx_count - 1u);

  var idx = idx0;
  var i: u32 = 0u;
  loop {{
    if (i > l) {{
      break;
    }}
    let row_len = l - i + 1u;
    if (idx < row_len) {{
      break;
    }}
    idx = idx - row_len;
    i = i + 1u;
  }}

  let j = idx;
  let k = l - i - j;
  let inv = 1.0 / f32(l);
  return vec3<f32>(f32(i) * inv, f32(j) * inv, f32(k) * inv);
}}

fn tri_ceil_sqrt_u32(v: u32) -> u32 {{
  if (v == 0u) {{
    return 0u;
  }}
  // v is small (<= MAX_TESS_FACTOR^2) so using f32 sqrt is safe and faster than integer loops.
  let f = sqrt(f32(v));
  var r: u32 = u32(f);
  if (r * r < v) {{
    r = r + 1u;
  }}
  return r;
}}

fn tri_index_to_vertex_indices(level: u32, local_triangle: u32) -> vec3<u32> {{
  let l = tri_clamp_level(level);
  let tri_count = l * l;
  let tri_id = min(local_triangle, tri_count - 1u);

  // Closed-form row decoding:
  // row_base(i) = level^2 - (level - i)^2
  let remaining: u32 = tri_count - tri_id;
  let k: u32 = tri_ceil_sqrt_u32(remaining);
  let i: u32 = l - k;
  let row_base: u32 = tri_count - k * k;
  let t: u32 = tri_id - row_base;

  let j = t / 2u;
  let is_down = (t & 1u) == 1u;
  if (!is_down) {{
    // "Up" triangle.
    let a = tri_vertex_index(l, i, j);
    let b = tri_vertex_index(l, i + 1u, j);
    let c = tri_vertex_index(l, i, j + 1u);
    return vec3<u32>(a, b, c);
  }}

  // "Down" triangle.
  let a = tri_vertex_index(l, i + 1u, j);
  let b = tri_vertex_index(l, i + 1u, j + 1u);
  let c = tri_vertex_index(l, i, j + 1u);
  return vec3<u32>(a, b, c);
}}

// ---- Integer partitioning helpers (aliases / extra outputs) ----
//
// These functions exist to keep names aligned with the Rust-side reference
// implementation and to support compute shaders that want integer barycentric
// coordinates or a fixed CW winding without additional swapping logic.

fn tri_integer_vertex_count(level: u32) -> u32 {{
  return tri_vertex_count(level);
}}

fn tri_integer_index_count(level: u32) -> u32 {{
  return tri_index_count(level);
}}

fn tri_integer_triangle_count(level: u32) -> u32 {{
  return tri_integer_index_count(level) / 3u;
}}

fn tri_integer_row_start(level: u32, i: u32) -> u32 {{
  let l = tri_clamp_level(level);
  return tri_vertex_index(l, i, 0u);
}}

fn tri_integer_vertex_index(level: u32, i: u32, j: u32) -> u32 {{
  let l = tri_clamp_level(level);
  return tri_vertex_index(l, i, j);
}}

fn tri_integer_vertex_index_from_ijk(level: u32, ijk: vec3<u32>) -> u32 {{
  return tri_integer_vertex_index(level, ijk.x, ijk.y);
}}

fn tri_integer_vertex_ijk(level: u32, local_vertex: u32) -> vec3<u32> {{
  let l = tri_clamp_level(level);
  let vtx_count = tri_vertex_count(l);
  let idx0 = min(local_vertex, vtx_count - 1u);

  var idx = idx0;
  var i: u32 = 0u;
  loop {{
    if (i > l) {{
      break;
    }}
    let row_len = l - i + 1u;
    if (idx < row_len) {{
      break;
    }}
    idx = idx - row_len;
    i = i + 1u;
  }}

  let j = idx;
  let k = l - i - j;
  return vec3<u32>(i, j, k);
}}

// CW variant of `tri_index_to_vertex_indices` in the canonical `(j,k)` lattice
// coordinate system.
fn tri_index_to_vertex_indices_cw(level: u32, local_triangle: u32) -> vec3<u32> {{
  let v = tri_index_to_vertex_indices(level, local_triangle);
  return vec3<u32>(v.x, v.z, v.y);
}}
"#
    )
}

/// WGSL tessellator library using [`MAX_TESS_FACTOR`].
pub fn wgsl_tri_tessellator_lib_default() -> String {
    wgsl_tri_tessellator_lib(MAX_TESS_FACTOR)
}

/// Integer barycentric coordinates for a tessellated triangle domain point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TriIntegerBarycentric {
    pub i: u32,
    pub j: u32,
    pub k: u32,
}

/// Vertex count for integer-partitioned triangle domain with tess factor `n`.
///
/// `V(n) = (n + 1)(n + 2)/2`.
pub fn tri_integer_vertex_count(n: u32) -> u32 {
    // Use u64 to avoid intermediate overflow for callers that don't clamp `n`.
    let n = n as u64;
    (((n + 1) * (n + 2)) / 2) as u32
}

/// Index count for integer-partitioned triangle domain with tess factor `n`.
///
/// `I(n) = n² * 3`.
pub fn tri_integer_index_count(n: u32) -> u32 {
    let n = n as u64;
    (n * n * 3) as u32
}

/// Triangle count for integer-partitioned triangle domain with tess factor `n`.
///
/// `T(n) = n²`.
pub fn tri_integer_triangle_count(n: u32) -> u32 {
    let n = n as u64;
    (n * n) as u32
}

/// Starting vertex index of integer barycentric row `i` (`i = 0..=n`).
///
/// Rows follow the canonical vertex enumeration:
/// - row `i` has `n - i + 1` vertices (`j = 0..=(n - i)`),
/// - and row starts are:
///   `start(i) = sum_{r=0..i-1} (n - r + 1) = i*(2n + 3 - i)/2`.
pub fn tri_integer_row_start(n: u32, i: u32) -> u32 {
    assert!(i <= n, "row index i={i} out of range for n={n}");
    let n = n as u64;
    let i = i as u64;
    ((i * (2 * n + 3 - i)) / 2) as u32
}

/// Map `(i, j)` integer barycentric coordinates to local vertex index.
///
/// Requires `i + j <= n` (and therefore `k = n - i - j >= 0`).
pub fn tri_integer_vertex_index(n: u32, i: u32, j: u32) -> u32 {
    assert!(i <= n, "i={i} out of range for n={n}");
    assert!(j <= n - i, "j={j} out of range for n={n}, i={i}");
    tri_integer_row_start(n, i) + j
}

/// Map `local_vertex_index -> (i, j, k)` integer barycentric coordinates where `i + j + k = n`.
///
/// This follows the canonical tessellator vertex enumeration described in the module docs:
/// - rows `i = 0..=n`
/// - columns `j = 0..=(n - i)`
/// - `k = n - i - j`
pub fn tri_integer_vertex_ijk(n: u32, local_vertex_index: u32) -> TriIntegerBarycentric {
    let vertex_count = tri_integer_vertex_count(n);
    assert!(
        local_vertex_index < vertex_count,
        "local_vertex_index {local_vertex_index} out of bounds for n={n} (vertex_count={vertex_count})"
    );

    let mut idx = local_vertex_index;
    for i in 0..=n {
        let row_len = n - i + 1;
        if idx < row_len {
            let j = idx;
            let k = n - i - j;
            return TriIntegerBarycentric { i, j, k };
        }
        idx -= row_len;
    }

    unreachable!("index bounds checked above; scan must have returned")
}

/// Generate clockwise (CW) triangle list indices for triangle-domain integer partitioning.
///
/// Returns a flat index buffer with length [`tri_integer_index_count`]. Each consecutive triplet is
/// a triangle.
pub fn tri_integer_indices_cw(n: u32) -> Vec<u32> {
    if n == 0 {
        return Vec::new();
    }

    let mut out = Vec::with_capacity(tri_integer_index_count(n) as usize);

    // Use the same triangle ordering as `tri_index_to_vertex_indices` (row-major, interleaved up
    // and down triangles), but emit vertices in CW order in the `(j, k)` lattice.
    for i in 0..n {
        let row_n = n - i;
        for j in 0..row_n {
            // Base (CCW) order: (i,j), (i+1,j), (i,j+1).
            // CW order swaps the last two vertices.
            let a = tri_vertex_index(n, i, j);
            let b = tri_vertex_index(n, i + 1, j);
            let c = tri_vertex_index(n, i, j + 1);
            out.extend_from_slice(&[a, c, b]);

            if j + 1 < row_n {
                // Base (CCW) order: (i+1,j), (i+1,j+1), (i,j+1).
                let a = b;
                let b = tri_vertex_index(n, i + 1, j + 1);
                out.extend_from_slice(&[a, c, b]);
            }
        }
    }

    debug_assert_eq!(out.len(), tri_integer_index_count(n) as usize);
    out
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use naga::valid::{Capabilities, ValidationFlags, Validator};

    #[test]
    fn counts_levels_1_through_5() {
        for level in 1..=5u32 {
            let expected_vtx = (level + 1) * (level + 2) / 2;
            let expected_idx = level * level * 3;
            assert_eq!(tri_vertex_count(level), expected_vtx, "level={level}");
            assert_eq!(tri_index_count(level), expected_idx, "level={level}");
        }
    }

    #[test]
    fn domain_locations_are_barycentric_and_in_range() {
        const EPS: f32 = 1e-6;
        for level in 1..=5u32 {
            let vtx_count = tri_vertex_count(level);
            for v in 0..vtx_count {
                let loc = tri_vertex_domain_location(level, v);
                for (c_i, &c) in loc.iter().enumerate() {
                    assert!(
                        (-EPS..=(1.0 + EPS)).contains(&c),
                        "level={level} v={v} c[{c_i}] out of range: {c}"
                    );
                }
                let sum = loc[0] + loc[1] + loc[2];
                assert!(
                    (sum - 1.0).abs() <= EPS,
                    "level={level} v={v} bary sum != 1 (sum={sum})"
                );

                // Each component should land on the integer grid.
                for &c in &loc {
                    let scaled = c * level as f32;
                    let rounded = scaled.round();
                    assert!(
                        (scaled - rounded).abs() <= EPS,
                        "level={level} v={v} component not on grid: {c}"
                    );
                }
            }
        }
    }

    #[test]
    fn indices_are_in_bounds_and_unique_per_level() {
        for level in 1..=5u32 {
            let vtx_count = tri_vertex_count(level);
            let tri_count = level * level;
            assert_eq!(
                tri_index_count(level),
                tri_count * 3,
                "index count must match triangle_count*3"
            );

            let mut seen = HashSet::<(u32, u32, u32)>::new();

            for t in 0..tri_count {
                let [a, b, c] = tri_index_to_vertex_indices(level, t);
                for (idx_i, idx) in [a, b, c].iter().copied().enumerate() {
                    assert!(
                        idx < vtx_count,
                        "level={level} t={t} idx[{idx_i}] out of bounds: {idx} >= {vtx_count}"
                    );
                }
                assert!(
                    a != b && b != c && a != c,
                    "level={level} t={t} degenerate triangle"
                );

                // Track uniqueness ignoring winding.
                let mut tri = [a, b, c];
                tri.sort_unstable();
                assert!(
                    seen.insert((tri[0], tri[1], tri[2])),
                    "level={level} t={t} duplicate triangle indices {tri:?}"
                );
            }

            assert_eq!(
                seen.len() as u32,
                tri_count,
                "level={level} triangle coverage mismatch"
            );
        }
    }

    #[test]
    fn wgsl_lib_parses_and_validates() {
        let lib = wgsl_tri_tessellator_lib_default();
        let wgsl = format!(
            r#"
{lib}

@compute @workgroup_size(1)
fn main() {{
  let _vc = tri_vertex_count(4u);
  let _ic = tri_index_count(4u);
  let _loc = tri_vertex_domain_location(4u, 0u);
  let _tri = tri_index_to_vertex_indices(4u, 0u);
  let _vc_i = tri_integer_vertex_count(4u);
  let _ic_i = tri_integer_index_count(4u);
  let _tc_i = tri_integer_triangle_count(4u);
  let _ijk = tri_integer_vertex_ijk(4u, 0u);
  let _row = tri_integer_row_start(4u, 0u);
  let _vid = tri_integer_vertex_index_from_ijk(4u, _ijk);
  let _tri_cw = tri_index_to_vertex_indices_cw(4u, 0u);
}}
"#
        );

        let module = naga::front::wgsl::parse_str(&wgsl).expect("generated WGSL failed to parse");
        let mut validator = Validator::new(ValidationFlags::all(), Capabilities::all());
        validator
            .validate(&module)
            .expect("generated WGSL failed to validate");
    }

    #[test]
    fn triangle_integer_spec_levels_match_formulas_and_normalize() {
        // Mirror the task-spec invariants on the primary helpers used by the tessellation
        // implementation (`tri_*`), for a small set of representative tess factors.
        const EPS: f32 = 1e-5;
        for level in [1u32, 2, 3, 4, 16] {
            let vc_expected = (level + 1) * (level + 2) / 2;
            let ic_expected = level * level * 3;

            let vc = tri_vertex_count(level);
            let ic = tri_index_count(level);
            assert_eq!(
                vc, vc_expected,
                "vertex_count formula mismatch for level={level}"
            );
            assert_eq!(
                ic, ic_expected,
                "index_count formula mismatch for level={level}"
            );

            // Vertex-domain barycentrics.
            for v in 0..vc {
                let loc = tri_vertex_domain_location(level, v);
                let sum = loc[0] + loc[1] + loc[2];
                assert!(
                    (sum - 1.0).abs() <= EPS,
                    "bary sum != 1 for level={level} v={v} (sum={sum})"
                );
            }

            // Triangle connectivity indices.
            let tri_count = level * level;
            for t in 0..tri_count {
                let [a, b, c] = tri_index_to_vertex_indices(level, t);
                for (idx_i, idx) in [a, b, c].iter().copied().enumerate() {
                    assert!(
                        idx < vc,
                        "index out of bounds for level={level} tri={t} idx[{idx_i}]={idx} vc={vc}"
                    );
                }
            }
        }
    }
}
