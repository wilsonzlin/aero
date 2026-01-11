/// Pipeline cache key used by the `aerogpu_cmd` D3D11 executor.
///
/// We reuse the generic `aero-gpu` render-pipeline key type so other backends can
/// share the same cache infrastructure.
pub type PipelineKey = aero_gpu::pipeline_key::RenderPipelineKey;

/// AeroGPU command-stream handle type.
///
/// This mirrors `aerogpu_handle_t` / `AerogpuHandle` in the guest↔host ABI.
pub type AerogpuHandle = u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VertexBufferBinding {
    pub buffer: AerogpuHandle,
    pub stride: u32,
    pub offset: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IndexBufferBinding {
    pub buffer: AerogpuHandle,
    pub format: wgpu::IndexFormat,
    pub offset: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub min_depth: f32,
    pub max_depth: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScissorRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlendState {
    pub blend: Option<wgpu::BlendState>,
    pub write_mask: wgpu::ColorWrites,
}

impl Default for BlendState {
    fn default() -> Self {
        // D3D11 default: blending disabled, write all channels.
        Self {
            blend: None,
            write_mask: wgpu::ColorWrites::ALL,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DepthStencilState {
    pub depth_enable: bool,
    pub depth_write_enable: bool,
    pub depth_compare: wgpu::CompareFunction,

    pub stencil_enable: bool,
    pub stencil_read_mask: u8,
    pub stencil_write_mask: u8,

    /// D3D11 `DepthBias` (constant bias).
    pub depth_bias: i32,
}

impl Default for DepthStencilState {
    fn default() -> Self {
        // D3D11 default depth-stencil state:
        // - depth enabled
        // - depth write enabled
        // - compare LESS
        // - stencil disabled
        Self {
            depth_enable: true,
            depth_write_enable: true,
            depth_compare: wgpu::CompareFunction::Less,
            stencil_enable: false,
            stencil_read_mask: 0xff,
            stencil_write_mask: 0xff,
            depth_bias: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RasterizerState {
    pub cull_mode: Option<wgpu::Face>,
    pub front_face: wgpu::FrontFace,
    pub scissor_enable: bool,
}

impl Default for RasterizerState {
    fn default() -> Self {
        // D3D11 default rasterizer state:
        // - cull back faces
        // - front faces are clockwise (FrontCounterClockwise = FALSE)
        // - scissor disabled
        Self {
            cull_mode: Some(wgpu::Face::Back),
            front_face: wgpu::FrontFace::Cw,
            scissor_enable: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrimitiveTopology {
    PointList,
    LineList,
    LineStrip,
    TriangleList,
    TriangleStrip,
    TriangleFan,
}

impl Default for PrimitiveTopology {
    fn default() -> Self {
        // D3D11's IA topology default is unspecified; use a common safe default.
        Self::TriangleList
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RenderTargets {
    pub colors: [Option<AerogpuHandle>; 8],
    pub depth_stencil: Option<AerogpuHandle>,
}

impl Default for RenderTargets {
    fn default() -> Self {
        Self {
            colors: [None; 8],
            depth_stencil: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct StageBindings {
    pub constant_buffers: Vec<Option<AerogpuHandle>>,
    pub textures: Vec<Option<AerogpuHandle>>,
    pub samplers: Vec<Option<AerogpuHandle>>,
}

impl Default for StageBindings {
    fn default() -> Self {
        Self {
            constant_buffers: vec![None; 16],
            textures: vec![None; 16],
            samplers: vec![None; 16],
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ResourceBindings {
    pub vs: StageBindings,
    pub ps: StageBindings,
}

/// Shadow copy of the D3D11-style immediate-context state that the guest streams.
///
/// This state is updated incrementally by `aerogpu_cmd` opcodes (BindShaders,
/// SetVertexBuffers, SetBlendState, ...). When a draw is issued, the executor
/// derives a [`RenderPipelineKey`] from this state and asks the pipeline cache to
/// materialize a `wgpu::RenderPipeline` on demand.
#[derive(Clone, Debug)]
pub struct D3D11ShadowState {
    pub vs: Option<AerogpuHandle>,
    pub ps: Option<AerogpuHandle>,

    pub input_layout: Option<AerogpuHandle>,

    pub vertex_buffers: Vec<Option<VertexBufferBinding>>,
    pub index_buffer: Option<IndexBufferBinding>,
    pub primitive_topology: PrimitiveTopology,

    pub render_targets: RenderTargets,
    pub viewport: Option<Viewport>,
    pub scissor: Option<ScissorRect>,

    pub blend_state: BlendState,
    pub depth_stencil_state: DepthStencilState,
    pub rasterizer_state: RasterizerState,

    pub bindings: ResourceBindings,
}

impl Default for D3D11ShadowState {
    fn default() -> Self {
        Self {
            vs: None,
            ps: None,
            input_layout: None,
            vertex_buffers: vec![None; 16],
            index_buffer: None,
            primitive_topology: PrimitiveTopology::default(),
            render_targets: RenderTargets::default(),
            viewport: None,
            scissor: None,
            blend_state: BlendState::default(),
            depth_stencil_state: DepthStencilState::default(),
            rasterizer_state: RasterizerState::default(),
            bindings: ResourceBindings::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aero_gpu::pipeline_key::{ColorTargetKey, PipelineLayoutKey, ShaderHash};
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    #[test]
    fn render_pipeline_key_hash_is_stable_for_identical_state() {
        // This is a pure unit test: we don't need wgpu to create an actual pipeline,
        // only to ensure the key is Hash/Eq-stable as we build it from state.
        let mk = |vs: ShaderHash, ps: ShaderHash, scissor_enabled: bool| PipelineKey {
            vertex_shader: vs,
            fragment_shader: ps,
            color_targets: vec![ColorTargetKey {
                format: wgpu::TextureFormat::Rgba8Unorm,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            }],
            depth_stencil: None,
            primitive_topology: wgpu::PrimitiveTopology::TriangleList,
            cull_mode: None,
            front_face: wgpu::FrontFace::Ccw,
            scissor_enabled,
            vertex_buffers: vec![],
            sample_count: 1,
            layout: PipelineLayoutKey::empty(),
        };

        let k1 = mk(1, 2, false);
        let k2 = mk(1, 2, false);

        assert_eq!(k1, k2);
        let mut h1 = DefaultHasher::new();
        k1.hash(&mut h1);
        let mut h2 = DefaultHasher::new();
        k2.hash(&mut h2);
        assert_eq!(h1.finish(), h2.finish());

        let k3 = mk(1, 2, true);
        assert_ne!(k1, k3);
    }
}
