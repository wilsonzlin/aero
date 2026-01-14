//! Helpers for converting strip primitives into list primitives.
//!
//! Aero's GS emulation pipeline (D3D10/11 → WebGPU) expands `line_strip` / `triangle_strip`
//! outputs into `line_list` / `triangle_list` so we don't rely on primitive-restart indices.
//!
//! A key semantic requirement is that `CutVertex` / `TriangleStream.RestartStrip()` terminates the
//! current strip and prevents bridging primitives from connecting across the cut boundary.
//!
//! This module also contains a deterministic reference implementation for converting a
//! triangle-strip index buffer (interleaved with [`CUT`] markers) into a triangle-list index
//! buffer. This is used to validate CPU and WGSL implementations of strip expansion.

/// Marker value for "primitive restart" / "strip cut" in index buffers.
///
/// D3D11 supports a configurable strip-cut value (`0xFFFF` for 16-bit indices, `0xFFFF_FFFF` for
/// 32-bit). The helper in this crate operates on `u32` indices and uses a single canonical cut
/// value to keep the reference implementation deterministic.
pub const CUT: u32 = 0xFFFF_FFFF;

/// Supported strip output topologies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StripTopology {
    LineStrip,
    TriangleStrip,
}

/// An event in a geometry-shader output stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamEvent {
    /// Emission of a vertex identified by its index in the output vertex buffer.
    Vertex(u32),
    /// Strip cut (`cut` / `RestartStrip`).
    Cut,
}

/// Convert a stream of strip vertices + cuts into list indices.
///
/// The returned indices refer to the `Vertex(u32)` values in `events` (typically sequential).
///
/// - `LineStrip` → `line_list` indices (pairs).
/// - `TriangleStrip` → `triangle_list` indices (triplets), with correct per-triangle winding for
///   D3D-style triangle strips.
pub fn strip_to_list_indices(topology: StripTopology, events: &[StreamEvent]) -> Vec<u32> {
    match topology {
        StripTopology::LineStrip => line_strip_to_list_indices(events),
        StripTopology::TriangleStrip => triangle_strip_to_list_indices(events),
    }
}

fn line_strip_to_list_indices(events: &[StreamEvent]) -> Vec<u32> {
    let mut out = Vec::new();
    let mut prev: Option<u32> = None;
    for ev in events {
        match *ev {
            StreamEvent::Vertex(v) => {
                if let Some(p) = prev {
                    out.push(p);
                    out.push(v);
                }
                prev = Some(v);
            }
            StreamEvent::Cut => {
                prev = None;
            }
        }
    }
    out
}

fn triangle_strip_to_list_indices(events: &[StreamEvent]) -> Vec<u32> {
    let mut out = Vec::new();
    let mut v0: Option<u32> = None;
    let mut v1: Option<u32> = None;
    // `false` => even parity (first triangle uses (v0, v1, v2)).
    // `true`  => odd parity  (next triangle uses (v1, v0, v2)).
    let mut odd = false;

    for ev in events {
        match *ev {
            StreamEvent::Vertex(v) => match (v0, v1) {
                (None, _) => v0 = Some(v),
                (Some(_), None) => v1 = Some(v),
                (Some(a), Some(b)) => {
                    if odd {
                        out.extend_from_slice(&[b, a, v]);
                    } else {
                        out.extend_from_slice(&[a, b, v]);
                    }
                    odd = !odd;
                    v0 = Some(b);
                    v1 = Some(v);
                }
            },
            StreamEvent::Cut => {
                v0 = None;
                v1 = None;
                odd = false;
            }
        }
    }
    out
}

