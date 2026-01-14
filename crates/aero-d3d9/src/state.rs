//! D3D9 state translation.
//!
//! This module keeps the state model explicit and serialisable so it can be fed
//! from a guest command stream.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CullMode {
    None,
    Front,
    Back,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlendFactor {
    Zero,
    One,
    SrcColor,
    OneMinusSrcColor,
    SrcAlpha,
    OneMinusSrcAlpha,
    DstColor,
    OneMinusDstColor,
    DstAlpha,
    OneMinusDstAlpha,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlendOp {
    Add,
    Subtract,
    ReverseSubtract,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlendState {
    pub enabled: bool,
    pub src_factor: BlendFactor,
    pub dst_factor: BlendFactor,
    pub op: BlendOp,
}

impl Default for BlendState {
    fn default() -> Self {
        Self {
            enabled: false,
            src_factor: BlendFactor::One,
            dst_factor: BlendFactor::Zero,
            op: BlendOp::Add,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DepthState {
    pub enabled: bool,
    pub write_enabled: bool,
    pub func: CompareFunc,
}

impl Default for DepthState {
    fn default() -> Self {
        Self {
            enabled: false,
            write_enabled: false,
            func: CompareFunc::LessEqual,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RasterState {
    pub cull: CullMode,
}

impl Default for RasterState {
    fn default() -> Self {
        Self {
            cull: CullMode::Back,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FilterMode {
    Point,
    Linear,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AddressMode {
    Clamp,
    Wrap,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SamplerState {
    pub min_filter: FilterMode,
    pub mag_filter: FilterMode,
    pub address_u: AddressMode,
    pub address_v: AddressMode,
    pub address_w: AddressMode,
}

impl Default for SamplerState {
    fn default() -> Self {
        Self {
            min_filter: FilterMode::Linear,
            mag_filter: FilterMode::Linear,
            address_u: AddressMode::Wrap,
            address_v: AddressMode::Wrap,
            address_w: AddressMode::Wrap,
        }
    }
}

/// D3D9 vertex declaration element type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VertexElementType {
    Float1,
    Float2,
    Float3,
    Float4,
    Color, // D3DCOLOR (BGRA8)
}

impl VertexElementType {
    pub fn byte_size(self) -> usize {
        match self {
            Self::Float1 => 4,
            Self::Float2 => 8,
            Self::Float3 => 12,
            Self::Float4 => 16,
            Self::Color => 4,
        }
    }
}

/// Semantic usage for a vertex element.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VertexUsage {
    Position,
    TexCoord,
    Color,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VertexElement {
    pub offset: u32,
    pub ty: VertexElementType,
    pub usage: VertexUsage,
    pub usage_index: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VertexDecl {
    pub stride: u32,
    pub elements: Vec<VertexElement>,
}

impl VertexDecl {
    pub fn new(stride: u32, elements: Vec<VertexElement>) -> Self {
        Self { stride, elements }
    }
}

impl fmt::Display for VertexDecl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "stride={} [", self.stride)?;
        for (i, e) in self.elements.iter().enumerate() {
            if i != 0 {
                write!(f, ", ")?;
            }
            write!(
                f,
                "{:?}{:?} off={} ty={:?}",
                e.usage, e.usage_index, e.offset, e.ty
            )?;
        }
        write!(f, "]")
    }
}

/// Subset of D3D9 surface formats needed for desktop/Aero bring-up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TextureFormat {
    /// `D3DFMT_A8R8G8B8` (little-endian BGRA in memory).
    A8R8G8B8,
    /// `D3DFMT_X8R8G8B8` (little-endian BGRX in memory).
    X8R8G8B8,
    /// `D3DFMT_A8`.
    A8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WebGpuTextureFormat {
    Rgba8Unorm,
    Bgra8Unorm,
    R8Unorm,
}

pub fn texture_format_to_webgpu(fmt: TextureFormat) -> WebGpuTextureFormat {
    match fmt {
        TextureFormat::A8R8G8B8 | TextureFormat::X8R8G8B8 => WebGpuTextureFormat::Bgra8Unorm,
        TextureFormat::A8 => WebGpuTextureFormat::R8Unorm,
    }
}

impl TextureFormat {
    pub fn bytes_per_pixel(self) -> usize {
        match self {
            TextureFormat::A8R8G8B8 | TextureFormat::X8R8G8B8 => 4,
            TextureFormat::A8 => 1,
        }
    }
}

/// Convert a tightly-packed guest texture into RGBA8 for CPU-side testing or
/// WebGPU upload staging.
///
/// - `pitch_bytes` is the guest row pitch (bytes per row).
/// - The returned buffer is tightly packed RGBA8 (width * height * 4).
pub fn convert_guest_texture_to_rgba8(
    fmt: TextureFormat,
    width: u32,
    height: u32,
    pitch_bytes: usize,
    data: &[u8],
) -> Vec<u8> {
    let width = width as usize;
    let height = height as usize;
    let mut out = vec![0u8; width * height * 4];

    match fmt {
        TextureFormat::A8R8G8B8 => {
            for y in 0..height {
                let src_row = &data[y * pitch_bytes..y * pitch_bytes + width * 4];
                let dst_row = &mut out[y * width * 4..(y + 1) * width * 4];
                for (src, dst) in src_row.chunks_exact(4).zip(dst_row.chunks_exact_mut(4)) {
                    // BGRA -> RGBA
                    dst[0] = src[2];
                    dst[1] = src[1];
                    dst[2] = src[0];
                    dst[3] = src[3];
                }
            }
        }
        TextureFormat::X8R8G8B8 => {
            for y in 0..height {
                let src_row = &data[y * pitch_bytes..y * pitch_bytes + width * 4];
                let dst_row = &mut out[y * width * 4..(y + 1) * width * 4];
                for (src, dst) in src_row.chunks_exact(4).zip(dst_row.chunks_exact_mut(4)) {
                    // BGRX -> RGBA
                    dst[0] = src[2];
                    dst[1] = src[1];
                    dst[2] = src[0];
                    dst[3] = 0xFF;
                }
            }
        }
        TextureFormat::A8 => {
            for y in 0..height {
                let src_row = &data[y * pitch_bytes..y * pitch_bytes + width];
                let dst_row = &mut out[y * width * 4..(y + 1) * width * 4];
                for (src, dst) in src_row.iter().zip(dst_row.chunks_exact_mut(4)) {
                    dst[0] = 0xFF;
                    dst[1] = 0xFF;
                    dst[2] = 0xFF;
                    dst[3] = *src;
                }
            }
        }
    }

    out
}

// ---------------------------------------------------------------------------
// WebGPU pipeline translation + cache (D3D9 fixed-function state → wgpu)
// ---------------------------------------------------------------------------

pub mod pipeline_cache;
pub mod topology;
pub mod tracker;
pub mod translate;

pub use pipeline_cache::{PipelineCache, PipelineCacheStats};
pub use topology::{
    expand_triangle_fan_nonindexed_u32, expand_triangle_fan_u16, expand_triangle_fan_u32,
    translate_primitive_topology, D3DPrimitiveType, PrimitiveTopologyTranslation,
};
pub use tracker::{
    PipelineKey, ShaderKey, StateTracker, VertexAttributeKey, VertexBufferLayoutKey,
};
pub use translate::{
    translate_blend_factor, translate_blend_op, translate_color_write_mask, translate_compare_func,
    translate_cull_and_front_face, translate_depth_stencil_state, translate_pipeline_state,
    translate_rasterizer_state, translate_stencil_op, translate_texture_format_srgb,
};
pub use translate::{DynamicRenderState, TranslatedPipelineState};
