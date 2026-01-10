use crate::state::topology::{translate_primitive_topology, D3DPrimitiveType};
use std::hash::{Hash, Hasher};
use xxhash_rust::xxh3::Xxh3;

/// Stable identifier for a shader module (typically a hash of DXBC or the
/// WGSL output).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ShaderKey(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CullMode {
    None,
    /// Cull faces with clockwise winding in D3D9 terms.
    CW,
    /// Cull faces with counter-clockwise winding in D3D9 terms.
    CCW,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CompareFunc {
    Never,
    Less,
    Equal,
    LessEqual,
    Greater,
    NotEqual,
    GreaterEqual,
    Always,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StencilOp {
    Keep,
    Zero,
    Replace,
    IncrSat,
    DecrSat,
    Invert,
    Incr,
    Decr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BlendFactor {
    Zero,
    One,
    SrcColor,
    InvSrcColor,
    SrcAlpha,
    InvSrcAlpha,
    DestAlpha,
    InvDestAlpha,
    DestColor,
    InvDestColor,
    SrcAlphaSat,
    BlendFactor,
    InvBlendFactor,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BlendOp {
    Add,
    Subtract,
    RevSubtract,
    Min,
    Max,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ColorWriteMask(pub u8);

impl ColorWriteMask {
    pub const NONE: Self = Self(0);
    pub const RGBA: Self = Self(0b1111);
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RasterizerState {
    pub cull_mode: CullMode,
    /// D3D9 `D3DRS_FRONTCOUNTERCLOCKWISE`.
    pub front_counter_clockwise: bool,
    /// D3D9 `D3DRS_FILLMODE`.
    pub fill_wireframe: bool,
}

impl Default for RasterizerState {
    fn default() -> Self {
        Self {
            cull_mode: CullMode::CCW,
            front_counter_clockwise: false,
            fill_wireframe: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DepthStencilState {
    pub depth_enable: bool,
    pub depth_write_enable: bool,
    pub depth_func: CompareFunc,

    pub stencil_enable: bool,
    pub stencil_func: CompareFunc,
    pub stencil_fail: StencilOp,
    pub stencil_zfail: StencilOp,
    pub stencil_pass: StencilOp,
    pub stencil_read_mask: u8,
    pub stencil_write_mask: u8,
    pub stencil_ref: u8,
}

impl Default for DepthStencilState {
    fn default() -> Self {
        Self {
            depth_enable: false,
            depth_write_enable: false,
            depth_func: CompareFunc::LessEqual,

            stencil_enable: false,
            stencil_func: CompareFunc::Always,
            stencil_fail: StencilOp::Keep,
            stencil_zfail: StencilOp::Keep,
            stencil_pass: StencilOp::Keep,
            stencil_read_mask: 0xFF,
            stencil_write_mask: 0xFF,
            stencil_ref: 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DepthStencilPipelineState {
    pub depth_enable: bool,
    pub depth_write_enable: bool,
    pub depth_func: CompareFunc,

    pub stencil_enable: bool,
    pub stencil_func: CompareFunc,
    pub stencil_fail: StencilOp,
    pub stencil_zfail: StencilOp,
    pub stencil_pass: StencilOp,
    pub stencil_read_mask: u8,
    pub stencil_write_mask: u8,
}

impl From<&DepthStencilState> for DepthStencilPipelineState {
    fn from(value: &DepthStencilState) -> Self {
        Self {
            depth_enable: value.depth_enable,
            depth_write_enable: value.depth_write_enable,
            depth_func: value.depth_func,
            stencil_enable: value.stencil_enable,
            stencil_func: value.stencil_func,
            stencil_fail: value.stencil_fail,
            stencil_zfail: value.stencil_zfail,
            stencil_pass: value.stencil_pass,
            stencil_read_mask: value.stencil_read_mask,
            stencil_write_mask: value.stencil_write_mask,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlendState {
    pub alpha_blend_enable: bool,
    pub src_blend: BlendFactor,
    pub dst_blend: BlendFactor,
    pub blend_op: BlendOp,

    pub separate_alpha_blend_enable: bool,
    pub src_blend_alpha: BlendFactor,
    pub dst_blend_alpha: BlendFactor,
    pub blend_op_alpha: BlendOp,

    /// D3D9 `D3DRS_BLENDFACTOR` packed ARGB (0xAARRGGBB).
    pub blend_factor: u32,
}

impl Default for BlendState {
    fn default() -> Self {
        Self {
            alpha_blend_enable: false,
            src_blend: BlendFactor::One,
            dst_blend: BlendFactor::Zero,
            blend_op: BlendOp::Add,

            separate_alpha_blend_enable: false,
            src_blend_alpha: BlendFactor::One,
            dst_blend_alpha: BlendFactor::Zero,
            blend_op_alpha: BlendOp::Add,

            blend_factor: 0xFFFF_FFFF,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BlendPipelineState {
    pub alpha_blend_enable: bool,
    pub src_blend: BlendFactor,
    pub dst_blend: BlendFactor,
    pub blend_op: BlendOp,

    pub separate_alpha_blend_enable: bool,
    pub src_blend_alpha: BlendFactor,
    pub dst_blend_alpha: BlendFactor,
    pub blend_op_alpha: BlendOp,
}

impl From<&BlendState> for BlendPipelineState {
    fn from(value: &BlendState) -> Self {
        Self {
            alpha_blend_enable: value.alpha_blend_enable,
            src_blend: value.src_blend,
            dst_blend: value.dst_blend,
            blend_op: value.blend_op,
            separate_alpha_blend_enable: value.separate_alpha_blend_enable,
            src_blend_alpha: value.src_blend_alpha,
            dst_blend_alpha: value.dst_blend_alpha,
            blend_op_alpha: value.blend_op_alpha,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SamplerState {
    pub address_u: u32,
    pub address_v: u32,
    pub min_filter: u32,
    pub mag_filter: u32,
    pub mip_filter: u32,
}

impl Default for SamplerState {
    fn default() -> Self {
        Self {
            address_u: 0,
            address_v: 0,
            min_filter: 0,
            mag_filter: 0,
            mip_filter: 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct VertexAttributeKey {
    pub format: wgpu::VertexFormat,
    pub offset: u64,
    pub shader_location: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct VertexBufferLayoutKey {
    pub array_stride: u64,
    pub step_mode: wgpu::VertexStepMode,
    pub attributes: Vec<VertexAttributeKey>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RenderTargetState {
    /// D3D9 allows up to 4 MRTs, but we keep the formats as a dense list starting
    /// from slot 0.
    pub color_formats: Vec<wgpu::TextureFormat>,
    pub depth_stencil_format: Option<wgpu::TextureFormat>,
    pub srgb_write_enable: bool,
    pub color_write_masks: Vec<ColorWriteMask>,
}

impl Default for RenderTargetState {
    fn default() -> Self {
        Self {
            color_formats: Vec::new(),
            depth_stencil_format: None,
            srgb_write_enable: false,
            color_write_masks: Vec::new(),
        }
    }
}

/// Full D3D9 state snapshot needed to compute a pipeline key and dynamic state.
#[derive(Clone, Debug)]
pub struct StateTracker {
    pub vertex_shader: Option<ShaderKey>,
    pub pixel_shader: Option<ShaderKey>,

    pub rasterizer: RasterizerState,
    pub depth_stencil: DepthStencilState,
    pub blend: BlendState,
    pub render_targets: RenderTargetState,

    pub samplers: Vec<SamplerState>,
    pub textures: Vec<Option<u64>>,

    pub viewport: Option<Viewport>,
    pub scissor: Option<ScissorRect>,

    pub vertex_layouts: Vec<VertexBufferLayoutKey>,
    pub primitive_type: D3DPrimitiveType,
}

impl Default for StateTracker {
    fn default() -> Self {
        Self {
            vertex_shader: None,
            pixel_shader: None,

            rasterizer: RasterizerState::default(),
            depth_stencil: DepthStencilState::default(),
            blend: BlendState::default(),
            render_targets: RenderTargetState::default(),

            samplers: vec![SamplerState::default(); 16],
            textures: vec![None; 16],

            viewport: None,
            scissor: None,

            vertex_layouts: Vec::new(),
            primitive_type: D3DPrimitiveType::TriangleList,
        }
    }
}

impl StateTracker {
    pub fn set_vertex_shader(&mut self, shader: Option<ShaderKey>) {
        self.vertex_shader = shader;
    }

    pub fn set_pixel_shader(&mut self, shader: Option<ShaderKey>) {
        self.pixel_shader = shader;
    }

    pub fn set_render_targets(
        &mut self,
        color_formats: Vec<wgpu::TextureFormat>,
        depth_stencil_format: Option<wgpu::TextureFormat>,
    ) {
        self.render_targets.color_formats = color_formats;
        self.render_targets.depth_stencil_format = depth_stencil_format;

        // Keep masks sized correctly.
        if self.render_targets.color_write_masks.len() != self.render_targets.color_formats.len() {
            self.render_targets.color_write_masks =
                vec![ColorWriteMask::RGBA; self.render_targets.color_formats.len()];
        }
    }

    pub fn set_color_write_mask(&mut self, rt_index: usize, mask: ColorWriteMask) {
        if rt_index >= self.render_targets.color_write_masks.len() {
            self.render_targets
                .color_write_masks
                .resize(rt_index + 1, ColorWriteMask::RGBA);
        }
        self.render_targets.color_write_masks[rt_index] = mask;
    }

    pub fn set_srgb_write_enable(&mut self, enable: bool) {
        self.render_targets.srgb_write_enable = enable;
    }

    pub fn set_viewport(&mut self, viewport: Viewport) {
        self.viewport = Some(viewport);
    }

    pub fn set_scissor_rect(&mut self, rect: ScissorRect) {
        self.scissor = Some(rect);
    }

    pub fn set_vertex_layouts(&mut self, layouts: Vec<VertexBufferLayoutKey>) {
        self.vertex_layouts = layouts;
    }

    pub fn set_primitive_type(&mut self, prim: D3DPrimitiveType) {
        self.primitive_type = prim;
    }

    /// Returns `None` if the state is incomplete for a draw call (missing shaders
    /// or render targets).
    pub fn pipeline_key(&self) -> Option<PipelineKey> {
        let vs = self.vertex_shader?;
        let ps = self.pixel_shader?;

        if self.render_targets.color_formats.is_empty() {
            return None;
        }

        Some(PipelineKey {
            vertex_shader: vs,
            pixel_shader: ps,
            vertex_layouts: self.vertex_layouts.clone(),
            render_targets: self.render_targets.clone(),
            rasterizer: self.rasterizer.clone(),
            depth_stencil: (&self.depth_stencil).into(),
            blend: (&self.blend).into(),
            primitive_type: self.primitive_type,
            // `PipelineKey` also includes the translated WebGPU topology for safety.
            topology: translate_primitive_topology(self.primitive_type).topology,
        })
    }
}

/// D3D9 viewport state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub min_depth: f32,
    pub max_depth: f32,
}

/// D3D9 scissor rect.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ScissorRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Pipeline-affecting D3D9 state, suitable as a cache key for `wgpu::RenderPipeline`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PipelineKey {
    pub vertex_shader: ShaderKey,
    pub pixel_shader: ShaderKey,

    pub vertex_layouts: Vec<VertexBufferLayoutKey>,
    pub render_targets: RenderTargetState,

    pub rasterizer: RasterizerState,
    pub depth_stencil: DepthStencilPipelineState,
    pub blend: BlendPipelineState,

    pub primitive_type: D3DPrimitiveType,
    pub topology: wgpu::PrimitiveTopology,
}

impl PipelineKey {
    /// Stable 64-bit hash of the pipeline key. This can be used for fast-path
    /// logging/debug output or as an external cache key.
    pub fn stable_hash64(&self) -> u64 {
        let mut hasher = Xxh3::new();
        self.hash(&mut hasher);
        hasher.finish()
    }
}
