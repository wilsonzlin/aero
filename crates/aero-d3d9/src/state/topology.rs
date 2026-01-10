use std::fmt;

/// D3D9 primitive types we need to support for draw calls.
///
/// This is intentionally a "semantic" enum (not the raw D3D9 constants) so the
/// rest of the renderer can remain platform-independent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum D3DPrimitiveType {
    TriangleList,
    TriangleStrip,
    TriangleFan,
    LineList,
    LineStrip,
    PointList,
}

impl fmt::Display for D3DPrimitiveType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            D3DPrimitiveType::TriangleList => "triangle_list",
            D3DPrimitiveType::TriangleStrip => "triangle_strip",
            D3DPrimitiveType::TriangleFan => "triangle_fan",
            D3DPrimitiveType::LineList => "line_list",
            D3DPrimitiveType::LineStrip => "line_strip",
            D3DPrimitiveType::PointList => "point_list",
        };
        f.write_str(s)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrimitiveTopologyTranslation {
    pub topology: wgpu::PrimitiveTopology,
    /// WebGPU has no `TriangleFan`; callers must expand indices on the CPU.
    pub needs_triangle_fan_emulation: bool,
}

pub fn translate_primitive_topology(primitive: D3DPrimitiveType) -> PrimitiveTopologyTranslation {
    match primitive {
        D3DPrimitiveType::TriangleList => PrimitiveTopologyTranslation {
            topology: wgpu::PrimitiveTopology::TriangleList,
            needs_triangle_fan_emulation: false,
        },
        D3DPrimitiveType::TriangleStrip => PrimitiveTopologyTranslation {
            topology: wgpu::PrimitiveTopology::TriangleStrip,
            needs_triangle_fan_emulation: false,
        },
        D3DPrimitiveType::TriangleFan => PrimitiveTopologyTranslation {
            // WebGPU doesn't support triangle-fan directly.
            topology: wgpu::PrimitiveTopology::TriangleList,
            needs_triangle_fan_emulation: true,
        },
        D3DPrimitiveType::LineList => PrimitiveTopologyTranslation {
            topology: wgpu::PrimitiveTopology::LineList,
            needs_triangle_fan_emulation: false,
        },
        D3DPrimitiveType::LineStrip => PrimitiveTopologyTranslation {
            topology: wgpu::PrimitiveTopology::LineStrip,
            needs_triangle_fan_emulation: false,
        },
        D3DPrimitiveType::PointList => PrimitiveTopologyTranslation {
            topology: wgpu::PrimitiveTopology::PointList,
            needs_triangle_fan_emulation: false,
        },
    }
}

/// Expand a triangle-fan into a triangle-list, using the D3D9 index ordering:
/// triangle(i) = (0, i, i+1).
pub fn expand_triangle_fan_u16(indices: &[u16]) -> Vec<u16> {
    if indices.len() < 3 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity((indices.len() - 2) * 3);
    for i in 1..(indices.len() - 1) {
        out.push(indices[0]);
        out.push(indices[i]);
        out.push(indices[i + 1]);
    }
    out
}

/// Expand a triangle-fan into a triangle-list, using the D3D9 index ordering:
/// triangle(i) = (0, i, i+1).
pub fn expand_triangle_fan_u32(indices: &[u32]) -> Vec<u32> {
    if indices.len() < 3 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity((indices.len() - 2) * 3);
    for i in 1..(indices.len() - 1) {
        out.push(indices[0]);
        out.push(indices[i]);
        out.push(indices[i + 1]);
    }
    out
}

/// Build an index buffer for a non-indexed triangle-fan draw call.
pub fn expand_triangle_fan_nonindexed_u32(vertex_count: u32) -> Vec<u32> {
    if vertex_count < 3 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(((vertex_count - 2) * 3) as usize);
    for i in 1..(vertex_count - 1) {
        out.push(0);
        out.push(i);
        out.push(i + 1);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triangle_fan_u16_expands_correctly() {
        let indices = [10u16, 11, 12, 13];
        let expanded = expand_triangle_fan_u16(&indices);
        assert_eq!(expanded, vec![10, 11, 12, 10, 12, 13]);
    }

    #[test]
    fn triangle_fan_nonindexed_expands_correctly() {
        let expanded = expand_triangle_fan_nonindexed_u32(5);
        assert_eq!(expanded, vec![0, 1, 2, 0, 2, 3, 0, 3, 4]);
    }
}
