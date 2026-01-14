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
    level.min(MAX_TESS_FACTOR).max(1)
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
    let mut t = local_triangle.min(tri_count.saturating_sub(1));

    // Each triangle row `i` contributes:
    //   N = level - i
    //   row_triangles = up(N) + down(N-1) = 2N - 1
    let mut i: u32 = 0;
    while i < level {
        let n = level - i;
        let row_tris = 2 * n - 1;
        if t < row_tris {
            break;
        }
        t -= row_tris;
        i += 1;
    }

    let j = t / 2;
    let is_down = (t & 1) == 1;

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

fn tri_index_to_vertex_indices(level: u32, local_triangle: u32) -> vec3<u32> {{
  let l = tri_clamp_level(level);
  let tri_count = l * l;
  let t0 = min(local_triangle, tri_count - 1u);

  var t = t0;
  var i: u32 = 0u;
  loop {{
    if (i >= l) {{
      break;
    }}
    let n = l - i;
    let row_tris = 2u * n - 1u;
    if (t < row_tris) {{
      break;
    }}
    t = t - row_tris;
    i = i + 1u;
  }}

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
"#
    )
}

/// WGSL tessellator library using [`MAX_TESS_FACTOR`].
pub fn wgsl_tri_tessellator_lib_default() -> String {
    wgsl_tri_tessellator_lib(MAX_TESS_FACTOR)
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
}}
"#
        );

        let module = naga::front::wgsl::parse_str(&wgsl).expect("generated WGSL failed to parse");
        let mut validator = Validator::new(ValidationFlags::all(), Capabilities::all());
        validator
            .validate(&module)
            .expect("generated WGSL failed to validate");
    }
}