/// Converts a triangle-strip index buffer (interleaved with [`CUT`] markers) into a triangle-list
/// index buffer.
///
/// Semantics:
/// - Maintains a rolling window of the last 2 vertices per strip.
/// - For each new vertex after the first 2, emits one triangle.
/// - The winding alternates per emitted triangle (strip parity).
/// - When [`CUT`] is encountered, the strip window and parity are reset.
///
/// This function intentionally does **not** drop degenerate triangles (repeated indices). The GPU
/// primitive assembler still forms them and they can affect downstream stages (e.g. geometry
/// shaders) even if they get culled later.
pub fn strip_to_triangle_list(indices_with_cuts: &[u32]) -> Vec<u32> {
    // Upper bound: each input index after the first two in its strip contributes one triangle.
    // Cuts reduce the output size, but `with_capacity` is just a hint.
    let mut out = Vec::with_capacity(indices_with_cuts.len().saturating_sub(2) * 3);

    let mut v0: Option<u32> = None;
    let mut v1: Option<u32> = None;
    let mut odd = false;

    for &idx in indices_with_cuts {
        if idx == CUT {
            v0 = None;
            v1 = None;
            odd = false;
            continue;
        }

        match (v0, v1) {
            (None, _) => v0 = Some(idx),
            (Some(_), None) => v1 = Some(idx),
            (Some(a), Some(b)) => {
                if odd {
                    out.extend_from_slice(&[b, a, idx]);
                } else {
                    out.extend_from_slice(&[a, b, idx]);
                }
                odd = !odd;
                v0 = Some(b);
                v1 = Some(idx);
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triangle_strip_cut_splits_list() {
        // Two 4-vertex strips with a cut in between.
        let events = [
            StreamEvent::Vertex(0),
            StreamEvent::Vertex(1),
            StreamEvent::Vertex(2),
            StreamEvent::Vertex(3),
            StreamEvent::Cut,
            StreamEvent::Vertex(4),
            StreamEvent::Vertex(5),
            StreamEvent::Vertex(6),
            StreamEvent::Vertex(7),
        ];

        let indices = strip_to_list_indices(StripTopology::TriangleStrip, &events);
        // 2 triangles per quad (4 vertices in a strip → 2 triangles) = 4 total triangles.
        assert_eq!(
            indices,
            vec![
                0, 1, 2, // tri0
                2, 1, 3, // tri1
                4, 5, 6, // tri2
                6, 5, 7, // tri3
            ]
        );
    }

    #[test]
    fn triangle_strip_without_cut_produces_bridge_triangles() {
        // Same vertices as above, but with the cut omitted. This would incorrectly connect the
        // two quads by generating extra triangles across the boundary.
        let events = [
            StreamEvent::Vertex(0),
            StreamEvent::Vertex(1),
            StreamEvent::Vertex(2),
            StreamEvent::Vertex(3),
            StreamEvent::Vertex(4),
            StreamEvent::Vertex(5),
            StreamEvent::Vertex(6),
            StreamEvent::Vertex(7),
        ];

        let indices = strip_to_list_indices(StripTopology::TriangleStrip, &events);
        // 8 vertices in one strip → 6 triangles.
        assert_eq!(indices.len(), 6 * 3);
        // The two bridge triangles are:
        // - (2, 3, 4)
        // - (4, 3, 5)
        assert_eq!(&indices[6..12], &[2, 3, 4, 4, 3, 5]);
    }

    #[test]
    fn line_strip_cut_splits_segments() {
        let events = [
            StreamEvent::Vertex(10),
            StreamEvent::Vertex(11),
            StreamEvent::Vertex(12),
            StreamEvent::Cut,
            StreamEvent::Vertex(20),
            StreamEvent::Vertex(21),
        ];

        let indices = strip_to_list_indices(StripTopology::LineStrip, &events);
        assert_eq!(indices, vec![10, 11, 11, 12, 20, 21]);
    }

    #[test]
    fn strip_to_triangle_list_single_strip() {
        let out = strip_to_triangle_list(&[0, 1, 2, 3]);
        assert_eq!(out, vec![0, 1, 2, 2, 1, 3]);
    }

    #[test]
    fn strip_to_triangle_list_two_strips() {
        let out = strip_to_triangle_list(&[0, 1, 2, 3, CUT, 4, 5, 6, 7]);
        assert_eq!(out, vec![0, 1, 2, 2, 1, 3, 4, 5, 6, 6, 5, 7]);
    }

    #[test]
    fn strip_to_triangle_list_degenerate_cases() {
        // Empty inputs / too-short strips.
        assert!(strip_to_triangle_list(&[]).is_empty());
        assert!(strip_to_triangle_list(&[0]).is_empty());
        assert!(strip_to_triangle_list(&[0, 1]).is_empty());
        assert!(strip_to_triangle_list(&[CUT]).is_empty());
        assert!(strip_to_triangle_list(&[0, CUT]).is_empty());
        assert!(strip_to_triangle_list(&[0, 1, CUT]).is_empty());

        // Leading + consecutive cuts must restart the strip.
        let out = strip_to_triangle_list(&[CUT, CUT, 0, 1, 2, CUT, CUT, 3, 4, 5]);
        assert_eq!(out, vec![0, 1, 2, 3, 4, 5]);

        // Cut before a strip has produced any triangles.
        let out = strip_to_triangle_list(&[0, 1, CUT, 2, 3, 4]);
        assert_eq!(out, vec![2, 3, 4]);
    }
}
