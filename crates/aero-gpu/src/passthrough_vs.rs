//! Deterministic generation of a "passthrough" vertex shader for expanded-geometry draws.
//!
//! Expanded draws bypass the app-provided vertex shader: instead, upstream expansion writes the
//! post-VS vertex data (at minimum `@builtin(position)` plus any `@location(N)` varyings required
//! by the pixel shader) into an intermediate buffer.
//!
//! This module generates a vertex shader that reads that expanded vertex data from a single
//! vertex-buffer binding (preferred over storage-buffer vertex pulling for backend compatibility)
//! and forwards it to the fragment stage unchanged.

use crate::pipeline_key::{VertexAttributeKey, VertexBufferLayoutKey};

/// Stable "output signature" for the generated passthrough vertex shader.
///
/// The signature is the set of user varyings (`@location(N)`) that must be written to match the
/// fragment shader's input interface. `@builtin(position)` is always emitted.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PassthroughVertexShaderKey {
    /// Sorted, de-duplicated list of fragment-stage varying locations.
    locations: Vec<u32>,
}

impl PassthroughVertexShaderKey {
    /// Build a key from an arbitrary list of locations.
    ///
    /// The resulting key is canonicalized (sorted + de-duplicated) to ensure deterministic WGSL
    /// generation and stable caching.
    pub fn new(mut locations: Vec<u32>) -> Self {
        locations.sort_unstable();
        locations.dedup();
        Self { locations }
    }

    pub fn from_locations<I>(iter: I) -> Self
    where
        I: IntoIterator<Item = u32>,
    {
        Self::new(iter.into_iter().collect())
    }

    pub fn locations(&self) -> &[u32] {
        &self.locations
    }

    /// Generate WGSL for a passthrough vertex shader matching this signature.
    ///
    /// Vertex input layout:
    /// - `@location(0)` = clip-space position `vec4<f32>`
    /// - `@location(i)` = `vec4<f32>` for the `i-1`th varying in [`Self::locations`]
    ///
    /// Vertex output layout:
    /// - `@builtin(position)` = forwarded from input
    /// - `@location(N)` = forwarded for each `N` in [`Self::locations`]
    pub fn wgsl(&self) -> String {
        // Keep formatting stable: use explicit `\n`, stable member order, and avoid iterating any
        // unordered collections.
        let mut out = String::new();

        out.push_str("// Auto-generated passthrough VS (aero-gpu)\n\n");

        // ---------------------------------------------------------------------
        // Inputs (expanded vertex buffer)
        // ---------------------------------------------------------------------
        out.push_str("struct VsIn {\n");
        out.push_str("    @location(0) a0: vec4<f32>,\n");
        for (i, _loc) in self.locations.iter().enumerate() {
            let input_loc = 1u32 + i as u32;
            out.push_str(&format!(
                "    @location({input_loc}) a{input_loc}: vec4<f32>,\n"
            ));
        }
        out.push_str("};\n\n");

        // ---------------------------------------------------------------------
        // Outputs (must match PS input interface)
        // ---------------------------------------------------------------------
        out.push_str("struct VsOut {\n");
        out.push_str("    @builtin(position) pos: vec4<f32>,\n");
        for loc in &self.locations {
            out.push_str(&format!("    @location({loc}) o{loc}: vec4<f32>,\n"));
        }
        out.push_str("};\n\n");

        out.push_str("@vertex\n");
        out.push_str("fn vs_main(input: VsIn) -> VsOut {\n");
        out.push_str("    var out: VsOut;\n");
        out.push_str("    out.pos = input.a0;\n");
        for (i, loc) in self.locations.iter().enumerate() {
            let input_loc = 1u32 + i as u32;
            out.push_str(&format!("    out.o{loc} = input.a{input_loc};\n"));
        }
        out.push_str("    return out;\n");
        out.push_str("}\n");

        out
    }

    /// Vertex buffer layout for the expanded vertex data consumed by the generated vertex shader.
    pub fn expanded_vertex_layout(&self) -> ExpandedVertexLayout {
        let mut attributes = Vec::with_capacity(1 + self.locations.len());

        // Position: vec4<f32>.
        attributes.push(wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x4,
            offset: 0,
            shader_location: 0,
        });

        // One vec4<f32> per varying, packed tightly.
        for i in 0..self.locations.len() {
            let input_loc = 1u32 + i as u32;
            let offset = (16u64) * (input_loc as u64);
            attributes.push(wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x4,
                offset,
                shader_location: input_loc,
            });
        }

        let array_stride = 16u64 * (1 + self.locations.len() as u64);
        ExpandedVertexLayout {
            array_stride,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes,
        }
    }
}

/// Owned vertex-buffer layout matching the generated passthrough vertex shader input interface.
#[derive(Debug, Clone, PartialEq)]
pub struct ExpandedVertexLayout {
    pub array_stride: wgpu::BufferAddress,
    pub step_mode: wgpu::VertexStepMode,
    pub attributes: Vec<wgpu::VertexAttribute>,
}

impl ExpandedVertexLayout {
    pub fn as_wgpu(&self) -> wgpu::VertexBufferLayout<'_> {
        wgpu::VertexBufferLayout {
            array_stride: self.array_stride,
            step_mode: self.step_mode,
            attributes: &self.attributes,
        }
    }

    pub fn key(&self) -> VertexBufferLayoutKey {
        VertexBufferLayoutKey {
            array_stride: self.array_stride,
            step_mode: self.step_mode,
            attributes: self
                .attributes
                .iter()
                .copied()
                .map(VertexAttributeKey::from)
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_is_canonicalized() {
        let key = PassthroughVertexShaderKey::new(vec![3, 1, 3, 2]);
        assert_eq!(key.locations(), &[1, 2, 3]);
    }

    #[test]
    fn wgsl_is_deterministic_and_mentions_locations() {
        let key = PassthroughVertexShaderKey::from_locations([10u32, 1u32]);
        let wgsl = key.wgsl();
        // Sorted order.
        let o1 = wgsl.find("@location(1)").unwrap();
        let o10 = wgsl.find("@location(10)").unwrap();
        assert!(o1 < o10);
        assert!(wgsl.contains("out.o1 = input.a1;"));
        assert!(wgsl.contains("out.o10 = input.a2;"));
    }

    #[test]
    fn expanded_vertex_layout_matches_expected_stride_and_locations() {
        let key = PassthroughVertexShaderKey::from_locations([5u32, 2u32]);
        let layout = key.expanded_vertex_layout();
        assert_eq!(layout.array_stride, 16 * 3);
        assert_eq!(layout.attributes.len(), 3);
        assert_eq!(layout.attributes[0].shader_location, 0);
        assert_eq!(layout.attributes[1].shader_location, 1);
        assert_eq!(layout.attributes[2].shader_location, 2);
    }
}
