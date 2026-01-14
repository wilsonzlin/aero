use std::collections::HashSet;

use aero_d3d11::runtime::tessellator::{
    tri_index_to_vertex_indices_cw, tri_integer_index_count, tri_integer_indices_cw,
    tri_integer_triangle_count, tri_integer_vertex_count, tri_integer_vertex_ijk,
    tri_integer_vertex_index, tri_vertex_domain_location, TriIntegerBarycentric,
};

#[test]
fn triangle_integer_counts_match_formulas() {
    for n in 0u32..=32 {
        let vc = tri_integer_vertex_count(n);
        let vc_expected = ((n as u64 + 1) * (n as u64 + 2) / 2) as u32;
        assert_eq!(vc, vc_expected, "vertex_count formula mismatch for n={n}");

        let ic = tri_integer_index_count(n);
        let ic_expected = ((n as u64) * (n as u64) * 3) as u32;
        assert_eq!(ic, ic_expected, "index_count formula mismatch for n={n}");

        let tc = tri_integer_triangle_count(n);
        let tc_expected = ((n as u64) * (n as u64)) as u32;
        assert_eq!(tc, tc_expected, "triangle_count formula mismatch for n={n}");
        assert_eq!(
            ic,
            tc * 3,
            "index_count must equal triangle_count*3 for n={n}"
        );
    }
}

#[test]
fn triangle_integer_vertex_ijk_is_valid_and_unique() {
    for n in 1u32..=16 {
        let vc = tri_integer_vertex_count(n);
        let mut seen = HashSet::<TriIntegerBarycentric>::with_capacity(vc as usize);

        for v in 0..vc {
            let ijk = tri_integer_vertex_ijk(n, v);
            assert_eq!(
                ijk.i + ijk.j + ijk.k,
                n,
                "i+j+k must equal n for n={n}, v={v}: {ijk:?}"
            );
            assert!(ijk.i <= n && ijk.j <= n && ijk.k <= n);
            assert!(
                seen.insert(ijk),
                "duplicate barycentric coords for n={n}: {ijk:?}"
            );

            // Inverse mapping: local index -> ijk -> (i,j) must round-trip.
            assert_eq!(
                tri_integer_vertex_index(n, ijk.i, ijk.j),
                v,
                "vertex_index(i,j) must invert vertex_ijk for n={n}, v={v}"
            );
        }

        assert_eq!(
            seen.len(),
            vc as usize,
            "vertex_ijk must cover all unique vertices for n={n}"
        );
    }
}

#[test]
fn triangle_integer_indices_are_in_bounds_non_degenerate_and_cw() {
    for n in 1u32..=32 {
        let vc = tri_integer_vertex_count(n);
        let indices = tri_integer_indices_cw(n);
        assert_eq!(
            indices.len(),
            tri_integer_index_count(n) as usize,
            "index buffer length mismatch for n={n}"
        );

        // Verify CPU reference implementation matches the per-triangle helper (same as GPU path
        // order, but CW).
        for (tri_id, tri) in indices.chunks_exact(3).enumerate() {
            let expected = tri_index_to_vertex_indices_cw(n, tri_id as u32);
            assert_eq!(
                tri, expected,
                "triangle mismatch for n={n}, tri_id={tri_id}"
            );
        }

        for tri in indices.chunks_exact(3) {
            let a = tri[0];
            let b = tri[1];
            let c = tri[2];

            assert!(a < vc && b < vc && c < vc, "index out of bounds for n={n}");
            assert!(a != b && b != c && a != c, "degenerate triangle for n={n}");

            // Winding check: in the lattice coordinate system (x, y) = (j, k),
            // CW triangles have a negative signed area.
            let TriIntegerBarycentric { j: aj, k: ak, .. } = tri_integer_vertex_ijk(n, a);
            let TriIntegerBarycentric { j: bj, k: bk, .. } = tri_integer_vertex_ijk(n, b);
            let TriIntegerBarycentric { j: cj, k: ck, .. } = tri_integer_vertex_ijk(n, c);

            let ax = aj as i32;
            let ay = ak as i32;
            let bx = bj as i32;
            let by = bk as i32;
            let cx = cj as i32;
            let cy = ck as i32;

            let area2 = (bx - ax) * (cy - ay) - (by - ay) * (cx - ax);
            assert_eq!(
                area2, -1,
                "unexpected triangle area/winding for n={n}, tri={tri:?} (area2={area2})"
            );
        }
    }
}

