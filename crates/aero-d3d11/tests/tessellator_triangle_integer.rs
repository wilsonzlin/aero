use std::collections::HashSet;

use aero_d3d11::runtime::tessellator::{
    tri_integer_index_count, tri_integer_indices_cw, tri_integer_vertex_count,
    tri_integer_vertex_ijk, TriIntegerBarycentric,
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
