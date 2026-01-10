use std::hash::{Hash, Hasher};

/// A stable hash of WGSL shader source.
pub type ShaderHash = u128;

/// Hash WGSL bytes using a fast, high-quality hash (XXH3 128-bit).
#[inline]
pub fn hash_wgsl(wgsl: &str) -> ShaderHash {
    xxhash_rust::xxh3::xxh3_128(wgsl.as_bytes())
}

/// Shader stage used for shader module caching.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ShaderStage {
    Vertex,
    Fragment,
    Compute,
}

/// Cache key for shader modules.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ShaderModuleKey {
    pub hash: ShaderHash,
    pub stage: ShaderStage,
}

/// Hash/signature of a pipeline layout, expressed as a list of bind-group-layout
/// hashes.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PipelineLayoutKey {
    pub bind_group_layout_hashes: Vec<u64>,
}

impl PipelineLayoutKey {
    pub fn empty() -> Self {
        Self {
            bind_group_layout_hashes: Vec::new(),
        }
    }
}

/// Hashable representation of `wgpu::VertexAttribute`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VertexAttributeKey {
    pub format: wgpu::VertexFormat,
    pub offset: u64,
    pub shader_location: u32,
}

impl From<wgpu::VertexAttribute> for VertexAttributeKey {
    fn from(attr: wgpu::VertexAttribute) -> Self {
        Self {
            format: attr.format,
            offset: attr.offset,
            shader_location: attr.shader_location,
        }
    }
}

/// Hashable representation of `wgpu::VertexBufferLayout`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct VertexBufferLayoutKey {
    pub array_stride: u64,
    pub step_mode: wgpu::VertexStepMode,
    pub attributes: Vec<VertexAttributeKey>,
}

impl<'a> From<&'a wgpu::VertexBufferLayout<'a>> for VertexBufferLayoutKey {
    fn from(layout: &'a wgpu::VertexBufferLayout<'a>) -> Self {
        Self {
            array_stride: layout.array_stride,
            step_mode: layout.step_mode,
            attributes: layout.attributes.iter().copied().map(Into::into).collect(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BlendComponentKey {
    pub src_factor: wgpu::BlendFactor,
    pub dst_factor: wgpu::BlendFactor,
    pub operation: wgpu::BlendOperation,
}

impl From<wgpu::BlendComponent> for BlendComponentKey {
    fn from(c: wgpu::BlendComponent) -> Self {
        Self {
            src_factor: c.src_factor,
            dst_factor: c.dst_factor,
            operation: c.operation,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BlendStateKey {
    pub color: BlendComponentKey,
    pub alpha: BlendComponentKey,
}

impl From<wgpu::BlendState> for BlendStateKey {
    fn from(s: wgpu::BlendState) -> Self {
        Self {
            color: s.color.into(),
            alpha: s.alpha.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ColorTargetKey {
    pub format: wgpu::TextureFormat,
    pub blend: Option<BlendStateKey>,
    pub write_mask: wgpu::ColorWrites,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StencilFaceStateKey {
    pub compare: wgpu::CompareFunction,
    pub fail_op: wgpu::StencilOperation,
    pub depth_fail_op: wgpu::StencilOperation,
    pub pass_op: wgpu::StencilOperation,
}

impl From<wgpu::StencilFaceState> for StencilFaceStateKey {
    fn from(s: wgpu::StencilFaceState) -> Self {
        Self {
            compare: s.compare,
            fail_op: s.fail_op,
            depth_fail_op: s.depth_fail_op,
            pass_op: s.pass_op,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StencilStateKey {
    pub front: StencilFaceStateKey,
    pub back: StencilFaceStateKey,
    pub read_mask: u32,
    pub write_mask: u32,
}

impl From<wgpu::StencilState> for StencilStateKey {
    fn from(s: wgpu::StencilState) -> Self {
        Self {
            front: s.front.into(),
            back: s.back.into(),
            read_mask: s.read_mask,
            write_mask: s.write_mask,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct DepthBiasStateKey {
    pub constant: i32,
    pub slope_scale_bits: u32,
    pub clamp_bits: u32,
}

impl PartialEq for DepthBiasStateKey {
    fn eq(&self, other: &Self) -> bool {
        self.constant == other.constant
            && self.slope_scale_bits == other.slope_scale_bits
            && self.clamp_bits == other.clamp_bits
    }
}

impl Eq for DepthBiasStateKey {}

impl Hash for DepthBiasStateKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.constant.hash(state);
        self.slope_scale_bits.hash(state);
        self.clamp_bits.hash(state);
    }
}

impl From<wgpu::DepthBiasState> for DepthBiasStateKey {
    fn from(s: wgpu::DepthBiasState) -> Self {
        Self {
            constant: s.constant,
            slope_scale_bits: s.slope_scale.to_bits(),
            clamp_bits: s.clamp.to_bits(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DepthStencilKey {
    pub format: wgpu::TextureFormat,
    pub depth_write_enabled: bool,
    pub depth_compare: wgpu::CompareFunction,
    pub stencil: StencilStateKey,
    pub bias: DepthBiasStateKey,
}

impl From<wgpu::DepthStencilState> for DepthStencilKey {
    fn from(s: wgpu::DepthStencilState) -> Self {
        Self {
            format: s.format,
            depth_write_enabled: s.depth_write_enabled,
            depth_compare: s.depth_compare,
            stencil: s.stencil.into(),
            bias: s.bias.into(),
        }
    }
}

/// Key for caching render pipelines.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RenderPipelineKey {
    pub vertex_shader: ShaderHash,
    pub fragment_shader: ShaderHash,

    pub color_targets: Vec<ColorTargetKey>,
    pub depth_stencil: Option<DepthStencilKey>,

    pub primitive_topology: wgpu::PrimitiveTopology,
    pub cull_mode: Option<wgpu::Face>,
    pub front_face: wgpu::FrontFace,

    pub vertex_buffers: Vec<VertexBufferLayoutKey>,
    pub sample_count: u32,

    pub layout: PipelineLayoutKey,
}

/// Key for caching compute pipelines.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ComputePipelineKey {
    pub shader: ShaderHash,
    pub layout: PipelineLayoutKey,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::hash_map::DefaultHasher;

    #[test]
    fn render_pipeline_key_hash_and_eq() {
        let k1 = RenderPipelineKey {
            vertex_shader: 1,
            fragment_shader: 2,
            color_targets: vec![ColorTargetKey {
                format: wgpu::TextureFormat::Rgba8Unorm,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            }],
            depth_stencil: None,
            primitive_topology: wgpu::PrimitiveTopology::TriangleList,
            cull_mode: Some(wgpu::Face::Back),
            front_face: wgpu::FrontFace::Ccw,
            vertex_buffers: vec![],
            sample_count: 1,
            layout: PipelineLayoutKey::empty(),
        };

        let k2 = k1.clone();
        assert_eq!(k1, k2);

        let mut h1 = DefaultHasher::new();
        k1.hash(&mut h1);
        let mut h2 = DefaultHasher::new();
        k2.hash(&mut h2);
        assert_eq!(h1.finish(), h2.finish());

        let mut k3 = k1.clone();
        k3.sample_count = 4;
        assert_ne!(k1, k3);
    }
}