#[test]
fn triangle_integer_barycentrics_are_normalized_for_common_levels() {
    const EPS: f32 = 1e-5;

    // Keep this focused on a few representative levels so it stays fast and deterministic while
    // still exercising non-power-of-two division (e.g. n=3).
    for n in [1u32, 2, 3, 4, 16] {
        let vc = tri_integer_vertex_count(n);
        for v in 0..vc {
            let ijk = tri_integer_vertex_ijk(n, v);
            assert_eq!(
                ijk.i + ijk.j + ijk.k,
                n,
                "i+j+k must equal n for n={n}, v={v}: {ijk:?}"
            );

            let inv = 1.0 / n as f32;
            let u = ijk.i as f32 * inv;
            let vv = ijk.j as f32 * inv;
            let w = ijk.k as f32 * inv;
            let sum = u + vv + w;
            assert!(
                (sum - 1.0).abs() <= EPS,
                "float barycentrics must sum to 1 (n={n}, v={v}, ijk={ijk:?}, sum={sum})"
            );
        }
    }
}

#[test]
fn triangle_integer_common_levels_match_spec_invariants() {
    // This test mirrors the task-spec invariants directly on a small set of representative
    // tessellation factors so failures are easy to diagnose.
    const EPS: f32 = 1e-5;

    for n in [1u32, 2, 3, 4, 16] {
        let expected_vc = ((n as u64 + 1) * (n as u64 + 2) / 2) as u32;
        let expected_ic = ((n as u64) * (n as u64) * 3) as u32;

        let vc = tri_integer_vertex_count(n);
        assert_eq!(vc, expected_vc, "vertex_count formula mismatch for n={n}");

        let indices = tri_integer_indices_cw(n);
        assert_eq!(
            indices.len(),
            expected_ic as usize,
            "index_count formula mismatch for n={n}"
        );

        // Index range.
        for (idx_i, &idx) in indices.iter().enumerate() {
            assert!(
                idx < vc,
                "index out of range for n={n}: indices[{idx_i}]={idx} vertex_count={vc}"
            );
        }

        // Barycentric normalization.
        for v in 0..vc {
            let ijk = tri_integer_vertex_ijk(n, v);
            assert_eq!(
                ijk.i + ijk.j + ijk.k,
                n,
                "i+j+k must equal n for n={n}, v={v}: {ijk:?}"
            );

            let inv = 1.0 / n as f32;
            let sum = ijk.i as f32 * inv + ijk.j as f32 * inv + ijk.k as f32 * inv;
            assert!(
                (sum - 1.0).abs() <= EPS,
                "float barycentrics must sum to 1 (n={n}, v={v}, ijk={ijk:?}, sum={sum})"
            );
        }
    }
}

#[test]
fn triangle_integer_domain_location_matches_integer_barycentrics() {
    const EPS: f32 = 1e-5;

    for n in [1u32, 2, 3, 4, 16] {
        let vc = tri_integer_vertex_count(n);
        let inv = 1.0 / n as f32;

        for v in 0..vc {
            let ijk = tri_integer_vertex_ijk(n, v);
            let loc = tri_vertex_domain_location(n, v);
            let expected = [
                ijk.i as f32 * inv,
                ijk.j as f32 * inv,
                ijk.k as f32 * inv,
            ];

            for c in 0..3 {
                assert!(
                    (loc[c] - expected[c]).abs() <= EPS,
                    "domain_location mismatch (n={n} v={v} ijk={ijk:?} loc={loc:?} expected={expected:?})"
                );
            }
        }
    }
}
