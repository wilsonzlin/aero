//! Helpers for converting geometry-shader strip output into list primitives.
//!
//! Aero's GS emulation pipeline (D3D10/11 → WebGPU) expands `line_strip` / `triangle_strip`
//! outputs into `line_list` / `triangle_list` so we don't rely on primitive-restart indices.
//!
//! A key semantic requirement is that `CutVertex` / `TriangleStream.RestartStrip()` terminates the
//! current strip and prevents bridging primitives from connecting across the cut boundary.

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
    let mut strip: Vec<u32> = Vec::new();
    for ev in events {
        match *ev {
            StreamEvent::Vertex(v) => {
                strip.push(v);
                let vi = strip.len().wrapping_sub(1);
                if strip.len() >= 3 {
                    // D3D-style triangle strip winding alternates each vertex.
                    let a = strip[vi - 2];
                    let b = strip[vi - 1];
                    let c = strip[vi];
                    if (vi % 2) == 0 {
                        out.extend_from_slice(&[a, b, c]);
                    } else {
                        out.extend_from_slice(&[b, a, c]);
                    }
                }
            }
            StreamEvent::Cut => {
                strip.clear();
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
}

