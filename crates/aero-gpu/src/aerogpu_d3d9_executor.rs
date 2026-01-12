use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::{Arc, OnceLock};

#[cfg(not(target_arch = "wasm32"))]
use std::sync::mpsc;

use aero_d3d9::shader;
use aero_d3d9::vertex::{StandardLocationMap, VertexDeclaration, VertexLocationMap};
use aero_protocol::aerogpu::aerogpu_cmd as cmd;
use aero_protocol::aerogpu::aerogpu_cmd::AEROGPU_COPY_FLAG_WRITEBACK_DST;
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::aerogpu_ring as ring;
use futures_intrusive::channel::shared::oneshot_channel;
use thiserror::Error;
use tracing::debug;
use wgpu::util::DeviceExt;

use crate::aerogpu_executor::{AllocEntry, AllocTable};
use crate::bc_decompress::{
    decompress_bc1_rgba8, decompress_bc2_rgba8, decompress_bc3_rgba8, decompress_bc7_rgba8,
};
use crate::guest_memory::{GuestMemory, GuestMemoryError};
use crate::protocol::{parse_cmd_stream, AeroGpuCmd, AeroGpuCmdStreamParseError};
use crate::texture_manager::TextureRegion;
use crate::{readback_depth32f, readback_rgba8, readback_stencil8};

/// Minimal executor for the D3D9 UMD-produced `aerogpu_cmd.h` command stream.
///
/// This is intentionally a bring-up implementation: it focuses on enough
/// resource/state tracking to render basic D3D9Ex/DWM scenes, starting with a
/// deterministic triangle test.
pub struct AerogpuD3d9Executor {
    device: wgpu::Device,
    queue: wgpu::Queue,

    shader_cache: shader::ShaderCache,

    resources: HashMap<u32, Resource>,
    /// Handle indirection table for shared resources.
    ///
    /// - Original resources are stored as `handle -> handle`.
    /// - Imported shared resources are stored as `alias_handle -> underlying_handle`.
    resource_handles: HashMap<u32, u32>,
    /// Refcount table keyed by the underlying handle.
    ///
    /// Refcount includes the original handle entry plus all imported aliases.
    resource_refcounts: HashMap<u32, u32>,
    /// share_token -> underlying resource handle.
    shared_surface_by_token: HashMap<u64, u32>,
    /// share_token values that were previously valid but were released (or otherwise removed).
    ///
    /// Prevents misbehaving guests from "re-arming" a released token by re-exporting it for a
    /// different resource.
    retired_share_tokens: HashSet<u64>,
    shaders: HashMap<u32, Shader>,
    input_layouts: HashMap<u32, InputLayout>,

    constants_buffer: wgpu::Buffer,

    dummy_texture_view: wgpu::TextureView,
    downlevel_flags: wgpu::DownlevelFlags,

    bind_group_layout: wgpu::BindGroupLayout,
    pipeline_layout: wgpu::PipelineLayout,
    bind_group: Option<wgpu::BindGroup>,
    bind_group_dirty: bool,
    samplers_vs: [Arc<wgpu::Sampler>; MAX_SAMPLERS],
    sampler_state_vs: [D3d9SamplerState; MAX_SAMPLERS],
    samplers_ps: [Arc<wgpu::Sampler>; MAX_SAMPLERS],
    sampler_state_ps: [D3d9SamplerState; MAX_SAMPLERS],
    sampler_cache: HashMap<D3d9SamplerState, Arc<wgpu::Sampler>>,

    pipelines: HashMap<PipelineCacheKey, wgpu::RenderPipeline>,
    alpha_test_pixel_shaders: HashMap<AlphaTestShaderModuleKey, Arc<wgpu::ShaderModule>>,

    clear_shader: wgpu::ShaderModule,
    clear_bind_group: wgpu::BindGroup,
    clear_pipeline_layout: wgpu::PipelineLayout,
    clear_color_buffer: wgpu::Buffer,
    clear_pipelines: HashMap<wgpu::TextureFormat, wgpu::RenderPipeline>,
    clear_depth_pipelines: HashMap<ClearDepthPipelineKey, wgpu::RenderPipeline>,
    clear_dummy_color_targets: HashMap<(u32, u32), ClearDummyColorTarget>,

    presented_scanouts: HashMap<u32, u32>,

    triangle_fan_index_buffers: HashMap<u32, TriangleFanIndexBuffer>,

    contexts: HashMap<u32, ContextState>,
    current_context_id: u32,

    state: State,
    encoder: Option<wgpu::CommandEncoder>,
}

/// Metadata + view for a scanout that has been presented via `PRESENT`/`PRESENT_EX`.
///
/// This is primarily intended for WASM presenters which need to blit the scanout to a surface
/// without performing a CPU readback.
pub struct PresentedScanout<'a> {
    pub view: &'a wgpu::TextureView,
    pub format: wgpu::TextureFormat,
    pub width: u32,
    pub height: u32,
}
struct SubmissionCtx<'a> {
    guest_memory: Option<&'a mut dyn GuestMemory>,
    alloc_table: Option<&'a AllocTable>,
}

impl<'a> SubmissionCtx<'a> {
    fn require_alloc_entry(&self, alloc_id: u32) -> Result<&'a AllocEntry, AerogpuD3d9Error> {
        let table = self
            .alloc_table
            .ok_or(AerogpuD3d9Error::MissingAllocationTable(alloc_id))?;
        table
            .get(alloc_id)
            .ok_or(AerogpuD3d9Error::MissingAllocTable(alloc_id))
    }
}

#[derive(Debug)]
struct ContextState {
    constants_buffer: wgpu::Buffer,
    bind_group: Option<wgpu::BindGroup>,
    bind_group_dirty: bool,
    samplers_vs: [Arc<wgpu::Sampler>; MAX_SAMPLERS],
    sampler_state_vs: [D3d9SamplerState; MAX_SAMPLERS],
    samplers_ps: [Arc<wgpu::Sampler>; MAX_SAMPLERS],
    sampler_state_ps: [D3d9SamplerState; MAX_SAMPLERS],
    state: State,
}

impl ContextState {
    fn new(device: &wgpu::Device, default_sampler: Arc<wgpu::Sampler>) -> Self {
        Self {
            constants_buffer: create_constants_buffer(device),
            bind_group: None,
            bind_group_dirty: true,
            samplers_vs: std::array::from_fn(|_| default_sampler.clone()),
            sampler_state_vs: std::array::from_fn(|_| D3d9SamplerState::default()),
            samplers_ps: std::array::from_fn(|_| default_sampler.clone()),
            sampler_state_ps: std::array::from_fn(|_| D3d9SamplerState::default()),
            state: create_default_state(),
        }
    }
}

#[derive(Debug, Error)]
pub enum AerogpuD3d9Error {
    #[error("wgpu adapter not found")]
    AdapterNotFound,
    #[error("request_device failed: {0}")]
    RequestDevice(String),
    #[error("failed to parse AeroGPU command stream: {0}")]
    Parse(#[from] AeroGpuCmdStreamParseError),
    #[error("unknown resource handle {0}")]
    UnknownResource(u32),
    #[error("resource handle {0} is already in use")]
    ResourceHandleInUse(u32),
    #[error("shader handle {0} is already in use")]
    ShaderHandleInUse(u32),
    #[error("input layout handle {0} is already in use")]
    InputLayoutHandleInUse(u32),
    #[error("unknown shader handle {0}")]
    UnknownShader(u32),
    #[error("unknown input layout handle {0}")]
    UnknownInputLayout(u32),
    #[error("shader translation failed: {0}")]
    ShaderTranslation(String),
    #[error("shader handle {shader_handle} has stage {actual:?}, expected {expected:?}")]
    ShaderStageMismatch {
        shader_handle: u32,
        expected: shader::ShaderStage,
        actual: shader::ShaderStage,
    },
    #[error("invalid vertex declaration: {0}")]
    VertexDeclaration(String),
    #[error("draw called without a bound vertex and pixel shader")]
    MissingShaders,
    #[error("draw called without an input layout")]
    MissingInputLayout,
    #[error("draw called without any render target bound")]
    MissingRenderTargets,
    #[error("draw called without a bound vertex buffer for stream {stream}")]
    MissingVertexBuffer { stream: u8 },
    #[error("draw_indexed called without an index buffer")]
    MissingIndexBuffer,
    #[error("unsupported aerogpu_format {0}")]
    UnsupportedFormat(u32),
    #[error("unsupported primitive topology {0}")]
    UnsupportedTopology(u32),
    #[error("upload_resource target {0} is not an uploadable resource")]
    UploadNotSupported(u32),
    #[error("upload_resource out of bounds for resource {0}")]
    UploadOutOfBounds(u32),
    #[error("copy operation not supported for src={src} dst={dst}")]
    CopyNotSupported { src: u32, dst: u32 },
    #[error("copy operation out of bounds for src={src} dst={dst}")]
    CopyOutOfBounds { src: u32, dst: u32 },
    #[error("readback only supported for RGBA8/BGRA8 textures (handle {0})")]
    ReadbackUnsupported(u32),
    #[error("stencil readback only supported for D24_UNORM_S8_UINT textures (handle {0})")]
    ReadbackStencilUnsupported(u32),
    #[error("depth readback only supported for D32_FLOAT textures (handle {0})")]
    ReadbackDepthUnsupported(u32),
    #[error("unknown shared surface token 0x{0:016X}")]
    UnknownShareToken(u64),
    #[error("shared surface token 0x{0:016X} was previously released and cannot be reused")]
    ShareTokenRetired(u64),
    #[error(
        "shared surface token 0x{share_token:016X} already exported (existing_handle={existing} new_handle={new})"
    )]
    ShareTokenAlreadyExported {
        share_token: u64,
        existing: u32,
        new: u32,
    },
    #[error(
        "shared surface alias handle {alias} already bound (existing_handle={existing} new_handle={new})"
    )]
    SharedSurfaceAliasAlreadyBound { alias: u32, existing: u32, new: u32 },
    #[error("submission is missing an allocation table required to resolve alloc_id={0}")]
    MissingAllocationTable(u32),
    #[error("allocation table does not contain alloc_id={0}")]
    MissingAllocTable(u32),
    #[error("missing guest memory for dirty guest-backed resource {0}")]
    MissingGuestMemory(u32),
    #[error("validation error: {0}")]
    Validation(String),
    #[error(transparent)]
    GuestMemory(#[from] GuestMemoryError),
}

#[derive(Debug, Clone, Copy)]
struct GuestBufferBacking {
    alloc_id: u32,
    alloc_offset_bytes: u64,
}

#[derive(Debug, Clone, Copy)]
struct GuestTextureBacking {
    alloc_id: u32,
    alloc_offset_bytes: u64,
    row_pitch_bytes: u32,
    size_bytes: u64,
}

/// Subresource layout for guest-backed textures with mip levels and array layers.
///
/// MVP layout:
/// - Subresources are tightly packed (no per-row padding beyond `mip_width * bpp`).
/// - For each array layer, mip levels are laid out sequentially starting at mip 0.
/// - Array layers are laid out sequentially.
#[derive(Debug, Clone, Copy)]
struct GuestTextureSubresourceLayout {
    width: u32,
    height: u32,
    mip_level_count: u32,
    array_layers: u32,
    bytes_per_pixel: u32,
}

impl GuestTextureSubresourceLayout {
    fn new(
        width: u32,
        height: u32,
        mip_level_count: u32,
        array_layers: u32,
        bytes_per_pixel: u32,
    ) -> Self {
        Self {
            width,
            height,
            mip_level_count,
            array_layers,
            bytes_per_pixel,
        }
    }

    fn mip_width(&self, mip_level: u32) -> u32 {
        mip_dim(self.width, mip_level)
    }

    fn mip_height(&self, mip_level: u32) -> u32 {
        mip_dim(self.height, mip_level)
    }

    fn subresource_row_pitch_bytes(
        &self,
        _array_layer: u32,
        mip_level: u32,
    ) -> Result<u32, AerogpuD3d9Error> {
        self.mip_width(mip_level).checked_mul(self.bytes_per_pixel).ok_or_else(|| {
            AerogpuD3d9Error::Validation("texture subresource row pitch overflow".into())
        })
    }

    fn subresource_size_bytes(
        &self,
        array_layer: u32,
        mip_level: u32,
    ) -> Result<u64, AerogpuD3d9Error> {
        let bpr = self.subresource_row_pitch_bytes(array_layer, mip_level)? as u64;
        let height = self.mip_height(mip_level) as u64;
        bpr.checked_mul(height)
            .ok_or_else(|| AerogpuD3d9Error::Validation("texture subresource size overflow".into()))
    }

    fn layer_size_bytes(&self) -> Result<u64, AerogpuD3d9Error> {
        let mut total = 0u64;
        for mip in 0..self.mip_level_count {
            total = total
                .checked_add(self.subresource_size_bytes(0, mip)?)
                .ok_or_else(|| AerogpuD3d9Error::Validation("texture backing overflow".into()))?;
        }
        Ok(total)
    }

    fn total_size_bytes(&self) -> Result<u64, AerogpuD3d9Error> {
        let layer = self.layer_size_bytes()?;
        layer
            .checked_mul(self.array_layers as u64)
            .ok_or_else(|| AerogpuD3d9Error::Validation("texture backing overflow".into()))
    }

    fn subresource_offset_bytes(
        &self,
        array_layer: u32,
        mip_level: u32,
    ) -> Result<u64, AerogpuD3d9Error> {
        if array_layer >= self.array_layers || mip_level >= self.mip_level_count {
            return Err(AerogpuD3d9Error::Validation(
                "subresource index out of bounds".into(),
            ));
        }

        let layer_size = self.layer_size_bytes()?;
        let mut offset = layer_size
            .checked_mul(array_layer as u64)
            .ok_or_else(|| AerogpuD3d9Error::Validation("texture backing overflow".into()))?;
        for mip in 0..mip_level {
            offset = offset
                .checked_add(self.subresource_size_bytes(array_layer, mip)?)
                .ok_or_else(|| AerogpuD3d9Error::Validation("texture backing overflow".into()))?;
        }
        Ok(offset)
    }
}

#[derive(Debug, Clone, Copy)]
struct TextureWritebackPlan {
    backing: GuestTextureBacking,
    backing_gpa: u64,
    dst_mip_level: u32,
    dst_array_layer: u32,
    dst_subresource_offset_bytes: u64,
    dst_subresource_row_pitch_bytes: u32,
    dst_x: u32,
    dst_y: u32,
    height: u32,
    format_raw: u32,
    is_x8: bool,
    guest_bytes_per_pixel: u32,
    host_bytes_per_pixel: u32,
    guest_unpadded_bytes_per_row: u32,
    host_unpadded_bytes_per_row: u32,
    padded_bytes_per_row: u32,
}

#[derive(Debug)]
enum PendingWriteback {
    Buffer {
        staging: wgpu::Buffer,
        dst_gpa: u64,
        size_bytes: u64,
    },
    Texture2d {
        staging: wgpu::Buffer,
        plan: TextureWritebackPlan,
    },
}

#[derive(Debug)]
enum Resource {
    Buffer {
        buffer: wgpu::Buffer,
        size: u64,
        usage_flags: u32,
        backing: Option<GuestBufferBacking>,
        dirty_ranges: Vec<Range<u64>>,
        /// CPU shadow copy of the buffer contents.
        ///
        /// This is currently used to support vertex format conversions (e.g. D3D9 `D3DCOLOR`
        /// BGRA-in-memory → RGBA-in-shader) without requiring shader-side workarounds.
        shadow: Vec<u8>,
    },
    Texture2d {
        texture: wgpu::Texture,
        view: wgpu::TextureView,
        view_srgb: Option<wgpu::TextureView>,
        usage_flags: u32,
        format_raw: u32,
        format: wgpu::TextureFormat,
        width: u32,
        height: u32,
        mip_level_count: u32,
        array_layers: u32,
        row_pitch_bytes: u32,
        backing: Option<GuestTextureBacking>,
        dirty_ranges: Vec<Range<u64>>,
    },
}

#[derive(Debug)]
struct Shader {
    stage: shader::ShaderStage,
    key: u64,
    module: wgpu::ShaderModule,
    wgsl: String,
    entry_point: &'static str,
    uses_semantic_locations: bool,
    used_samplers_mask: u16,
}

#[derive(Debug)]
struct InputLayout {
    decl: VertexDeclaration,
}

#[derive(Debug)]
struct VertexInputs {
    streams: Vec<u8>,
    stream_to_slot: HashMap<u8, u32>,
    buffers: Vec<VertexBufferLayoutOwned>,
}

#[derive(Debug)]
struct VertexBufferLayoutOwned {
    array_stride: u64,
    step_mode: wgpu::VertexStepMode,
    attributes: Vec<wgpu::VertexAttribute>,
}

#[derive(Debug, Clone, Copy)]
struct VertexBufferBinding {
    buffer: u32,
    stride_bytes: u32,
    offset_bytes: u32,
}

#[derive(Debug, Clone, Copy)]
struct IndexBufferBinding {
    buffer: u32,
    format: wgpu::IndexFormat,
    offset_bytes: u32,
}

struct TriangleFanIndexBuffer {
    buffer: wgpu::Buffer,
    format: wgpu::IndexFormat,
}

#[derive(Debug, Default)]
struct State {
    vs: u32,
    ps: u32,
    input_layout: u32,

    render_targets: RenderTargetsState,
    viewport: Option<ViewportState>,
    scissor: Option<(u32, u32, u32, u32)>,

    vertex_buffers: [Option<VertexBufferBinding>; 16],
    index_buffer: Option<IndexBufferBinding>,
    topology_raw: u32,
    topology: wgpu::PrimitiveTopology,

    blend_state: BlendState,
    blend_constant: [f32; 4],
    sample_mask: u32,
    depth_stencil_state: DepthStencilState,
    rasterizer_state: RasterizerState,

    alpha_test_enable: bool,
    alpha_test_func: u32,
    alpha_test_ref: u8,

    textures_vs: [u32; MAX_SAMPLERS],
    textures_ps: [u32; MAX_SAMPLERS],

    render_states: Vec<u32>,
    sampler_states_vs: [Vec<u32>; MAX_SAMPLERS],
    sampler_states_ps: [Vec<u32>; MAX_SAMPLERS],
}

#[derive(Debug, Clone, Copy)]
struct ViewportState {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    min_depth: f32,
    max_depth: f32,
}

#[derive(Debug, Default, Clone, Copy)]
struct RenderTargetsState {
    color_count: u32,
    colors: [u32; 8],
    depth_stencil: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct BlendState {
    enable: bool,
    src_factor: u32,
    dst_factor: u32,
    blend_op: u32,
    src_factor_alpha: u32,
    dst_factor_alpha: u32,
    blend_op_alpha: u32,
    color_write_mask: [u8; 8],
}

impl Default for BlendState {
    fn default() -> Self {
        Self {
            enable: false,
            // REPLACE
            src_factor: 1,
            dst_factor: 0,
            blend_op: 0,
            src_factor_alpha: 1,
            dst_factor_alpha: 0,
            blend_op_alpha: 0,
            color_write_mask: [0xF; 8],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct DepthStencilState {
    depth_enable: bool,
    depth_write_enable: bool,
    depth_func: u32,
    stencil_enable: bool,
    two_sided_stencil_enable: bool,
    stencil_read_mask: u8,
    stencil_write_mask: u8,
    stencil_func: u32,
    stencil_fail_op: u32,
    stencil_depth_fail_op: u32,
    stencil_pass_op: u32,
    ccw_stencil_func: u32,
    ccw_stencil_fail_op: u32,
    ccw_stencil_depth_fail_op: u32,
    ccw_stencil_pass_op: u32,
}

impl Default for DepthStencilState {
    fn default() -> Self {
        Self {
            // Match D3D9 API defaults.
            depth_enable: true,
            depth_write_enable: true,
            depth_func: 3, // LESS_EQUAL
            stencil_enable: false,
            two_sided_stencil_enable: false,
            stencil_read_mask: 0xFF,
            stencil_write_mask: 0xFF,
            stencil_func: 7,          // ALWAYS
            stencil_fail_op: 0,       // KEEP
            stencil_depth_fail_op: 0, // KEEP
            stencil_pass_op: 0,       // KEEP
            // Match D3D9's defaults for CCW stencil state. When two-sided stencil mode is
            // disabled, these are ignored and the regular stencil state applies to both sides.
            ccw_stencil_func: 7,          // ALWAYS
            ccw_stencil_fail_op: 0,       // KEEP
            ccw_stencil_depth_fail_op: 0, // KEEP
            ccw_stencil_pass_op: 0,       // KEEP
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct RasterizerState {
    cull_mode: u32,
    front_ccw: bool,
    scissor_enable: bool,
    depth_bias: i32,
}

impl Default for RasterizerState {
    fn default() -> Self {
        Self {
            // D3D9 defaults to `D3DCULL_CCW` with `FRONTCOUNTERCLOCKWISE = FALSE`, meaning
            // clockwise triangles are front faces and counter-clockwise triangles are culled
            // (back-face culling).
            cull_mode: cmd::AerogpuCullMode::Back as u32,
            front_ccw: false,
            scissor_enable: false,
            depth_bias: 0,
        }
    }
}

const MAX_SAMPLERS: usize = 16;
const CONSTANTS_BUFFER_SIZE_BYTES: usize = 512 * 16;
const MAX_REASONABLE_RENDER_STATE_ID: u32 = 4096;
const MAX_REASONABLE_SAMPLER_STATE_ID: u32 = 4096;

const CLEAR_SCISSOR_WGSL: &str = r#"
struct ClearParams {
    color: vec4<f32>,
    depth: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};

@group(0) @binding(0) var<uniform> params: ClearParams;

@vertex
fn vs(@builtin(vertex_index) idx: u32) -> @builtin(position) vec4<f32> {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    let p = positions[idx];
    return vec4<f32>(p.x, p.y, 0.0, 1.0);
}

@vertex
fn vs_depth(@builtin(vertex_index) idx: u32) -> @builtin(position) vec4<f32> {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    let p = positions[idx];
    return vec4<f32>(p.x, p.y, params.depth, 1.0);
}

@fragment
fn fs() -> @location(0) vec4<f32> {
    return params.color;
}

struct DepthColorOut {
    @location(0) color: vec4<f32>,
    @builtin(frag_depth) depth: f32,
};

@fragment
fn fs_depth() -> DepthColorOut {
    var out: DepthColorOut;
    out.color = params.color;
    out.depth = params.depth;
    return out;
}
"#;
fn create_constants_buffer(device: &wgpu::Device) -> wgpu::Buffer {
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("aerogpu-d3d9.constants"),
        contents: &[0u8; CONSTANTS_BUFFER_SIZE_BYTES],
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    })
}

fn create_default_render_states() -> Vec<u32> {
    // Keep this list scoped to the render states we use for pipeline derivation. Other render
    // states are initialized to 0 until written.
    //
    // IMPORTANT: `set_render_state_u32` caches state values and early-returns when the render-state
    // value is unchanged. For states whose D3D9 defaults are non-zero, we must seed the cache with
    // those defaults so the first explicit set-to-zero call is not dropped.
    let mut states = vec![0u32; d3d9::D3DRS_BLENDOPALPHA as usize + 1];

    states[d3d9::D3DRS_COLORWRITEENABLE as usize] = 0xF;
    states[d3d9::D3DRS_COLORWRITEENABLE1 as usize] = 0xF;
    states[d3d9::D3DRS_COLORWRITEENABLE2 as usize] = 0xF;
    states[d3d9::D3DRS_COLORWRITEENABLE3 as usize] = 0xF;

    states[d3d9::D3DRS_SRCBLEND as usize] = d3d9::D3DBLEND_ONE;
    states[d3d9::D3DRS_DESTBLEND as usize] = d3d9::D3DBLEND_ZERO;
    states[d3d9::D3DRS_BLENDOP as usize] = 1; // D3DBLENDOP_ADD

    states[d3d9::D3DRS_ZENABLE as usize] = 1;
    states[d3d9::D3DRS_ZWRITEENABLE as usize] = 1;
    states[d3d9::D3DRS_ZFUNC as usize] = 4; // D3DCMP_LESSEQUAL

    states[d3d9::D3DRS_CULLMODE as usize] = d3d9::D3DCULL_CCW;

    states[d3d9::D3DRS_BLENDFACTOR as usize] = 0xFFFF_FFFF;

    states[d3d9::D3DRS_STENCILFUNC as usize] = 8; // D3DCMP_ALWAYS
    states[d3d9::D3DRS_STENCILFAIL as usize] = 1; // D3DSTENCILOP_KEEP
    states[d3d9::D3DRS_STENCILZFAIL as usize] = 1; // D3DSTENCILOP_KEEP
    states[d3d9::D3DRS_STENCILPASS as usize] = 1; // D3DSTENCILOP_KEEP
    states[d3d9::D3DRS_STENCILMASK as usize] = 0xFFFF_FFFF;
    states[d3d9::D3DRS_STENCILWRITEMASK as usize] = 0xFFFF_FFFF;

    states[d3d9::D3DRS_TWOSIDEDSTENCILMODE as usize] = 0;
    states[d3d9::D3DRS_CCW_STENCILFUNC as usize] = 8; // D3DCMP_ALWAYS
    states[d3d9::D3DRS_CCW_STENCILFAIL as usize] = 1; // D3DSTENCILOP_KEEP
    states[d3d9::D3DRS_CCW_STENCILZFAIL as usize] = 1; // D3DSTENCILOP_KEEP
    states[d3d9::D3DRS_CCW_STENCILPASS as usize] = 1; // D3DSTENCILOP_KEEP
    states[d3d9::D3DRS_ALPHAFUNC as usize] = 8; // D3DCMP_ALWAYS

    states
}

fn create_default_sampler_states_ps() -> [Vec<u32>; MAX_SAMPLERS] {
    // `set_sampler_state_u32` caches state values and early-returns when the sampler-state value
    // is unchanged. For states whose D3D9 defaults are non-zero, we must seed the cache with those
    // defaults so the first explicit set-to-zero call is not dropped.
    let mut states = vec![0u32; d3d9::D3DSAMP_SRGBTEXTURE as usize + 1];
    states[d3d9::D3DSAMP_ADDRESSU as usize] = d3d9::D3DTADDRESS_WRAP;
    states[d3d9::D3DSAMP_ADDRESSV as usize] = d3d9::D3DTADDRESS_WRAP;
    states[d3d9::D3DSAMP_ADDRESSW as usize] = d3d9::D3DTADDRESS_WRAP;
    states[d3d9::D3DSAMP_MINFILTER as usize] = d3d9::D3DTEXF_POINT;
    states[d3d9::D3DSAMP_MAGFILTER as usize] = d3d9::D3DTEXF_POINT;
    states[d3d9::D3DSAMP_MIPFILTER as usize] = d3d9::D3DTEXF_NONE;
    states[d3d9::D3DSAMP_MAXANISOTROPY as usize] = 1;
    states[d3d9::D3DSAMP_MAXMIPLEVEL as usize] = 0;
    states[d3d9::D3DSAMP_BORDERCOLOR as usize] = 0;
    states[d3d9::D3DSAMP_SRGBTEXTURE as usize] = 0;

    std::array::from_fn(|_| states.clone())
}

fn create_default_state() -> State {
    let mut state = State {
        topology_raw: cmd::AerogpuPrimitiveTopology::TriangleList as u32,
        topology: wgpu::PrimitiveTopology::TriangleList,
        blend_constant: [1.0; 4],
        sample_mask: 0xFFFF_FFFF,
        alpha_test_func: 8, // D3DCMP_ALWAYS
        ..Default::default()
    };
    state.render_states = create_default_render_states();
    state.sampler_states_vs = create_default_sampler_states_ps();
    state.sampler_states_ps = create_default_sampler_states_ps();
    state
}

fn build_alpha_test_wgsl_variant(
    base: &str,
    alpha_test_func: u32,
    alpha_test_ref: u8,
) -> Result<String, AerogpuD3d9Error> {
    const FS_SIG_WITH_INPUT: &str = "@fragment\nfn fs_main(input: PsInput) -> PsOutput {\n";
    const FS_SIG_NO_INPUT: &str = "@fragment\nfn fs_main() -> PsOutput {\n";

    let (old_sig, new_sig, wrapper_sig, call_expr) = if base.contains(FS_SIG_WITH_INPUT) {
        (
            FS_SIG_WITH_INPUT,
            "fn fs_main_inner(input: PsInput) -> PsOutput {\n",
            "fn fs_main(input: PsInput) -> PsOutput {\n",
            "fs_main_inner(input)",
        )
    } else if base.contains(FS_SIG_NO_INPUT) {
        (
            FS_SIG_NO_INPUT,
            "fn fs_main_inner() -> PsOutput {\n",
            "fn fs_main() -> PsOutput {\n",
            "fs_main_inner()",
        )
    } else {
        return Err(AerogpuD3d9Error::ShaderTranslation(
            "alpha-test WGSL injection failed: unrecognized fs_main signature".into(),
        ));
    };

    let mut out = base.replacen(old_sig, new_sig, 1);
    out.push_str("\n@fragment\n");
    out.push_str(wrapper_sig);
    out.push_str(&format!("  let out = {};\n", call_expr));
    out.push_str("  let a: f32 = clamp(out.oC0.a, 0.0, 1.0);\n");
    out.push_str(&format!(
        "  let alpha_ref: f32 = f32({}u) / 255.0;\n",
        alpha_test_ref
    ));

    let passes = match alpha_test_func {
        1 => "false",                 // D3DCMP_NEVER
        2 => "(a < alpha_ref)",        // D3DCMP_LESS
        3 => "(a == alpha_ref)",       // D3DCMP_EQUAL
        4 => "(a <= alpha_ref)",       // D3DCMP_LESSEQUAL
        5 => "(a > alpha_ref)",        // D3DCMP_GREATER
        6 => "(a != alpha_ref)",       // D3DCMP_NOTEQUAL
        7 => "(a >= alpha_ref)",       // D3DCMP_GREATEREQUAL
        8 => "true",                  // D3DCMP_ALWAYS
        _ => "true",
    };
    out.push_str(&format!(
        "  if !({}) {{\n    discard;\n  }}\n",
        passes
    ));
    out.push_str("  return out;\n}\n");
    Ok(out)
}

fn create_default_sampler(
    device: &wgpu::Device,
    downlevel_flags: wgpu::DownlevelFlags,
) -> Arc<wgpu::Sampler> {
    Arc::new(create_wgpu_sampler(
        device,
        downlevel_flags,
        &D3d9SamplerState::default(),
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct D3d9SamplerState {
    address_u: u32,
    address_v: u32,
    address_w: u32,
    border_color: u32,
    max_anisotropy: u32,
    max_mip_level: u32,
    min_filter: u32,
    mag_filter: u32,
    mip_filter: u32,
}

impl Default for D3d9SamplerState {
    fn default() -> Self {
        // Match D3D9 sampler defaults:
        // - ADDRESSU/V/W = WRAP (1)
        // - MIN/MAG = POINT (1)
        // - MIP = NONE (0)
        // - BORDERCOLOR = 0
        // - MAXANISOTROPY = 1
        // - MAXMIPLEVEL = 0
        Self {
            address_u: d3d9::D3DTADDRESS_WRAP,
            address_v: d3d9::D3DTADDRESS_WRAP,
            address_w: d3d9::D3DTADDRESS_WRAP,
            border_color: 0,
            max_anisotropy: 1,
            max_mip_level: 0,
            min_filter: d3d9::D3DTEXF_POINT,
            mag_filter: d3d9::D3DTEXF_POINT,
            mip_filter: d3d9::D3DTEXF_NONE,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PipelineCacheKey {
    vs: u64,
    ps: u64,
    alpha_test_enable: bool,
    alpha_test_func: u32,
    alpha_test_ref: u8,
    vertex_buffers: Vec<crate::pipeline_key::VertexBufferLayoutKey>,
    color_formats: Vec<Option<wgpu::TextureFormat>>,
    /// Per color attachment: true if the bound render target is an X8 format (e.g. X8R8G8B8).
    ///
    /// These formats are mapped to wgpu formats that *do* have an alpha channel (RGBA/BGRA), but
    /// D3D9 semantics require that alpha writes are ignored and alpha reads behave as opaque.
    /// This needs to be part of the cache key to avoid reusing a pipeline with a mismatched color
    /// write mask between X8 and A8 render targets of the same wgpu format.
    x8_mask: Vec<bool>,
    depth_format: Option<wgpu::TextureFormat>,
    topology: wgpu::PrimitiveTopology,
    blend: BlendState,
    depth_stencil: DepthStencilState,
    raster: RasterizerPipelineKey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct AlphaTestShaderModuleKey {
    ps: u64,
    alpha_test_func: u32,
    alpha_test_ref: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct RasterizerPipelineKey {
    cull_mode: u32,
    front_ccw: bool,
    depth_bias: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ClearDepthPipelineKey {
    format: wgpu::TextureFormat,
    write_depth: bool,
    write_stencil: bool,
}

struct ClearDummyColorTarget {
    #[allow(dead_code)]
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

impl AerogpuD3d9Executor {
    /// Create a headless executor suitable for tests.
    pub async fn new_headless() -> Result<Self, AerogpuD3d9Error> {
        // Ensure `wgpu` has somewhere to put its runtime files on Unix CI.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let needs_runtime_dir = std::env::var("XDG_RUNTIME_DIR")
                .ok()
                .map(|v| v.is_empty())
                .unwrap_or(true);

            if needs_runtime_dir {
                let dir = std::env::temp_dir().join(format!(
                    "aero-gpu-xdg-runtime-{}-d3d9-exec",
                    std::process::id()
                ));
                let _ = std::fs::create_dir_all(&dir);
                let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
                std::env::set_var("XDG_RUNTIME_DIR", &dir);
            }
        }

        // Prefer GL on Linux CI to avoid crashes in some Vulkan software adapters (seen with
        // `gpu-alloc` UB checks). The GL backend is sufficient for the headless integration tests
        // we can run on CI; tests that require higher downlevel capabilities should skip when the
        // backend does not support them.
        let backends = if cfg!(target_os = "linux") {
            wgpu::Backends::GL
        } else {
            wgpu::Backends::all()
        };
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends,
            ..Default::default()
        });
        let adapter = match instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: None,
                force_fallback_adapter: true,
            })
            .await
        {
            Some(adapter) => adapter,
            None => instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::LowPower,
                    compatible_surface: None,
                    force_fallback_adapter: false,
                })
                .await
                .ok_or(AerogpuD3d9Error::AdapterNotFound)?,
        };

        let downlevel_flags = adapter.get_downlevel_capabilities().flags;

        let required_features = crate::wgpu_features::negotiated_features(&adapter);
        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("aero-gpu AerogpuD3d9Executor"),
                    required_features,
                    required_limits: wgpu::Limits::downlevel_defaults(),
                },
                None,
            )
            .await
            .map_err(|e| AerogpuD3d9Error::RequestDevice(e.to_string()))?;

        Ok(Self::new(device, queue, downlevel_flags))
    }

    pub fn new(
        device: wgpu::Device,
        queue: wgpu::Queue,
        downlevel_flags: wgpu::DownlevelFlags,
    ) -> Self {
        // The D3D9 token-stream translator packs vertex + pixel constant registers into a single
        // uniform buffer:
        // - c[0..255]   = vertex constants
        // - c[256..511] = pixel constants
        let constants_buffer = create_constants_buffer(&device);

        // Dummy bindings for unbound textures/samplers.
        let dummy_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("aerogpu-d3d9.dummy_texture"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &dummy_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &[0xFF, 0xFF, 0xFF, 0xFF],
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(4),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        let dummy_texture_view = dummy_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let bind_group_layout = create_bind_group_layout(&device);
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("aerogpu-d3d9.pipeline_layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let clear_color_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aerogpu-d3d9.clear_params"),
            size: 32,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let clear_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("aerogpu-d3d9.clear_bind_group_layout"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(32),
                    },
                    count: None,
                }],
            });
        let clear_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("aerogpu-d3d9.clear_pipeline_layout"),
                bind_group_layouts: &[&clear_bind_group_layout],
                push_constant_ranges: &[],
            });
        let clear_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("aerogpu-d3d9.clear_bind_group"),
            layout: &clear_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: clear_color_buffer.as_entire_binding(),
            }],
        });
        let clear_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("aerogpu-d3d9.clear_shader"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(CLEAR_SCISSOR_WGSL)),
        });

        let default_sampler = create_default_sampler(&device, downlevel_flags);
        let samplers_vs = std::array::from_fn(|_| default_sampler.clone());
        let samplers_ps = std::array::from_fn(|_| default_sampler.clone());
        let sampler_state_vs = std::array::from_fn(|_| D3d9SamplerState::default());
        let sampler_state_ps = std::array::from_fn(|_| D3d9SamplerState::default());
        let mut sampler_cache = HashMap::new();
        sampler_cache.insert(D3d9SamplerState::default(), default_sampler.clone());

        Self {
            device,
            queue,
            shader_cache: shader::ShaderCache::default(),
            resources: HashMap::new(),
            resource_handles: HashMap::new(),
            resource_refcounts: HashMap::new(),
            shared_surface_by_token: HashMap::new(),
            retired_share_tokens: HashSet::new(),
            shaders: HashMap::new(),
            input_layouts: HashMap::new(),
            constants_buffer,
            dummy_texture_view,
            downlevel_flags,
            bind_group_layout,
            pipeline_layout,
            bind_group: None,
            bind_group_dirty: true,
            samplers_vs,
            sampler_state_vs,
            samplers_ps,
            sampler_state_ps,
            sampler_cache,
            pipelines: HashMap::new(),
            alpha_test_pixel_shaders: HashMap::new(),
            clear_shader,
            clear_bind_group,
            clear_pipeline_layout,
            clear_color_buffer,
            clear_pipelines: HashMap::new(),
            clear_depth_pipelines: HashMap::new(),
            clear_dummy_color_targets: HashMap::new(),
            presented_scanouts: HashMap::new(),
            triangle_fan_index_buffers: HashMap::new(),
            contexts: HashMap::new(),
            current_context_id: 0,
            state: create_default_state(),
            encoder: None,
        }
    }

    pub fn reset(&mut self) {
        self.shader_cache = shader::ShaderCache::default();
        self.resources.clear();
        self.resource_handles.clear();
        self.resource_refcounts.clear();
        self.shared_surface_by_token.clear();
        self.retired_share_tokens.clear();
        self.shaders.clear();
        self.input_layouts.clear();
        self.presented_scanouts.clear();
        self.pipelines.clear();
        self.alpha_test_pixel_shaders.clear();
        self.clear_pipelines.clear();
        self.clear_depth_pipelines.clear();
        self.clear_dummy_color_targets.clear();
        self.triangle_fan_index_buffers.clear();
        self.contexts.clear();
        self.current_context_id = 0;
        self.bind_group = None;
        self.bind_group_dirty = true;
        self.sampler_state_vs = std::array::from_fn(|_| D3d9SamplerState::default());
        self.sampler_state_ps = std::array::from_fn(|_| D3d9SamplerState::default());
        self.sampler_cache.clear();
        let default_sampler = create_default_sampler(&self.device, self.downlevel_flags);
        self.sampler_cache
            .insert(D3d9SamplerState::default(), default_sampler.clone());
        self.samplers_vs = std::array::from_fn(|_| default_sampler.clone());
        self.samplers_ps = std::array::from_fn(|_| default_sampler.clone());
        self.state = create_default_state();
        self.encoder = None;

        // Avoid leaking constants across resets; the next draw will rewrite what it needs.
        self.queue.write_buffer(
            &self.constants_buffer,
            0,
            &[0u8; CONSTANTS_BUFFER_SIZE_BYTES],
        );
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    pub fn downlevel_flags(&self) -> wgpu::DownlevelFlags {
        self.downlevel_flags
    }

    pub fn supports_view_formats(&self) -> bool {
        self.downlevel_flags
            .contains(wgpu::DownlevelFlags::VIEW_FORMATS)
    }

    pub fn supports_depth_texture_and_buffer_copies(&self) -> bool {
        self.downlevel_flags
            .contains(wgpu::DownlevelFlags::DEPTH_TEXTURE_AND_BUFFER_COPIES)
    }

    pub fn poll(&self) {
        #[cfg(not(target_arch = "wasm32"))]
        self.device.poll(wgpu::Maintain::Wait);

        #[cfg(target_arch = "wasm32")]
        self.device.poll(wgpu::Maintain::Poll);
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn readback_buffer_bytes(&self, buffer: &wgpu::Buffer) -> Result<Vec<u8>, AerogpuD3d9Error> {
        let slice = buffer.slice(..);
        let (sender, receiver) = mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |res| {
            let _ = sender.send(res);
        });
        self.poll();
        receiver
            .recv()
            .map_err(|_| AerogpuD3d9Error::Validation("map_async sender dropped".into()))?
            .map_err(|err| AerogpuD3d9Error::Validation(format!("map_async failed: {err:?}")))?;

        let mapped = slice.get_mapped_range();
        let out = mapped.to_vec();
        drop(mapped);
        buffer.unmap();
        Ok(out)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn flush_pending_writebacks_blocking(
        &self,
        pending: Vec<PendingWriteback>,
        guest_memory: &mut dyn GuestMemory,
    ) -> Result<(), AerogpuD3d9Error> {
        for writeback in pending {
            match writeback {
                PendingWriteback::Buffer {
                    staging,
                    dst_gpa,
                    size_bytes,
                } => {
                    let bytes = self.readback_buffer_bytes(&staging)?;
                    let len: usize = size_bytes.try_into().map_err(|_| {
                        AerogpuD3d9Error::Validation("buffer writeback size out of range".into())
                    })?;
                    let slice = bytes.get(..len).ok_or_else(|| {
                        AerogpuD3d9Error::Validation("buffer writeback size mismatch".into())
                    })?;
                    guest_memory.write(dst_gpa, slice)?;
                }
                PendingWriteback::Texture2d { staging, plan } => {
                    let mut bytes = self.readback_buffer_bytes(&staging)?;
                    if plan.is_x8 && plan.host_bytes_per_pixel == 4 {
                        for row in 0..plan.height as usize {
                            let start = row * plan.padded_bytes_per_row as usize;
                            let end = start + plan.host_unpadded_bytes_per_row as usize;
                            let row_bytes = bytes.get_mut(start..end).ok_or_else(|| {
                                AerogpuD3d9Error::Validation(
                                    "texture writeback staging out of bounds".into(),
                                )
                            })?;
                            force_opaque_alpha_rgba8(row_bytes);
                        }
                    }
                    let row_pitch = plan.dst_subresource_row_pitch_bytes as u64;
                    let dst_x_bytes = (plan.dst_x as u64)
                        .checked_mul(plan.guest_bytes_per_pixel as u64)
                        .ok_or_else(|| {
                            AerogpuD3d9Error::Validation("texture writeback dst_x overflow".into())
                        })?;
                    for row in 0..plan.height {
                        let src_start = row as usize * plan.padded_bytes_per_row as usize;
                        let src_end = src_start + plan.host_unpadded_bytes_per_row as usize;
                        let row_bytes = bytes.get(src_start..src_end).ok_or_else(|| {
                            AerogpuD3d9Error::Validation(
                                "texture writeback staging out of bounds".into(),
                            )
                        })?;
                        let mut packed: Vec<u8> = Vec::new();
                        let row_bytes = match plan.format_raw {
                            x if x == AerogpuFormat::B5G6R5Unorm as u32 => {
                                packed.resize(plan.guest_unpadded_bytes_per_row as usize, 0);
                                pack_rgba8_to_b5g6r5_unorm(row_bytes, &mut packed);
                                packed.as_slice()
                            }
                            x if x == AerogpuFormat::B5G5R5A1Unorm as u32 => {
                                packed.resize(plan.guest_unpadded_bytes_per_row as usize, 0);
                                pack_rgba8_to_b5g5r5a1_unorm(row_bytes, &mut packed);
                                packed.as_slice()
                            }
                            _ => {
                                if row_bytes.len() != plan.guest_unpadded_bytes_per_row as usize {
                                    return Err(AerogpuD3d9Error::Validation(
                                        "texture writeback row size mismatch".into(),
                                    ));
                                }
                                row_bytes
                            }
                        };
                        let row_index = plan.dst_y.checked_add(row).ok_or_else(|| {
                            AerogpuD3d9Error::Validation("texture writeback dst_y overflow".into())
                        })?;
                        let row_off =
                            (row_index as u64).checked_mul(row_pitch).ok_or_else(|| {
                                AerogpuD3d9Error::Validation(
                                    "texture writeback row offset overflow".into(),
                                )
                            })?;
                        let dst_off = plan
                            .dst_subresource_offset_bytes
                            .checked_add(row_off)
                            .and_then(|v| v.checked_add(dst_x_bytes))
                            .ok_or_else(|| {
                            AerogpuD3d9Error::Validation(
                                "texture writeback backing overflow".into(),
                            )
                        })?;
                        let dst_end =
                            dst_off.checked_add(row_bytes.len() as u64).ok_or_else(|| {
                                AerogpuD3d9Error::Validation(
                                    "texture writeback backing overflow".into(),
                                )
                            })?;
                        if dst_end > plan.backing.size_bytes {
                            return Err(AerogpuD3d9Error::Validation(format!(
                                "texture writeback backing out of bounds (mip_level={} array_layer={} end=0x{:x} size=0x{:x})",
                                plan.dst_mip_level,
                                plan.dst_array_layer,
                                dst_end,
                                plan.backing.size_bytes
                            )));
                        }

                        let dst_gpa = plan.backing_gpa.checked_add(dst_off).ok_or_else(|| {
                            AerogpuD3d9Error::Validation("texture writeback GPA overflow".into())
                        })?;
                        if dst_gpa.checked_add(row_bytes.len() as u64).is_none() {
                            return Err(AerogpuD3d9Error::Validation(
                                "texture writeback GPA overflow".into(),
                            ));
                        }
                        guest_memory.write(dst_gpa, row_bytes)?;
                    }
                }
            }
        }
        Ok(())
    }

    async fn flush_pending_writebacks_async(
        &self,
        pending: Vec<PendingWriteback>,
        guest_memory: &mut dyn GuestMemory,
    ) -> Result<(), AerogpuD3d9Error> {
        for writeback in pending {
            match writeback {
                PendingWriteback::Buffer {
                    staging,
                    dst_gpa,
                    size_bytes,
                } => {
                    let bytes = self.readback_buffer_bytes_async(&staging).await?;
                    let len: usize = size_bytes.try_into().map_err(|_| {
                        AerogpuD3d9Error::Validation("buffer writeback size out of range".into())
                    })?;
                    let slice = bytes.get(..len).ok_or_else(|| {
                        AerogpuD3d9Error::Validation("buffer writeback size mismatch".into())
                    })?;
                    guest_memory.write(dst_gpa, slice)?;
                }
                PendingWriteback::Texture2d { staging, plan } => {
                    let mut bytes = self.readback_buffer_bytes_async(&staging).await?;
                    if plan.is_x8 && plan.host_bytes_per_pixel == 4 {
                        for row in 0..plan.height as usize {
                            let start = row * plan.padded_bytes_per_row as usize;
                            let end = start + plan.host_unpadded_bytes_per_row as usize;
                            let row_bytes = bytes.get_mut(start..end).ok_or_else(|| {
                                AerogpuD3d9Error::Validation(
                                    "texture writeback staging out of bounds".into(),
                                )
                            })?;
                            force_opaque_alpha_rgba8(row_bytes);
                        }
                    }
                    let row_pitch = plan.dst_subresource_row_pitch_bytes as u64;
                    let dst_x_bytes = (plan.dst_x as u64)
                        .checked_mul(plan.guest_bytes_per_pixel as u64)
                        .ok_or_else(|| {
                            AerogpuD3d9Error::Validation("texture writeback dst_x overflow".into())
                        })?;
                    for row in 0..plan.height {
                        let src_start = row as usize * plan.padded_bytes_per_row as usize;
                        let src_end = src_start + plan.host_unpadded_bytes_per_row as usize;
                        let row_bytes = bytes.get(src_start..src_end).ok_or_else(|| {
                            AerogpuD3d9Error::Validation(
                                "texture writeback staging out of bounds".into(),
                            )
                        })?;
                        let mut packed: Vec<u8> = Vec::new();
                        let row_bytes = match plan.format_raw {
                            x if x == AerogpuFormat::B5G6R5Unorm as u32 => {
                                packed.resize(plan.guest_unpadded_bytes_per_row as usize, 0);
                                pack_rgba8_to_b5g6r5_unorm(row_bytes, &mut packed);
                                packed.as_slice()
                            }
                            x if x == AerogpuFormat::B5G5R5A1Unorm as u32 => {
                                packed.resize(plan.guest_unpadded_bytes_per_row as usize, 0);
                                pack_rgba8_to_b5g5r5a1_unorm(row_bytes, &mut packed);
                                packed.as_slice()
                            }
                            _ => {
                                if row_bytes.len() != plan.guest_unpadded_bytes_per_row as usize {
                                    return Err(AerogpuD3d9Error::Validation(
                                        "texture writeback row size mismatch".into(),
                                    ));
                                }
                                row_bytes
                            }
                        };
                        let row_index = plan.dst_y.checked_add(row).ok_or_else(|| {
                            AerogpuD3d9Error::Validation("texture writeback dst_y overflow".into())
                        })?;
                        let row_off =
                            (row_index as u64).checked_mul(row_pitch).ok_or_else(|| {
                                AerogpuD3d9Error::Validation(
                                    "texture writeback row offset overflow".into(),
                                )
                            })?;
                        let dst_off = plan
                            .dst_subresource_offset_bytes
                            .checked_add(row_off)
                            .and_then(|v| v.checked_add(dst_x_bytes))
                            .ok_or_else(|| {
                            AerogpuD3d9Error::Validation(
                                "texture writeback backing overflow".into(),
                            )
                        })?;
                        let dst_end =
                            dst_off.checked_add(row_bytes.len() as u64).ok_or_else(|| {
                                AerogpuD3d9Error::Validation(
                                    "texture writeback backing overflow".into(),
                                )
                            })?;
                        if dst_end > plan.backing.size_bytes {
                            return Err(AerogpuD3d9Error::Validation(format!(
                                "texture writeback backing out of bounds (mip_level={} array_layer={} end=0x{:x} size=0x{:x})",
                                plan.dst_mip_level,
                                plan.dst_array_layer,
                                dst_end,
                                plan.backing.size_bytes
                            )));
                        }

                        let dst_gpa = plan.backing_gpa.checked_add(dst_off).ok_or_else(|| {
                            AerogpuD3d9Error::Validation("texture writeback GPA overflow".into())
                        })?;
                        if dst_gpa.checked_add(row_bytes.len() as u64).is_none() {
                            return Err(AerogpuD3d9Error::Validation(
                                "texture writeback GPA overflow".into(),
                            ));
                        }
                        guest_memory.write(dst_gpa, row_bytes)?;
                    }
                }
            }
        }
        Ok(())
    }

    async fn readback_buffer_bytes_async(
        &self,
        buffer: &wgpu::Buffer,
    ) -> Result<Vec<u8>, AerogpuD3d9Error> {
        let slice = buffer.slice(..);
        let (sender, receiver) = oneshot_channel();
        slice.map_async(wgpu::MapMode::Read, move |res| {
            sender.send(res).ok();
        });
        self.poll();

        receiver
            .receive()
            .await
            .ok_or_else(|| AerogpuD3d9Error::Validation("map_async sender dropped".into()))?
            .map_err(|err| AerogpuD3d9Error::Validation(format!("map_async failed: {err:?}")))?;

        let mapped = slice.get_mapped_range();
        let out = mapped.to_vec();
        drop(mapped);
        buffer.unmap();
        Ok(out)
    }

    pub fn presented_scanout(&self, scanout_id: u32) -> Option<PresentedScanout<'_>> {
        let &underlying = self.presented_scanouts.get(&scanout_id)?;
        match self.resources.get(&underlying)? {
            Resource::Texture2d {
                view,
                format,
                width,
                height,
                ..
            } => Some(PresentedScanout {
                view,
                format: *format,
                width: *width,
                height: *height,
            }),
            _ => None,
        }
    }

    pub fn execute_cmd_stream(&mut self, bytes: &[u8]) -> Result<(), AerogpuD3d9Error> {
        self.execute_cmd_stream_for_context(0, bytes)
    }

    pub fn execute_cmd_stream_for_context(
        &mut self,
        context_id: u32,
        bytes: &[u8],
    ) -> Result<(), AerogpuD3d9Error> {
        self.execute_cmd_stream_with_ctx(
            context_id,
            bytes,
            SubmissionCtx {
                guest_memory: None,
                alloc_table: None,
            },
        )
    }

    pub fn execute_cmd_stream_with_guest_memory(
        &mut self,
        bytes: &[u8],
        guest_memory: &mut dyn GuestMemory,
        alloc_table: Option<&AllocTable>,
    ) -> Result<(), AerogpuD3d9Error> {
        self.execute_cmd_stream_with_guest_memory_for_context(0, bytes, guest_memory, alloc_table)
    }

    pub fn execute_cmd_stream_with_guest_memory_for_context(
        &mut self,
        context_id: u32,
        bytes: &[u8],
        guest_memory: &mut dyn GuestMemory,
        alloc_table: Option<&AllocTable>,
    ) -> Result<(), AerogpuD3d9Error> {
        self.execute_cmd_stream_with_ctx(
            context_id,
            bytes,
            SubmissionCtx {
                guest_memory: Some(guest_memory),
                alloc_table,
            },
        )
    }

    pub async fn execute_cmd_stream_with_guest_memory_async(
        &mut self,
        bytes: &[u8],
        guest_memory: &mut dyn GuestMemory,
        alloc_table: Option<&AllocTable>,
    ) -> Result<(), AerogpuD3d9Error> {
        self.execute_cmd_stream_with_guest_memory_for_context_async(
            0,
            bytes,
            guest_memory,
            alloc_table,
        )
        .await
    }

    /// WASM-friendly async variant of `execute_cmd_stream_with_guest_memory_for_context`.
    ///
    /// On WASM targets, `wgpu::Buffer::map_async` completion is delivered via the JS event loop,
    /// so synchronous waiting would deadlock. This method awaits writeback staging buffer maps
    /// when `AEROGPU_COPY_FLAG_WRITEBACK_DST` is used.
    pub async fn execute_cmd_stream_with_guest_memory_for_context_async(
        &mut self,
        context_id: u32,
        bytes: &[u8],
        guest_memory: &mut dyn GuestMemory,
        alloc_table: Option<&AllocTable>,
    ) -> Result<(), AerogpuD3d9Error> {
        let stream = parse_cmd_stream(bytes)?;
        self.switch_context(context_id);
        let mut pending_writebacks = Vec::new();
        let mut ctx = SubmissionCtx {
            guest_memory: Some(guest_memory),
            alloc_table,
        };
        for cmd in stream.cmds {
            if let Err(err) = self.execute_cmd(cmd, &mut ctx, &mut pending_writebacks) {
                self.encoder = None;
                self.queue.submit([]);
                return Err(err);
            }
        }
        self.flush()?;
        if !pending_writebacks.is_empty() {
            let guest_memory = ctx
                .guest_memory
                .take()
                .expect("ctx always contains guest memory for async execution");
            self.flush_pending_writebacks_async(pending_writebacks, guest_memory)
                .await?;
        }
        Ok(())
    }

    fn switch_context(&mut self, context_id: u32) {
        if self.current_context_id == context_id {
            return;
        }

        let mut next = if let Some(ctx) = self.contexts.remove(&context_id) {
            ctx
        } else {
            let default_sampler = self.sampler_for_state(D3d9SamplerState::default());
            ContextState::new(&self.device, default_sampler)
        };

        std::mem::swap(&mut self.constants_buffer, &mut next.constants_buffer);
        std::mem::swap(&mut self.bind_group, &mut next.bind_group);
        std::mem::swap(&mut self.bind_group_dirty, &mut next.bind_group_dirty);
        std::mem::swap(&mut self.samplers_vs, &mut next.samplers_vs);
        std::mem::swap(&mut self.sampler_state_vs, &mut next.sampler_state_vs);
        std::mem::swap(&mut self.samplers_ps, &mut next.samplers_ps);
        std::mem::swap(&mut self.sampler_state_ps, &mut next.sampler_state_ps);
        std::mem::swap(&mut self.state, &mut next.state);

        let old_context = self.current_context_id;
        self.current_context_id = context_id;
        self.contexts.insert(old_context, next);
    }

    fn execute_cmd_stream_with_ctx(
        &mut self,
        context_id: u32,
        bytes: &[u8],
        mut ctx: SubmissionCtx<'_>,
    ) -> Result<(), AerogpuD3d9Error> {
        let stream = parse_cmd_stream(bytes)?;

        // Avoid partially executing streams on wasm when `WRITEBACK_DST` is present. The writeback
        // requires `wgpu::Buffer::map_async` completion, which is delivered via the JS event loop
        // and cannot be waited on synchronously.
        #[cfg(target_arch = "wasm32")]
        {
            let writeback_at = stream.cmds.iter().position(|cmd| match cmd {
                AeroGpuCmd::CopyBuffer { flags, .. } | AeroGpuCmd::CopyTexture2d { flags, .. } => {
                    (flags & AEROGPU_COPY_FLAG_WRITEBACK_DST) != 0
                }
                _ => false,
            });
            if let Some(at) = writeback_at {
                return Err(AerogpuD3d9Error::Validation(
                    format!(
                        "WRITEBACK_DST requires async execution on wasm (call execute_cmd_stream_with_guest_memory_for_context_async); first WRITEBACK_DST at packet {at}"
                    ),
                ));
            }
        }

        self.switch_context(context_id);
        let mut pending_writebacks = Vec::new();
        for cmd in stream.cmds {
            if let Err(err) = self.execute_cmd(cmd, &mut ctx, &mut pending_writebacks) {
                // Do not submit partially-recorded work; drop the encoder but still push an empty
                // submit boundary so `queue.write_texture` calls don't stay queued indefinitely.
                self.encoder = None;
                self.queue.submit([]);
                return Err(err);
            }
        }
        // Make sure we don't keep uploads queued indefinitely if the guest forgets to present.
        self.flush()?;

        if pending_writebacks.is_empty() {
            return Ok(());
        }

        #[cfg(target_arch = "wasm32")]
        {
            return Err(AerogpuD3d9Error::Validation(
                "WRITEBACK_DST requires async execution on wasm (call execute_cmd_stream_with_guest_memory_for_context_async)"
                    .into(),
            ));
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            let guest_memory = ctx.guest_memory.ok_or_else(|| {
                AerogpuD3d9Error::Validation("WRITEBACK_DST requires guest memory".into())
            })?;
            self.flush_pending_writebacks_blocking(pending_writebacks, guest_memory)
        }
    }

    pub async fn read_presented_scanout_rgba8(
        &self,
        scanout_id: u32,
    ) -> Result<Option<(u32, u32, Vec<u8>)>, AerogpuD3d9Error> {
        let Some(&underlying) = self.presented_scanouts.get(&scanout_id) else {
            return Ok(None);
        };
        let (w, h, rgba8) = self.readback_texture_rgba8_underlying(underlying).await?;
        Ok(Some((w, h, rgba8)))
    }

    pub async fn readback_texture_rgba8(
        &self,
        texture_handle: u32,
    ) -> Result<(u32, u32, Vec<u8>), AerogpuD3d9Error> {
        let underlying = self.resolve_resource_handle(texture_handle)?;
        self.readback_texture_rgba8_underlying(underlying).await
    }

    pub async fn readback_texture_stencil8(
        &self,
        texture_handle: u32,
    ) -> Result<(u32, u32, Vec<u8>), AerogpuD3d9Error> {
        let underlying = self.resolve_resource_handle(texture_handle)?;
        self.readback_texture_stencil8_underlying(underlying).await
    }

    pub async fn readback_texture_depth32f(
        &self,
        texture_handle: u32,
    ) -> Result<(u32, u32, Vec<f32>), AerogpuD3d9Error> {
        let underlying = self.resolve_resource_handle(texture_handle)?;
        self.readback_texture_depth32f_underlying(underlying).await
    }

    async fn readback_texture_rgba8_underlying(
        &self,
        texture_handle: u32,
    ) -> Result<(u32, u32, Vec<u8>), AerogpuD3d9Error> {
        let res = self
            .resources
            .get(&texture_handle)
            .ok_or(AerogpuD3d9Error::UnknownResource(texture_handle))?;
        let (texture, format, width, height) = match res {
            Resource::Texture2d {
                texture,
                format,
                width,
                height,
                ..
            } => (texture, *format, *width, *height),
            _ => return Err(AerogpuD3d9Error::ReadbackUnsupported(texture_handle)),
        };

        let bytes = readback_rgba8(
            &self.device,
            &self.queue,
            texture,
            TextureRegion {
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
            },
        )
        .await;

        let out = match format {
            wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Rgba8UnormSrgb => bytes,
            wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb => {
                let mut rgba = bytes;
                for px in rgba.chunks_exact_mut(4) {
                    px.swap(0, 2);
                }
                rgba
            }
            _ => return Err(AerogpuD3d9Error::ReadbackUnsupported(texture_handle)),
        };

        Ok((width, height, out))
    }

    async fn readback_texture_stencil8_underlying(
        &self,
        texture_handle: u32,
    ) -> Result<(u32, u32, Vec<u8>), AerogpuD3d9Error> {
        let res = self
            .resources
            .get(&texture_handle)
            .ok_or(AerogpuD3d9Error::UnknownResource(texture_handle))?;
        let (texture, format, width, height) = match res {
            Resource::Texture2d {
                texture,
                format,
                width,
                height,
                ..
            } => (texture, *format, *width, *height),
            _ => return Err(AerogpuD3d9Error::ReadbackStencilUnsupported(texture_handle)),
        };

        if format != wgpu::TextureFormat::Depth24PlusStencil8 {
            return Err(AerogpuD3d9Error::ReadbackStencilUnsupported(texture_handle));
        }

        if !self
            .downlevel_flags
            .contains(wgpu::DownlevelFlags::DEPTH_TEXTURE_AND_BUFFER_COPIES)
        {
            return Err(AerogpuD3d9Error::Validation(
                "stencil readback requires DownlevelFlags::DEPTH_TEXTURE_AND_BUFFER_COPIES".into(),
            ));
        }

        let bytes = readback_stencil8(
            &self.device,
            &self.queue,
            texture,
            TextureRegion {
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
            },
        )
        .await;

        Ok((width, height, bytes))
    }

    async fn readback_texture_depth32f_underlying(
        &self,
        texture_handle: u32,
    ) -> Result<(u32, u32, Vec<f32>), AerogpuD3d9Error> {
        let res = self
            .resources
            .get(&texture_handle)
            .ok_or(AerogpuD3d9Error::UnknownResource(texture_handle))?;
        let (texture, format, width, height) = match res {
            Resource::Texture2d {
                texture,
                format,
                width,
                height,
                ..
            } => (texture, *format, *width, *height),
            _ => return Err(AerogpuD3d9Error::ReadbackDepthUnsupported(texture_handle)),
        };

        if format != wgpu::TextureFormat::Depth32Float {
            return Err(AerogpuD3d9Error::ReadbackDepthUnsupported(texture_handle));
        }

        if !self
            .downlevel_flags
            .contains(wgpu::DownlevelFlags::DEPTH_TEXTURE_AND_BUFFER_COPIES)
        {
            return Err(AerogpuD3d9Error::Validation(
                "depth readback requires DownlevelFlags::DEPTH_TEXTURE_AND_BUFFER_COPIES".into(),
            ));
        }

        let depth = readback_depth32f(
            &self.device,
            &self.queue,
            texture,
            TextureRegion {
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
            },
        )
        .await;

        Ok((width, height, depth))
    }

    fn resolve_resource_handle(&self, handle: u32) -> Result<u32, AerogpuD3d9Error> {
        if handle == 0 {
            return Ok(0);
        }
        self.resource_handles
            .get(&handle)
            .copied()
            .ok_or(AerogpuD3d9Error::UnknownResource(handle))
    }

    fn handle_in_use(&self, handle: u32) -> bool {
        self.resource_handles.contains_key(&handle)
            || self.resources.contains_key(&handle)
            || self.shaders.contains_key(&handle)
            || self.input_layouts.contains_key(&handle)
    }

    fn register_resource_handle(&mut self, handle: u32) {
        if handle == 0 {
            return;
        }
        if self.resource_handles.contains_key(&handle) {
            return;
        }
        self.resource_handles.insert(handle, handle);
        *self.resource_refcounts.entry(handle).or_insert(0) += 1;
    }

    fn invalidate_bind_groups(&mut self) {
        self.bind_group = None;
        self.bind_group_dirty = true;
        for ctx in self.contexts.values_mut() {
            ctx.bind_group = None;
            ctx.bind_group_dirty = true;
        }
    }

    fn destroy_resource_handle(&mut self, handle: u32) {
        if handle == 0 {
            return;
        }

        let Some(underlying) = self.resource_handles.remove(&handle) else {
            return;
        };

        // Texture bindings (and therefore bind groups) may reference the destroyed handle. Drop
        // cached bind groups so subsequent draws re-resolve handles against the updated table.
        self.invalidate_bind_groups();

        let Some(count) = self.resource_refcounts.get_mut(&underlying) else {
            return;
        };
        *count = count.saturating_sub(1);
        if *count != 0 {
            return;
        }

        self.resource_refcounts.remove(&underlying);
        self.resources.remove(&underlying);
        let to_retire: Vec<u64> = self
            .shared_surface_by_token
            .iter()
            .filter_map(|(k, v)| (*v == underlying).then_some(*k))
            .collect();
        for token in to_retire {
            self.shared_surface_by_token.remove(&token);
            self.retired_share_tokens.insert(token);
        }
        self.presented_scanouts.retain(|_, v| *v != underlying);
    }

    fn release_shared_surface_token(&mut self, share_token: u64) {
        // KMD-emitted "share token is no longer importable" signal.
        //
        // Existing imported aliases remain valid and keep the underlying resource alive. We only
        // remove the token mapping so future imports fail deterministically.
        if share_token == 0 {
            return;
        }
        self.shared_surface_by_token.remove(&share_token);
        self.retired_share_tokens.insert(share_token);
    }

    fn execute_cmd(
        &mut self,
        cmd: AeroGpuCmd<'_>,
        ctx: &mut SubmissionCtx<'_>,
        pending_writebacks: &mut Vec<PendingWriteback>,
    ) -> Result<(), AerogpuD3d9Error> {
        match cmd {
            AeroGpuCmd::Nop
            | AeroGpuCmd::DebugMarker { .. }
            | AeroGpuCmd::CreateSampler { .. }
            | AeroGpuCmd::DestroySampler { .. }
            | AeroGpuCmd::SetSamplers { .. }
            | AeroGpuCmd::SetConstantBuffers { .. }
            | AeroGpuCmd::Unknown { .. } => Ok(()),
            AeroGpuCmd::CreateBuffer {
                buffer_handle,
                usage_flags,
                size_bytes,
                backing_alloc_id,
                backing_offset_bytes,
                ..
            } => {
                if buffer_handle == 0 {
                    return Err(AerogpuD3d9Error::Validation(
                        "CREATE_BUFFER: resource handle 0 is reserved".into(),
                    ));
                }
                if self.shaders.contains_key(&buffer_handle)
                    || self.input_layouts.contains_key(&buffer_handle)
                {
                    return Err(AerogpuD3d9Error::ResourceHandleInUse(buffer_handle));
                }
                // Underlying handles remain reserved as long as any aliases still reference them.
                // If the original handle was destroyed, reject reusing it until the underlying
                // resource is fully released.
                if !self.resource_handles.contains_key(&buffer_handle)
                    && self.resource_refcounts.contains_key(&buffer_handle)
                {
                    return Err(AerogpuD3d9Error::ResourceHandleInUse(buffer_handle));
                }

                if size_bytes == 0 {
                    return Err(AerogpuD3d9Error::Validation(
                        "CREATE_BUFFER: size_bytes must be > 0".into(),
                    ));
                }
                if !size_bytes.is_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT) {
                    return Err(AerogpuD3d9Error::Validation(format!(
                        "CREATE_BUFFER: size_bytes must be a multiple of {} (got {size_bytes})",
                        wgpu::COPY_BUFFER_ALIGNMENT
                    )));
                }

                let backing = if backing_alloc_id == 0 {
                    None
                } else {
                    let entry = ctx.require_alloc_entry(backing_alloc_id)?;
                    let backing_offset = backing_offset_bytes as u64;
                    let required = backing_offset.checked_add(size_bytes).ok_or_else(|| {
                        AerogpuD3d9Error::Validation("buffer backing overflow".into())
                    })?;
                    if required > entry.size_bytes {
                        return Err(AerogpuD3d9Error::Validation(format!(
                            "buffer backing out of bounds (alloc_id={backing_alloc_id} offset={backing_offset_bytes} size={size_bytes} alloc_size={})",
                            entry.size_bytes
                        )));
                    }
                    let _base_gpa = entry.gpa.checked_add(backing_offset).ok_or_else(|| {
                        AerogpuD3d9Error::Validation("buffer backing gpa overflow".into())
                    })?;
                    Some(GuestBufferBacking {
                        alloc_id: backing_alloc_id,
                        alloc_offset_bytes: backing_offset,
                    })
                };

                if self.resource_handles.contains_key(&buffer_handle) {
                    let underlying = self.resolve_resource_handle(buffer_handle)?;
                    let Some(res) = self.resources.get_mut(&underlying) else {
                        return Err(AerogpuD3d9Error::UnknownResource(buffer_handle));
                    };
                    match res {
                        Resource::Buffer {
                            size,
                            usage_flags: existing_usage_flags,
                            backing: existing_backing,
                            ..
                        } => {
                            if *size != size_bytes || *existing_usage_flags != usage_flags {
                                return Err(AerogpuD3d9Error::Validation(format!(
                                    "CREATE_* for existing handle {buffer_handle} has mismatched immutable properties; destroy and recreate the handle"
                                )));
                            }
                            *existing_backing = backing;
                            Ok(())
                        }
                        Resource::Texture2d { .. } => Err(AerogpuD3d9Error::Validation(format!(
                            "CREATE_BUFFER: handle {buffer_handle} is already bound to a texture"
                        ))),
                    }
                } else {
                    let mut buffer_usage = wgpu::BufferUsages::COPY_DST
                        | wgpu::BufferUsages::COPY_SRC
                        | wgpu::BufferUsages::VERTEX
                        | wgpu::BufferUsages::INDEX;
                    if (usage_flags & cmd::AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER) != 0 {
                        buffer_usage |= wgpu::BufferUsages::UNIFORM;
                    }

                    let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("aerogpu-d3d9.buffer"),
                        size: size_bytes,
                        usage: buffer_usage,
                        mapped_at_creation: false,
                    });
                    let shadow_len = usize::try_from(size_bytes).map_err(|_| {
                        AerogpuD3d9Error::Validation(format!(
                            "CREATE_BUFFER: size_bytes is too large for host shadow copy (size_bytes={size_bytes})"
                        ))
                    })?;
                    self.resources.insert(
                        buffer_handle,
                        Resource::Buffer {
                            buffer,
                            size: size_bytes,
                            usage_flags,
                            backing,
                            dirty_ranges: Vec::new(),
                            shadow: vec![0u8; shadow_len],
                        },
                    );
                    self.register_resource_handle(buffer_handle);
                    Ok(())
                }
            }
            AeroGpuCmd::CreateTexture2d {
                texture_handle,
                usage_flags,
                format: format_raw,
                width,
                height,
                mip_levels,
                array_layers,
                row_pitch_bytes,
                backing_alloc_id,
                backing_offset_bytes,
                ..
            } => {
                if texture_handle == 0 {
                    return Err(AerogpuD3d9Error::Validation(
                        "CREATE_TEXTURE2D: resource handle 0 is reserved".into(),
                    ));
                }
                if self.shaders.contains_key(&texture_handle)
                    || self.input_layouts.contains_key(&texture_handle)
                {
                    return Err(AerogpuD3d9Error::ResourceHandleInUse(texture_handle));
                }
                if !self.resource_handles.contains_key(&texture_handle)
                    && self.resource_refcounts.contains_key(&texture_handle)
                {
                    return Err(AerogpuD3d9Error::ResourceHandleInUse(texture_handle));
                }
                if width == 0 || height == 0 {
                    return Err(AerogpuD3d9Error::Validation(
                        "CREATE_TEXTURE2D: width/height must be non-zero".into(),
                    ));
                }
                if mip_levels == 0 || array_layers == 0 {
                    return Err(AerogpuD3d9Error::Validation(
                        "CREATE_TEXTURE2D: mip_levels/array_layers must be >= 1".into(),
                    ));
                }
                let mapped_format = map_aerogpu_format(format_raw)?;
                let format = match mapped_format {
                    // Allow BC formats to fall back to CPU decompression + RGBA8 uploads when the
                    // device can't sample BC textures (e.g. wgpu GL/WebGL2 paths).
                    wgpu::TextureFormat::Bc1RgbaUnorm
                    | wgpu::TextureFormat::Bc2RgbaUnorm
                    | wgpu::TextureFormat::Bc3RgbaUnorm
                    | wgpu::TextureFormat::Bc7RgbaUnorm
                        if !self
                            .device
                            .features()
                            .contains(wgpu::Features::TEXTURE_COMPRESSION_BC) =>
                    {
                        wgpu::TextureFormat::Rgba8Unorm
                    }
                    other => other,
                };
                let mip_level_count = mip_levels;
                let backing = if backing_alloc_id == 0 {
                    None
                } else {
                    if row_pitch_bytes == 0 {
                        return Err(AerogpuD3d9Error::Validation(
                            "CREATE_TEXTURE2D: row_pitch_bytes must be non-zero when backing_alloc_id != 0"
                                .into(),
                        ));
                    }
                    let entry = ctx.require_alloc_entry(backing_alloc_id)?;
                    let layout = guest_texture_linear_layout(
                        format_raw,
                        width,
                        height,
                        mip_level_count,
                        array_layers,
                        row_pitch_bytes,
                    )?;
                    let required = layout.total_size_bytes;
                    let backing_offset = backing_offset_bytes as u64;
                    let required_end = backing_offset.checked_add(required).ok_or_else(|| {
                        AerogpuD3d9Error::Validation(
                            "CREATE_TEXTURE2D: texture backing overflow".into(),
                        )
                    })?;
                    if required_end > entry.size_bytes {
                        return Err(AerogpuD3d9Error::Validation(format!(
                            "texture backing out of bounds (alloc_id={backing_alloc_id} offset={backing_offset_bytes} required={required} alloc_size={})",
                            entry.size_bytes
                        )));
                    }
                    let _base_gpa = entry.gpa.checked_add(backing_offset).ok_or_else(|| {
                        AerogpuD3d9Error::Validation(
                            "CREATE_TEXTURE2D: backing gpa overflow".into(),
                        )
                    })?;
                    Some(GuestTextureBacking {
                        alloc_id: backing_alloc_id,
                        alloc_offset_bytes: backing_offset,
                        row_pitch_bytes: row_pitch_bytes,
                        size_bytes: required,
                    })
                };

                if self.resource_handles.contains_key(&texture_handle) {
                    let underlying = self.resolve_resource_handle(texture_handle)?;
                    let Some(res) = self.resources.get_mut(&underlying) else {
                        return Err(AerogpuD3d9Error::UnknownResource(texture_handle));
                    };
                    match res {
                        Resource::Texture2d {
                            usage_flags: existing_usage_flags,
                            format_raw: existing_format_raw,
                            width: existing_width,
                            height: existing_height,
                            mip_level_count: existing_mip_levels,
                            array_layers: existing_layers,
                            row_pitch_bytes: existing_row_pitch_bytes,
                            backing: existing_backing,
                            ..
                        } => {
                            if *existing_usage_flags != usage_flags
                                || *existing_format_raw != format_raw
                                || *existing_width != width
                                || *existing_height != height
                                || *existing_mip_levels != mip_level_count
                                || *existing_layers != array_layers
                                || *existing_row_pitch_bytes != row_pitch_bytes
                            {
                                return Err(AerogpuD3d9Error::Validation(format!(
                                    "CREATE_* for existing handle {texture_handle} has mismatched immutable properties; destroy and recreate the handle"
                                )));
                            }
                            *existing_backing = backing;
                            Ok(())
                        }
                        Resource::Buffer { .. } => Err(AerogpuD3d9Error::Validation(format!(
                            "CREATE_TEXTURE2D: handle {texture_handle} is already bound to a buffer"
                        ))),
                    }
                } else {
                    let view_formats = if self
                        .downlevel_flags
                        .contains(wgpu::DownlevelFlags::VIEW_FORMATS)
                    {
                        match format {
                            wgpu::TextureFormat::Rgba8Unorm => {
                                vec![wgpu::TextureFormat::Rgba8UnormSrgb]
                            }
                            wgpu::TextureFormat::Bgra8Unorm => {
                                vec![wgpu::TextureFormat::Bgra8UnormSrgb]
                            }
                            wgpu::TextureFormat::Bc1RgbaUnorm => {
                                vec![wgpu::TextureFormat::Bc1RgbaUnormSrgb]
                            }
                            wgpu::TextureFormat::Bc2RgbaUnorm => {
                                vec![wgpu::TextureFormat::Bc2RgbaUnormSrgb]
                            }
                            wgpu::TextureFormat::Bc3RgbaUnorm => {
                                vec![wgpu::TextureFormat::Bc3RgbaUnormSrgb]
                            }
                            wgpu::TextureFormat::Bc7RgbaUnorm => {
                                vec![wgpu::TextureFormat::Bc7RgbaUnormSrgb]
                            }
                            _ => Vec::new(),
                        }
                    } else {
                        Vec::new()
                    };
                    let mut usage = wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC;
                    if (usage_flags & cmd::AEROGPU_RESOURCE_USAGE_TEXTURE) != 0 {
                        usage |= wgpu::TextureUsages::TEXTURE_BINDING;
                    }
                    if (usage_flags
                        & (cmd::AEROGPU_RESOURCE_USAGE_RENDER_TARGET
                            | cmd::AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL
                            | cmd::AEROGPU_RESOURCE_USAGE_SCANOUT))
                        != 0
                    {
                        usage |= wgpu::TextureUsages::RENDER_ATTACHMENT;
                    }
                    // Compressed formats can't be used as render attachments.
                    if matches!(
                        format,
                        wgpu::TextureFormat::Bc1RgbaUnorm
                            | wgpu::TextureFormat::Bc2RgbaUnorm
                            | wgpu::TextureFormat::Bc3RgbaUnorm
                            | wgpu::TextureFormat::Bc7RgbaUnorm
                    ) {
                        usage.remove(wgpu::TextureUsages::RENDER_ATTACHMENT);
                    }
                    let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                        label: Some("aerogpu-d3d9.texture2d"),
                        size: wgpu::Extent3d {
                            width,
                            height,
                            depth_or_array_layers: array_layers,
                        },
                        mip_level_count,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format,
                        usage,
                        view_formats: &view_formats,
                    });
                    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
                    let view_srgb = if self
                        .downlevel_flags
                        .contains(wgpu::DownlevelFlags::VIEW_FORMATS)
                    {
                        match format {
                            wgpu::TextureFormat::Rgba8Unorm => {
                                Some(texture.create_view(&wgpu::TextureViewDescriptor {
                                    format: Some(wgpu::TextureFormat::Rgba8UnormSrgb),
                                    ..Default::default()
                                }))
                            }
                            wgpu::TextureFormat::Bgra8Unorm => {
                                Some(texture.create_view(&wgpu::TextureViewDescriptor {
                                    format: Some(wgpu::TextureFormat::Bgra8UnormSrgb),
                                    ..Default::default()
                                }))
                            }
                            wgpu::TextureFormat::Bc1RgbaUnorm => {
                                Some(texture.create_view(&wgpu::TextureViewDescriptor {
                                    format: Some(wgpu::TextureFormat::Bc1RgbaUnormSrgb),
                                    ..Default::default()
                                }))
                            }
                            wgpu::TextureFormat::Bc2RgbaUnorm => {
                                Some(texture.create_view(&wgpu::TextureViewDescriptor {
                                    format: Some(wgpu::TextureFormat::Bc2RgbaUnormSrgb),
                                    ..Default::default()
                                }))
                            }
                            wgpu::TextureFormat::Bc3RgbaUnorm => {
                                Some(texture.create_view(&wgpu::TextureViewDescriptor {
                                    format: Some(wgpu::TextureFormat::Bc3RgbaUnormSrgb),
                                    ..Default::default()
                                }))
                            }
                            wgpu::TextureFormat::Bc7RgbaUnorm => {
                                Some(texture.create_view(&wgpu::TextureViewDescriptor {
                                    format: Some(wgpu::TextureFormat::Bc7RgbaUnormSrgb),
                                    ..Default::default()
                                }))
                            }
                            _ => None,
                        }
                    } else {
                        None
                    };
                    self.resources.insert(
                        texture_handle,
                        Resource::Texture2d {
                            texture,
                            view,
                            view_srgb,
                            usage_flags,
                            format_raw,
                            format,
                            width,
                            height,
                            mip_level_count,
                            array_layers,
                            row_pitch_bytes,
                            backing,
                            dirty_ranges: Vec::new(),
                        },
                    );
                    self.register_resource_handle(texture_handle);
                    Ok(())
                }
            }
            AeroGpuCmd::DestroyResource { resource_handle } => {
                self.destroy_resource_handle(resource_handle);
                Ok(())
            }
            AeroGpuCmd::UploadResource {
                resource_handle,
                offset_bytes,
                size_bytes,
                data,
            } => {
                if size_bytes == 0 {
                    return Ok(());
                }
                let underlying = self.resolve_resource_handle(resource_handle)?;

                let Some(res) = self.resources.get(&underlying) else {
                    return Err(AerogpuD3d9Error::UnknownResource(resource_handle));
                };

                match res {
                    Resource::Buffer { size, backing, .. } => {
                        if backing.is_some() {
                            return Err(AerogpuD3d9Error::Validation(format!(
                                "UPLOAD_RESOURCE on guest-backed buffer {resource_handle} is not supported (use RESOURCE_DIRTY_RANGE)"
                            )));
                        }
                        let end = offset_bytes
                            .checked_add(size_bytes)
                            .ok_or(AerogpuD3d9Error::UploadOutOfBounds(resource_handle))?;
                        if end > *size {
                            return Err(AerogpuD3d9Error::UploadOutOfBounds(resource_handle));
                        }
                    }
                    Resource::Texture2d {
                        format_raw,
                        format: _format,
                        width,
                        height,
                        row_pitch_bytes,
                        backing,
                        ..
                    } => {
                        if backing.is_some() {
                            return Err(AerogpuD3d9Error::Validation(format!(
                                "UPLOAD_RESOURCE on guest-backed texture {resource_handle} is not supported (use RESOURCE_DIRTY_RANGE)"
                            )));
                        }
                        let block = aerogpu_format_texel_block_info(*format_raw)?;
                        let expected_row_pitch = block.row_pitch_bytes(*width)?;
                        let src_pitch = if *row_pitch_bytes != 0 {
                            (*row_pitch_bytes).max(expected_row_pitch)
                        } else {
                            expected_row_pitch
                        } as u64;
                        let rows = u64::from(block.rows_per_image(*height));
                        let total_size = src_pitch.checked_mul(rows).ok_or_else(|| {
                            AerogpuD3d9Error::Validation(
                                "UPLOAD_RESOURCE: texture size overflow".into(),
                            )
                        })?;
                        let end = offset_bytes
                            .checked_add(size_bytes)
                            .ok_or(AerogpuD3d9Error::UploadOutOfBounds(resource_handle))?;
                        if end > total_size {
                            return Err(AerogpuD3d9Error::UploadOutOfBounds(resource_handle));
                        }
                    }
                }

                // Perform the upload using encoder-ordered copies so interleaved uploads and draws
                // observe correct ordering within a submission.
                self.ensure_encoder();
                let mut encoder_opt = Some(self.encoder.take().unwrap());

                let result = (|| -> Result<(), AerogpuD3d9Error> {
                    let Some(res) = self.resources.get_mut(&underlying) else {
                        return Err(AerogpuD3d9Error::UnknownResource(resource_handle));
                    };
                    match res {
                        Resource::Buffer {
                            buffer,
                            size,
                            shadow,
                            ..
                        } => {
                            let end = offset_bytes
                                .checked_add(size_bytes)
                                .ok_or(AerogpuD3d9Error::UploadOutOfBounds(resource_handle))?;
                            if end > *size {
                                return Err(AerogpuD3d9Error::UploadOutOfBounds(resource_handle));
                            }

                            let off = usize::try_from(offset_bytes).map_err(|_| {
                                AerogpuD3d9Error::Validation(
                                    "UPLOAD_RESOURCE: buffer offset_bytes overflow".into(),
                                )
                            })?;
                            let len = usize::try_from(size_bytes).map_err(|_| {
                                AerogpuD3d9Error::Validation(
                                    "UPLOAD_RESOURCE: buffer size_bytes overflow".into(),
                                )
                            })?;
                            let end_usize = off
                                .checked_add(len)
                                .ok_or(AerogpuD3d9Error::Validation(
                                    "UPLOAD_RESOURCE: buffer offset/size overflow".into(),
                                ))?;
                            if end_usize > shadow.len() {
                                return Err(AerogpuD3d9Error::UploadOutOfBounds(resource_handle));
                            }

                            let alignment = wgpu::COPY_BUFFER_ALIGNMENT;
                            if (size_bytes % alignment) != 0 {
                                return Err(AerogpuD3d9Error::Validation(format!(
                                    "UPLOAD_RESOURCE: buffer size_bytes must be a multiple of {alignment} (handle={resource_handle} size_bytes={size_bytes})"
                                )));
                            }

                            if (offset_bytes % alignment) != 0 {
                                return Err(AerogpuD3d9Error::Validation(format!(
                                    "UPLOAD_RESOURCE: buffer offset_bytes must be a multiple of {alignment} (handle={resource_handle} offset_bytes={offset_bytes})"
                                )));
                            }

                            let staging =
                                self.device
                                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                                        label: Some("aerogpu-d3d9.upload_resource_staging"),
                                        contents: data,
                                        usage: wgpu::BufferUsages::COPY_SRC,
                                    });
                            let encoder = encoder_opt
                                .as_mut()
                                .expect("encoder exists for aligned upload_resource");
                            encoder.copy_buffer_to_buffer(
                                &staging,
                                0,
                                buffer,
                                offset_bytes,
                                size_bytes,
                            );

                            // Keep the CPU shadow copy in sync with the GPU buffer contents.
                            shadow[off..end_usize].copy_from_slice(data);
                            Ok(())
                        }
                        Resource::Texture2d {
                            texture,
                            format_raw,
                            format,
                            width,
                            height,
                            row_pitch_bytes,
                            ..
                        } => {
                            let src_block = aerogpu_format_texel_block_info(*format_raw)?;
                            let bc_format = aerogpu_format_bc(*format_raw);
                            let dst_is_bc = matches!(
                                *format,
                                wgpu::TextureFormat::Bc1RgbaUnorm
                                    | wgpu::TextureFormat::Bc2RgbaUnorm
                                    | wgpu::TextureFormat::Bc3RgbaUnorm
                                    | wgpu::TextureFormat::Bc7RgbaUnorm
                            );

                            let expected_row_pitch = src_block.row_pitch_bytes(*width)?;
                            let src_pitch = if *row_pitch_bytes != 0 {
                                (*row_pitch_bytes).max(expected_row_pitch)
                            } else {
                                expected_row_pitch
                            };
                            let src_pitch_u64 = src_pitch as u64;
                            if src_pitch_u64 == 0 {
                                return Err(AerogpuD3d9Error::UploadNotSupported(resource_handle));
                            }

                            let total_rows = src_block.rows_per_image(*height);
                            let total_size = src_pitch_u64
                                .checked_mul(u64::from(total_rows))
                                .ok_or_else(|| {
                                    AerogpuD3d9Error::Validation(
                                        "UPLOAD_RESOURCE: texture size overflow".into(),
                                    )
                                })?;
                            let end = offset_bytes
                                .checked_add(size_bytes)
                                .ok_or(AerogpuD3d9Error::UploadOutOfBounds(resource_handle))?;
                            if end > total_size {
                                return Err(AerogpuD3d9Error::UploadOutOfBounds(resource_handle));
                            }

                            let bytes_per_unit = u64::from(src_block.bytes_per_block);
                            let x_bytes = offset_bytes % src_pitch_u64;
                            let y_row = offset_bytes / src_pitch_u64;
                            if y_row >= u64::from(total_rows) {
                                return Err(AerogpuD3d9Error::UploadOutOfBounds(resource_handle));
                            }
                            if !x_bytes.is_multiple_of(bytes_per_unit) {
                                return Err(AerogpuD3d9Error::UploadNotSupported(resource_handle));
                            }
                            let x_unit = u32::try_from(x_bytes / bytes_per_unit).map_err(|_| {
                                AerogpuD3d9Error::UploadNotSupported(resource_handle)
                            })?;

                            let units_per_row = width.div_ceil(src_block.block_width);
                            let origin_y_row = y_row as u32;

                            let full_rows = x_bytes == 0 && size_bytes.is_multiple_of(src_pitch_u64);

                            let (
                                origin_x_texels,
                                origin_y_texels,
                                copy_w_texels,
                                copy_h_texels,
                                buffer_rows,
                                src_bpr_bytes,
                            ) = if full_rows {
                                let buffer_rows = u32::try_from(size_bytes / src_pitch_u64)
                                    .map_err(|_| {
                                        AerogpuD3d9Error::Validation(
                                            "UPLOAD_RESOURCE: texture row count out of range"
                                                .into(),
                                        )
                                    })?;
                                if buffer_rows == 0 {
                                    return Ok(());
                                }
                                if origin_y_row.saturating_add(buffer_rows) > total_rows {
                                    return Err(AerogpuD3d9Error::UploadOutOfBounds(
                                        resource_handle,
                                    ));
                                }

                                let origin_y_texels = origin_y_row
                                    .checked_mul(src_block.block_height)
                                    .ok_or_else(|| {
                                        AerogpuD3d9Error::Validation(
                                            "UPLOAD_RESOURCE: origin_y overflow".into(),
                                        )
                                    })?;
                                let end_row = origin_y_row + buffer_rows;
                                let copy_h_texels = if end_row == total_rows {
                                    height
                                        .checked_sub(origin_y_texels)
                                        .ok_or_else(|| {
                                            AerogpuD3d9Error::Validation(
                                                "UPLOAD_RESOURCE: origin_y out of bounds".into(),
                                            )
                                        })?
                                } else {
                                    buffer_rows
                                        .checked_mul(src_block.block_height)
                                        .ok_or_else(|| {
                                            AerogpuD3d9Error::Validation(
                                                "UPLOAD_RESOURCE: copy height overflow".into(),
                                            )
                                        })?
                                };
                                (
                                    0u32,
                                    origin_y_texels,
                                    *width,
                                    copy_h_texels,
                                    buffer_rows,
                                    src_pitch,
                                )
                            } else {
                                // Single-row upload (eg `UpdateSurface`, or chunked texture uploads).
                                if x_bytes.saturating_add(size_bytes) > src_pitch_u64 {
                                    return Err(AerogpuD3d9Error::UploadNotSupported(
                                        resource_handle,
                                    ));
                                }
                                if !size_bytes.is_multiple_of(bytes_per_unit) {
                                    return Err(AerogpuD3d9Error::UploadNotSupported(
                                        resource_handle,
                                    ));
                                }
                                let units_to_copy = u32::try_from(size_bytes / bytes_per_unit)
                                    .map_err(|_| {
                                        AerogpuD3d9Error::Validation(
                                            "UPLOAD_RESOURCE: texture copy width out of range"
                                                .into(),
                                        )
                                    })?;
                                if units_to_copy == 0 {
                                    return Ok(());
                                }
                                if x_unit.saturating_add(units_to_copy) > units_per_row {
                                    return Err(AerogpuD3d9Error::UploadOutOfBounds(
                                        resource_handle,
                                    ));
                                }

                                let origin_x_texels = x_unit
                                    .checked_mul(src_block.block_width)
                                    .ok_or_else(|| {
                                        AerogpuD3d9Error::Validation(
                                            "UPLOAD_RESOURCE: origin_x overflow".into(),
                                        )
                                    })?;
                                let origin_y_texels = origin_y_row
                                    .checked_mul(src_block.block_height)
                                    .ok_or_else(|| {
                                        AerogpuD3d9Error::Validation(
                                            "UPLOAD_RESOURCE: origin_y overflow".into(),
                                        )
                                    })?;

                                let end_unit = x_unit + units_to_copy;
                                let copy_w_texels = if end_unit == units_per_row {
                                    width.checked_sub(origin_x_texels).ok_or_else(|| {
                                        AerogpuD3d9Error::Validation(
                                            "UPLOAD_RESOURCE: origin_x out of bounds".into(),
                                        )
                                    })?
                                } else {
                                    units_to_copy
                                        .checked_mul(src_block.block_width)
                                        .ok_or_else(|| {
                                            AerogpuD3d9Error::Validation(
                                                "UPLOAD_RESOURCE: copy width overflow".into(),
                                            )
                                        })?
                                };

                                let end_row = origin_y_row + 1;
                                let copy_h_texels = if end_row == total_rows {
                                    height
                                        .checked_sub(origin_y_texels)
                                        .ok_or_else(|| {
                                            AerogpuD3d9Error::Validation(
                                                "UPLOAD_RESOURCE: origin_y out of bounds".into(),
                                            )
                                        })?
                                } else {
                                    src_block.block_height
                                };

                                let row_len_bytes = units_to_copy
                                    .checked_mul(src_block.bytes_per_block)
                                    .ok_or_else(|| {
                                        AerogpuD3d9Error::Validation(
                                            "UPLOAD_RESOURCE: row byte size overflow".into(),
                                        )
                                    })?;
                                (
                                    origin_x_texels,
                                    origin_y_texels,
                                    copy_w_texels,
                                    copy_h_texels,
                                    1u32,
                                    row_len_bytes,
                                )
                            };

                            let needs_16bit_expand = matches!(
                                *format_raw,
                                x if x == AerogpuFormat::B5G6R5Unorm as u32
                                    || x == AerogpuFormat::B5G5R5A1Unorm as u32
                            );

                            if let (Some(bc), false) = (bc_format, dst_is_bc) {
                                // CPU BC fallback upload into RGBA8.
                                let tight_row_bytes = src_block.row_pitch_bytes(copy_w_texels)?;

                                let bc_bytes: Vec<u8> = if full_rows {
                                    if src_bpr_bytes == tight_row_bytes {
                                        data.to_vec()
                                    } else {
                                        let src_bpr_usize: usize = src_bpr_bytes as usize;
                                        let tight_usize: usize = tight_row_bytes as usize;
                                        let buffer_rows_usize: usize = buffer_rows as usize;
                                        let mut packed =
                                            vec![0u8; tight_usize * buffer_rows_usize];
                                        for row in 0..buffer_rows_usize {
                                            let src_start = row * src_bpr_usize;
                                            let dst_start = row * tight_usize;
                                            packed[dst_start..dst_start + tight_usize]
                                                .copy_from_slice(
                                                    &data[src_start..src_start + tight_usize],
                                                );
                                        }
                                        packed
                                    }
                                } else {
                                    // single row segment is already tight.
                                    data.to_vec()
                                };

                                let rgba = match bc {
                                    BcFormat::Bc1 => {
                                        decompress_bc1_rgba8(copy_w_texels, copy_h_texels, &bc_bytes)
                                    }
                                    BcFormat::Bc2 => {
                                        decompress_bc2_rgba8(copy_w_texels, copy_h_texels, &bc_bytes)
                                    }
                                    BcFormat::Bc3 => {
                                        decompress_bc3_rgba8(copy_w_texels, copy_h_texels, &bc_bytes)
                                    }
                                    BcFormat::Bc7 => {
                                        decompress_bc7_rgba8(copy_w_texels, copy_h_texels, &bc_bytes)
                                    }
                                };

                                let unpadded_bpr =
                                    copy_w_texels.checked_mul(4).ok_or_else(|| {
                                        AerogpuD3d9Error::Validation(
                                            "UPLOAD_RESOURCE: bytes_per_row overflow".into(),
                                        )
                                    })?;
                                let padded_bpr =
                                    align_to(unpadded_bpr, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
                                let bytes = if padded_bpr == unpadded_bpr {
                                    rgba
                                } else {
                                    let height_usize: usize = copy_h_texels.try_into().map_err(
                                        |_| {
                                            AerogpuD3d9Error::Validation(
                                                "UPLOAD_RESOURCE: height out of range".into(),
                                            )
                                        },
                                    )?;
                                    let padded_usize: usize = padded_bpr as usize;
                                    let unpadded_usize: usize = unpadded_bpr as usize;
                                    let mut padded = vec![0u8; padded_usize * height_usize];
                                    for row in 0..height_usize {
                                        let src_start = row * unpadded_usize;
                                        let dst_start = row * padded_usize;
                                        padded[dst_start..dst_start + unpadded_usize]
                                            .copy_from_slice(
                                                &rgba[src_start..src_start + unpadded_usize],
                                            );
                                    }
                                    padded
                                };

                                let staging =
                                    self.device
                                        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                                            label: Some("aerogpu-d3d9.upload_resource_staging"),
                                            contents: &bytes,
                                            usage: wgpu::BufferUsages::COPY_SRC,
                                        });
                                let encoder = encoder_opt
                                    .as_mut()
                                    .expect("encoder exists for upload_resource");
                                encoder.copy_buffer_to_texture(
                                    wgpu::ImageCopyBuffer {
                                        buffer: &staging,
                                        layout: wgpu::ImageDataLayout {
                                            offset: 0,
                                            bytes_per_row: Some(padded_bpr),
                                            rows_per_image: Some(copy_h_texels),
                                        },
                                    },
                                    wgpu::ImageCopyTexture {
                                        texture,
                                        mip_level: 0,
                                        origin: wgpu::Origin3d {
                                            x: origin_x_texels,
                                            y: origin_y_texels,
                                            z: 0,
                                        },
                                        aspect: wgpu::TextureAspect::All,
                                    },
                                    wgpu::Extent3d {
                                        width: copy_w_texels,
                                        height: copy_h_texels,
                                        depth_or_array_layers: 1,
                                    },
                                );
                                Ok(())
                            } else if needs_16bit_expand {
                                // CPU expansion: 16-bit packed -> RGBA8
                                let src_row_bytes = copy_w_texels.checked_mul(2).ok_or_else(|| {
                                    AerogpuD3d9Error::Validation(
                                        "UPLOAD_RESOURCE: 16-bit row byte size overflow".into(),
                                    )
                                })?;
                                let unpadded_bpr = copy_w_texels.checked_mul(4).ok_or_else(|| {
                                    AerogpuD3d9Error::Validation(
                                        "UPLOAD_RESOURCE: bytes_per_row overflow".into(),
                                    )
                                })?;
                                let bytes_per_row =
                                    align_to(unpadded_bpr, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);

                                let height_usize: usize = copy_h_texels.try_into().map_err(|_| {
                                    AerogpuD3d9Error::Validation(
                                        "UPLOAD_RESOURCE: height out of range".into(),
                                    )
                                })?;
                                let bytes_per_row_usize = bytes_per_row as usize;
                                let unpadded_usize = unpadded_bpr as usize;
                                let src_pitch_usize = src_pitch as usize;
                                let src_row_usize = src_row_bytes as usize;

                                let mut bytes = vec![0u8; bytes_per_row_usize * height_usize];
                                for row in 0..height_usize {
                                    let src_start = if full_rows {
                                        row.checked_mul(src_pitch_usize).ok_or_else(|| {
                                            AerogpuD3d9Error::Validation(
                                                "UPLOAD_RESOURCE: texture row offset overflow"
                                                    .into(),
                                            )
                                        })?
                                    } else {
                                        0
                                    };
                                    let src_end =
                                        src_start.checked_add(src_row_usize).ok_or_else(|| {
                                            AerogpuD3d9Error::Validation(
                                                "UPLOAD_RESOURCE: texture row offset overflow"
                                                    .into(),
                                            )
                                        })?;
                                    let src = data.get(src_start..src_end).ok_or_else(|| {
                                        AerogpuD3d9Error::Validation(
                                            "UPLOAD_RESOURCE: texture upload out of bounds".into(),
                                        )
                                    })?;

                                    let dst_start = row * bytes_per_row_usize;
                                    let dst_end = dst_start + unpadded_usize;
                                    let dst = bytes.get_mut(dst_start..dst_end).ok_or_else(|| {
                                        AerogpuD3d9Error::Validation(
                                            "UPLOAD_RESOURCE: staging out of bounds".into(),
                                        )
                                    })?;

                                    match *format_raw {
                                        x if x == AerogpuFormat::B5G6R5Unorm as u32 => {
                                            expand_b5g6r5_unorm_to_rgba8(src, dst);
                                        }
                                        x if x == AerogpuFormat::B5G5R5A1Unorm as u32 => {
                                            expand_b5g5r5a1_unorm_to_rgba8(src, dst);
                                        }
                                        _ => unreachable!("needs_16bit_expand checked above"),
                                    }
                                }

                                let staging =
                                    self.device
                                        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                                            label: Some("aerogpu-d3d9.upload_resource_staging"),
                                            contents: &bytes,
                                            usage: wgpu::BufferUsages::COPY_SRC,
                                        });
                                let encoder = encoder_opt
                                    .as_mut()
                                    .expect("encoder exists for upload_resource");
                                encoder.copy_buffer_to_texture(
                                    wgpu::ImageCopyBuffer {
                                        buffer: &staging,
                                        layout: wgpu::ImageDataLayout {
                                            offset: 0,
                                            bytes_per_row: Some(bytes_per_row),
                                            rows_per_image: Some(copy_h_texels),
                                        },
                                    },
                                    wgpu::ImageCopyTexture {
                                        texture,
                                        mip_level: 0,
                                        origin: wgpu::Origin3d {
                                            x: origin_x_texels,
                                            y: origin_y_texels,
                                            z: 0,
                                        },
                                        aspect: wgpu::TextureAspect::All,
                                    },
                                    wgpu::Extent3d {
                                        width: copy_w_texels,
                                        height: copy_h_texels,
                                        depth_or_array_layers: 1,
                                    },
                                );
                                Ok(())
                            } else {
                                // Direct upload.
                                let bytes_per_row =
                                    align_to(src_bpr_bytes, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);

                                let bytes: Vec<u8> = if full_rows {
                                    if bytes_per_row != src_bpr_bytes {
                                        let src_bpr_usize: usize = src_bpr_bytes as usize;
                                        let dst_bpr_usize: usize = bytes_per_row as usize;
                                        let rows_usize: usize = buffer_rows as usize;
                                        let mut staging = vec![0u8; dst_bpr_usize * rows_usize];
                                        for row in 0..rows_usize {
                                            let src_start = row * src_bpr_usize;
                                            let dst_start = row * dst_bpr_usize;
                                            staging[dst_start..dst_start + src_bpr_usize]
                                                .copy_from_slice(
                                                    &data[src_start..src_start + src_bpr_usize],
                                                );
                                        }
                                        staging
                                    } else {
                                        data.to_vec()
                                    }
                                } else if bytes_per_row != src_bpr_bytes {
                                    let mut staging = vec![0u8; bytes_per_row as usize];
                                    staging[..src_bpr_bytes as usize].copy_from_slice(data);
                                    staging
                                } else {
                                    data.to_vec()
                                };

                                let mut bytes = bytes;

                                let force_opaque_alpha = is_x8_format(*format_raw)
                                    && src_block.block_width == 1
                                    && src_block.block_height == 1
                                    && src_block.bytes_per_block == 4;
                                if force_opaque_alpha {
                                    let row_bytes = copy_w_texels as usize * 4;
                                    let stride = bytes_per_row as usize;
                                    for row in 0..copy_h_texels as usize {
                                        let start = row * stride;
                                        force_opaque_alpha_rgba8(
                                            &mut bytes[start..start + row_bytes],
                                        );
                                    }
                                }

                                let staging =
                                    self.device
                                        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                                            label: Some("aerogpu-d3d9.upload_resource_staging"),
                                            contents: &bytes,
                                            usage: wgpu::BufferUsages::COPY_SRC,
                                        });
                                let encoder = encoder_opt
                                    .as_mut()
                                    .expect("encoder exists for upload_resource");
                                encoder.copy_buffer_to_texture(
                                    wgpu::ImageCopyBuffer {
                                        buffer: &staging,
                                        layout: wgpu::ImageDataLayout {
                                            offset: 0,
                                            bytes_per_row: Some(bytes_per_row),
                                            rows_per_image: Some(buffer_rows),
                                        },
                                    },
                                    wgpu::ImageCopyTexture {
                                        texture,
                                        mip_level: 0,
                                        origin: wgpu::Origin3d {
                                            x: origin_x_texels,
                                            y: origin_y_texels,
                                            z: 0,
                                        },
                                        aspect: wgpu::TextureAspect::All,
                                    },
                                    wgpu::Extent3d {
                                        width: copy_w_texels,
                                        height: copy_h_texels,
                                        depth_or_array_layers: 1,
                                    },
                                );
                                Ok(())
                            }
                        }
                    }
                })();

                if let Some(encoder) = encoder_opt {
                    self.encoder = Some(encoder);
                }
                result
            }
            AeroGpuCmd::CopyBuffer {
                dst_buffer,
                src_buffer,
                dst_offset_bytes,
                src_offset_bytes,
                size_bytes,
                flags,
                ..
            } => {
                self.ensure_encoder();
                let mut encoder_opt = self.encoder.take();
                let mut writeback_entry: Option<PendingWriteback> = None;
                let result = (|| -> Result<(), AerogpuD3d9Error> {
                    if size_bytes == 0 {
                        return Ok(());
                    }
                    // `WRITEBACK_DST` requires async execution on wasm so we can await
                    // `wgpu::Buffer::map_async`. The sync entrypoint errors out when pending
                    // writebacks are present; the async entrypoint flushes + awaits them.
                    let writeback = (flags & AEROGPU_COPY_FLAG_WRITEBACK_DST) != 0;
                    if (flags & !AEROGPU_COPY_FLAG_WRITEBACK_DST) != 0 {
                        return Err(AerogpuD3d9Error::Validation(format!(
                            "COPY_BUFFER: unknown flags 0x{flags:08X}"
                        )));
                    }

                    let align = wgpu::COPY_BUFFER_ALIGNMENT;
                    if !dst_offset_bytes.is_multiple_of(align)
                        || !src_offset_bytes.is_multiple_of(align)
                        || !size_bytes.is_multiple_of(align)
                    {
                        return Err(AerogpuD3d9Error::Validation(format!(
                            "COPY_BUFFER: offsets and size must be {}-byte aligned (dst_offset_bytes={dst_offset_bytes} src_offset_bytes={src_offset_bytes} size_bytes={size_bytes})",
                            wgpu::COPY_BUFFER_ALIGNMENT
                        )));
                    }

                    let src_underlying = self.resolve_resource_handle(src_buffer)?;
                    let dst_underlying = self.resolve_resource_handle(dst_buffer)?;
                    let (src_size, dst_size, dst_backing) = {
                        let src = self
                            .resources
                            .get(&src_underlying)
                            .ok_or(AerogpuD3d9Error::UnknownResource(src_buffer))?;
                        let dst = self
                            .resources
                            .get(&dst_underlying)
                            .ok_or(AerogpuD3d9Error::UnknownResource(dst_buffer))?;

                        let src_size = match src {
                            Resource::Buffer { size, .. } => *size,
                            _ => {
                                return Err(AerogpuD3d9Error::CopyNotSupported {
                                    src: src_buffer,
                                    dst: dst_buffer,
                                })
                            }
                        };
                        let (dst_size, dst_backing) = match dst {
                            Resource::Buffer { size, backing, .. } => (*size, *backing),
                            _ => {
                                return Err(AerogpuD3d9Error::CopyNotSupported {
                                    src: src_buffer,
                                    dst: dst_buffer,
                                })
                            }
                        };
                        (src_size, dst_size, dst_backing)
                    };
                    if writeback && dst_backing.is_none() {
                        return Err(AerogpuD3d9Error::Validation(format!(
                            "COPY_BUFFER: WRITEBACK_DST requires dst buffer to be guest-backed (handle={dst_buffer})"
                        )));
                    }

                    let src_end = src_offset_bytes.checked_add(size_bytes).ok_or(
                        AerogpuD3d9Error::CopyOutOfBounds {
                            src: src_buffer,
                            dst: dst_buffer,
                        },
                    )?;
                    let dst_end = dst_offset_bytes.checked_add(size_bytes).ok_or(
                        AerogpuD3d9Error::CopyOutOfBounds {
                            src: src_buffer,
                            dst: dst_buffer,
                        },
                    )?;
                    if src_end > src_size || dst_end > dst_size {
                        return Err(AerogpuD3d9Error::CopyOutOfBounds {
                            src: src_buffer,
                            dst: dst_buffer,
                        });
                    }

                    let dst_writeback_gpa = if writeback {
                        let dst_backing = dst_backing.ok_or_else(|| {
                            AerogpuD3d9Error::Validation(
                                "COPY_BUFFER: WRITEBACK_DST requires guest-backed dst".into(),
                            )
                        })?;
                        if ctx.guest_memory.is_none() {
                            return Err(AerogpuD3d9Error::MissingGuestMemory(dst_buffer));
                        }

                        let alloc = ctx.require_alloc_entry(dst_backing.alloc_id)?;
                        if (alloc.flags & ring::AEROGPU_ALLOC_FLAG_READONLY) != 0 {
                            return Err(AerogpuD3d9Error::Validation(format!(
                                "COPY_BUFFER: WRITEBACK_DST to READONLY alloc_id={}",
                                dst_backing.alloc_id
                            )));
                        }
                        let alloc_offset = dst_backing
                            .alloc_offset_bytes
                            .checked_add(dst_offset_bytes)
                            .ok_or_else(|| {
                                AerogpuD3d9Error::Validation(
                                    "COPY_BUFFER: dst backing overflow".into(),
                                )
                            })?;
                        let alloc_end = alloc_offset.checked_add(size_bytes).ok_or_else(|| {
                            AerogpuD3d9Error::Validation("COPY_BUFFER: dst backing overflow".into())
                        })?;
                        if alloc_end > alloc.size_bytes {
                            return Err(AerogpuD3d9Error::Validation(format!(
                                "COPY_BUFFER: dst backing out of bounds (alloc_id={} offset=0x{:x} size=0x{:x} alloc_size=0x{:x})",
                                dst_backing.alloc_id, alloc_offset, size_bytes, alloc.size_bytes
                            )));
                        }
                        let dst_gpa = alloc.gpa.checked_add(alloc_offset).ok_or_else(|| {
                            AerogpuD3d9Error::Validation("COPY_BUFFER: dst backing overflow".into())
                        })?;
                        if dst_gpa.checked_add(size_bytes).is_none() {
                            return Err(AerogpuD3d9Error::Validation(
                                "COPY_BUFFER: dst backing overflow".into(),
                            ));
                        }
                        Some(dst_gpa)
                    } else {
                        None
                    };

                    // Flush any pending CPU writes before GPU reads/writes this buffer.
                    self.flush_buffer_if_dirty(encoder_opt.as_mut(), src_buffer, ctx)?;
                    self.flush_buffer_if_dirty(encoder_opt.as_mut(), dst_buffer, ctx)?;

                    let encoder = encoder_opt.as_mut().ok_or_else(|| {
                        AerogpuD3d9Error::Validation("COPY_BUFFER: missing encoder".into())
                    })?;

                    {
                        let src_buf = match self
                            .resources
                            .get(&src_underlying)
                            .ok_or(AerogpuD3d9Error::UnknownResource(src_buffer))?
                        {
                            Resource::Buffer { buffer, .. } => buffer,
                            _ => {
                                return Err(AerogpuD3d9Error::CopyNotSupported {
                                    src: src_buffer,
                                    dst: dst_buffer,
                                })
                            }
                        };
                        let dst_buf = match self
                            .resources
                            .get(&dst_underlying)
                            .ok_or(AerogpuD3d9Error::UnknownResource(dst_buffer))?
                        {
                            Resource::Buffer { buffer, .. } => buffer,
                            _ => {
                                return Err(AerogpuD3d9Error::CopyNotSupported {
                                    src: src_buffer,
                                    dst: dst_buffer,
                                })
                            }
                        };

                        encoder.copy_buffer_to_buffer(
                            src_buf,
                            src_offset_bytes,
                            dst_buf,
                            dst_offset_bytes,
                            size_bytes,
                        );

                        if let Some(dst_gpa) = dst_writeback_gpa {
                            let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
                                label: Some("aerogpu-d3d9.copy_buffer.writeback_staging"),
                                size: size_bytes,
                                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                                mapped_at_creation: false,
                            });
                            encoder.copy_buffer_to_buffer(
                                dst_buf,
                                dst_offset_bytes,
                                &staging,
                                0,
                                size_bytes,
                            );
                            writeback_entry = Some(PendingWriteback::Buffer {
                                staging,
                                dst_gpa,
                                size_bytes,
                            });
                        }
                    }

                    // Keep CPU shadow copies in sync with the GPU copy.
                    //
                    // This is required for vertex format conversions which operate on the CPU
                    // shadow (e.g. D3DDECLTYPE_D3DCOLOR BGRA->RGBA conversion).
                    if src_underlying == dst_underlying {
                        if let Some(Resource::Buffer { shadow, .. }) =
                            self.resources.get_mut(&src_underlying)
                        {
                            let src_off = usize::try_from(src_offset_bytes).map_err(|_| {
                                AerogpuD3d9Error::Validation(
                                    "COPY_BUFFER: src_offset_bytes overflow".into(),
                                )
                            })?;
                            let dst_off = usize::try_from(dst_offset_bytes).map_err(|_| {
                                AerogpuD3d9Error::Validation(
                                    "COPY_BUFFER: dst_offset_bytes overflow".into(),
                                )
                            })?;
                            let len = usize::try_from(size_bytes).map_err(|_| {
                                AerogpuD3d9Error::Validation(
                                    "COPY_BUFFER: size_bytes overflow".into(),
                                )
                            })?;
                            let src_end = src_off.checked_add(len).ok_or_else(|| {
                                AerogpuD3d9Error::Validation(
                                    "COPY_BUFFER: src range overflow".into(),
                                )
                            })?;
                            let dst_end = dst_off.checked_add(len).ok_or_else(|| {
                                AerogpuD3d9Error::Validation(
                                    "COPY_BUFFER: dst range overflow".into(),
                                )
                            })?;
                            if src_end > shadow.len() || dst_end > shadow.len() {
                                return Err(AerogpuD3d9Error::CopyOutOfBounds {
                                    src: src_buffer,
                                    dst: dst_buffer,
                                });
                            }
                            shadow.copy_within(src_off..src_end, dst_off);
                        }
                    } else {
                        let src_bytes = match self
                            .resources
                            .get(&src_underlying)
                            .ok_or(AerogpuD3d9Error::UnknownResource(src_buffer))?
                        {
                            Resource::Buffer { shadow, .. } => {
                                let src_off = usize::try_from(src_offset_bytes).map_err(|_| {
                                    AerogpuD3d9Error::Validation(
                                        "COPY_BUFFER: src_offset_bytes overflow".into(),
                                    )
                                })?;
                                let len = usize::try_from(size_bytes).map_err(|_| {
                                    AerogpuD3d9Error::Validation(
                                        "COPY_BUFFER: size_bytes overflow".into(),
                                    )
                                })?;
                                let src_end = src_off.checked_add(len).ok_or_else(|| {
                                    AerogpuD3d9Error::Validation(
                                        "COPY_BUFFER: src range overflow".into(),
                                    )
                                })?;
                                if src_end > shadow.len() {
                                    return Err(AerogpuD3d9Error::CopyOutOfBounds {
                                        src: src_buffer,
                                        dst: dst_buffer,
                                    });
                                }
                                shadow[src_off..src_end].to_vec()
                            }
                            _ => {
                                return Err(AerogpuD3d9Error::CopyNotSupported {
                                    src: src_buffer,
                                    dst: dst_buffer,
                                })
                            }
                        };
                        match self
                            .resources
                            .get_mut(&dst_underlying)
                            .ok_or(AerogpuD3d9Error::UnknownResource(dst_buffer))?
                        {
                            Resource::Buffer { shadow, .. } => {
                                let dst_off = usize::try_from(dst_offset_bytes).map_err(|_| {
                                    AerogpuD3d9Error::Validation(
                                        "COPY_BUFFER: dst_offset_bytes overflow".into(),
                                    )
                                })?;
                                let dst_end =
                                    dst_off.checked_add(src_bytes.len()).ok_or_else(|| {
                                        AerogpuD3d9Error::Validation(
                                            "COPY_BUFFER: dst range overflow".into(),
                                        )
                                    })?;
                                if dst_end > shadow.len() {
                                    return Err(AerogpuD3d9Error::CopyOutOfBounds {
                                        src: src_buffer,
                                        dst: dst_buffer,
                                    });
                                }
                                shadow[dst_off..dst_end].copy_from_slice(&src_bytes);
                            }
                            _ => {
                                return Err(AerogpuD3d9Error::CopyNotSupported {
                                    src: src_buffer,
                                    dst: dst_buffer,
                                })
                            }
                        }
                    }
                    Ok(())
                })();
                self.encoder = encoder_opt;
                if result.is_ok() {
                    if let Some(entry) = writeback_entry {
                        pending_writebacks.push(entry);
                    }
                }
                result
            }
            AeroGpuCmd::CopyTexture2d {
                dst_texture,
                src_texture,
                dst_mip_level,
                dst_array_layer,
                src_mip_level,
                src_array_layer,
                dst_x,
                dst_y,
                src_x,
                src_y,
                width,
                height,
                flags,
                ..
            } => {
                self.ensure_encoder();
                let mut encoder_opt = self.encoder.take();
                let mut writeback_entry: Option<PendingWriteback> = None;
                let result = (|| -> Result<(), AerogpuD3d9Error> {
                    if width == 0 || height == 0 {
                        return Ok(());
                    }
                    // `WRITEBACK_DST` requires async execution on wasm so we can await
                    // `wgpu::Buffer::map_async`. The sync entrypoint errors out when pending
                    // writebacks are present; the async entrypoint flushes + awaits them.
                    let writeback = (flags & AEROGPU_COPY_FLAG_WRITEBACK_DST) != 0;
                    if (flags & !AEROGPU_COPY_FLAG_WRITEBACK_DST) != 0 {
                        return Err(AerogpuD3d9Error::Validation(format!(
                            "COPY_TEXTURE2D: unknown flags 0x{flags:08X}"
                        )));
                    }

                    let src_underlying = self.resolve_resource_handle(src_texture)?;
                    let dst_underlying = self.resolve_resource_handle(dst_texture)?;
                    let (
                        (src_format_raw, _src_format, src_w, src_h, src_mips, src_layers),
                        (
                            dst_format_raw,
                            dst_format,
                            dst_w,
                            dst_h,
                            dst_mips,
                            dst_layers,
                            dst_backing,
                        ),
                    ) = {
                        let src = self
                            .resources
                            .get(&src_underlying)
                            .ok_or(AerogpuD3d9Error::UnknownResource(src_texture))?;
                        let dst = self
                            .resources
                            .get(&dst_underlying)
                            .ok_or(AerogpuD3d9Error::UnknownResource(dst_texture))?;

                        let src_info = match src {
                            Resource::Texture2d {
                                format_raw,
                                format,
                                width,
                                height,
                                mip_level_count,
                                array_layers,
                                ..
                            } => (
                                *format_raw,
                                *format,
                                *width,
                                *height,
                                *mip_level_count,
                                *array_layers,
                            ),
                            _ => {
                                return Err(AerogpuD3d9Error::CopyNotSupported {
                                    src: src_texture,
                                    dst: dst_texture,
                                })
                            }
                        };
                        let dst_info = match dst {
                            Resource::Texture2d {
                                format_raw,
                                format,
                                width,
                                height,
                                mip_level_count,
                                array_layers,
                                backing,
                                ..
                            } => (
                                *format_raw,
                                *format,
                                *width,
                                *height,
                                *mip_level_count,
                                *array_layers,
                                *backing,
                            ),
                            _ => {
                                return Err(AerogpuD3d9Error::CopyNotSupported {
                                    src: src_texture,
                                    dst: dst_texture,
                                })
                            }
                        };
                        (src_info, dst_info)
                    };

                    if src_format_raw != dst_format_raw {
                        return Err(AerogpuD3d9Error::CopyNotSupported {
                            src: src_texture,
                            dst: dst_texture,
                        });
                    }
                    if writeback && dst_backing.is_none() {
                        return Err(AerogpuD3d9Error::Validation(
                            "COPY_TEXTURE2D: WRITEBACK_DST requires dst_texture to be guest-backed"
                                .into(),
                        ));
                    }

                    if dst_mip_level >= dst_mips
                        || dst_array_layer >= dst_layers
                        || src_mip_level >= src_mips
                        || src_array_layer >= src_layers
                    {
                        return Err(AerogpuD3d9Error::CopyOutOfBounds {
                            src: src_texture,
                            dst: dst_texture,
                        });
                    }

                    let dst_mip_w = mip_dim(dst_w, dst_mip_level);
                    let dst_mip_h = mip_dim(dst_h, dst_mip_level);
                    let src_mip_w = mip_dim(src_w, src_mip_level);
                    let src_mip_h = mip_dim(src_h, src_mip_level);

                    let dst_x_end =
                        dst_x
                            .checked_add(width)
                            .ok_or(AerogpuD3d9Error::CopyOutOfBounds {
                                src: src_texture,
                                dst: dst_texture,
                            })?;
                    let dst_y_end =
                        dst_y
                            .checked_add(height)
                            .ok_or(AerogpuD3d9Error::CopyOutOfBounds {
                                src: src_texture,
                                dst: dst_texture,
                            })?;
                    let src_x_end =
                        src_x
                            .checked_add(width)
                            .ok_or(AerogpuD3d9Error::CopyOutOfBounds {
                                src: src_texture,
                                dst: dst_texture,
                            })?;
                    let src_y_end =
                        src_y
                            .checked_add(height)
                            .ok_or(AerogpuD3d9Error::CopyOutOfBounds {
                                src: src_texture,
                                dst: dst_texture,
                            })?;

                    if dst_x_end > dst_mip_w
                        || dst_y_end > dst_mip_h
                        || src_x_end > src_mip_w
                        || src_y_end > src_mip_h
                    {
                        return Err(AerogpuD3d9Error::CopyOutOfBounds {
                            src: src_texture,
                            dst: dst_texture,
                        });
                    }

                    let dst_writeback_plan = if writeback {
                        let dst_backing = dst_backing.ok_or_else(|| {
                            AerogpuD3d9Error::Validation(
                                "COPY_TEXTURE2D: WRITEBACK_DST requires guest-backed dst".into(),
                            )
                        })?;
                        if aerogpu_format_bc(dst_format_raw).is_some() {
                            return Err(AerogpuD3d9Error::Validation(
                                "COPY_TEXTURE2D: WRITEBACK_DST is not supported for BC textures"
                                    .into(),
                            ));
                        }
                        if ctx.guest_memory.is_none() {
                            return Err(AerogpuD3d9Error::MissingGuestMemory(dst_texture));
                        }

                        let guest_bpp = bytes_per_pixel_aerogpu_format(dst_format_raw)?;
                        let host_bpp = bytes_per_pixel(dst_format);
                        let guest_unpadded_bpr =
                            width.checked_mul(guest_bpp).ok_or_else(|| {
                                AerogpuD3d9Error::Validation(
                                    "COPY_TEXTURE2D: bytes_per_row overflow".into(),
                                )
                            })?;
                        let host_unpadded_bpr = width.checked_mul(host_bpp).ok_or_else(|| {
                            AerogpuD3d9Error::Validation(
                                "COPY_TEXTURE2D: bytes_per_row overflow".into(),
                            )
                        })?;
                        let padded_bpr =
                            align_to(host_unpadded_bpr, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);

                        let (dst_subresource_offset_bytes, dst_subresource_row_pitch_bytes) =
                            if dst_mips == 1 && dst_layers == 1 {
                                // Single-subresource: allow padded row pitch (the legacy path).
                                (0u64, dst_backing.row_pitch_bytes)
                            } else {
                                // Multi-subresource: MVP assumes tight packing in guest memory.
                                let layout = GuestTextureSubresourceLayout::new(
                                    dst_w,
                                    dst_h,
                                    dst_mips,
                                    dst_layers,
                                    guest_bpp,
                                );
                                let offset =
                                    layout.subresource_offset_bytes(dst_array_layer, dst_mip_level)?;
                                let row_pitch = layout
                                    .subresource_row_pitch_bytes(dst_array_layer, dst_mip_level)?;

                                let sub_size = layout.subresource_size_bytes(
                                    dst_array_layer,
                                    dst_mip_level,
                                )?;
                                let sub_end = offset.checked_add(sub_size).ok_or_else(|| {
                                    AerogpuD3d9Error::Validation(
                                        "COPY_TEXTURE2D: dst subresource overflow".into(),
                                    )
                                })?;
                                if sub_end > dst_backing.size_bytes {
                                    return Err(AerogpuD3d9Error::Validation(
                                        "COPY_TEXTURE2D: dst subresource out of bounds".into(),
                                    ));
                                }

                                (offset, row_pitch)
                            };

                        let dst_x_bytes =
                            (dst_x as u64).checked_mul(guest_bpp as u64).ok_or_else(|| {
                                AerogpuD3d9Error::Validation(
                                    "COPY_TEXTURE2D: dst_x byte offset overflow".into(),
                                )
                            })?;
                        let row_pitch = dst_subresource_row_pitch_bytes as u64;
                        if row_pitch == 0 {
                            return Err(AerogpuD3d9Error::Validation(
                                "COPY_TEXTURE2D: dst texture row_pitch_bytes is 0".into(),
                            ));
                        }
                        if dst_x_bytes
                            .checked_add(guest_unpadded_bpr as u64)
                            .ok_or_else(|| {
                                AerogpuD3d9Error::Validation(
                                    "COPY_TEXTURE2D: dst row byte range overflow".into(),
                                )
                            })?
                            > row_pitch
                        {
                            return Err(AerogpuD3d9Error::Validation(
                                "COPY_TEXTURE2D: dst row pitch is too small for writeback region"
                                    .into(),
                            ));
                        }

                        let alloc = ctx.require_alloc_entry(dst_backing.alloc_id)?;
                        if (alloc.flags & ring::AEROGPU_ALLOC_FLAG_READONLY) != 0 {
                            return Err(AerogpuD3d9Error::Validation(format!(
                                "COPY_TEXTURE2D: WRITEBACK_DST to READONLY alloc_id={}",
                                dst_backing.alloc_id
                            )));
                        }
                        let backing_end = dst_backing
                            .alloc_offset_bytes
                            .checked_add(dst_backing.size_bytes)
                            .ok_or_else(|| {
                                AerogpuD3d9Error::Validation(
                                    "COPY_TEXTURE2D: dst backing overflow".into(),
                                )
                            })?;
                        if backing_end > alloc.size_bytes {
                            return Err(AerogpuD3d9Error::Validation(format!(
                                "COPY_TEXTURE2D: dst backing out of bounds (alloc_id={} offset=0x{:x} size=0x{:x} alloc_size=0x{:x})",
                                dst_backing.alloc_id,
                                dst_backing.alloc_offset_bytes,
                                dst_backing.size_bytes,
                                alloc.size_bytes
                            )));
                        }
                        let backing_gpa = alloc
                            .gpa
                            .checked_add(dst_backing.alloc_offset_bytes)
                            .ok_or_else(|| {
                                AerogpuD3d9Error::Validation(
                                    "COPY_TEXTURE2D: dst backing overflow".into(),
                                )
                            })?;
                        if backing_gpa.checked_add(dst_backing.size_bytes).is_none() {
                            return Err(AerogpuD3d9Error::Validation(
                                "COPY_TEXTURE2D: dst backing overflow".into(),
                            ));
                        }

                        Some(TextureWritebackPlan {
                            backing: dst_backing,
                            backing_gpa,
                            dst_mip_level,
                            dst_array_layer,
                            dst_subresource_offset_bytes,
                            dst_subresource_row_pitch_bytes,
                            dst_x,
                            dst_y,
                            height,
                            format_raw: dst_format_raw,
                            is_x8: is_x8_format(dst_format_raw),
                            guest_bytes_per_pixel: guest_bpp,
                            host_bytes_per_pixel: host_bpp,
                            guest_unpadded_bytes_per_row: guest_unpadded_bpr,
                            host_unpadded_bytes_per_row: host_unpadded_bpr,
                            padded_bytes_per_row: padded_bpr,
                        })
                    } else {
                        None
                    };

                    // Flush any pending CPU writes before GPU reads/writes these subresources.
                    self.flush_texture_if_dirty_strict(encoder_opt.as_mut(), src_texture, ctx)?;
                    self.flush_texture_if_dirty_strict(encoder_opt.as_mut(), dst_texture, ctx)?;

                    let encoder = encoder_opt.as_mut().ok_or_else(|| {
                        AerogpuD3d9Error::Validation("COPY_TEXTURE2D: missing encoder".into())
                    })?;

                    let src_tex = match self
                        .resources
                        .get(&src_underlying)
                        .ok_or(AerogpuD3d9Error::UnknownResource(src_texture))?
                    {
                        Resource::Texture2d { texture, .. } => texture,
                        _ => {
                            return Err(AerogpuD3d9Error::CopyNotSupported {
                                src: src_texture,
                                dst: dst_texture,
                            })
                        }
                    };
                    let dst_tex = match self
                        .resources
                        .get(&dst_underlying)
                        .ok_or(AerogpuD3d9Error::UnknownResource(dst_texture))?
                    {
                        Resource::Texture2d { texture, .. } => texture,
                        _ => {
                            return Err(AerogpuD3d9Error::CopyNotSupported {
                                src: src_texture,
                                dst: dst_texture,
                            })
                        }
                    };

                    encoder.copy_texture_to_texture(
                        wgpu::ImageCopyTexture {
                            texture: src_tex,
                            mip_level: src_mip_level,
                            origin: wgpu::Origin3d {
                                x: src_x,
                                y: src_y,
                                z: src_array_layer,
                            },
                            aspect: wgpu::TextureAspect::All,
                        },
                        wgpu::ImageCopyTexture {
                            texture: dst_tex,
                            mip_level: dst_mip_level,
                            origin: wgpu::Origin3d {
                                x: dst_x,
                                y: dst_y,
                                z: dst_array_layer,
                            },
                            aspect: wgpu::TextureAspect::All,
                        },
                        wgpu::Extent3d {
                            width,
                            height,
                            depth_or_array_layers: 1,
                        },
                    );

                    if let Some(plan) = dst_writeback_plan {
                        // WRITEBACK_DST snapshots the copied texels into a staging buffer. The
                        // staging buffer is mapped and committed into guest memory after a submit
                        // boundary at the end of the command stream.
                        let staging_size = (plan.padded_bytes_per_row as u64)
                            .checked_mul(plan.height as u64)
                            .ok_or_else(|| {
                                AerogpuD3d9Error::Validation(
                                    "COPY_TEXTURE2D: staging buffer size overflow".into(),
                                )
                            })?;
                        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
                            label: Some("aerogpu-d3d9.copy_texture2d.writeback_staging"),
                            size: staging_size,
                            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                            mapped_at_creation: false,
                        });
                        encoder.copy_texture_to_buffer(
                            wgpu::ImageCopyTexture {
                                texture: dst_tex,
                                mip_level: dst_mip_level,
                                origin: wgpu::Origin3d {
                                    x: dst_x,
                                    y: dst_y,
                                    z: dst_array_layer,
                                },
                                aspect: wgpu::TextureAspect::All,
                            },
                            wgpu::ImageCopyBuffer {
                                buffer: &staging,
                                layout: wgpu::ImageDataLayout {
                                    offset: 0,
                                    bytes_per_row: Some(plan.padded_bytes_per_row),
                                    rows_per_image: Some(plan.height),
                                },
                            },
                            wgpu::Extent3d {
                                width,
                                height,
                                depth_or_array_layers: 1,
                            },
                        );
                        writeback_entry = Some(PendingWriteback::Texture2d { staging, plan });
                    }
                    Ok(())
                })();
                self.encoder = encoder_opt;
                if result.is_ok() {
                    if let Some(entry) = writeback_entry {
                        pending_writebacks.push(entry);
                    }
                }
                result
            }
            AeroGpuCmd::CreateShaderDxbc {
                shader_handle,
                stage,
                dxbc_bytes,
                ..
            } => {
                if shader_handle == 0 {
                    return Err(AerogpuD3d9Error::Validation(
                        "CREATE_SHADER_DXBC: shader handle 0 is reserved".into(),
                    ));
                }
                if self.handle_in_use(shader_handle) {
                    return Err(AerogpuD3d9Error::ShaderHandleInUse(shader_handle));
                }

                let expected_stage = match stage {
                    s if s == cmd::AerogpuShaderStage::Vertex as u32 => shader::ShaderStage::Vertex,
                    s if s == cmd::AerogpuShaderStage::Pixel as u32 => shader::ShaderStage::Pixel,
                    _ => {
                        return Err(AerogpuD3d9Error::Validation(format!(
                            "CREATE_SHADER_DXBC: unsupported stage {stage}"
                        )));
                    }
                };

                let key = xxhash_rust::xxh3::xxh3_64(dxbc_bytes);
                let cached = self
                    .shader_cache
                    .get_or_translate(dxbc_bytes)
                    .map_err(|e| AerogpuD3d9Error::ShaderTranslation(e.to_string()))?;
                let bytecode_stage = cached.ir.version.stage;
                if expected_stage != bytecode_stage {
                    return Err(AerogpuD3d9Error::ShaderStageMismatch {
                        shader_handle,
                        expected: expected_stage,
                        actual: bytecode_stage,
                    });
                }
                let wgsl = cached.wgsl.wgsl.clone();
                let module = self
                    .device
                    .create_shader_module(wgpu::ShaderModuleDescriptor {
                        label: Some("aerogpu-d3d9.shader"),
                        source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(wgsl.as_str())),
                    });
                let mut used_samplers_mask = 0u16;
                for &s in &cached.ir.used_samplers {
                    if (s as usize) < MAX_SAMPLERS {
                        used_samplers_mask |= 1u16 << s;
                    } else {
                        debug!(
                            shader_handle,
                            sampler = s,
                            "shader uses out-of-range sampler index"
                        );
                    }
                }
                self.shaders.insert(
                    shader_handle,
                    Shader {
                        stage: bytecode_stage,
                        key,
                        module,
                        wgsl,
                        entry_point: cached.wgsl.entry_point,
                        uses_semantic_locations: cached.ir.uses_semantic_locations
                            && bytecode_stage == shader::ShaderStage::Vertex,
                        used_samplers_mask,
                    },
                );
                Ok(())
            }
            AeroGpuCmd::DestroyShader { shader_handle } => {
                self.shaders.remove(&shader_handle);
                Ok(())
            }
            AeroGpuCmd::BindShaders { vs, ps, .. } => {
                self.state.vs = vs;
                self.state.ps = ps;
                // Bind group selection depends on which shader stage references each sampler.
                self.bind_group_dirty = true;
                Ok(())
            }
            AeroGpuCmd::SetShaderConstantsF {
                stage,
                start_register,
                vec4_count,
                data,
                ..
            } => {
                if data.is_empty() {
                    return Ok(());
                }

                // D3D9 keeps separate constant register files per stage; match the shader
                // translation layout (vertex constants first, then pixel constants).
                let stage_base = match stage {
                    s if s == cmd::AerogpuShaderStage::Vertex as u32 => 0u64,
                    s if s == cmd::AerogpuShaderStage::Pixel as u32 => 256u64 * 16,
                    _ => {
                        return Err(AerogpuD3d9Error::Validation(format!(
                            "SET_SHADER_CONSTANTS_F: unsupported stage {stage}"
                        )));
                    }
                };

                let end_register = start_register.checked_add(vec4_count).ok_or_else(|| {
                    AerogpuD3d9Error::Validation(
                        "SET_SHADER_CONSTANTS_F: register range overflow".into(),
                    )
                })?;
                if end_register > 256 {
                    return Err(AerogpuD3d9Error::Validation(format!(
                        "SET_SHADER_CONSTANTS_F: register range out of bounds (start_register={start_register} vec4_count={vec4_count})"
                    )));
                }

                let offset = stage_base
                    .checked_add(start_register as u64 * 16)
                    .ok_or_else(|| {
                        AerogpuD3d9Error::Validation(
                            "SET_SHADER_CONSTANTS_F: register offset overflow".into(),
                        )
                    })?;
                let end_offset = offset.checked_add(data.len() as u64).ok_or_else(|| {
                    AerogpuD3d9Error::Validation(
                        "SET_SHADER_CONSTANTS_F: data length overflow".into(),
                    )
                })?;
                if end_offset > CONSTANTS_BUFFER_SIZE_BYTES as u64 {
                    return Err(AerogpuD3d9Error::Validation(format!(
                        "SET_SHADER_CONSTANTS_F: upload out of bounds (end_offset={end_offset} buffer_size={})",
                        CONSTANTS_BUFFER_SIZE_BYTES
                    )));
                }

                // Use an encoder-ordered copy to guarantee the constants update is visible to
                // subsequent draws in the same submission.
                self.ensure_encoder();
                let staging = self
                    .device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("aerogpu-d3d9.constants_staging"),
                        contents: data,
                        usage: wgpu::BufferUsages::COPY_SRC,
                    });
                let mut encoder = self.encoder.take().unwrap();
                encoder.copy_buffer_to_buffer(
                    &staging,
                    0,
                    &self.constants_buffer,
                    offset,
                    data.len() as u64,
                );
                self.encoder = Some(encoder);
                Ok(())
            }
            AeroGpuCmd::CreateInputLayout {
                input_layout_handle,
                blob_bytes,
                ..
            } => {
                if input_layout_handle == 0 {
                    return Err(AerogpuD3d9Error::Validation(
                        "CREATE_INPUT_LAYOUT: input layout handle 0 is reserved".into(),
                    ));
                }
                if self.handle_in_use(input_layout_handle) {
                    return Err(AerogpuD3d9Error::InputLayoutHandleInUse(
                        input_layout_handle,
                    ));
                }
                let decl = VertexDeclaration::from_d3d_bytes(blob_bytes)
                    .map_err(|e| AerogpuD3d9Error::VertexDeclaration(e.to_string()))?;
                self.input_layouts
                    .insert(input_layout_handle, InputLayout { decl });
                Ok(())
            }
            AeroGpuCmd::DestroyInputLayout {
                input_layout_handle,
            } => {
                self.input_layouts.remove(&input_layout_handle);
                Ok(())
            }
            AeroGpuCmd::SetInputLayout {
                input_layout_handle,
            } => {
                self.state.input_layout = input_layout_handle;
                Ok(())
            }
            AeroGpuCmd::SetBlendState { state } => {
                self.state.blend_state = BlendState {
                    enable: state.enable != 0,
                    src_factor: state.src_factor,
                    dst_factor: state.dst_factor,
                    blend_op: state.blend_op,
                    src_factor_alpha: state.src_factor_alpha,
                    dst_factor_alpha: state.dst_factor_alpha,
                    blend_op_alpha: state.blend_op_alpha,
                    color_write_mask: [state.color_write_mask; 8],
                };
                self.state.blend_constant = state.blend_constant_rgba_f32.map(f32::from_bits);
                self.state.sample_mask = state.sample_mask;
                Ok(())
            }
            AeroGpuCmd::SetDepthStencilState { state } => {
                self.state.depth_stencil_state = DepthStencilState {
                    depth_enable: state.depth_enable != 0,
                    depth_write_enable: state.depth_write_enable != 0,
                    depth_func: state.depth_func,
                    stencil_enable: state.stencil_enable != 0,
                    stencil_read_mask: state.stencil_read_mask,
                    stencil_write_mask: state.stencil_write_mask,
                    ..Default::default()
                };
                Ok(())
            }
            AeroGpuCmd::SetRasterizerState { state } => {
                self.state.rasterizer_state = RasterizerState {
                    cull_mode: state.cull_mode,
                    front_ccw: state.front_ccw != 0,
                    scissor_enable: state.scissor_enable != 0,
                    depth_bias: state.depth_bias,
                };
                Ok(())
            }
            AeroGpuCmd::SetRenderTargets {
                color_count,
                depth_stencil,
                colors,
            } => {
                self.state.render_targets = RenderTargetsState {
                    color_count,
                    depth_stencil,
                    colors,
                };
                Ok(())
            }
            AeroGpuCmd::SetViewport {
                x_f32,
                y_f32,
                width_f32,
                height_f32,
                min_depth_f32,
                max_depth_f32,
            } => {
                self.state.viewport = Some(ViewportState {
                    x: f32::from_bits(x_f32),
                    y: f32::from_bits(y_f32),
                    width: f32::from_bits(width_f32),
                    height: f32::from_bits(height_f32),
                    min_depth: f32::from_bits(min_depth_f32),
                    max_depth: f32::from_bits(max_depth_f32),
                });
                Ok(())
            }
            AeroGpuCmd::SetScissor {
                x,
                y,
                width,
                height,
            } => {
                if width <= 0 || height <= 0 {
                    self.state.scissor = Some((0, 0, 0, 0));
                    return Ok(());
                }

                // Treat `x/y` as signed origins, and clamp the resulting rectangle to the
                // non-negative plane before later clamping to the current render target bounds.
                // This matches the D3D11 executor's scissor handling and avoids widening the
                // rectangle when `x/y` are negative.
                let left = x.max(0);
                let top = y.max(0);
                let right = x.saturating_add(width).max(0);
                let bottom = y.saturating_add(height).max(0);
                if right <= left || bottom <= top {
                    self.state.scissor = Some((0, 0, 0, 0));
                    return Ok(());
                }
                self.state.scissor = Some((
                    left as u32,
                    top as u32,
                    (right - left) as u32,
                    (bottom - top) as u32,
                ));
                Ok(())
            }
            AeroGpuCmd::SetVertexBuffers {
                start_slot,
                buffer_count,
                bindings_bytes,
            } => {
                let start = start_slot as usize;
                let count = buffer_count as usize;
                let end = start.checked_add(count).ok_or_else(|| {
                    AerogpuD3d9Error::Validation("SET_VERTEX_BUFFERS: slot range overflow".into())
                })?;
                if end > self.state.vertex_buffers.len() {
                    return Err(AerogpuD3d9Error::Validation(
                        "SET_VERTEX_BUFFERS: slot range out of bounds".into(),
                    ));
                }
                for i in 0..count {
                    let base = i * 16;
                    let binding = VertexBufferBinding {
                        buffer: u32::from_le_bytes(
                            bindings_bytes[base..base + 4].try_into().unwrap(),
                        ),
                        stride_bytes: u32::from_le_bytes(
                            bindings_bytes[base + 4..base + 8].try_into().unwrap(),
                        ),
                        offset_bytes: u32::from_le_bytes(
                            bindings_bytes[base + 8..base + 12].try_into().unwrap(),
                        ),
                    };
                    self.state.vertex_buffers[start + i] = if binding.buffer == 0 {
                        None
                    } else {
                        Some(binding)
                    };
                }
                Ok(())
            }
            AeroGpuCmd::SetIndexBuffer {
                buffer,
                format,
                offset_bytes,
            } => {
                if buffer == 0 {
                    self.state.index_buffer = None;
                    return Ok(());
                }
                let format = match format {
                    f if f == cmd::AerogpuIndexFormat::Uint16 as u32 => wgpu::IndexFormat::Uint16,
                    f if f == cmd::AerogpuIndexFormat::Uint32 as u32 => wgpu::IndexFormat::Uint32,
                    _ => {
                        return Err(AerogpuD3d9Error::Validation(format!(
                            "SET_INDEX_BUFFER: unknown index format {format}"
                        )))
                    }
                };
                self.state.index_buffer = Some(IndexBufferBinding {
                    buffer,
                    format,
                    offset_bytes,
                });
                Ok(())
            }
            AeroGpuCmd::SetPrimitiveTopology { topology } => {
                let wgpu_topology = map_topology(topology)?;
                self.state.topology_raw = topology;
                self.state.topology = wgpu_topology;
                Ok(())
            }
            AeroGpuCmd::SetTexture {
                shader_stage,
                slot,
                texture,
            } => {
                let slot_idx = slot as usize;
                if slot_idx >= MAX_SAMPLERS {
                    return Ok(());
                }

                match shader_stage {
                    s if s == cmd::AerogpuShaderStage::Vertex as u32 => {
                        self.state.textures_vs[slot_idx] = texture;
                        self.bind_group_dirty = true;
                    }
                    s if s == cmd::AerogpuShaderStage::Pixel as u32 => {
                        self.state.textures_ps[slot_idx] = texture;
                        self.bind_group_dirty = true;
                    }
                    _ => {}
                }
                Ok(())
            }
            AeroGpuCmd::SetSamplerState {
                shader_stage,
                slot,
                state,
                value,
            } => {
                self.set_sampler_state_u32(shader_stage, slot, state, value);
                Ok(())
            }
            AeroGpuCmd::SetRenderState { state, value } => {
                self.set_render_state_u32(state, value);
                Ok(())
            }
            AeroGpuCmd::Clear {
                flags,
                color_rgba_f32,
                depth_f32,
                stencil,
            } => {
                self.ensure_encoder();
                let color_rgba = color_rgba_f32.map(f32::from_bits);
                let depth = f32::from_bits(depth_f32);
                let mut encoder = self.encoder.take().unwrap();
                let result =
                    self.encode_clear(&mut encoder, ctx, flags, color_rgba, depth, stencil);
                self.encoder = Some(encoder);
                result
            }
            AeroGpuCmd::Draw {
                vertex_count,
                instance_count,
                first_vertex,
                first_instance,
            } => {
                self.ensure_encoder();
                let mut encoder = self.encoder.take().unwrap();
                let result = self.encode_draw(
                    &mut encoder,
                    ctx,
                    DrawParams::NonIndexed {
                        vertex_count,
                        instance_count,
                        first_vertex,
                        first_instance,
                    },
                );
                self.encoder = Some(encoder);
                result
            }
            AeroGpuCmd::DrawIndexed {
                index_count,
                instance_count,
                first_index,
                base_vertex,
                first_instance,
            } => {
                self.ensure_encoder();
                let mut encoder = self.encoder.take().unwrap();
                let result = self.encode_draw(
                    &mut encoder,
                    ctx,
                    DrawParams::Indexed {
                        index_count,
                        instance_count,
                        first_index,
                        base_vertex,
                        first_instance,
                    },
                );
                self.encoder = Some(encoder);
                result
            }
            AeroGpuCmd::Present { scanout_id, .. } => {
                self.record_present(scanout_id);
                self.flush()
            }
            AeroGpuCmd::PresentEx { scanout_id, .. } => {
                self.record_present(scanout_id);
                self.flush()
            }
            AeroGpuCmd::Flush => self.flush(),
            AeroGpuCmd::ExportSharedSurface {
                resource_handle,
                share_token,
            } => {
                if resource_handle == 0 {
                    return Err(AerogpuD3d9Error::Validation(
                        "EXPORT_SHARED_SURFACE: resource handle 0 is reserved".into(),
                    ));
                }
                if share_token == 0 {
                    return Err(AerogpuD3d9Error::Validation(
                        "EXPORT_SHARED_SURFACE: share_token 0 is reserved".into(),
                    ));
                }
                if self.retired_share_tokens.contains(&share_token) {
                    return Err(AerogpuD3d9Error::ShareTokenRetired(share_token));
                }
                let underlying = self.resolve_resource_handle(resource_handle)?;
                match self.resources.get(&underlying) {
                    Some(Resource::Texture2d { .. }) => {}
                    Some(Resource::Buffer { .. }) => {
                        return Err(AerogpuD3d9Error::Validation(format!(
                            "EXPORT_SHARED_SURFACE: only textures can be shared (handle={resource_handle})"
                        )));
                    }
                    None => return Err(AerogpuD3d9Error::UnknownResource(resource_handle)),
                }
                if let Some(existing) = self.shared_surface_by_token.get(&share_token).copied() {
                    if existing != underlying {
                        return Err(AerogpuD3d9Error::ShareTokenAlreadyExported {
                            share_token,
                            existing,
                            new: underlying,
                        });
                    }
                } else {
                    self.shared_surface_by_token.insert(share_token, underlying);
                }
                Ok(())
            }
            AeroGpuCmd::ImportSharedSurface {
                out_resource_handle,
                share_token,
            } => {
                if out_resource_handle == 0 {
                    return Err(AerogpuD3d9Error::Validation(
                        "IMPORT_SHARED_SURFACE: resource handle 0 is reserved".into(),
                    ));
                }
                if share_token == 0 {
                    return Err(AerogpuD3d9Error::Validation(
                        "IMPORT_SHARED_SURFACE: share_token 0 is reserved".into(),
                    ));
                }
                let Some(&underlying) = self.shared_surface_by_token.get(&share_token) else {
                    return Err(AerogpuD3d9Error::UnknownShareToken(share_token));
                };
                if !self.resource_refcounts.contains_key(&underlying) {
                    return Err(AerogpuD3d9Error::UnknownShareToken(share_token));
                }
                match self.resources.get(&underlying) {
                    Some(Resource::Texture2d { .. }) => {}
                    Some(Resource::Buffer { .. }) => {
                        return Err(AerogpuD3d9Error::Validation(format!(
                            "IMPORT_SHARED_SURFACE: token refers to a non-texture resource (share_token=0x{share_token:016X})"
                        )));
                    }
                    None => return Err(AerogpuD3d9Error::UnknownShareToken(share_token)),
                }

                if let Some(existing) = self.resource_handles.get(&out_resource_handle).copied() {
                    if existing != underlying {
                        return Err(AerogpuD3d9Error::SharedSurfaceAliasAlreadyBound {
                            alias: out_resource_handle,
                            existing,
                            new: underlying,
                        });
                    }
                } else {
                    if self.handle_in_use(out_resource_handle) {
                        return Err(AerogpuD3d9Error::ResourceHandleInUse(out_resource_handle));
                    }
                    self.resource_handles
                        .insert(out_resource_handle, underlying);
                    *self.resource_refcounts.entry(underlying).or_insert(0) += 1;
                    // Bringing a previously-unknown alias handle into existence changes how
                    // `SetTexture` bindings resolve. Drop any cached bind groups (including for
                    // other contexts) so subsequent draws re-resolve handles against the updated
                    // alias table.
                    self.invalidate_bind_groups();
                }

                Ok(())
            }
            AeroGpuCmd::ReleaseSharedSurface { share_token } => {
                self.release_shared_surface_token(share_token);
                Ok(())
            }
            AeroGpuCmd::ResourceDirtyRange {
                resource_handle,
                offset_bytes,
                size_bytes,
            } => self.resource_dirty_range(ctx, resource_handle, offset_bytes, size_bytes),
        }
    }

    fn record_present(&mut self, scanout_id: u32) {
        let rt = &self.state.render_targets;
        if rt.color_count == 0 {
            self.presented_scanouts.remove(&scanout_id);
            return;
        }
        let color0 = rt.colors[0];
        if color0 == 0 {
            self.presented_scanouts.remove(&scanout_id);
            return;
        }
        let Ok(underlying) = self.resolve_resource_handle(color0) else {
            self.presented_scanouts.remove(&scanout_id);
            return;
        };
        self.presented_scanouts.insert(scanout_id, underlying);
    }

    fn ensure_encoder(&mut self) {
        if self.encoder.is_some() {
            return;
        }
        self.encoder = Some(
            self.device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("aerogpu-d3d9.encoder"),
                }),
        );
    }

    fn flush(&mut self) -> Result<(), AerogpuD3d9Error> {
        if let Some(encoder) = self.encoder.take() {
            self.queue.submit([encoder.finish()]);
        } else {
            // Still flush pending `queue.write_texture` work (wgpu requires a submit boundary).
            self.queue.submit([]);
        }
        Ok(())
    }

    fn resource_dirty_range(
        &mut self,
        ctx: &mut SubmissionCtx<'_>,
        resource_handle: u32,
        offset_bytes: u64,
        size_bytes: u64,
    ) -> Result<(), AerogpuD3d9Error> {
        if size_bytes == 0 {
            return Ok(());
        }
        let end = offset_bytes.checked_add(size_bytes).ok_or_else(|| {
            AerogpuD3d9Error::Validation("dirty range offset/size overflow".into())
        })?;

        let underlying = self.resolve_resource_handle(resource_handle)?;
        let Some(res) = self.resources.get_mut(&underlying) else {
            return Err(AerogpuD3d9Error::UnknownResource(resource_handle));
        };
        match res {
            Resource::Buffer {
                size,
                backing,
                dirty_ranges,
                ..
            } => {
                let Some(backing) = backing.as_ref() else {
                    return Err(AerogpuD3d9Error::Validation(format!(
                        "RESOURCE_DIRTY_RANGE on host-owned buffer {resource_handle} is not supported (use UPLOAD_RESOURCE)"
                    )));
                };
                ctx.require_alloc_entry(backing.alloc_id)?;
                if end > *size {
                    return Err(AerogpuD3d9Error::Validation(format!(
                        "buffer dirty range out of bounds (handle={resource_handle} end={end} size={size})"
                    )));
                }
                let aligned_start = align_down_u64(offset_bytes, wgpu::COPY_BUFFER_ALIGNMENT);
                let aligned_end = align_up_u64(end, wgpu::COPY_BUFFER_ALIGNMENT)?.min(*size);
                dirty_ranges.push(aligned_start..aligned_end);
                coalesce_ranges(dirty_ranges);
                Ok(())
            }
            Resource::Texture2d {
                backing,
                dirty_ranges,
                ..
            } => {
                let Some(backing) = backing.as_ref() else {
                    return Err(AerogpuD3d9Error::Validation(format!(
                        "RESOURCE_DIRTY_RANGE on host-owned texture {resource_handle} is not supported (use UPLOAD_RESOURCE)"
                    )));
                };
                ctx.require_alloc_entry(backing.alloc_id)?;
                if end > backing.size_bytes {
                    return Err(AerogpuD3d9Error::Validation(format!(
                        "texture dirty range out of bounds (handle={resource_handle} end={end} size={})",
                        backing.size_bytes
                    )));
                }
                dirty_ranges.push(offset_bytes..end);
                coalesce_ranges(dirty_ranges);
                Ok(())
            }
        }
    }

    fn flush_buffer_if_dirty(
        &mut self,
        encoder: Option<&mut wgpu::CommandEncoder>,
        handle: u32,
        ctx: &mut SubmissionCtx<'_>,
    ) -> Result<(), AerogpuD3d9Error> {
        let underlying = self.resolve_resource_handle(handle)?;
        let Some(res) = self.resources.get_mut(&underlying) else {
            return Err(AerogpuD3d9Error::UnknownResource(handle));
        };
        let Resource::Buffer {
            buffer,
            size,
            backing,
            dirty_ranges,
            shadow,
            ..
        } = res
        else {
            return Err(AerogpuD3d9Error::UnknownResource(handle));
        };

        let Some(backing) = backing.as_ref() else {
            return Ok(());
        };
        if dirty_ranges.is_empty() {
            return Ok(());
        }
        let (alloc_gpa, alloc_size_bytes) = {
            let alloc = ctx.require_alloc_entry(backing.alloc_id)?;
            (alloc.gpa, alloc.size_bytes)
        };
        let Some(guest_memory) = ctx.guest_memory.as_deref_mut() else {
            return Err(AerogpuD3d9Error::MissingGuestMemory(handle));
        };

        let mut encoder = encoder;
        let ranges = dirty_ranges.clone();
        for range in &ranges {
            let aligned_start = align_down_u64(range.start, wgpu::COPY_BUFFER_ALIGNMENT);
            let aligned_end = align_up_u64(range.end, wgpu::COPY_BUFFER_ALIGNMENT)?.min(*size);
            let len_u64 = aligned_end.saturating_sub(aligned_start);
            let len = usize::try_from(len_u64)
                .map_err(|_| AerogpuD3d9Error::Validation("buffer dirty range too large".into()))?;
            let mut data = vec![0u8; len];
            let alloc_offset = backing
                .alloc_offset_bytes
                .checked_add(aligned_start)
                .ok_or_else(|| AerogpuD3d9Error::Validation("buffer backing overflow".into()))?;
            let alloc_end = alloc_offset
                .checked_add(len_u64)
                .ok_or_else(|| AerogpuD3d9Error::Validation("buffer backing overflow".into()))?;
            if alloc_end > alloc_size_bytes {
                return Err(AerogpuD3d9Error::Validation(format!(
                    "buffer backing out of bounds (alloc_id={} offset=0x{:x} size=0x{:x} alloc_size=0x{:x})",
                    backing.alloc_id, alloc_offset, len_u64, alloc_size_bytes
                )));
            }

            let src_gpa = alloc_gpa
                .checked_add(alloc_offset)
                .ok_or_else(|| AerogpuD3d9Error::Validation("buffer backing overflow".into()))?;
            if src_gpa.checked_add(len_u64).is_none() {
                return Err(AerogpuD3d9Error::Validation(
                    "buffer backing overflow".into(),
                ));
            }
            guest_memory.read(src_gpa, &mut data)?;

            // Keep CPU shadow in sync with the written range.
            let shadow_start = usize::try_from(aligned_start).map_err(|_| {
                AerogpuD3d9Error::Validation("buffer dirty range offset overflow".into())
            })?;
            let shadow_len = usize::try_from(len_u64).map_err(|_| {
                AerogpuD3d9Error::Validation("buffer dirty range size overflow".into())
            })?;
            let shadow_end = shadow_start.checked_add(shadow_len).ok_or_else(|| {
                AerogpuD3d9Error::Validation("buffer dirty range overflow".into())
            })?;
            if shadow_end > shadow.len() {
                return Err(AerogpuD3d9Error::Validation(format!(
                    "buffer shadow write out of bounds (handle={handle} range=0x{aligned_start:x}..0x{aligned_end:x} shadow_size=0x{:x})",
                    shadow.len()
                )));
            }
            shadow[shadow_start..shadow_end].copy_from_slice(&data);
            if let Some(encoder) = encoder.as_deref_mut() {
                // Prefer encoder-ordered buffer copies to preserve ordering with other operations
                // in this command buffer. If the range isn't aligned for `copy_buffer_to_buffer`,
                // submit the current encoder and fall back to `queue.write_buffer` so we still
                // update exactly the requested bytes.
                if aligned_start.is_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT)
                    && len_u64.is_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT)
                {
                    let staging =
                        self.device
                            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                                label: Some("aerogpu-d3d9.flush_dirty_range.buffer_staging"),
                                contents: &data,
                                usage: wgpu::BufferUsages::COPY_SRC,
                            });
                    encoder.copy_buffer_to_buffer(&staging, 0, buffer, aligned_start, len_u64);
                } else {
                    let submitted = std::mem::replace(
                        encoder,
                        self.device
                            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                                label: Some("aerogpu-d3d9.encoder"),
                            }),
                    );
                    self.queue.submit([submitted.finish()]);
                    self.queue.write_buffer(buffer, aligned_start, &data);
                }
            } else {
                // No active encoder yet: use `queue.write_buffer` as a fast path.
                self.queue.write_buffer(buffer, aligned_start, &data);
            }
        }

        dirty_ranges.clear();
        Ok(())
    }

    fn flush_texture_if_dirty(
        &mut self,
        encoder: Option<&mut wgpu::CommandEncoder>,
        handle: u32,
        ctx: &mut SubmissionCtx<'_>,
        strict: bool,
    ) -> Result<(), AerogpuD3d9Error> {
        let underlying = match self.resolve_resource_handle(handle) {
            Ok(h) => h,
            Err(err) => {
                return if strict { Err(err) } else { Ok(()) };
            }
        };

        let Some(res) = self.resources.get_mut(&underlying) else {
            return if strict {
                Err(AerogpuD3d9Error::UnknownResource(handle))
            } else {
                Ok(())
            };
        };
        let Resource::Texture2d {
            texture,
            format_raw,
            format,
            width,
            height,
            mip_level_count,
            array_layers,
            backing,
            dirty_ranges,
            ..
        } = res
        else {
            return if strict {
                Err(AerogpuD3d9Error::UnknownResource(handle))
            } else {
                Ok(())
            };
        };

        let Some(backing) = backing.as_ref() else {
            return Ok(());
        };
        if dirty_ranges.is_empty() {
            return Ok(());
        }
        let (alloc_gpa, alloc_size_bytes) = {
            let alloc = ctx.require_alloc_entry(backing.alloc_id)?;
            (alloc.gpa, alloc.size_bytes)
        };
        let Some(guest_memory) = ctx.guest_memory.as_deref_mut() else {
            return Err(AerogpuD3d9Error::MissingGuestMemory(handle));
        };
        let backing_end = backing
            .alloc_offset_bytes
            .checked_add(backing.size_bytes)
            .ok_or_else(|| AerogpuD3d9Error::Validation("texture backing overflow".into()))?;
        if backing_end > alloc_size_bytes {
            return Err(AerogpuD3d9Error::Validation(format!(
                "texture backing out of bounds (alloc_id={} offset=0x{:x} size=0x{:x} alloc_size=0x{:x})",
                backing.alloc_id,
                backing.alloc_offset_bytes,
                backing.size_bytes,
                alloc_size_bytes
            )));
        }

        let mut encoder = encoder;
        let ranges = dirty_ranges.clone();

        let layout = guest_texture_linear_layout(
            *format_raw,
            *width,
            *height,
            *mip_level_count,
            *array_layers,
            backing.row_pitch_bytes,
        )?;
        debug_assert_eq!(layout.total_size_bytes, backing.size_bytes);
        let base_gpa = alloc_gpa
            .checked_add(backing.alloc_offset_bytes)
            .ok_or_else(|| AerogpuD3d9Error::Validation("texture backing overflow".into()))?;

        #[allow(clippy::too_many_arguments)]
        fn read_rows_into(
            guest_memory: &mut dyn GuestMemory,
            base_gpa: u64,
            src_row_pitch_bytes: u32,
            dst_row_pitch_bytes: u32,
            row_len_bytes: u32,
            rows: u32,
            force_opaque_alpha: bool,
        ) -> Result<Vec<u8>, AerogpuD3d9Error> {
            let row_len_usize: usize = row_len_bytes.try_into().map_err(|_| {
                AerogpuD3d9Error::Validation("texture row size out of range".into())
            })?;
            let dst_pitch_usize: usize = dst_row_pitch_bytes.try_into().map_err(|_| {
                AerogpuD3d9Error::Validation("texture row pitch out of range".into())
            })?;
            let rows_usize: usize = rows.try_into().map_err(|_| {
                AerogpuD3d9Error::Validation("texture rows out of range".into())
            })?;
            let total = dst_pitch_usize.checked_mul(rows_usize).ok_or_else(|| {
                AerogpuD3d9Error::Validation("texture upload staging size overflow".into())
            })?;
            let mut out = vec![0u8; total];

            for row in 0..rows {
                let src_off = u64::from(src_row_pitch_bytes)
                    .checked_mul(u64::from(row))
                    .ok_or_else(|| AerogpuD3d9Error::Validation("texture backing overflow".into()))?;
                let src_gpa = base_gpa.checked_add(src_off).ok_or_else(|| {
                    AerogpuD3d9Error::Validation("texture backing overflow".into())
                })?;
                if src_gpa.checked_add(u64::from(row_len_bytes)).is_none() {
                    return Err(AerogpuD3d9Error::Validation(
                        "texture backing overflow".into(),
                    ));
                }

                let dst_start = (row as usize)
                    .checked_mul(dst_pitch_usize)
                    .ok_or_else(|| {
                        AerogpuD3d9Error::Validation("texture upload staging size overflow".into())
                    })?;
                let dst_end = dst_start.checked_add(row_len_usize).ok_or_else(|| {
                    AerogpuD3d9Error::Validation("texture upload staging size overflow".into())
                })?;
                let dst_slice = out.get_mut(dst_start..dst_end).ok_or_else(|| {
                    AerogpuD3d9Error::Validation("texture upload staging out of bounds".into())
                })?;

                guest_memory.read(src_gpa, dst_slice)?;
                if force_opaque_alpha {
                    force_opaque_alpha_rgba8(dst_slice);
                }
            }

            Ok(out)
        }

        #[allow(clippy::too_many_arguments)]
        fn upload_subresource(
            device: &wgpu::Device,
            queue: &wgpu::Queue,
            encoder: Option<&mut wgpu::CommandEncoder>,
            texture: &wgpu::Texture,
            mip_level: u32,
            array_layer: u32,
            width: u32,
            height: u32,
            bytes: &[u8],
            bytes_per_row: u32,
            rows_per_image: u32,
        ) {
            if let Some(encoder) = encoder {
                let staging = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("aerogpu-d3d9.flush_dirty_range.texture_staging"),
                    contents: bytes,
                    usage: wgpu::BufferUsages::COPY_SRC,
                });
                encoder.copy_buffer_to_texture(
                    wgpu::ImageCopyBuffer {
                        buffer: &staging,
                        layout: wgpu::ImageDataLayout {
                            offset: 0,
                            bytes_per_row: Some(bytes_per_row),
                            rows_per_image: Some(rows_per_image),
                        },
                    },
                    wgpu::ImageCopyTexture {
                        texture,
                        mip_level,
                        origin: wgpu::Origin3d {
                            x: 0,
                            y: 0,
                            z: array_layer,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                );
            } else {
                queue.write_texture(
                    wgpu::ImageCopyTexture {
                        texture,
                        mip_level,
                        origin: wgpu::Origin3d {
                            x: 0,
                            y: 0,
                            z: array_layer,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    bytes,
                    wgpu::ImageDataLayout {
                        offset: 0,
                        bytes_per_row: Some(bytes_per_row),
                        rows_per_image: Some(rows_per_image),
                    },
                    wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                );
            }
        }

        let src_block = aerogpu_format_texel_block_info(*format_raw)?;
        let bc_format = aerogpu_format_bc(*format_raw);
        let dst_is_bc = matches!(
            *format,
            wgpu::TextureFormat::Bc1RgbaUnorm
                | wgpu::TextureFormat::Bc2RgbaUnorm
                | wgpu::TextureFormat::Bc3RgbaUnorm
                | wgpu::TextureFormat::Bc7RgbaUnorm
        );

        // Upload whole subresources (layer-major, then mip-major).
        for layer in 0..*array_layers {
            let layer_off = layout
                .layer_stride_bytes
                .checked_mul(layer as u64)
                .ok_or_else(|| AerogpuD3d9Error::Validation("texture backing overflow".into()))?;
            for mip in 0..*mip_level_count {
                let in_layer_off = layout.mip_offsets[mip as usize];
                let in_layer_end = if mip + 1 < *mip_level_count {
                    layout.mip_offsets[(mip + 1) as usize]
                } else {
                    layout.layer_stride_bytes
                };
                let sub_size = in_layer_end
                    .checked_sub(in_layer_off)
                    .ok_or_else(|| {
                        AerogpuD3d9Error::Validation("texture backing overflow".into())
                    })?;
                let sub_start = layer_off.checked_add(in_layer_off).ok_or_else(|| {
                    AerogpuD3d9Error::Validation("texture backing overflow".into())
                })?;
                let sub_end = sub_start.checked_add(sub_size).ok_or_else(|| {
                    AerogpuD3d9Error::Validation("texture backing overflow".into())
                })?;

                // Skip untouched subresources to avoid reading uninitialized guest memory.
                if !ranges.iter().any(|r| r.start < sub_end && r.end > sub_start) {
                    continue;
                }

                let mip_w = mip_extent(*width, mip);
                let mip_h = mip_extent(*height, mip);

                let src_unpadded_bpr = src_block.row_pitch_bytes(mip_w)?;
                let src_rows = src_block.rows_per_image(mip_h);
                let src_row_pitch = if mip == 0 {
                    backing.row_pitch_bytes
                } else {
                    src_unpadded_bpr
                };
                if src_row_pitch == 0 {
                    return Err(AerogpuD3d9Error::Validation(
                        "texture row_pitch_bytes is 0".into(),
                    ));
                }

                let sub_gpa = base_gpa.checked_add(sub_start).ok_or_else(|| {
                    AerogpuD3d9Error::Validation("texture backing overflow".into())
                })?;

                if let (Some(bc), false) = (bc_format, dst_is_bc) {
                    // CPU fallback: BC -> RGBA8
                    let bc_tight = read_rows_into(
                        guest_memory,
                        sub_gpa,
                        src_row_pitch,
                        src_unpadded_bpr,
                        src_unpadded_bpr,
                        src_rows,
                        false,
                    )?;
                    let rgba = match bc {
                        BcFormat::Bc1 => decompress_bc1_rgba8(mip_w, mip_h, &bc_tight),
                        BcFormat::Bc2 => decompress_bc2_rgba8(mip_w, mip_h, &bc_tight),
                        BcFormat::Bc3 => decompress_bc3_rgba8(mip_w, mip_h, &bc_tight),
                        BcFormat::Bc7 => decompress_bc7_rgba8(mip_w, mip_h, &bc_tight),
                    };

                    let dst_unpadded_bpr = mip_w.checked_mul(4).ok_or_else(|| {
                        AerogpuD3d9Error::Validation("texture bytes_per_row overflow".into())
                    })?;
                    // `copy_buffer_to_texture` requires the stride to respect `COPY_BYTES_PER_ROW_ALIGNMENT`,
                    // even for single-row copies.
                    let dst_bpr = align_to(dst_unpadded_bpr, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);

                    let upload = if dst_bpr == dst_unpadded_bpr {
                        rgba
                    } else {
                        let height_usize: usize = mip_h.try_into().map_err(|_| {
                            AerogpuD3d9Error::Validation("texture height out of range".into())
                        })?;
                        let dst_bpr_usize: usize = dst_bpr.try_into().map_err(|_| {
                            AerogpuD3d9Error::Validation("texture bytes_per_row out of range".into())
                        })?;
                        let unpadded_usize: usize = dst_unpadded_bpr.try_into().unwrap();
                        let mut padded = vec![0u8; dst_bpr_usize * height_usize];
                        for row in 0..height_usize {
                            let src_start = row * unpadded_usize;
                            let dst_start = row * dst_bpr_usize;
                            padded[dst_start..dst_start + unpadded_usize]
                                .copy_from_slice(&rgba[src_start..src_start + unpadded_usize]);
                        }
                        padded
                    };

                    upload_subresource(
                        &self.device,
                        &self.queue,
                        encoder.as_deref_mut(),
                        texture,
                        mip,
                        layer,
                        mip_w,
                        mip_h,
                        &upload,
                        dst_bpr,
                        mip_h,
                    );
                } else if matches!(
                    *format_raw,
                    x if x == AerogpuFormat::B5G6R5Unorm as u32
                        || x == AerogpuFormat::B5G5R5A1Unorm as u32
                ) {
                    // CPU expansion: 16-bit packed -> RGBA8
                    let dst_unpadded_bpr = mip_w.checked_mul(4).ok_or_else(|| {
                        AerogpuD3d9Error::Validation("texture bytes_per_row overflow".into())
                    })?;
                    let dst_bpr = align_to(dst_unpadded_bpr, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);

                    let height_usize: usize = mip_h.try_into().map_err(|_| {
                        AerogpuD3d9Error::Validation("texture height out of range".into())
                    })?;
                    let dst_bpr_usize: usize = dst_bpr.try_into().map_err(|_| {
                        AerogpuD3d9Error::Validation("texture bytes_per_row out of range".into())
                    })?;
                    let dst_unpadded_usize: usize =
                        dst_unpadded_bpr.try_into().map_err(|_| {
                            AerogpuD3d9Error::Validation("texture row size out of range".into())
                        })?;
                    let total = dst_bpr_usize.checked_mul(height_usize).ok_or_else(|| {
                        AerogpuD3d9Error::Validation("texture upload staging size overflow".into())
                    })?;
                    let mut staging = vec![0u8; total];

                    let src_row_len_usize: usize = src_unpadded_bpr.try_into().map_err(|_| {
                        AerogpuD3d9Error::Validation("texture row size out of range".into())
                    })?;
                    let mut row_buf = vec![0u8; src_row_len_usize];

                    for row in 0..mip_h {
                        let src_off = u64::from(src_row_pitch)
                            .checked_mul(u64::from(row))
                            .ok_or_else(|| {
                                AerogpuD3d9Error::Validation("texture backing overflow".into())
                            })?;
                        let src_row_gpa = sub_gpa.checked_add(src_off).ok_or_else(|| {
                            AerogpuD3d9Error::Validation("texture backing overflow".into())
                        })?;
                        if src_row_gpa
                            .checked_add(u64::from(src_unpadded_bpr))
                            .is_none()
                        {
                            return Err(AerogpuD3d9Error::Validation(
                                "texture backing overflow".into(),
                            ));
                        }
                        guest_memory.read(src_row_gpa, &mut row_buf)?;

                        let row_usize: usize = row.try_into().unwrap();
                        let dst_start = row_usize
                            .checked_mul(dst_bpr_usize)
                            .ok_or_else(|| {
                                AerogpuD3d9Error::Validation(
                                    "texture upload staging size overflow".into(),
                                )
                            })?;
                        let dst_end = dst_start.checked_add(dst_unpadded_usize).ok_or_else(|| {
                            AerogpuD3d9Error::Validation(
                                "texture upload staging size overflow".into(),
                            )
                        })?;
                        let dst = staging.get_mut(dst_start..dst_end).ok_or_else(|| {
                            AerogpuD3d9Error::Validation(
                                "texture upload staging out of bounds".into(),
                            )
                        })?;

                        match *format_raw {
                            x if x == AerogpuFormat::B5G6R5Unorm as u32 => {
                                expand_b5g6r5_unorm_to_rgba8(&row_buf, dst)
                            }
                            x if x == AerogpuFormat::B5G5R5A1Unorm as u32 => {
                                expand_b5g5r5a1_unorm_to_rgba8(&row_buf, dst)
                            }
                            _ => unreachable!("matches! above only allows B5* formats"),
                        }
                    }

                    upload_subresource(
                        &self.device,
                        &self.queue,
                        encoder.as_deref_mut(),
                        texture,
                        mip,
                        layer,
                        mip_w,
                        mip_h,
                        &staging,
                        dst_bpr,
                        mip_h,
                    );
                } else {
                    // Direct upload (RGBA/BGRA/depth or BC when supported).
                    // `copy_buffer_to_texture` requires the stride to respect `COPY_BYTES_PER_ROW_ALIGNMENT`,
                    // even for single-row copies.
                    let upload_bpr = align_to(src_unpadded_bpr, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
                    let force_opaque_alpha = is_x8_format(*format_raw)
                        && src_block.block_width == 1
                        && src_block.block_height == 1
                        && src_block.bytes_per_block == 4;
                    let upload = read_rows_into(
                        guest_memory,
                        sub_gpa,
                        src_row_pitch,
                        upload_bpr,
                        src_unpadded_bpr,
                        src_rows,
                        force_opaque_alpha,
                    )?;

                    upload_subresource(
                        &self.device,
                        &self.queue,
                        encoder.as_deref_mut(),
                        texture,
                        mip,
                        layer,
                        mip_w,
                        mip_h,
                        &upload,
                        upload_bpr,
                        src_rows,
                    );
                }
            }
        }

        dirty_ranges.clear();
        Ok(())
    }

    fn flush_texture_if_dirty_strict(
        &mut self,
        encoder: Option<&mut wgpu::CommandEncoder>,
        handle: u32,
        ctx: &mut SubmissionCtx<'_>,
    ) -> Result<(), AerogpuD3d9Error> {
        self.flush_texture_if_dirty(encoder, handle, ctx, true)
    }

    fn flush_texture_binding_if_dirty(
        &mut self,
        encoder: Option<&mut wgpu::CommandEncoder>,
        handle: u32,
        ctx: &mut SubmissionCtx<'_>,
    ) -> Result<(), AerogpuD3d9Error> {
        self.flush_texture_if_dirty(encoder, handle, ctx, false)
    }

    fn encode_clear(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        ctx: &mut SubmissionCtx<'_>,
        flags: u32,
        color_rgba: [f32; 4],
        depth: f32,
        stencil: u32,
    ) -> Result<(), AerogpuD3d9Error> {
        let clear_color_enabled = (flags & cmd::AEROGPU_CLEAR_COLOR) != 0;
        let clear_depth_enabled = (flags & cmd::AEROGPU_CLEAR_DEPTH) != 0;
        let clear_stencil_enabled = (flags & cmd::AEROGPU_CLEAR_STENCIL) != 0;

        // `wgpu::LoadOp::Clear` ignores the render pass scissor rectangle. If a D3D9 caller is
        // using `SetScissorRect` + `Clear` to implement rectangle clears (Win7 D3D9 semantics),
        // we must preserve pixels outside the scissor region.
        let scissor = self.state.scissor;
        let scissor_enabled = self.state.rasterizer_state.scissor_enable && scissor.is_some();
        let scissor_is_subrect = if scissor_enabled
            && (clear_color_enabled || clear_depth_enabled || clear_stencil_enabled)
        {
            let (rt_w, rt_h) = self.render_target_extent()?;
            let (x, y, w, h) = scissor.expect("scissor_enabled implies scissor is Some");
            let clamped = clamp_scissor_rect(x, y, w, h, rt_w, rt_h);
            clamped != Some((0, 0, rt_w, rt_h))
        } else {
            false
        };

        if scissor_is_subrect {
            return self.encode_clear_scissored(encoder, ctx, flags, color_rgba, depth, stencil);
        }

        if !clear_color_enabled || !clear_depth_enabled || !clear_stencil_enabled {
            let rt = self.state.render_targets;
            if !clear_color_enabled {
                for slot in 0..rt.color_count.min(8) as usize {
                    let handle = rt.colors[slot];
                    if handle == 0 {
                        continue;
                    }
                    self.flush_texture_if_dirty_strict(Some(encoder), handle, ctx)?;
                }
            }
            if (!clear_depth_enabled || !clear_stencil_enabled) && rt.depth_stencil != 0 {
                self.flush_texture_if_dirty_strict(Some(encoder), rt.depth_stencil, ctx)?;
            }
        }

        let (color_attachments, depth_stencil) = self.render_target_attachments()?;
        let (_, color_is_x8, depth_format) = self.render_target_formats()?;
        let depth_has_stencil =
            matches!(depth_format, Some(wgpu::TextureFormat::Depth24PlusStencil8));

        let clear_color = wgpu::Color {
            r: color_rgba[0] as f64,
            g: color_rgba[1] as f64,
            b: color_rgba[2] as f64,
            a: color_rgba[3] as f64,
        };
        let clear_color_opaque = wgpu::Color {
            r: clear_color.r,
            g: clear_color.g,
            b: clear_color.b,
            a: 1.0,
        };

        let mut color_attachments_out = Vec::with_capacity(color_attachments.len());
        for (idx, attachment) in color_attachments.into_iter().enumerate() {
            let Some(view) = attachment else {
                color_attachments_out.push(None);
                continue;
            };
            let clear_color_for_rt = if color_is_x8.get(idx).copied().unwrap_or(false) {
                clear_color_opaque
            } else {
                clear_color
            };
            color_attachments_out.push(Some(wgpu::RenderPassColorAttachment {
                view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: if clear_color_enabled {
                        wgpu::LoadOp::Clear(clear_color_for_rt)
                    } else {
                        wgpu::LoadOp::Load
                    },
                    store: wgpu::StoreOp::Store,
                },
            }));
        }

        let depth_attachment = depth_stencil.map(|view| wgpu::RenderPassDepthStencilAttachment {
            view,
            depth_ops: Some(wgpu::Operations {
                load: if clear_depth_enabled {
                    wgpu::LoadOp::Clear(depth)
                } else {
                    wgpu::LoadOp::Load
                },
                store: wgpu::StoreOp::Store,
            }),
            stencil_ops: depth_has_stencil.then_some(wgpu::Operations {
                load: if clear_stencil_enabled {
                    wgpu::LoadOp::Clear(stencil & 0xFF)
                } else {
                    wgpu::LoadOp::Load
                },
                store: wgpu::StoreOp::Store,
            }),
        });

        let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("aerogpu-d3d9.clear"),
            color_attachments: &color_attachments_out,
            depth_stencil_attachment: depth_attachment,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        Ok(())
    }

    fn encode_clear_scissored(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        ctx: &mut SubmissionCtx<'_>,
        flags: u32,
        color_rgba: [f32; 4],
        depth: f32,
        stencil: u32,
    ) -> Result<(), AerogpuD3d9Error> {
        let clear_color_enabled = (flags & cmd::AEROGPU_CLEAR_COLOR) != 0;
        let clear_depth_enabled = (flags & cmd::AEROGPU_CLEAR_DEPTH) != 0;
        let clear_stencil_enabled = (flags & cmd::AEROGPU_CLEAR_STENCIL) != 0;

        let rt = self.state.render_targets;

        // When doing a scissored clear we must preserve pixels outside the scissor region.
        // That means any attachment we touch needs to be loaded, so flush pending guest writes.
        if clear_color_enabled {
            for slot in 0..rt.color_count.min(8) as usize {
                let handle = rt.colors[slot];
                if handle == 0 {
                    continue;
                }
                self.flush_texture_if_dirty_strict(Some(encoder), handle, ctx)?;
            }
        }
        let scissor = self
            .state
            .scissor
            .expect("encode_clear_scissored requires scissor to be set");

        let mut params_bytes = [0u8; 32];
        for (i, f) in color_rgba.iter().enumerate() {
            params_bytes[i * 4..i * 4 + 4].copy_from_slice(&f.to_le_bytes());
        }
        params_bytes[16..20].copy_from_slice(&depth.to_le_bytes());
        let mut params_bytes_opaque = params_bytes;
        params_bytes_opaque[12..16].copy_from_slice(&1.0f32.to_le_bytes());

        let staging_normal = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("aerogpu-d3d9.clear_params_staging"),
                contents: &params_bytes,
                usage: wgpu::BufferUsages::COPY_SRC,
            });
        let staging_opaque = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("aerogpu-d3d9.clear_params_staging_opaque"),
                contents: &params_bytes_opaque,
                usage: wgpu::BufferUsages::COPY_SRC,
            });
        encoder.copy_buffer_to_buffer(&staging_normal, 0, &self.clear_color_buffer, 0, 32);

        if clear_color_enabled {
            let srgb_write = self
                .state
                .render_states
                .get(d3d9::D3DRS_SRGBWRITEENABLE as usize)
                .copied()
                .unwrap_or(0)
                != 0;

            // Collect render target formats/extents so we can build per-format pipelines without
            // holding borrows into `self.resources`.
            let mut targets: Vec<(u32, wgpu::TextureFormat, u32, u32, bool, bool)> = Vec::new();
            for slot in 0..rt.color_count.min(8) as usize {
                let handle = rt.colors[slot];
                if handle == 0 {
                    continue;
                }
                let underlying = self.resolve_resource_handle(handle)?;
                let res = self
                    .resources
                    .get(&underlying)
                    .ok_or(AerogpuD3d9Error::UnknownResource(handle))?;
                match res {
                    Resource::Texture2d {
                        format,
                        format_raw,
                        view_srgb,
                        width,
                        height,
                        ..
                    } => {
                        let is_x8 = is_x8_format(*format_raw);
                        let mut out_format = *format;
                        let mut use_srgb_view = false;
                        if srgb_write && view_srgb.is_some() {
                            out_format = match out_format {
                                wgpu::TextureFormat::Rgba8Unorm => {
                                    wgpu::TextureFormat::Rgba8UnormSrgb
                                }
                                wgpu::TextureFormat::Bgra8Unorm => {
                                    wgpu::TextureFormat::Bgra8UnormSrgb
                                }
                                other => other,
                            };
                            use_srgb_view = out_format != *format;
                        }
                        targets.push((underlying, out_format, *width, *height, is_x8, use_srgb_view))
                    }
                    _ => return Err(AerogpuD3d9Error::UnknownResource(handle)),
                }
            }

            let mut current_is_x8 = false;
            for (underlying, format, width, height, is_x8, use_srgb_view) in targets {
                let Some((x, y, w, h)) =
                    clamp_scissor_rect(scissor.0, scissor.1, scissor.2, scissor.3, width, height)
                else {
                    continue;
                };

                self.ensure_clear_pipeline(format);
                let pipeline = self.clear_pipeline(format);
                if is_x8 != current_is_x8 {
                    let src = if is_x8 {
                        &staging_opaque
                    } else {
                        &staging_normal
                    };
                    encoder.copy_buffer_to_buffer(src, 0, &self.clear_color_buffer, 0, 32);
                    current_is_x8 = is_x8;
                }
                let view = match self
                    .resources
                    .get(&underlying)
                    .ok_or(AerogpuD3d9Error::UnknownResource(underlying))?
                {
                    Resource::Texture2d {
                        view, view_srgb, ..
                    } => {
                        if use_srgb_view {
                            view_srgb.as_ref().unwrap_or(view)
                        } else {
                            view
                        }
                    }
                    _ => return Err(AerogpuD3d9Error::UnknownResource(underlying)),
                };
                let attachment = wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                };
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("aerogpu-d3d9.clear_scissor_color"),
                    color_attachments: &[Some(attachment)],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
                pass.set_scissor_rect(x, y, w, h);
                pass.set_pipeline(pipeline);
                pass.set_bind_group(0, &self.clear_bind_group, &[]);
                pass.draw(0..3, 0..1);
            }
        }

        if (clear_depth_enabled || clear_stencil_enabled) && rt.depth_stencil != 0 {
            let depth_handle = rt.depth_stencil;
            let underlying = self.resolve_resource_handle(depth_handle)?;
            let (depth_format, depth_width, depth_height, depth_has_stencil) = match self
                .resources
                .get(&underlying)
                .ok_or(AerogpuD3d9Error::UnknownResource(depth_handle))?
            {
                Resource::Texture2d {
                    format,
                    width,
                    height,
                    ..
                } => (
                    *format,
                    *width,
                    *height,
                    matches!(format, wgpu::TextureFormat::Depth24PlusStencil8),
                ),
                _ => return Err(AerogpuD3d9Error::UnknownResource(depth_handle)),
            };

            let write_depth = clear_depth_enabled;
            let write_stencil = clear_stencil_enabled && depth_has_stencil;
            if write_depth || write_stencil {
                // Preserve pixels outside the scissor region: load the current contents before
                // applying the scissored clear.
                self.flush_texture_if_dirty_strict(Some(encoder), depth_handle, ctx)?;

                let Some((x, y, w, h)) = clamp_scissor_rect(
                    scissor.0,
                    scissor.1,
                    scissor.2,
                    scissor.3,
                    depth_width,
                    depth_height,
                ) else {
                    return Ok(());
                };

                self.ensure_clear_dummy_color_target(depth_width, depth_height);

                let key = ClearDepthPipelineKey {
                    format: depth_format,
                    write_depth,
                    write_stencil,
                };
                self.ensure_clear_depth_pipeline(key);
                let pipeline = self.clear_depth_pipeline(key);

                let depth_view = match self
                    .resources
                    .get(&underlying)
                    .ok_or(AerogpuD3d9Error::UnknownResource(depth_handle))?
                {
                    Resource::Texture2d { view, .. } => view,
                    _ => return Err(AerogpuD3d9Error::UnknownResource(depth_handle)),
                };

                let dummy_color_view = self.clear_dummy_color_view(depth_width, depth_height);

                let color_attachments = [Some(wgpu::RenderPassColorAttachment {
                    view: dummy_color_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.0,
                            g: 0.0,
                            b: 0.0,
                            a: 0.0,
                        }),
                        store: wgpu::StoreOp::Discard,
                    },
                })];

                let depth_attachment = wgpu::RenderPassDepthStencilAttachment {
                    view: depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: depth_has_stencil.then_some(wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    }),
                };

                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("aerogpu-d3d9.clear_scissor_depth"),
                    color_attachments: &color_attachments,
                    depth_stencil_attachment: Some(depth_attachment),
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
                pass.set_viewport(0.0, 0.0, depth_width as f32, depth_height as f32, 0.0, 1.0);
                pass.set_scissor_rect(x, y, w, h);
                pass.set_pipeline(pipeline);
                pass.set_bind_group(0, &self.clear_bind_group, &[]);
                if write_stencil {
                    pass.set_stencil_reference(stencil & 0xFF);
                }
                pass.draw(0..3, 0..1);
            }
        }

        Ok(())
    }

    fn ensure_clear_depth_pipeline(&mut self, key: ClearDepthPipelineKey) {
        if self.clear_depth_pipelines.contains_key(&key) {
            return;
        }

        let depth_has_stencil = matches!(key.format, wgpu::TextureFormat::Depth24PlusStencil8);
        let stencil_write = depth_has_stencil && key.write_stencil;
        let stencil_face = if stencil_write {
            wgpu::StencilFaceState {
                compare: wgpu::CompareFunction::Always,
                fail_op: wgpu::StencilOperation::Keep,
                depth_fail_op: wgpu::StencilOperation::Keep,
                pass_op: wgpu::StencilOperation::Replace,
            }
        } else {
            wgpu::StencilFaceState::IGNORE
        };

        let pipeline = self
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("aerogpu-d3d9.clear_depth_pipeline"),
                layout: Some(&self.clear_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &self.clear_shader,
                    entry_point: "vs",
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &self.clear_shader,
                    entry_point: "fs_depth",
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: key.format,
                    depth_write_enabled: key.write_depth,
                    depth_compare: wgpu::CompareFunction::Always,
                    stencil: wgpu::StencilState {
                        front: stencil_face,
                        back: stencil_face,
                        read_mask: 0xFF,
                        write_mask: if stencil_write { 0xFF } else { 0 },
                    },
                    bias: wgpu::DepthBiasState::default(),
                }),
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
            });
        self.clear_depth_pipelines.insert(key, pipeline);
    }

    fn clear_depth_pipeline(&self, key: ClearDepthPipelineKey) -> &wgpu::RenderPipeline {
        self.clear_depth_pipelines.get(&key).expect(
            "missing clear depth pipeline; ensure_clear_depth_pipeline should be called first",
        )
    }

    fn ensure_clear_dummy_color_target(&mut self, width: u32, height: u32) {
        let key = (width, height);
        if self.clear_dummy_color_targets.contains_key(&key) {
            return;
        }

        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("aerogpu-d3d9.clear_dummy_color"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.clear_dummy_color_targets
            .insert(key, ClearDummyColorTarget { texture, view });
    }

    fn clear_dummy_color_view(&self, width: u32, height: u32) -> &wgpu::TextureView {
        &self
            .clear_dummy_color_targets
            .get(&(width, height))
            .expect("missing clear dummy color target; ensure_clear_dummy_color_target should be called first")
            .view
    }

    fn ensure_clear_pipeline(&mut self, format: wgpu::TextureFormat) {
        if self.clear_pipelines.contains_key(&format) {
            return;
        }
        let pipeline = self
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("aerogpu-d3d9.clear_pipeline"),
                layout: Some(&self.clear_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &self.clear_shader,
                    entry_point: "vs",
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &self.clear_shader,
                    entry_point: "fs",
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
            });
        self.clear_pipelines.insert(format, pipeline);
    }

    fn clear_pipeline(&self, format: wgpu::TextureFormat) -> &wgpu::RenderPipeline {
        self.clear_pipelines
            .get(&format)
            .expect("missing clear pipeline; ensure_clear_pipeline should be called first")
    }

    fn render_target_extent(&self) -> Result<(u32, u32), AerogpuD3d9Error> {
        let rt = &self.state.render_targets;
        for slot in 0..rt.color_count.min(8) as usize {
            let handle = rt.colors[slot];
            if handle == 0 {
                continue;
            }
            let underlying = self.resolve_resource_handle(handle)?;
            let res = self
                .resources
                .get(&underlying)
                .ok_or(AerogpuD3d9Error::UnknownResource(handle))?;
            match res {
                Resource::Texture2d { width, height, .. } => return Ok((*width, *height)),
                _ => return Err(AerogpuD3d9Error::UnknownResource(handle)),
            }
        }

        if rt.depth_stencil != 0 {
            let handle = rt.depth_stencil;
            let underlying = self.resolve_resource_handle(handle)?;
            let res = self
                .resources
                .get(&underlying)
                .ok_or(AerogpuD3d9Error::UnknownResource(handle))?;
            match res {
                Resource::Texture2d { width, height, .. } => return Ok((*width, *height)),
                _ => return Err(AerogpuD3d9Error::UnknownResource(handle)),
            }
        }

        Err(AerogpuD3d9Error::MissingRenderTargets)
    }

    fn ensure_triangle_fan_index_buffer(&mut self, vertex_count: u32) -> Result<(), AerogpuD3d9Error> {
        if self.triangle_fan_index_buffers.contains_key(&vertex_count) {
            return Ok(());
        }

        if vertex_count < 3 {
            return Err(AerogpuD3d9Error::Validation(
                "TriangleFan draw requires vertex_count >= 3".into(),
            ));
        }

        let format = if vertex_count <= (u16::MAX as u32) + 1 {
            wgpu::IndexFormat::Uint16
        } else {
            wgpu::IndexFormat::Uint32
        };

        let tri_count = vertex_count
            .checked_sub(2)
            .expect("vertex_count >= 3 checked above");
        let index_count = tri_count.checked_mul(3).ok_or_else(|| {
            AerogpuD3d9Error::Validation("TriangleFan index count overflow".into())
        })?;
        let index_count_usize: usize = index_count.try_into().map_err(|_| {
            AerogpuD3d9Error::Validation("TriangleFan index count out of range".into())
        })?;

        let label = format!("aerogpu-d3d9.triangle_fan_indices.{vertex_count}");
        let buffer = match format {
            wgpu::IndexFormat::Uint16 => {
                let mut indices = Vec::with_capacity(index_count_usize);
                for i in 0..tri_count {
                    indices.push(0u16);
                    indices.push((i + 1) as u16);
                    indices.push((i + 2) as u16);
                }
                debug_assert_eq!(indices.len(), index_count_usize);
                self.device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some(&label),
                        contents: bytemuck::cast_slice(&indices),
                        usage: wgpu::BufferUsages::INDEX,
                    })
            }
            wgpu::IndexFormat::Uint32 => {
                let mut indices = Vec::with_capacity(index_count_usize);
                for i in 0..tri_count {
                    indices.push(0u32);
                    indices.push(i + 1);
                    indices.push(i + 2);
                }
                debug_assert_eq!(indices.len(), index_count_usize);
                self.device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some(&label),
                        contents: bytemuck::cast_slice(&indices),
                        usage: wgpu::BufferUsages::INDEX,
                    })
            }
        };

        self.triangle_fan_index_buffers
            .insert(vertex_count, TriangleFanIndexBuffer { buffer, format });

        Ok(())
    }

    fn encode_draw(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        ctx: &mut SubmissionCtx<'_>,
        draw: DrawParams,
    ) -> Result<(), AerogpuD3d9Error> {
        let vs_handle = self.state.vs;
        let ps_handle = self.state.ps;
        if vs_handle == 0 || ps_handle == 0 {
            return Err(AerogpuD3d9Error::MissingShaders);
        }
        let layout_handle = self.state.input_layout;
        if layout_handle == 0 {
            return Err(AerogpuD3d9Error::MissingInputLayout);
        }

        let (rt_w, rt_h) = self.render_target_extent()?;

        // wgpu requires viewport/scissor rectangles to be within the render target bounds.
        // Clamp dynamic state to match D3D9 behavior (and keep the executor resilient to
        // out-of-bounds values such as "disabled scissor" sent as a huge rect).
        let viewport = self.state.viewport.and_then(|vp| {
            if !vp.x.is_finite()
                || !vp.y.is_finite()
                || !vp.width.is_finite()
                || !vp.height.is_finite()
                || !vp.min_depth.is_finite()
                || !vp.max_depth.is_finite()
            {
                return None;
            }

            let max_w = rt_w as f32;
            let max_h = rt_h as f32;

            let left = vp.x.max(0.0);
            let top = vp.y.max(0.0);
            let right = (vp.x + vp.width).max(0.0).min(max_w);
            let bottom = (vp.y + vp.height).max(0.0).min(max_h);
            let width = (right - left).max(0.0);
            let height = (bottom - top).max(0.0);

            if width <= 0.0 || height <= 0.0 {
                return None;
            }

            let mut min_depth = vp.min_depth.clamp(0.0, 1.0);
            let mut max_depth = vp.max_depth.clamp(0.0, 1.0);
            if min_depth > max_depth {
                std::mem::swap(&mut min_depth, &mut max_depth);
            }

            Some(ViewportState {
                x: left,
                y: top,
                width,
                height,
                min_depth,
                max_depth,
            })
        });
        if self.state.viewport.is_some() && viewport.is_none() {
            // Viewport is empty after clamping, so the draw would have no effect.
            return Ok(());
        }

        let scissor = if self.state.rasterizer_state.scissor_enable {
            let raw = self.state.scissor;
            let clamped = raw.and_then(|(x, y, w, h)| clamp_scissor_rect(x, y, w, h, rt_w, rt_h));
            if raw.is_some() && clamped.is_none() {
                // Scissor test enabled but empty after clamping, so the draw would have no effect.
                return Ok(());
            }
            clamped
        } else {
            None
        };

        let is_triangle_fan =
            self.state.topology_raw == cmd::AerogpuPrimitiveTopology::TriangleFan as u32;
        let sample_mask_allows_draw = (self.state.sample_mask & 1) != 0;

        // If the guest requested a non-indexed TriangleFan draw, emulate by expanding the fan into
        // a triangle-list index buffer (cached by vertex_count).
        let triangle_fan_nonindexed_plan: Option<(u32, u32, i32, u32, u32)> = if is_triangle_fan {
            match draw {
                DrawParams::NonIndexed {
                    vertex_count,
                    instance_count,
                    first_vertex,
                    first_instance,
                } => {
                    if !sample_mask_allows_draw || vertex_count < 3 || instance_count == 0 {
                        None
                    } else {
                        let tri_count = vertex_count
                            .checked_sub(2)
                            .expect("vertex_count >= 3 checked above");
                        let index_count = tri_count.checked_mul(3).ok_or_else(|| {
                            AerogpuD3d9Error::Validation("TriangleFan index count overflow".into())
                        })?;
                        let base_vertex: i32 = first_vertex.try_into().map_err(|_| {
                            AerogpuD3d9Error::Validation(
                                "TriangleFan first_vertex out of range".into(),
                            )
                        })?;
                        Some((
                            vertex_count,
                            index_count,
                            base_vertex,
                            first_instance,
                            instance_count,
                        ))
                    }
                }
                DrawParams::Indexed { .. } => None,
            }
        } else {
            None
        };
        if let Some((vertex_count, ..)) = triangle_fan_nonindexed_plan {
            self.ensure_triangle_fan_index_buffer(vertex_count)?;
        }

        // Flush guest-backed resources touched by this draw before we bind them.
        let rt = self.state.render_targets;
        let index_binding = self.state.index_buffer;
        let textures_vs = self.state.textures_vs;
        let textures_ps = self.state.textures_ps;
        let streams = {
            let layout = self
                .input_layouts
                .get(&layout_handle)
                .ok_or(AerogpuD3d9Error::UnknownInputLayout(layout_handle))?;
            let mut streams: Vec<u8> = layout.decl.elements.iter().map(|e| e.stream).collect();
            streams.sort_unstable();
            streams.dedup();
            streams
        };
        for slot in 0..rt.color_count.min(8) as usize {
            let handle = rt.colors[slot];
            if handle == 0 {
                continue;
            }
            self.flush_texture_if_dirty_strict(Some(encoder), handle, ctx)?;
        }
        if rt.depth_stencil != 0 {
            self.flush_texture_if_dirty_strict(Some(encoder), rt.depth_stencil, ctx)?;
        }

        for stream in streams {
            let Some(binding) = self
                .state
                .vertex_buffers
                .get(stream as usize)
                .copied()
                .flatten()
            else {
                continue;
            };
            if binding.buffer != 0 {
                self.flush_buffer_if_dirty(Some(encoder), binding.buffer, ctx)?;
            }
        }

        if let DrawParams::Indexed { .. } = draw {
            let index_binding = index_binding.ok_or(AerogpuD3d9Error::MissingIndexBuffer)?;
            if index_binding.buffer != 0 {
                self.flush_buffer_if_dirty(Some(encoder), index_binding.buffer, ctx)?;
            }
        }

        for tex_handle in textures_vs.iter().copied().chain(textures_ps.iter().copied()) {
            if tex_handle == 0 {
                continue;
            }
            self.flush_texture_binding_if_dirty(Some(encoder), tex_handle, ctx)?;
        }
        self.ensure_bind_group();
        let (vs_key, ps_key, vs_uses_semantic_locations) = {
            let vs = self
                .shaders
                .get(&vs_handle)
                .ok_or(AerogpuD3d9Error::UnknownShader(vs_handle))?;
            let ps = self
                .shaders
                .get(&ps_handle)
                .ok_or(AerogpuD3d9Error::UnknownShader(ps_handle))?;
            if vs.stage != shader::ShaderStage::Vertex {
                return Err(AerogpuD3d9Error::ShaderStageMismatch {
                    shader_handle: vs_handle,
                    expected: shader::ShaderStage::Vertex,
                    actual: vs.stage,
                });
            }
            if ps.stage != shader::ShaderStage::Pixel {
                return Err(AerogpuD3d9Error::ShaderStageMismatch {
                    shader_handle: ps_handle,
                    expected: shader::ShaderStage::Pixel,
                    actual: ps.stage,
                });
            }
            (vs.key, ps.key, vs.uses_semantic_locations)
        };
        let (color_formats, color_is_x8, depth_format) = self.render_target_formats()?;
        let depth_has_stencil =
            matches!(depth_format, Some(wgpu::TextureFormat::Depth24PlusStencil8));
        let vertex_buffers = {
            let layout = self
                .input_layouts
                .get(&layout_handle)
                .ok_or(AerogpuD3d9Error::UnknownInputLayout(layout_handle))?;
            self.vertex_buffer_layouts(layout, vs_uses_semantic_locations)?
        };
        let vertex_buffers_ref = vertex_buffers
            .buffers
            .iter()
            .map(|b| wgpu::VertexBufferLayout {
                array_stride: b.array_stride,
                step_mode: b.step_mode,
                attributes: &b.attributes,
            })
            .collect::<Vec<_>>();

        let targets = color_formats
            .iter()
            .enumerate()
            .map(|(idx, fmt)| {
                let is_x8 = color_is_x8.get(idx).copied().unwrap_or(false);
                fmt.map(|format| {
                    let mut write_mask = map_color_write_mask(
                        self.state
                            .blend_state
                            .color_write_mask
                            .get(idx)
                            .copied()
                            .unwrap_or(0xF),
                    );
                    if is_x8 {
                        write_mask &= !wgpu::ColorWrites::ALPHA;
                    }
                    wgpu::ColorTargetState {
                        format,
                        blend: map_blend_state(self.state.blend_state),
                        write_mask,
                    }
                })
            })
            .collect::<Vec<_>>();

        let vertex_buffer_keys = vertex_buffers
            .buffers
            .iter()
            .map(|b| crate::pipeline_key::VertexBufferLayoutKey {
                array_stride: b.array_stride,
                step_mode: b.step_mode,
                attributes: b.attributes.iter().copied().map(Into::into).collect(),
            })
            .collect::<Vec<_>>();

        let alpha_test_enable = self.state.alpha_test_enable;
        let alpha_test_func = self.state.alpha_test_func;
        let alpha_test_ref = self.state.alpha_test_ref;
        // When alpha testing is disabled, ALPHAFUNC/ALPHAREF should not affect pipeline selection.
        let (pipeline_alpha_enable, pipeline_alpha_func, pipeline_alpha_ref) = if alpha_test_enable {
            (true, alpha_test_func, alpha_test_ref)
        } else {
            (false, 0, 0)
        };

        let pipeline_key = PipelineCacheKey {
            vs: vs_key,
            ps: ps_key,
            alpha_test_enable: pipeline_alpha_enable,
            alpha_test_func: pipeline_alpha_func,
            alpha_test_ref: pipeline_alpha_ref,
            vertex_buffers: vertex_buffer_keys,
            color_formats: color_formats.clone(),
            x8_mask: color_is_x8.clone(),
            depth_format,
            topology: self.state.topology,
            blend: self.state.blend_state,
            depth_stencil: self.state.depth_stencil_state,
            raster: RasterizerPipelineKey {
                cull_mode: self.state.rasterizer_state.cull_mode,
                front_ccw: self.state.rasterizer_state.front_ccw,
                depth_bias: self.state.rasterizer_state.depth_bias,
            },
        };

        let pipeline = if let Some(existing) = self.pipelines.get(&pipeline_key) {
            existing
        } else {
            let alpha_test_ps_module = if alpha_test_enable {
                let ps_wgsl = {
                    let ps = self
                        .shaders
                        .get(&ps_handle)
                        .ok_or(AerogpuD3d9Error::UnknownShader(ps_handle))?;
                    ps.wgsl.clone()
                };
                Some(self.alpha_test_pixel_shader_module(
                    ps_key,
                    &ps_wgsl,
                    alpha_test_func,
                    alpha_test_ref,
                )?)
            } else {
                None
            };

            let pipeline = {
                let vs = self
                    .shaders
                    .get(&vs_handle)
                    .ok_or(AerogpuD3d9Error::UnknownShader(vs_handle))?;
                let ps = self
                    .shaders
                    .get(&ps_handle)
                    .ok_or(AerogpuD3d9Error::UnknownShader(ps_handle))?;
                let ps_module: &wgpu::ShaderModule = alpha_test_ps_module
                    .as_ref()
                    .map(|m| m.as_ref())
                    .unwrap_or(&ps.module);
                self.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("aerogpu-d3d9.pipeline"),
                    layout: Some(&self.pipeline_layout),
                    vertex: wgpu::VertexState {
                        module: &vs.module,
                        entry_point: vs.entry_point,
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                        buffers: &vertex_buffers_ref,
                    },
                    fragment: Some(wgpu::FragmentState {
                        module: ps_module,
                        entry_point: ps.entry_point,
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                        targets: &targets,
                    }),
                    primitive: wgpu::PrimitiveState {
                        topology: self.state.topology,
                        strip_index_format: None,
                        front_face: if self.state.rasterizer_state.front_ccw {
                            wgpu::FrontFace::Ccw
                        } else {
                            wgpu::FrontFace::Cw
                        },
                        cull_mode: match self.state.rasterizer_state.cull_mode {
                            1 => Some(wgpu::Face::Front),
                            2 => Some(wgpu::Face::Back),
                            _ => None,
                        },
                        ..Default::default()
                    },
                    depth_stencil: depth_format.map(|format| wgpu::DepthStencilState {
                        format,
                        depth_write_enabled: self.state.depth_stencil_state.depth_enable
                            && self.state.depth_stencil_state.depth_write_enable,
                        depth_compare: if self.state.depth_stencil_state.depth_enable {
                            map_compare_func(self.state.depth_stencil_state.depth_func)
                        } else {
                            wgpu::CompareFunction::Always
                        },
                        stencil: if depth_has_stencil && self.state.depth_stencil_state.stencil_enable
                        {
                            let cw_face = wgpu::StencilFaceState {
                                compare: map_compare_func(self.state.depth_stencil_state.stencil_func),
                                fail_op: map_stencil_op(self.state.depth_stencil_state.stencil_fail_op),
                                depth_fail_op: map_stencil_op(
                                    self.state.depth_stencil_state.stencil_depth_fail_op,
                                ),
                                pass_op: map_stencil_op(self.state.depth_stencil_state.stencil_pass_op),
                            };
                            let ccw_face = wgpu::StencilFaceState {
                                compare: map_compare_func(self.state.depth_stencil_state.ccw_stencil_func),
                                fail_op: map_stencil_op(self.state.depth_stencil_state.ccw_stencil_fail_op),
                                depth_fail_op: map_stencil_op(
                                    self.state.depth_stencil_state.ccw_stencil_depth_fail_op,
                                ),
                                pass_op: map_stencil_op(self.state.depth_stencil_state.ccw_stencil_pass_op),
                            };

                            // D3D9's CCW stencil state is keyed to winding order, not "back face".
                            // We configure `primitive.front_face` from `FRONTCOUNTERCLOCKWISE`;
                            // map the CCW winding state onto the corresponding wgpu face.
                            let (front, back) =
                                if self.state.depth_stencil_state.two_sided_stencil_enable {
                                    if self.state.rasterizer_state.front_ccw {
                                        (ccw_face, cw_face)
                                    } else {
                                        (cw_face, ccw_face)
                                    }
                                } else {
                                    (cw_face, cw_face)
                                };
                            wgpu::StencilState {
                                front,
                                back,
                                read_mask: self.state.depth_stencil_state.stencil_read_mask as u32,
                                write_mask: self.state.depth_stencil_state.stencil_write_mask as u32,
                            }
                        } else {
                            wgpu::StencilState {
                                front: wgpu::StencilFaceState::IGNORE,
                                back: wgpu::StencilFaceState::IGNORE,
                                read_mask: 0,
                                write_mask: 0,
                            }
                        },
                        bias: wgpu::DepthBiasState {
                            constant: self.state.rasterizer_state.depth_bias,
                            slope_scale: 0.0,
                            clamp: 0.0,
                        },
                    }),
                    multisample: wgpu::MultisampleState::default(),
                    multiview: None,
                })
            };
            self.pipelines.insert(pipeline_key.clone(), pipeline);
            self.pipelines
                .get(&pipeline_key)
                .expect("pipeline was just inserted")
        };

        let triangle_fan_index_buffer = match triangle_fan_nonindexed_plan {
            Some((vertex_count, ..)) => self.triangle_fan_index_buffers.get(&vertex_count),
            None => None,
        };

        let bind_group = self
            .bind_group
            .as_ref()
            .expect("ensure_bind_group initializes bind group");

        let (color_views, depth_view) = self.render_target_attachments()?;
        let color_attachments = color_views
            .into_iter()
            .map(|view| {
                view.map(|view| wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })
            })
            .collect::<Vec<_>>();

        let depth_stencil_attachment =
            depth_view.map(|view| wgpu::RenderPassDepthStencilAttachment {
                view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: depth_has_stencil.then_some(wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                }),
            });

        // Prepare per-draw converted vertex buffers for D3D9 `D3DDECLTYPE_D3DCOLOR`.
        //
        // D3DCOLOR is 0xAARRGGBB, but in little-endian vertex buffers it's stored as BGRA bytes.
        // D3D9 vertex fetch presents it to shaders as RGBA, so we must swap the red/blue channels
        // before WebGPU reads it as `unorm8x4`.
        //
        // Note: this is intentionally implemented as a per-draw conversion into temporary buffers
        // to avoid requiring any shader-side workarounds.
        let mut d3dcolor_offsets_by_stream: HashMap<u8, Vec<u16>> = HashMap::new();
        {
            let layout = self
                .input_layouts
                .get(&layout_handle)
                .ok_or(AerogpuD3d9Error::UnknownInputLayout(layout_handle))?;
            for e in &layout.decl.elements {
                if e.ty == aero_d3d9::vertex::DeclType::D3dColor {
                    d3dcolor_offsets_by_stream
                        .entry(e.stream)
                        .or_default()
                        .push(e.offset);
                }
            }
        }
        for offsets in d3dcolor_offsets_by_stream.values_mut() {
            offsets.sort_unstable();
            offsets.dedup();
        }

        let (swizzle_vertex_start, swizzle_vertex_end) = match draw {
            DrawParams::NonIndexed {
                vertex_count,
                first_vertex,
                ..
            } => (
                first_vertex,
                first_vertex
                    .checked_add(vertex_count)
                    .unwrap_or(u32::MAX),
            ),
            DrawParams::Indexed {
                index_count,
                first_index,
                base_vertex,
                ..
            } => {
                let index_binding = index_binding.ok_or(AerogpuD3d9Error::MissingIndexBuffer)?;
                let underlying = self.resolve_resource_handle(index_binding.buffer)?;
                let res = self
                    .resources
                    .get(&underlying)
                    .ok_or(AerogpuD3d9Error::UnknownResource(index_binding.buffer))?;
                let Resource::Buffer { shadow, size, .. } = res else {
                    return Err(AerogpuD3d9Error::UnknownResource(index_binding.buffer));
                };
                let bytes_per_index = match index_binding.format {
                    wgpu::IndexFormat::Uint16 => 2u64,
                    wgpu::IndexFormat::Uint32 => 4u64,
                };
                let start_byte = (index_binding.offset_bytes as u64)
                    .checked_add((first_index as u64).saturating_mul(bytes_per_index))
                    .ok_or_else(|| {
                        AerogpuD3d9Error::Validation("index buffer range overflow".into())
                    })?;
                let byte_len = (index_count as u64)
                    .checked_mul(bytes_per_index)
                    .ok_or_else(|| {
                        AerogpuD3d9Error::Validation("index buffer range overflow".into())
                    })?;
                let end_byte = start_byte.checked_add(byte_len).ok_or_else(|| {
                    AerogpuD3d9Error::Validation("index buffer range overflow".into())
                })?;
                if end_byte > *size {
                    return Err(AerogpuD3d9Error::Validation(format!(
                        "indexed draw out of bounds for index buffer (handle={} start=0x{start_byte:x} end=0x{end_byte:x} size=0x{size:x})",
                        index_binding.buffer
                    )));
                }
                let start_usize = usize::try_from(start_byte).map_err(|_| {
                    AerogpuD3d9Error::Validation("index buffer offset overflow".into())
                })?;
                let end_usize = usize::try_from(end_byte).map_err(|_| {
                    AerogpuD3d9Error::Validation("index buffer size overflow".into())
                })?;
                if end_usize > shadow.len() {
                    return Err(AerogpuD3d9Error::Validation(format!(
                        "index buffer shadow too small (handle={} end=0x{end_byte:x} shadow_size=0x{:x})",
                        index_binding.buffer,
                        shadow.len()
                    )));
                }
                let indices_bytes = &shadow[start_usize..end_usize];

                let mut min_index: u32 = u32::MAX;
                let mut max_index: u32 = 0;
                if index_binding.format == wgpu::IndexFormat::Uint16 {
                    for chunk in indices_bytes.chunks_exact(2) {
                        let idx = u16::from_le_bytes([chunk[0], chunk[1]]) as u32;
                        min_index = min_index.min(idx);
                        max_index = max_index.max(idx);
                    }
                } else {
                    for chunk in indices_bytes.chunks_exact(4) {
                        let idx = u32::from_le_bytes(chunk.try_into().unwrap());
                        min_index = min_index.min(idx);
                        max_index = max_index.max(idx);
                    }
                }

                if min_index == u32::MAX {
                    // No indices.
                    (0, 0)
                } else {
                    let base = base_vertex as i64;
                    let min_v_i64 = base.saturating_add(min_index as i64);
                    let max_v_excl_i64 = base
                        .saturating_add(max_index as i64)
                        .saturating_add(1);
                    let clamp_u32 = |v: i64| -> u32 {
                        if v <= 0 {
                            0
                        } else if v >= u32::MAX as i64 {
                            u32::MAX
                        } else {
                            v as u32
                        }
                    };
                    (clamp_u32(min_v_i64), clamp_u32(max_v_excl_i64))
                }
            }
        };

        let mut converted_vertex_buffers: HashMap<u8, wgpu::Buffer> = HashMap::new();
        if swizzle_vertex_end != 0 && !d3dcolor_offsets_by_stream.is_empty() {
            for stream in &vertex_buffers.streams {
                let Some(d3dcolor_offsets) = d3dcolor_offsets_by_stream.get(stream) else {
                    continue;
                };
                if d3dcolor_offsets.is_empty() {
                    continue;
                }

                let Some(binding) = self
                    .state
                    .vertex_buffers
                    .get(*stream as usize)
                    .copied()
                    .flatten()
                else {
                    continue;
                };

                let underlying = self.resolve_resource_handle(binding.buffer)?;
                let res = self
                    .resources
                    .get(&underlying)
                    .ok_or(AerogpuD3d9Error::UnknownResource(binding.buffer))?;
                let Resource::Buffer { shadow, size, .. } = res else {
                    return Err(AerogpuD3d9Error::UnknownResource(binding.buffer));
                };

                let stride = binding.stride_bytes as u64;
                if stride == 0 {
                    return Err(AerogpuD3d9Error::Validation(format!(
                        "vertex buffer stride is 0 for stream {stream}"
                    )));
                }

                let desired_len = (swizzle_vertex_end as u64)
                    .checked_mul(stride)
                    .ok_or_else(|| AerogpuD3d9Error::Validation("vertex range overflow".into()))?;
                let base_offset = binding.offset_bytes as u64;
                let available = size.saturating_sub(base_offset);
                let len = desired_len.min(available);
                if len == 0 {
                    continue;
                }

                let base_usize = usize::try_from(base_offset).map_err(|_| {
                    AerogpuD3d9Error::Validation("vertex buffer offset overflow".into())
                })?;
                let len_usize = usize::try_from(len).map_err(|_| {
                    AerogpuD3d9Error::Validation("vertex buffer conversion size overflow".into())
                })?;
                let end_usize = base_usize.checked_add(len_usize).ok_or_else(|| {
                    AerogpuD3d9Error::Validation("vertex buffer conversion range overflow".into())
                })?;
                if end_usize > shadow.len() {
                    return Err(AerogpuD3d9Error::Validation(format!(
                        "vertex buffer shadow too small (handle={} end=0x{:x} shadow_size=0x{:x})",
                        binding.buffer, end_usize, shadow.len()
                    )));
                }

                let mut converted = shadow[base_usize..end_usize].to_vec();

                // Clamp swizzle range to the converted data slice.
                let max_vertices = (len / stride) as u32;
                let swizzle_start = swizzle_vertex_start.min(max_vertices);
                let swizzle_end = swizzle_vertex_end.min(max_vertices);
                for v in swizzle_start..swizzle_end {
                    let v_base = (v as u64).saturating_mul(stride);
                    for &attr_off in d3dcolor_offsets {
                        let off = v_base.saturating_add(attr_off as u64);
                        if off.saturating_add(3) >= len {
                            continue;
                        }
                        // BGRA -> RGBA (swap R/B).
                        let i0 = off as usize;
                        let i2 = (off + 2) as usize;
                        converted.swap(i0, i2);
                    }
                }

                let converted_buffer =
                    self.device
                        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                            label: Some("aerogpu-d3d9.vertex.d3dcolor_bgra_to_rgba"),
                            contents: &converted,
                            usage: wgpu::BufferUsages::VERTEX,
                        });
                converted_vertex_buffers.insert(*stream, converted_buffer);
            }
        }

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("aerogpu-d3d9.render"),
            color_attachments: &color_attachments,
            depth_stencil_attachment,
            timestamp_writes: None,
            occlusion_query_set: None,
        });

        if let Some(viewport) = viewport.as_ref() {
            pass.set_viewport(
                viewport.x,
                viewport.y,
                viewport.width,
                viewport.height,
                viewport.min_depth,
                viewport.max_depth,
            );
        }

        if let Some((x, y, w, h)) = scissor {
            pass.set_scissor_rect(x, y, w, h);
        }

        if depth_has_stencil && self.state.depth_stencil_state.stencil_enable {
            let stencil_ref = self
                .state
                .render_states
                .get(d3d9::D3DRS_STENCILREF as usize)
                .copied()
                .unwrap_or(0);
            pass.set_stencil_reference(stencil_ref & 0xFF);
        }

        let uses_blend_constant = matches!(self.state.blend_state.src_factor, 6 | 7)
            || matches!(self.state.blend_state.dst_factor, 6 | 7)
            || matches!(self.state.blend_state.src_factor_alpha, 6 | 7)
            || matches!(self.state.blend_state.dst_factor_alpha, 6 | 7);
        if self.state.blend_state.enable && uses_blend_constant {
            let [r, g, b, a] = self.state.blend_constant;
            pass.set_blend_constant(wgpu::Color {
                r: r as f64,
                g: g as f64,
                b: b as f64,
                a: a as f64,
            });
        }

        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, bind_group, &[]);

        // Bind vertex buffers: wgpu slot is derived from the vertex declaration's used streams.
        for stream in &vertex_buffers.streams {
            let Some(binding) = self
                .state
                .vertex_buffers
                .get(*stream as usize)
                .copied()
                .flatten()
            else {
                continue;
            };
            let underlying = self.resolve_resource_handle(binding.buffer)?;
            let res = self
                .resources
                .get(&underlying)
                .ok_or(AerogpuD3d9Error::UnknownResource(binding.buffer))?;
            let Resource::Buffer { buffer, .. } = res else {
                return Err(AerogpuD3d9Error::UnknownResource(binding.buffer));
            };
            let wgpu_slot = *vertex_buffers
                .stream_to_slot
                .get(stream)
                .expect("stream_to_slot contains all streams");
            if let Some(converted) = converted_vertex_buffers.get(stream) {
                pass.set_vertex_buffer(wgpu_slot, converted.slice(..));
            } else {
                pass.set_vertex_buffer(wgpu_slot, buffer.slice(binding.offset_bytes as u64..));
            }
        }

        match draw {
            DrawParams::NonIndexed {
                vertex_count,
                instance_count,
                first_vertex,
                first_instance,
            } => {
                if let (Some((_vertex_count, index_count, base_vertex, first_instance, instance_count)), Some(fan_index)) =
                    (triangle_fan_nonindexed_plan, triangle_fan_index_buffer)
                {
                    pass.set_index_buffer(fan_index.buffer.slice(..), fan_index.format);
                    pass.draw_indexed(
                        0..index_count,
                        base_vertex,
                        first_instance..first_instance + instance_count,
                    );
                } else if sample_mask_allows_draw {
                    pass.draw(
                        first_vertex..first_vertex + vertex_count,
                        first_instance..first_instance + instance_count,
                    );
                }
            }
            DrawParams::Indexed {
                index_count,
                instance_count,
                first_index,
                base_vertex,
                first_instance,
            } => {
                if is_triangle_fan {
                    debug!(
                        index_count,
                        instance_count,
                        first_index,
                        base_vertex,
                        first_instance,
                        "indexed TriangleFan draw is not emulated; treating indices as TriangleList"
                    );
                }

                let index_binding = self
                    .state
                    .index_buffer
                    .ok_or(AerogpuD3d9Error::MissingIndexBuffer)?;
                let underlying = self.resolve_resource_handle(index_binding.buffer)?;
                let res = self
                    .resources
                    .get(&underlying)
                    .ok_or(AerogpuD3d9Error::UnknownResource(index_binding.buffer))?;
                let Resource::Buffer { buffer, .. } = res else {
                    return Err(AerogpuD3d9Error::UnknownResource(index_binding.buffer));
                };
                pass.set_index_buffer(
                    buffer.slice(index_binding.offset_bytes as u64..),
                    index_binding.format,
                );
                if sample_mask_allows_draw {
                    pass.draw_indexed(
                        first_index..first_index + index_count,
                        base_vertex,
                        first_instance..first_instance + instance_count,
                    );
                }
            }
        }

        Ok(())
    }

    fn alpha_test_pixel_shader_module(
        &mut self,
        ps_key: u64,
        ps_wgsl: &str,
        alpha_test_func: u32,
        alpha_test_ref: u8,
    ) -> Result<Arc<wgpu::ShaderModule>, AerogpuD3d9Error> {
        let key = AlphaTestShaderModuleKey {
            ps: ps_key,
            alpha_test_func,
            alpha_test_ref,
        };

        if let Some(hit) = self.alpha_test_pixel_shaders.get(&key) {
            return Ok(hit.clone());
        }

        let wgsl = build_alpha_test_wgsl_variant(ps_wgsl, alpha_test_func, alpha_test_ref)?;
        let module = Arc::new(self.device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("aerogpu-d3d9.shader.alpha_test"),
            source: wgpu::ShaderSource::Wgsl(wgsl.into()),
        }));
        self.alpha_test_pixel_shaders.insert(key, module.clone());
        Ok(module)
    }

    fn ensure_bind_group(&mut self) {
        if !self.bind_group_dirty && self.bind_group.is_some() {
            return;
        }

        // WebGPU binds resources per (group, binding) across shader stages. D3D9 allows vertex and
        // pixel shaders to bind different textures/samplers for the same sampler register index.
        //
        // Best-effort model:
        // - If a sampler index is referenced by only one stage, bind that stage's texture/sampler.
        // - If referenced by both, attempt to bind the shared resource if both stages agree;
        //   otherwise prefer the pixel-stage binding and log the conflict.
        let used_samplers_vs = self
            .shaders
            .get(&self.state.vs)
            .map(|s| s.used_samplers_mask)
            .unwrap_or(0);
        let used_samplers_ps = self
            .shaders
            .get(&self.state.ps)
            .map(|s| s.used_samplers_mask)
            .unwrap_or(0);

        let bind_group = {
            let mut entries: Vec<wgpu::BindGroupEntry> = Vec::with_capacity(1 + MAX_SAMPLERS * 2);
            entries.push(wgpu::BindGroupEntry {
                binding: 0,
                resource: self.constants_buffer.as_entire_binding(),
            });

            let srgb_enabled = |states: &Vec<u32>| -> bool {
                states
                    .get(d3d9::D3DSAMP_SRGBTEXTURE as usize)
                    .copied()
                    .unwrap_or(0)
                    != 0
            };

            for slot in 0..MAX_SAMPLERS {
                // `aero-d3d9` shader generation uses binding numbers derived from the sampler
                // register index.
                let tex_binding = 1u32 + slot as u32 * 2;
                let samp_binding = tex_binding + 1;

                let bit = 1u16 << slot;
                let uses_vs = (used_samplers_vs & bit) != 0;
                let uses_ps = (used_samplers_ps & bit) != 0;

                let (tex_handle, srgb_texture, sampler) = if uses_vs && !uses_ps {
                    (
                        self.state.textures_vs[slot],
                        srgb_enabled(&self.state.sampler_states_vs[slot]),
                        self.samplers_vs[slot].as_ref(),
                    )
                } else if uses_ps && !uses_vs {
                    (
                        self.state.textures_ps[slot],
                        srgb_enabled(&self.state.sampler_states_ps[slot]),
                        self.samplers_ps[slot].as_ref(),
                    )
                } else if uses_vs && uses_ps {
                    let tex_vs = self.state.textures_vs[slot];
                    let tex_ps = self.state.textures_ps[slot];
                    let srgb_vs = srgb_enabled(&self.state.sampler_states_vs[slot]);
                    let srgb_ps = srgb_enabled(&self.state.sampler_states_ps[slot]);

                    let same_texture = if tex_vs == tex_ps {
                        true
                    } else {
                        let vs_underlying = self.resolve_resource_handle(tex_vs).ok();
                        let ps_underlying = self.resolve_resource_handle(tex_ps).ok();
                        vs_underlying.is_some() && vs_underlying == ps_underlying
                    };
                    let compatible_sampler = self.sampler_state_vs[slot] == self.sampler_state_ps[slot]
                        && srgb_vs == srgb_ps;

                    if !(same_texture && compatible_sampler) {
                        debug!(
                            slot,
                            tex_vs,
                            tex_ps,
                            "VS/PS conflict for sampler index; binding pixel-stage texture/sampler"
                        );
                    }

                    (
                        tex_ps,
                        srgb_ps,
                        self.samplers_ps[slot].as_ref(),
                    )
                } else {
                    // Unused by both stages: bind the pixel-stage entry by convention.
                    (
                        self.state.textures_ps[slot],
                        srgb_enabled(&self.state.sampler_states_ps[slot]),
                        self.samplers_ps[slot].as_ref(),
                    )
                };

                let view: &wgpu::TextureView = if tex_handle == 0 {
                    &self.dummy_texture_view
                } else {
                    let underlying = self.resolve_resource_handle(tex_handle).ok();
                    match underlying.and_then(|h| self.resources.get(&h)) {
                        Some(Resource::Texture2d {
                            view, view_srgb, ..
                        }) => {
                            if srgb_texture {
                                view_srgb.as_ref().unwrap_or(view)
                            } else {
                                view
                            }
                        }
                        _ => &self.dummy_texture_view,
                    }
                };

                entries.push(wgpu::BindGroupEntry {
                    binding: tex_binding,
                    resource: wgpu::BindingResource::TextureView(view),
                });
                entries.push(wgpu::BindGroupEntry {
                    binding: samp_binding,
                    resource: wgpu::BindingResource::Sampler(sampler),
                });
            }

            self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("aerogpu-d3d9.bind_group"),
                layout: &self.bind_group_layout,
                entries: &entries,
            })
        };

        self.bind_group = Some(bind_group);
        self.bind_group_dirty = false;
    }

    fn set_render_state_u32(&mut self, state_id: u32, value: u32) {
        if state_id > MAX_REASONABLE_RENDER_STATE_ID {
            debug!(
                state_id,
                value, "ignoring suspiciously large D3D9 render state id"
            );
            return;
        }

        let idx = state_id as usize;
        if idx >= self.state.render_states.len() {
            self.state.render_states.resize(idx + 1, 0);
        }
        if self.state.render_states[idx] == value {
            return;
        }
        self.state.render_states[idx] = value;

        match state_id {
            d3d9::D3DRS_ZENABLE => self.state.depth_stencil_state.depth_enable = value != 0,
            d3d9::D3DRS_ZWRITEENABLE => {
                self.state.depth_stencil_state.depth_write_enable = value != 0
            }
            d3d9::D3DRS_ZFUNC => match d3d9_compare_to_aerogpu(value) {
                Some(func) => self.state.depth_stencil_state.depth_func = func,
                None => debug!(state_id, value, "unknown D3D9 compare func"),
            },
            d3d9::D3DRS_STENCILENABLE => self.state.depth_stencil_state.stencil_enable = value != 0,
            d3d9::D3DRS_STENCILMASK => {
                self.state.depth_stencil_state.stencil_read_mask = (value & 0xFF) as u8
            }
            d3d9::D3DRS_STENCILWRITEMASK => {
                self.state.depth_stencil_state.stencil_write_mask = (value & 0xFF) as u8
            }
            d3d9::D3DRS_STENCILFAIL => {
                let raw = if value == 0 { 1 } else { value };
                match d3d9_stencil_op_to_aerogpu(raw) {
                    Some(op) => self.state.depth_stencil_state.stencil_fail_op = op,
                    None => debug!(state_id, value, "unknown D3D9 stencil op"),
                }
            }
            d3d9::D3DRS_STENCILZFAIL => {
                let raw = if value == 0 { 1 } else { value };
                match d3d9_stencil_op_to_aerogpu(raw) {
                    Some(op) => self.state.depth_stencil_state.stencil_depth_fail_op = op,
                    None => debug!(state_id, value, "unknown D3D9 stencil op"),
                }
            }
            d3d9::D3DRS_STENCILPASS => {
                let raw = if value == 0 { 1 } else { value };
                match d3d9_stencil_op_to_aerogpu(raw) {
                    Some(op) => self.state.depth_stencil_state.stencil_pass_op = op,
                    None => debug!(state_id, value, "unknown D3D9 stencil op"),
                }
            }
            d3d9::D3DRS_STENCILFUNC => {
                let raw = if value == 0 { 8 } else { value };
                match d3d9_compare_to_aerogpu(raw) {
                    Some(func) => self.state.depth_stencil_state.stencil_func = func,
                    None => debug!(state_id, value, "unknown D3D9 compare func"),
                }
            }
            d3d9::D3DRS_TWOSIDEDSTENCILMODE => {
                self.state.depth_stencil_state.two_sided_stencil_enable = value != 0
            }
            d3d9::D3DRS_CCW_STENCILFAIL => {
                let raw = if value == 0 { 1 } else { value };
                match d3d9_stencil_op_to_aerogpu(raw) {
                    Some(op) => self.state.depth_stencil_state.ccw_stencil_fail_op = op,
                    None => debug!(state_id, value, "unknown D3D9 stencil op"),
                }
            }
            d3d9::D3DRS_CCW_STENCILZFAIL => {
                let raw = if value == 0 { 1 } else { value };
                match d3d9_stencil_op_to_aerogpu(raw) {
                    Some(op) => self.state.depth_stencil_state.ccw_stencil_depth_fail_op = op,
                    None => debug!(state_id, value, "unknown D3D9 stencil op"),
                }
            }
            d3d9::D3DRS_CCW_STENCILPASS => {
                let raw = if value == 0 { 1 } else { value };
                match d3d9_stencil_op_to_aerogpu(raw) {
                    Some(op) => self.state.depth_stencil_state.ccw_stencil_pass_op = op,
                    None => debug!(state_id, value, "unknown D3D9 stencil op"),
                }
            }
            d3d9::D3DRS_CCW_STENCILFUNC => {
                let raw = if value == 0 { 8 } else { value };
                match d3d9_compare_to_aerogpu(raw) {
                    Some(func) => self.state.depth_stencil_state.ccw_stencil_func = func,
                    None => debug!(state_id, value, "unknown D3D9 compare func"),
                }
            }
            d3d9::D3DRS_STENCILREF => {}
            d3d9::D3DRS_ALPHATESTENABLE => self.state.alpha_test_enable = value != 0,
            d3d9::D3DRS_ALPHAFUNC => {
                let raw = if value == 0 { 8 } else { value };
                self.state.alpha_test_func = raw;
            }
            d3d9::D3DRS_ALPHAREF => self.state.alpha_test_ref = (value & 0xFF) as u8,
            d3d9::D3DRS_ALPHABLENDENABLE => self.state.blend_state.enable = value != 0,
            d3d9::D3DRS_SRCBLEND => match value {
                d3d9::D3DBLEND_BOTHSRCALPHA => {
                    self.state.blend_state.src_factor = 2;
                    self.state.blend_state.dst_factor = 3;
                }
                d3d9::D3DBLEND_BOTHINVSRCALPHA => {
                    self.state.blend_state.src_factor = 3;
                    self.state.blend_state.dst_factor = 2;
                }
                _ => match d3d9_blend_to_aerogpu(value) {
                    Some(factor) => self.state.blend_state.src_factor = factor,
                    None => debug!(state_id, value, "unknown D3D9 blend factor"),
                },
            },
            d3d9::D3DRS_DESTBLEND => match d3d9_blend_to_aerogpu(value) {
                Some(factor) => self.state.blend_state.dst_factor = factor,
                None => debug!(state_id, value, "unknown D3D9 blend factor"),
            },
            d3d9::D3DRS_BLENDOP => match d3d9_blend_op_to_aerogpu(value) {
                Some(op) => self.state.blend_state.blend_op = op,
                None => debug!(state_id, value, "unknown D3D9 blend op"),
            },
            d3d9::D3DRS_SEPARATEALPHABLENDENABLE
            | d3d9::D3DRS_SRCBLENDALPHA
            | d3d9::D3DRS_DESTBLENDALPHA
            | d3d9::D3DRS_BLENDOPALPHA => self.update_separate_alpha_blend_from_render_state(),
            d3d9::D3DRS_COLORWRITEENABLE => {
                self.state.blend_state.color_write_mask[0] = (value & 0xF) as u8
            }
            d3d9::D3DRS_COLORWRITEENABLE1 => {
                self.state.blend_state.color_write_mask[1] = (value & 0xF) as u8
            }
            d3d9::D3DRS_COLORWRITEENABLE2 => {
                self.state.blend_state.color_write_mask[2] = (value & 0xF) as u8
            }
            d3d9::D3DRS_COLORWRITEENABLE3 => {
                self.state.blend_state.color_write_mask[3] = (value & 0xF) as u8
            }
            d3d9::D3DRS_BLENDFACTOR => {
                let a = ((value >> 24) & 0xFF) as f32 / 255.0;
                let r = ((value >> 16) & 0xFF) as f32 / 255.0;
                let g = ((value >> 8) & 0xFF) as f32 / 255.0;
                let b = (value & 0xFF) as f32 / 255.0;
                self.state.blend_constant = [r, g, b, a];
            }
            d3d9::D3DRS_SRGBWRITEENABLE => {
                // sRGB write is handled by selecting an sRGB render-target view at draw time.
            }
            d3d9::D3DRS_SCISSORTESTENABLE => {
                self.state.rasterizer_state.scissor_enable = value != 0
            }
            d3d9::D3DRS_FRONTCOUNTERCLOCKWISE => {
                self.state.rasterizer_state.front_ccw = value != 0;
                self.update_cull_mode_from_render_state();
            }
            d3d9::D3DRS_CULLMODE => self.update_cull_mode_from_render_state(),
            _ => debug!(state_id, value, "ignoring unsupported D3D9 render state"),
        }

        // D3D9 uses the color blend settings for alpha unless separate-alpha blending is enabled.
        if matches!(
            state_id,
            d3d9::D3DRS_SRCBLEND | d3d9::D3DRS_DESTBLEND | d3d9::D3DRS_BLENDOP
        ) && !self.separate_alpha_blend_enabled()
        {
            self.sync_alpha_blend_to_color();
        }
    }

    fn separate_alpha_blend_enabled(&self) -> bool {
        self.state
            .render_states
            .get(d3d9::D3DRS_SEPARATEALPHABLENDENABLE as usize)
            .copied()
            .unwrap_or(0)
            != 0
    }

    fn sync_alpha_blend_to_color(&mut self) {
        self.state.blend_state.src_factor_alpha = self.state.blend_state.src_factor;
        self.state.blend_state.dst_factor_alpha = self.state.blend_state.dst_factor;
        self.state.blend_state.blend_op_alpha = self.state.blend_state.blend_op;
    }

    fn map_d3d9_cull_mode(raw: u32, front_ccw: bool) -> Option<u32> {
        match raw {
            0 | d3d9::D3DCULL_NONE => Some(cmd::AerogpuCullMode::None as u32),
            d3d9::D3DCULL_CW => {
                if front_ccw {
                    // CW triangles are back faces when front faces are CCW.
                    Some(cmd::AerogpuCullMode::Back as u32)
                } else {
                    Some(cmd::AerogpuCullMode::Front as u32)
                }
            }
            d3d9::D3DCULL_CCW => {
                if front_ccw {
                    Some(cmd::AerogpuCullMode::Front as u32)
                } else {
                    Some(cmd::AerogpuCullMode::Back as u32)
                }
            }
            _ => None,
        }
    }

    fn update_separate_alpha_blend_from_render_state(&mut self) {
        if !self.separate_alpha_blend_enabled() {
            self.sync_alpha_blend_to_color();
            return;
        }

        let src_raw = self
            .state
            .render_states
            .get(d3d9::D3DRS_SRCBLENDALPHA as usize)
            .copied()
            .unwrap_or(d3d9::D3DBLEND_ONE);
        let dst_raw = self
            .state
            .render_states
            .get(d3d9::D3DRS_DESTBLENDALPHA as usize)
            .copied()
            .unwrap_or(d3d9::D3DBLEND_ZERO);
        let op_raw = self
            .state
            .render_states
            .get(d3d9::D3DRS_BLENDOPALPHA as usize)
            .copied()
            .unwrap_or(1);

        let src_raw = if src_raw == 0 {
            d3d9::D3DBLEND_ONE
        } else {
            src_raw
        };
        let dst_raw = if dst_raw == 0 {
            d3d9::D3DBLEND_ZERO
        } else {
            dst_raw
        };
        let op_raw = if op_raw == 0 { 1 } else { op_raw };

        match src_raw {
            d3d9::D3DBLEND_BOTHSRCALPHA => {
                self.state.blend_state.src_factor_alpha = 2;
                self.state.blend_state.dst_factor_alpha = 3;
            }
            d3d9::D3DBLEND_BOTHINVSRCALPHA => {
                self.state.blend_state.src_factor_alpha = 3;
                self.state.blend_state.dst_factor_alpha = 2;
            }
            _ => {
                if let Some(factor) = d3d9_blend_to_aerogpu(src_raw) {
                    self.state.blend_state.src_factor_alpha = factor;
                } else {
                    debug!(
                        state_id = d3d9::D3DRS_SRCBLENDALPHA,
                        value = src_raw,
                        "unknown D3D9 blend factor"
                    );
                }
                if let Some(factor) = d3d9_blend_to_aerogpu(dst_raw) {
                    self.state.blend_state.dst_factor_alpha = factor;
                } else {
                    debug!(
                        state_id = d3d9::D3DRS_DESTBLENDALPHA,
                        value = dst_raw,
                        "unknown D3D9 blend factor"
                    );
                }
            }
        }

        match d3d9_blend_op_to_aerogpu(op_raw) {
            Some(op) => self.state.blend_state.blend_op_alpha = op,
            None => debug!(
                state_id = d3d9::D3DRS_BLENDOPALPHA,
                value = op_raw,
                "unknown D3D9 blend op"
            ),
        }
    }

    fn update_cull_mode_from_render_state(&mut self) {
        let raw = self
            .state
            .render_states
            .get(d3d9::D3DRS_CULLMODE as usize)
            .copied()
            .unwrap_or(d3d9::D3DCULL_NONE);

        let front_ccw = self.state.rasterizer_state.front_ccw;
        let Some(mapped) = Self::map_d3d9_cull_mode(raw, front_ccw) else {
            debug!(raw, "unknown D3D9 cull mode");
            return;
        };
        self.state.rasterizer_state.cull_mode = mapped;
    }

    fn sampler_for_state(&mut self, state: D3d9SamplerState) -> Arc<wgpu::Sampler> {
        let state = self.canonicalize_sampler_state(state);
        if let Some(sampler) = self.sampler_cache.get(&state) {
            return sampler.clone();
        }

        let sampler = Arc::new(create_wgpu_sampler(&self.device, self.downlevel_flags, &state));
        self.sampler_cache.insert(state, sampler.clone());
        sampler
    }

    fn canonicalize_sampler_state(&self, mut state: D3d9SamplerState) -> D3d9SamplerState {
        let border_supported = self
            .device
            .features()
            .contains(wgpu::Features::ADDRESS_MODE_CLAMP_TO_BORDER);

        let mut uses_border = false;
        for addr in [&mut state.address_u, &mut state.address_v, &mut state.address_w] {
            *addr = match *addr {
                // `0` is sometimes used by guests to mean the default WRAP.
                0 | d3d9::D3DTADDRESS_WRAP => d3d9::D3DTADDRESS_WRAP,
                d3d9::D3DTADDRESS_MIRROR => d3d9::D3DTADDRESS_MIRROR,
                d3d9::D3DTADDRESS_CLAMP => d3d9::D3DTADDRESS_CLAMP,
                d3d9::D3DTADDRESS_BORDER => {
                    if border_supported {
                        uses_border = true;
                        d3d9::D3DTADDRESS_BORDER
                    } else {
                        // `create_wgpu_sampler` falls back to ClampToEdge when border mode is
                        // unsupported; canonicalize so the sampler-cache key matches that.
                        d3d9::D3DTADDRESS_CLAMP
                    }
                }
                d3d9::D3DTADDRESS_MIRRORONCE => d3d9::D3DTADDRESS_MIRROR,
                _ => d3d9::D3DTADDRESS_WRAP,
            };
        }

        if uses_border {
            state.border_color = match state.border_color {
                0x0000_0000 | 0xFF00_0000 | 0xFFFF_FFFF => state.border_color,
                other => {
                    log_unsupported_d3d9_border_color_once(other);
                    0x0000_0000
                }
            };
        } else {
            // Border color is ignored unless at least one address mode is BORDER.
            state.border_color = 0;
        }

        fn canonicalize_filter(value: u32, allow_none: bool) -> u32 {
            match value {
                d3d9::D3DTEXF_POINT => d3d9::D3DTEXF_POINT,
                d3d9::D3DTEXF_LINEAR => d3d9::D3DTEXF_LINEAR,
                d3d9::D3DTEXF_ANISOTROPIC => d3d9::D3DTEXF_ANISOTROPIC,
                d3d9::D3DTEXF_NONE => {
                    if allow_none {
                        d3d9::D3DTEXF_NONE
                    } else {
                        // D3D9 only defines NONE for the mip filter; map it to LINEAR for min/mag.
                        d3d9::D3DTEXF_LINEAR
                    }
                }
                _ => d3d9::D3DTEXF_LINEAR,
            }
        }

        state.min_filter = canonicalize_filter(state.min_filter, false);
        state.mag_filter = canonicalize_filter(state.mag_filter, false);
        state.mip_filter = canonicalize_filter(state.mip_filter, true);

        // Mipmap LOD clamp.
        let max_lod = if state.mip_filter == d3d9::D3DTEXF_NONE {
            0
        } else {
            32
        };
        state.max_mip_level = state.max_mip_level.min(max_lod);

        // Anisotropy.
        let anisotropic_requested = state.min_filter == d3d9::D3DTEXF_ANISOTROPIC
            || state.mag_filter == d3d9::D3DTEXF_ANISOTROPIC;
        let anisotropic_supported =
            self.downlevel_flags
                .contains(wgpu::DownlevelFlags::ANISOTROPIC_FILTERING);
        if anisotropic_requested && anisotropic_supported {
            state.max_anisotropy = state.max_anisotropy.clamp(1, 16);
        } else {
            state.max_anisotropy = 1;
        }

        state
    }

    fn set_sampler_state_u32(&mut self, shader_stage: u32, slot: u32, state_id: u32, value: u32) {
        let is_vertex = shader_stage == cmd::AerogpuShaderStage::Vertex as u32;
        let is_pixel = shader_stage == cmd::AerogpuShaderStage::Pixel as u32;
        if !is_vertex && !is_pixel {
            debug!(
                shader_stage,
                slot,
                state_id,
                value,
                "ignoring sampler state with unknown shader stage"
            );
            return;
        }

        if slot >= MAX_SAMPLERS as u32 {
            debug!(slot, state_id, value, "ignoring out-of-range sampler state");
            return;
        }

        if state_id > MAX_REASONABLE_SAMPLER_STATE_ID {
            debug!(
                slot,
                state_id, value, "ignoring suspiciously large D3D9 sampler state id"
            );
            return;
        }

        let slot = slot as usize;
        let idx = state_id as usize;

        // Update the raw state cache first (used for SRGBTEXTURE, and for parity with D3D9's
        // "unknown state ids still cache their values" behavior).
        {
            let raw_states = if is_vertex {
                &mut self.state.sampler_states_vs[slot]
            } else {
                &mut self.state.sampler_states_ps[slot]
            };
            if idx >= raw_states.len() {
                raw_states.resize(idx + 1, 0);
            }
            if raw_states[idx] == value {
                return;
            }
            raw_states[idx] = value;
        }

        let mut affects_sampler = false;
        let mut affects_bind_group = false;

        // Update our wgpu sampler cache state.
        let new_sampler_state = {
            let sampler_state = if is_vertex {
                &mut self.sampler_state_vs[slot]
            } else {
                &mut self.sampler_state_ps[slot]
            };

            match state_id {
                d3d9::D3DSAMP_ADDRESSU => {
                    sampler_state.address_u = value;
                    affects_sampler = true;
                    affects_bind_group = true;
                }
                d3d9::D3DSAMP_ADDRESSV => {
                    sampler_state.address_v = value;
                    affects_sampler = true;
                    affects_bind_group = true;
                }
                d3d9::D3DSAMP_ADDRESSW => {
                    sampler_state.address_w = value;
                    affects_sampler = true;
                    affects_bind_group = true;
                }
                d3d9::D3DSAMP_BORDERCOLOR => {
                    sampler_state.border_color = value;
                    affects_sampler = true;
                    affects_bind_group = true;
                }
                d3d9::D3DSAMP_MINFILTER => {
                    sampler_state.min_filter = value;
                    affects_sampler = true;
                    affects_bind_group = true;
                }
                d3d9::D3DSAMP_MAGFILTER => {
                    sampler_state.mag_filter = value;
                    affects_sampler = true;
                    affects_bind_group = true;
                }
                d3d9::D3DSAMP_MIPFILTER => {
                    sampler_state.mip_filter = value;
                    affects_sampler = true;
                    affects_bind_group = true;
                }
                d3d9::D3DSAMP_MAXANISOTROPY => {
                    sampler_state.max_anisotropy = value;
                    affects_sampler = true;
                    affects_bind_group = true;
                }
                d3d9::D3DSAMP_MAXMIPLEVEL => {
                    sampler_state.max_mip_level = value;
                    affects_sampler = true;
                    affects_bind_group = true;
                }
                d3d9::D3DSAMP_SRGBTEXTURE => {
                    // sRGB sampling is implemented by binding a view with an sRGB format, not by
                    // changing the wgpu sampler object.
                    affects_bind_group = true;
                }
                _ => {
                    debug!(
                        slot,
                        state_id, value, "ignoring unsupported D3D9 sampler state"
                    );
                }
            }

            affects_sampler.then_some(*sampler_state)
        };

        if let Some(state) = new_sampler_state {
            let sampler = self.sampler_for_state(state);
            let stage_sampler = if is_vertex {
                &mut self.samplers_vs[slot]
            } else {
                &mut self.samplers_ps[slot]
            };
            let changed = !Arc::ptr_eq(stage_sampler, &sampler);
            if changed {
                *stage_sampler = sampler;
            }
            // If the wgpu sampler object didn't change, we don't need to rebuild the bind group.
            affects_bind_group &= changed;
        }

        if affects_bind_group {
            self.bind_group_dirty = true;
        }
    }

    fn vertex_buffer_layouts(
        &self,
        input_layout: &InputLayout,
        uses_semantic_locations: bool,
    ) -> Result<VertexInputs, AerogpuD3d9Error> {
        let mut streams: Vec<u8> = input_layout
            .decl
            .elements
            .iter()
            .map(|e| e.stream)
            .collect();
        streams.sort_unstable();
        streams.dedup();

        let mut stream_to_slot = HashMap::<u8, u32>::new();
        for (slot, stream) in streams.iter().copied().enumerate() {
            stream_to_slot.insert(stream, slot as u32);
        }

        let mut buffers: Vec<VertexBufferLayoutOwned> = streams
            .iter()
            .map(|stream| {
                let stride = self
                    .state
                    .vertex_buffers
                    .get(*stream as usize)
                    .and_then(|b| b.as_ref())
                    .map(|b| b.stride_bytes as u64)
                    .unwrap_or(0);
                VertexBufferLayoutOwned {
                    array_stride: stride,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: Vec::new(),
                }
            })
            .collect();

        let location_map = StandardLocationMap;
        let mut seen_locations = HashMap::<u32, (aero_d3d9::vertex::DeclUsage, u8)>::new();

        // Map declaration elements to shader locations.
        for (i, e) in input_layout.decl.elements.iter().enumerate() {
            let Some(&slot) = stream_to_slot.get(&e.stream) else {
                continue;
            };
            let fmt = map_decl_type_to_vertex_format(e.ty)?;
            let shader_location = if uses_semantic_locations {
                let loc = location_map
                    .location_for(e.usage, e.usage_index)
                    .map_err(|e| AerogpuD3d9Error::VertexDeclaration(e.to_string()))?;
                if let Some((prev_usage, prev_index)) =
                    seen_locations.insert(loc, (e.usage, e.usage_index))
                {
                    return Err(AerogpuD3d9Error::VertexDeclaration(format!(
                        "vertex declaration maps multiple elements to WGSL @location({loc}): {prev_usage:?}{prev_index} and {:?}{}",
                        e.usage, e.usage_index
                    )));
                }
                loc
            } else {
                i as u32
            };
            buffers[slot as usize]
                .attributes
                .push(wgpu::VertexAttribute {
                    format: fmt,
                    offset: e.offset as u64,
                    shader_location,
                });
        }

        for (i, stream) in streams.iter().copied().enumerate() {
            if buffers[i].attributes.is_empty() {
                continue;
            }
            if buffers[i].array_stride == 0 {
                return Err(AerogpuD3d9Error::MissingVertexBuffer { stream });
            }
        }

        Ok(VertexInputs {
            streams,
            stream_to_slot,
            buffers,
        })
    }

    fn render_target_attachments(
        &self,
    ) -> Result<(Vec<Option<&wgpu::TextureView>>, Option<&wgpu::TextureView>), AerogpuD3d9Error>
    {
        let srgb_write = self
            .state
            .render_states
            .get(d3d9::D3DRS_SRGBWRITEENABLE as usize)
            .copied()
            .unwrap_or(0)
            != 0;
        let rt = &self.state.render_targets;
        if rt.color_count == 0 {
            return Err(AerogpuD3d9Error::MissingRenderTargets);
        }
        let mut colors = Vec::new();
        for slot in 0..rt.color_count.min(8) as usize {
            let handle = rt.colors[slot];
            if handle == 0 {
                colors.push(None);
                continue;
            }
            let underlying = self.resolve_resource_handle(handle)?;
            let res = self
                .resources
                .get(&underlying)
                .ok_or(AerogpuD3d9Error::UnknownResource(handle))?;
            match res {
                Resource::Texture2d {
                    view, view_srgb, ..
                } => {
                    let out = if srgb_write {
                        view_srgb.as_ref().unwrap_or(view)
                    } else {
                        view
                    };
                    colors.push(Some(out));
                }
                _ => return Err(AerogpuD3d9Error::UnknownResource(handle)),
            }
        }

        let depth = if rt.depth_stencil == 0 {
            None
        } else {
            let handle = rt.depth_stencil;
            let underlying = self.resolve_resource_handle(handle)?;
            let res = self
                .resources
                .get(&underlying)
                .ok_or(AerogpuD3d9Error::UnknownResource(handle))?;
            match res {
                Resource::Texture2d { view, .. } => Some(view),
                _ => return Err(AerogpuD3d9Error::UnknownResource(handle)),
            }
        };

        Ok((colors, depth))
    }

    fn render_target_formats(
        &self,
    ) -> Result<
        (
            Vec<Option<wgpu::TextureFormat>>,
            Vec<bool>,
            Option<wgpu::TextureFormat>,
        ),
        AerogpuD3d9Error,
    > {
        let srgb_write = self
            .state
            .render_states
            .get(d3d9::D3DRS_SRGBWRITEENABLE as usize)
            .copied()
            .unwrap_or(0)
            != 0;
        let rt = &self.state.render_targets;
        if rt.color_count == 0 {
            return Err(AerogpuD3d9Error::MissingRenderTargets);
        }
        let mut colors = Vec::new();
        let mut color_is_x8 = Vec::new();
        for slot in 0..rt.color_count.min(8) as usize {
            let handle = rt.colors[slot];
            if handle == 0 {
                colors.push(None);
                color_is_x8.push(false);
                continue;
            }
            let underlying = self.resolve_resource_handle(handle)?;
            let res = self
                .resources
                .get(&underlying)
                .ok_or(AerogpuD3d9Error::UnknownResource(handle))?;
            match res {
                Resource::Texture2d {
                    format,
                    format_raw,
                    view_srgb,
                    ..
                } => {
                    let mut out = *format;
                    if srgb_write && view_srgb.is_some() {
                        out = match out {
                            wgpu::TextureFormat::Rgba8Unorm => wgpu::TextureFormat::Rgba8UnormSrgb,
                            wgpu::TextureFormat::Bgra8Unorm => wgpu::TextureFormat::Bgra8UnormSrgb,
                            other => other,
                        };
                    }
                    colors.push(Some(out));
                    color_is_x8.push(is_x8_format(*format_raw));
                }
                _ => return Err(AerogpuD3d9Error::UnknownResource(handle)),
            }
        }

        let depth = if rt.depth_stencil == 0 {
            None
        } else {
            let handle = rt.depth_stencil;
            let underlying = self.resolve_resource_handle(handle)?;
            let res = self
                .resources
                .get(&underlying)
                .ok_or(AerogpuD3d9Error::UnknownResource(handle))?;
            match res {
                Resource::Texture2d { format, .. } => Some(*format),
                _ => return Err(AerogpuD3d9Error::UnknownResource(handle)),
            }
        };

        Ok((colors, color_is_x8, depth))
    }
}

#[derive(Debug, Clone, Copy)]
enum DrawParams {
    NonIndexed {
        vertex_count: u32,
        instance_count: u32,
        first_vertex: u32,
        first_instance: u32,
    },
    Indexed {
        index_count: u32,
        instance_count: u32,
        first_index: u32,
        base_vertex: i32,
        first_instance: u32,
    },
}

fn align_to(value: u32, alignment: u32) -> u32 {
    debug_assert!(alignment.is_power_of_two());
    (value + alignment - 1) & !(alignment - 1)
}

fn align_down_u64(value: u64, alignment: u64) -> u64 {
    debug_assert!(alignment.is_power_of_two());
    value & !(alignment - 1)
}

fn align_up_u64(value: u64, alignment: u64) -> Result<u64, AerogpuD3d9Error> {
    debug_assert!(alignment.is_power_of_two());
    value
        .checked_add(alignment - 1)
        .map(|v| v & !(alignment - 1))
        .ok_or_else(|| AerogpuD3d9Error::Validation("alignment overflow".into()))
}

fn mip_dim(base: u32, level: u32) -> u32 {
    base.checked_shr(level).unwrap_or(0).max(1)
}

fn mip_extent(base: u32, mip_level: u32) -> u32 {
    mip_dim(base, mip_level)
}

#[derive(Debug, Clone, Copy)]
struct TexelBlockInfo {
    block_width: u32,
    block_height: u32,
    bytes_per_block: u32,
}

impl TexelBlockInfo {
    fn row_pitch_bytes(self, width: u32) -> Result<u32, AerogpuD3d9Error> {
        let blocks_w = width.div_ceil(self.block_width);
        blocks_w
            .checked_mul(self.bytes_per_block)
            .ok_or_else(|| AerogpuD3d9Error::Validation("row pitch overflow".into()))
    }

    fn rows_per_image(self, height: u32) -> u32 {
        height.div_ceil(self.block_height)
    }
}

fn aerogpu_format_texel_block_info(format_raw: u32) -> Result<TexelBlockInfo, AerogpuD3d9Error> {
    Ok(match format_raw {
        x if x == AerogpuFormat::B8G8R8A8Unorm as u32
            || x == AerogpuFormat::B8G8R8X8Unorm as u32
            || x == AerogpuFormat::B8G8R8A8UnormSrgb as u32
            || x == AerogpuFormat::B8G8R8X8UnormSrgb as u32
            || x == AerogpuFormat::R8G8B8A8Unorm as u32
            || x == AerogpuFormat::R8G8B8X8Unorm as u32
            || x == AerogpuFormat::R8G8B8A8UnormSrgb as u32
            || x == AerogpuFormat::R8G8B8X8UnormSrgb as u32 =>
        {
            TexelBlockInfo {
                block_width: 1,
                block_height: 1,
                bytes_per_block: 4,
            }
        }
        x if x == AerogpuFormat::B5G6R5Unorm as u32
            || x == AerogpuFormat::B5G5R5A1Unorm as u32 =>
        {
            TexelBlockInfo {
                block_width: 1,
                block_height: 1,
                bytes_per_block: 2,
            }
        }
        x if x == AerogpuFormat::BC1RgbaUnorm as u32
            || x == AerogpuFormat::BC1RgbaUnormSrgb as u32 =>
        {
            TexelBlockInfo {
                block_width: 4,
                block_height: 4,
                bytes_per_block: 8,
            }
        }
        x if x == AerogpuFormat::BC2RgbaUnorm as u32
            || x == AerogpuFormat::BC2RgbaUnormSrgb as u32
            || x == AerogpuFormat::BC3RgbaUnorm as u32
            || x == AerogpuFormat::BC3RgbaUnormSrgb as u32
            || x == AerogpuFormat::BC7RgbaUnorm as u32
            || x == AerogpuFormat::BC7RgbaUnormSrgb as u32 =>
        {
            TexelBlockInfo {
                block_width: 4,
                block_height: 4,
                bytes_per_block: 16,
            }
        }
        x if x == AerogpuFormat::D24UnormS8Uint as u32 || x == AerogpuFormat::D32Float as u32 => {
            TexelBlockInfo {
                block_width: 1,
                block_height: 1,
                bytes_per_block: 4,
            }
        }
        other => return Err(AerogpuD3d9Error::UnsupportedFormat(other)),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BcFormat {
    Bc1,
    Bc2,
    Bc3,
    Bc7,
}

fn aerogpu_format_bc(format_raw: u32) -> Option<BcFormat> {
    match format_raw {
        x if x == AerogpuFormat::BC1RgbaUnorm as u32 || x == AerogpuFormat::BC1RgbaUnormSrgb as u32 => {
            Some(BcFormat::Bc1)
        }
        x if x == AerogpuFormat::BC2RgbaUnorm as u32 || x == AerogpuFormat::BC2RgbaUnormSrgb as u32 => {
            Some(BcFormat::Bc2)
        }
        x if x == AerogpuFormat::BC3RgbaUnorm as u32 || x == AerogpuFormat::BC3RgbaUnormSrgb as u32 => {
            Some(BcFormat::Bc3)
        }
        x if x == AerogpuFormat::BC7RgbaUnorm as u32 || x == AerogpuFormat::BC7RgbaUnormSrgb as u32 => {
            Some(BcFormat::Bc7)
        }
        _ => None,
    }
}

#[derive(Debug)]
struct GuestTextureLinearLayout {
    mip_offsets: Vec<u64>,
    layer_stride_bytes: u64,
    total_size_bytes: u64,
}

fn guest_texture_linear_layout(
    format_raw: u32,
    width: u32,
    height: u32,
    mip_level_count: u32,
    array_layers: u32,
    mip0_row_pitch_bytes: u32,
) -> Result<GuestTextureLinearLayout, AerogpuD3d9Error> {
    if mip_level_count == 0 || array_layers == 0 {
        return Err(AerogpuD3d9Error::Validation(
            "mip_level_count/array_layers must be >= 1".into(),
        ));
    }
    if mip0_row_pitch_bytes == 0 {
        return Err(AerogpuD3d9Error::Validation(
            "mip0 row_pitch_bytes must be non-zero".into(),
        ));
    }

    let block = aerogpu_format_texel_block_info(format_raw)?;

    // Validate mip0 row pitch against the minimum for the format.
    let min_row_pitch = block.row_pitch_bytes(width)?;
    if mip0_row_pitch_bytes < min_row_pitch {
        return Err(AerogpuD3d9Error::Validation(format!(
            "row_pitch_bytes {mip0_row_pitch_bytes} is smaller than required {min_row_pitch}"
        )));
    }

    let mut mip_offsets = Vec::with_capacity(mip_level_count as usize);
    let mut layer_stride: u64 = 0;
    for mip in 0..mip_level_count {
        mip_offsets.push(layer_stride);

        let mip_w = mip_extent(width, mip);
        let mip_h = mip_extent(height, mip);

        let row_pitch = if mip == 0 {
            mip0_row_pitch_bytes
        } else {
            block.row_pitch_bytes(mip_w)?
        };
        let rows = block.rows_per_image(mip_h);
        let size = u64::from(row_pitch)
            .checked_mul(u64::from(rows))
            .ok_or_else(|| AerogpuD3d9Error::Validation("texture backing overflow".into()))?;
        layer_stride = layer_stride
            .checked_add(size)
            .ok_or_else(|| AerogpuD3d9Error::Validation("texture backing overflow".into()))?;
    }

    let total_size = layer_stride
        .checked_mul(u64::from(array_layers))
        .ok_or_else(|| AerogpuD3d9Error::Validation("texture backing overflow".into()))?;

    Ok(GuestTextureLinearLayout {
        mip_offsets,
        layer_stride_bytes: layer_stride,
        total_size_bytes: total_size,
    })
}

fn is_x8_format(format_raw: u32) -> bool {
    format_raw == AerogpuFormat::B8G8R8X8Unorm as u32
        || format_raw == AerogpuFormat::R8G8B8X8Unorm as u32
        || format_raw == AerogpuFormat::B8G8R8X8UnormSrgb as u32
        || format_raw == AerogpuFormat::R8G8B8X8UnormSrgb as u32
}

fn force_opaque_alpha_rgba8(pixels: &mut [u8]) {
    for alpha in pixels.iter_mut().skip(3).step_by(4) {
        *alpha = 0xFF;
    }
}

fn expand_b5g6r5_unorm_to_rgba8(src: &[u8], dst: &mut [u8]) {
    debug_assert_eq!(src.len() % 2, 0);
    debug_assert_eq!(dst.len(), (src.len() / 2) * 4);
    for (src_px, dst_px) in src.chunks_exact(2).zip(dst.chunks_exact_mut(4)) {
        let v = u16::from_le_bytes([src_px[0], src_px[1]]);
        let b5 = (v & 0x1F) as u8;
        let g6 = ((v >> 5) & 0x3F) as u8;
        let r5 = ((v >> 11) & 0x1F) as u8;
        // Replicate bits to fill the 8-bit range.
        let r8 = (r5 << 3) | (r5 >> 2);
        let g8 = (g6 << 2) | (g6 >> 4);
        let b8 = (b5 << 3) | (b5 >> 2);
        dst_px[0] = r8;
        dst_px[1] = g8;
        dst_px[2] = b8;
        dst_px[3] = 0xFF;
    }
}

fn expand_b5g5r5a1_unorm_to_rgba8(src: &[u8], dst: &mut [u8]) {
    debug_assert_eq!(src.len() % 2, 0);
    debug_assert_eq!(dst.len(), (src.len() / 2) * 4);
    for (src_px, dst_px) in src.chunks_exact(2).zip(dst.chunks_exact_mut(4)) {
        let v = u16::from_le_bytes([src_px[0], src_px[1]]);
        let b5 = (v & 0x1F) as u8;
        let g5 = ((v >> 5) & 0x1F) as u8;
        let r5 = ((v >> 10) & 0x1F) as u8;
        let a1 = (v >> 15) as u8;
        let r8 = (r5 << 3) | (r5 >> 2);
        let g8 = (g5 << 3) | (g5 >> 2);
        let b8 = (b5 << 3) | (b5 >> 2);
        dst_px[0] = r8;
        dst_px[1] = g8;
        dst_px[2] = b8;
        dst_px[3] = if a1 != 0 { 0xFF } else { 0x00 };
    }
}

fn pack_rgba8_to_b5g6r5_unorm(src: &[u8], dst: &mut [u8]) {
    debug_assert_eq!(src.len() % 4, 0);
    debug_assert_eq!(dst.len(), (src.len() / 4) * 2);
    for (src_px, dst_px) in src.chunks_exact(4).zip(dst.chunks_exact_mut(2)) {
        let r8 = src_px[0];
        let g8 = src_px[1];
        let b8 = src_px[2];
        let r5 = (r8 >> 3) as u16;
        let g6 = (g8 >> 2) as u16;
        let b5 = (b8 >> 3) as u16;
        let v: u16 = b5 | (g6 << 5) | (r5 << 11);
        let out = v.to_le_bytes();
        dst_px[0] = out[0];
        dst_px[1] = out[1];
    }
}

fn pack_rgba8_to_b5g5r5a1_unorm(src: &[u8], dst: &mut [u8]) {
    debug_assert_eq!(src.len() % 4, 0);
    debug_assert_eq!(dst.len(), (src.len() / 4) * 2);
    for (src_px, dst_px) in src.chunks_exact(4).zip(dst.chunks_exact_mut(2)) {
        let r8 = src_px[0];
        let g8 = src_px[1];
        let b8 = src_px[2];
        let a8 = src_px[3];
        let r5 = (r8 >> 3) as u16;
        let g5 = (g8 >> 3) as u16;
        let b5 = (b8 >> 3) as u16;
        let a1 = if a8 >= 0x80 { 1u16 } else { 0u16 };
        let v: u16 = b5 | (g5 << 5) | (r5 << 10) | (a1 << 15);
        let out = v.to_le_bytes();
        dst_px[0] = out[0];
        dst_px[1] = out[1];
    }
}

fn coalesce_ranges(ranges: &mut Vec<Range<u64>>) {
    ranges.sort_by_key(|r| r.start);
    let mut out: Vec<Range<u64>> = Vec::with_capacity(ranges.len());
    for r in ranges.drain(..) {
        if r.start >= r.end {
            continue;
        }
        if let Some(last) = out.last_mut() {
            if r.start <= last.end {
                last.end = last.end.max(r.end);
                continue;
            }
        }
        out.push(r);
    }
    *ranges = out;
}

fn clamp_scissor_rect(
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    target_width: u32,
    target_height: u32,
) -> Option<(u32, u32, u32, u32)> {
    if width == 0 || height == 0 {
        return None;
    }
    if target_width == 0 || target_height == 0 {
        return None;
    }

    if x >= target_width || y >= target_height {
        return None;
    }

    let max_w = target_width - x;
    let max_h = target_height - y;
    let width = width.min(max_w);
    let height = height.min(max_h);

    if width == 0 || height == 0 {
        return None;
    }
    Some((x, y, width, height))
}

fn map_aerogpu_format(format: u32) -> Result<wgpu::TextureFormat, AerogpuD3d9Error> {
    Ok(match format {
        x if x == AerogpuFormat::B8G8R8A8Unorm as u32
            || x == AerogpuFormat::B8G8R8X8Unorm as u32
            || x == AerogpuFormat::B8G8R8A8UnormSrgb as u32
            || x == AerogpuFormat::B8G8R8X8UnormSrgb as u32 =>
        {
            wgpu::TextureFormat::Bgra8Unorm
        }
        x if x == AerogpuFormat::R8G8B8A8Unorm as u32
            || x == AerogpuFormat::R8G8B8X8Unorm as u32
            || x == AerogpuFormat::R8G8B8A8UnormSrgb as u32
            || x == AerogpuFormat::R8G8B8X8UnormSrgb as u32 =>
        {
            wgpu::TextureFormat::Rgba8Unorm
        }
        // wgpu 0.20's WebGPU `TextureFormat` does not expose 16-bit packed B5G6R5 / B5G5R5A1
        // formats, so we store these as RGBA8 and perform CPU conversion on upload paths.
        x if x == AerogpuFormat::B5G6R5Unorm as u32
            || x == AerogpuFormat::B5G5R5A1Unorm as u32 =>
        {
            wgpu::TextureFormat::Rgba8Unorm
        }
        x if x == AerogpuFormat::BC1RgbaUnorm as u32
            || x == AerogpuFormat::BC1RgbaUnormSrgb as u32 =>
        {
            wgpu::TextureFormat::Bc1RgbaUnorm
        }
        x if x == AerogpuFormat::BC2RgbaUnorm as u32
            || x == AerogpuFormat::BC2RgbaUnormSrgb as u32 =>
        {
            wgpu::TextureFormat::Bc2RgbaUnorm
        }
        x if x == AerogpuFormat::BC3RgbaUnorm as u32
            || x == AerogpuFormat::BC3RgbaUnormSrgb as u32 =>
        {
            wgpu::TextureFormat::Bc3RgbaUnorm
        }
        x if x == AerogpuFormat::BC7RgbaUnorm as u32
            || x == AerogpuFormat::BC7RgbaUnormSrgb as u32 =>
        {
            wgpu::TextureFormat::Bc7RgbaUnorm
        }
        x if x == AerogpuFormat::D24UnormS8Uint as u32 => wgpu::TextureFormat::Depth24PlusStencil8,
        x if x == AerogpuFormat::D32Float as u32 => wgpu::TextureFormat::Depth32Float,
        other => return Err(AerogpuD3d9Error::UnsupportedFormat(other)),
    })
}

fn bytes_per_pixel_aerogpu_format(format_raw: u32) -> Result<u32, AerogpuD3d9Error> {
    Ok(match format_raw {
        x if x == AerogpuFormat::B8G8R8A8Unorm as u32
            || x == AerogpuFormat::B8G8R8X8Unorm as u32 =>
        {
            4
        }
        x if x == AerogpuFormat::R8G8B8A8Unorm as u32
            || x == AerogpuFormat::R8G8B8X8Unorm as u32 =>
        {
            4
        }
        x if x == AerogpuFormat::B5G6R5Unorm as u32 => 2,
        x if x == AerogpuFormat::B5G5R5A1Unorm as u32 => 2,
        x if x == AerogpuFormat::D24UnormS8Uint as u32 => 4,
        x if x == AerogpuFormat::D32Float as u32 => 4,
        other => return Err(AerogpuD3d9Error::UnsupportedFormat(other)),
    })
}

fn bytes_per_pixel(format: wgpu::TextureFormat) -> u32 {
    match format {
        wgpu::TextureFormat::Rgba8Unorm
        | wgpu::TextureFormat::Rgba8UnormSrgb
        | wgpu::TextureFormat::Bgra8Unorm
        | wgpu::TextureFormat::Bgra8UnormSrgb => 4,
        wgpu::TextureFormat::Depth24PlusStencil8 => 4,
        wgpu::TextureFormat::Depth32Float => 4,
        _ => 4,
    }
}

fn map_topology(topology: u32) -> Result<wgpu::PrimitiveTopology, AerogpuD3d9Error> {
    Ok(match topology {
        x if x == cmd::AerogpuPrimitiveTopology::PointList as u32 => {
            wgpu::PrimitiveTopology::PointList
        }
        x if x == cmd::AerogpuPrimitiveTopology::LineList as u32 => {
            wgpu::PrimitiveTopology::LineList
        }
        x if x == cmd::AerogpuPrimitiveTopology::LineStrip as u32 => {
            wgpu::PrimitiveTopology::LineStrip
        }
        x if x == cmd::AerogpuPrimitiveTopology::TriangleList as u32 => {
            wgpu::PrimitiveTopology::TriangleList
        }
        x if x == cmd::AerogpuPrimitiveTopology::TriangleStrip as u32 => {
            wgpu::PrimitiveTopology::TriangleStrip
        }
        x if x == cmd::AerogpuPrimitiveTopology::TriangleFan as u32 => {
            // wgpu/WebGPU do not support TriangleFan directly. We use a TriangleList pipeline and
            // expand the fan into a triangle-list index buffer at draw time when needed.
            wgpu::PrimitiveTopology::TriangleList
        }
        other => return Err(AerogpuD3d9Error::UnsupportedTopology(other)),
    })
}

fn map_decl_type_to_vertex_format(
    ty: aero_d3d9::vertex::DeclType,
) -> Result<wgpu::VertexFormat, AerogpuD3d9Error> {
    use aero_d3d9::vertex::DeclType;
    Ok(match ty {
        DeclType::Float1 => wgpu::VertexFormat::Float32,
        DeclType::Float2 => wgpu::VertexFormat::Float32x2,
        DeclType::Float3 => wgpu::VertexFormat::Float32x3,
        DeclType::Float4 => wgpu::VertexFormat::Float32x4,
        DeclType::D3dColor => wgpu::VertexFormat::Unorm8x4,
        DeclType::UByte4N => wgpu::VertexFormat::Unorm8x4,
        DeclType::Unused => wgpu::VertexFormat::Float32x4,
        _ => wgpu::VertexFormat::Float32x4,
    })
}

fn map_color_write_mask(mask: u8) -> wgpu::ColorWrites {
    let mut out = wgpu::ColorWrites::empty();
    if mask & 0b0001 != 0 {
        out |= wgpu::ColorWrites::RED;
    }
    if mask & 0b0010 != 0 {
        out |= wgpu::ColorWrites::GREEN;
    }
    if mask & 0b0100 != 0 {
        out |= wgpu::ColorWrites::BLUE;
    }
    if mask & 0b1000 != 0 {
        out |= wgpu::ColorWrites::ALPHA;
    }
    out
}

fn map_blend_state(state: BlendState) -> Option<wgpu::BlendState> {
    if !state.enable {
        return None;
    }

    let color = wgpu::BlendComponent {
        src_factor: map_blend_factor(state.src_factor),
        dst_factor: map_blend_factor(state.dst_factor),
        operation: map_blend_op(state.blend_op),
    };
    let alpha = wgpu::BlendComponent {
        src_factor: map_blend_factor(state.src_factor_alpha),
        dst_factor: map_blend_factor(state.dst_factor_alpha),
        operation: map_blend_op(state.blend_op_alpha),
    };
    Some(wgpu::BlendState { color, alpha })
}

fn map_blend_factor(factor: u32) -> wgpu::BlendFactor {
    match factor {
        0 => wgpu::BlendFactor::Zero,
        1 => wgpu::BlendFactor::One,
        2 => wgpu::BlendFactor::SrcAlpha,
        3 => wgpu::BlendFactor::OneMinusSrcAlpha,
        4 => wgpu::BlendFactor::DstAlpha,
        5 => wgpu::BlendFactor::OneMinusDstAlpha,
        6 => wgpu::BlendFactor::Constant,
        7 => wgpu::BlendFactor::OneMinusConstant,
        _ => wgpu::BlendFactor::One,
    }
}

fn map_blend_op(op: u32) -> wgpu::BlendOperation {
    match op {
        0 => wgpu::BlendOperation::Add,
        1 => wgpu::BlendOperation::Subtract,
        2 => wgpu::BlendOperation::ReverseSubtract,
        3 => wgpu::BlendOperation::Min,
        4 => wgpu::BlendOperation::Max,
        _ => wgpu::BlendOperation::Add,
    }
}

fn map_compare_func(func: u32) -> wgpu::CompareFunction {
    match func {
        0 => wgpu::CompareFunction::Never,
        1 => wgpu::CompareFunction::Less,
        2 => wgpu::CompareFunction::Equal,
        3 => wgpu::CompareFunction::LessEqual,
        4 => wgpu::CompareFunction::Greater,
        5 => wgpu::CompareFunction::NotEqual,
        6 => wgpu::CompareFunction::GreaterEqual,
        7 => wgpu::CompareFunction::Always,
        _ => wgpu::CompareFunction::Always,
    }
}

fn map_stencil_op(op: u32) -> wgpu::StencilOperation {
    match op {
        0 => wgpu::StencilOperation::Keep,
        1 => wgpu::StencilOperation::Zero,
        2 => wgpu::StencilOperation::Replace,
        3 => wgpu::StencilOperation::IncrementClamp,
        4 => wgpu::StencilOperation::DecrementClamp,
        5 => wgpu::StencilOperation::Invert,
        6 => wgpu::StencilOperation::IncrementWrap,
        7 => wgpu::StencilOperation::DecrementWrap,
        _ => wgpu::StencilOperation::Keep,
    }
}

fn create_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    // Match `aero-d3d9` shader generation: all bindings are in group(0), with
    // binding(0)=constants and (texture,sampler) pairs laid out as:
    //   texture binding = 1 + 2*s
    //   sampler binding = 2 + 2*s
    let mut entries = Vec::with_capacity(1 + MAX_SAMPLERS * 2);
    entries.push(wgpu::BindGroupLayoutEntry {
        binding: 0,
        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: wgpu::BufferSize::new(CONSTANTS_BUFFER_SIZE_BYTES as u64),
        },
        count: None,
    });

    for slot in 0..MAX_SAMPLERS {
        let tex_binding = 1u32 + slot as u32 * 2;
        let samp_binding = tex_binding + 1;
        entries.push(wgpu::BindGroupLayoutEntry {
            binding: tex_binding,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Texture {
                multisampled: false,
                view_dimension: wgpu::TextureViewDimension::D2,
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
            },
            count: None,
        });
        entries.push(wgpu::BindGroupLayoutEntry {
            binding: samp_binding,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        });
    }

    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("aerogpu-d3d9.bind_group_layout"),
        entries: &entries,
    })
}

fn log_unsupported_d3d9_border_color_once(border_color: u32) {
    static SEEN_UNSUPPORTED: OnceLock<std::sync::Mutex<HashSet<u32>>> = OnceLock::new();
    let set = SEEN_UNSUPPORTED.get_or_init(|| std::sync::Mutex::new(HashSet::new()));
    if let Ok(mut guard) = set.lock() {
        if guard.insert(border_color) {
            debug!(
                value = border_color,
                "unsupported D3D9 border color; mapping to transparent black"
            );
        }
    }
}

fn create_wgpu_sampler(
    device: &wgpu::Device,
    downlevel_flags: wgpu::DownlevelFlags,
    state: &D3d9SamplerState,
) -> wgpu::Sampler {
    fn addr(device: &wgpu::Device, value: u32) -> wgpu::AddressMode {
        match value {
            d3d9::D3DTADDRESS_WRAP | 0 => wgpu::AddressMode::Repeat,
            d3d9::D3DTADDRESS_MIRROR => wgpu::AddressMode::MirrorRepeat,
            d3d9::D3DTADDRESS_CLAMP => wgpu::AddressMode::ClampToEdge,
            d3d9::D3DTADDRESS_BORDER => {
                if device
                    .features()
                    .contains(wgpu::Features::ADDRESS_MODE_CLAMP_TO_BORDER)
                {
                    wgpu::AddressMode::ClampToBorder
                } else {
                    wgpu::AddressMode::ClampToEdge
                }
            }
            d3d9::D3DTADDRESS_MIRRORONCE => wgpu::AddressMode::MirrorRepeat,
            _ => wgpu::AddressMode::Repeat,
        }
    }

    fn filter(value: u32) -> wgpu::FilterMode {
        match value {
            d3d9::D3DTEXF_POINT => wgpu::FilterMode::Nearest,
            d3d9::D3DTEXF_LINEAR | d3d9::D3DTEXF_ANISOTROPIC => wgpu::FilterMode::Linear,
            d3d9::D3DTEXF_NONE => wgpu::FilterMode::Linear,
            _ => wgpu::FilterMode::Linear,
        }
    }

    let address_mode_u = addr(device, state.address_u);
    let address_mode_v = addr(device, state.address_v);
    let address_mode_w = addr(device, state.address_w);

    let uses_border = matches!(address_mode_u, wgpu::AddressMode::ClampToBorder)
        || matches!(address_mode_v, wgpu::AddressMode::ClampToBorder)
        || matches!(address_mode_w, wgpu::AddressMode::ClampToBorder);

    let border_color = if uses_border
        && device
            .features()
            .contains(wgpu::Features::ADDRESS_MODE_CLAMP_TO_BORDER)
    {
        let mapped = match state.border_color {
            0x0000_0000 => wgpu::SamplerBorderColor::TransparentBlack,
            0xFF00_0000 => wgpu::SamplerBorderColor::OpaqueBlack,
            0xFFFF_FFFF => wgpu::SamplerBorderColor::OpaqueWhite,
            other => {
                log_unsupported_d3d9_border_color_once(other);
                wgpu::SamplerBorderColor::TransparentBlack
            }
        };
        Some(mapped)
    } else {
        None
    };

    let min_filter = filter(state.min_filter);
    let mag_filter = filter(state.mag_filter);
    let mipmap_filter = filter(state.mip_filter);
    let lod_max_clamp = if state.mip_filter == d3d9::D3DTEXF_NONE || state.mip_filter == 0 {
        0.0
    } else {
        32.0
    };
    // Prevent invalid sampler descriptors when the guest sets a huge MAXMIPLEVEL value or when
    // mipmapping is disabled (MIPFILTER=NONE implies lod_max_clamp=0.0).
    let lod_min_clamp = (state.max_mip_level as f32).min(lod_max_clamp);

    let anisotropy_clamp = if (state.min_filter == d3d9::D3DTEXF_ANISOTROPIC
        || state.mag_filter == d3d9::D3DTEXF_ANISOTROPIC)
        && downlevel_flags.contains(wgpu::DownlevelFlags::ANISOTROPIC_FILTERING)
    {
        state.max_anisotropy.clamp(1, 16) as u16
    } else {
        1
    };

    device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("aerogpu-d3d9.sampler"),
        address_mode_u,
        address_mode_v,
        address_mode_w,
        mag_filter,
        min_filter,
        mipmap_filter,
        lod_min_clamp,
        lod_max_clamp,
        compare: None,
        anisotropy_clamp,
        border_color,
    })
}

fn d3d9_compare_to_aerogpu(value: u32) -> Option<u32> {
    // D3D9: D3DCMP_* is 1-based, AeroGPU compare func is 0-based.
    match value {
        1..=8 => Some(value - 1),
        _ => None,
    }
}

fn d3d9_stencil_op_to_aerogpu(value: u32) -> Option<u32> {
    match value {
        1..=8 => Some(value - 1),
        _ => None,
    }
}

fn d3d9_blend_to_aerogpu(value: u32) -> Option<u32> {
    Some(match value {
        d3d9::D3DBLEND_ZERO => 0,
        d3d9::D3DBLEND_ONE => 1,
        d3d9::D3DBLEND_SRCALPHA => 2,
        d3d9::D3DBLEND_INVSRCALPHA => 3,
        d3d9::D3DBLEND_DESTALPHA => 4,
        d3d9::D3DBLEND_INVDESTALPHA => 5,
        d3d9::D3DBLEND_BLENDFACTOR => 6,
        d3d9::D3DBLEND_INVBLENDFACTOR => 7,
        _ => return None,
    })
}

fn d3d9_blend_op_to_aerogpu(value: u32) -> Option<u32> {
    // D3D9: D3DBLENDOP_* is 1-based, AeroGPU blend op is 0-based.
    match value {
        1..=5 => Some(value - 1),
        _ => None,
    }
}

mod d3d9 {
    // D3DRENDERSTATETYPE (subset).
    pub const D3DRS_ZENABLE: u32 = 7;
    pub const D3DRS_ZWRITEENABLE: u32 = 14;
    pub const D3DRS_ZFUNC: u32 = 23;

    pub const D3DRS_ALPHATESTENABLE: u32 = 15;
    pub const D3DRS_ALPHAREF: u32 = 24;
    pub const D3DRS_ALPHAFUNC: u32 = 25;

    pub const D3DRS_CULLMODE: u32 = 22;
    pub const D3DRS_FRONTCOUNTERCLOCKWISE: u32 = 18;

    pub const D3DRS_ALPHABLENDENABLE: u32 = 27;
    pub const D3DRS_SRCBLEND: u32 = 19;
    pub const D3DRS_DESTBLEND: u32 = 20;
    pub const D3DRS_BLENDOP: u32 = 171;
    pub const D3DRS_SEPARATEALPHABLENDENABLE: u32 = 206;
    pub const D3DRS_SRCBLENDALPHA: u32 = 207;
    pub const D3DRS_DESTBLENDALPHA: u32 = 208;
    pub const D3DRS_BLENDOPALPHA: u32 = 209;

    pub const D3DRS_COLORWRITEENABLE: u32 = 168;
    pub const D3DRS_COLORWRITEENABLE1: u32 = 190;
    pub const D3DRS_COLORWRITEENABLE2: u32 = 191;
    pub const D3DRS_COLORWRITEENABLE3: u32 = 192;
    pub const D3DRS_BLENDFACTOR: u32 = 193;
    pub const D3DRS_SRGBWRITEENABLE: u32 = 194;
    pub const D3DRS_SCISSORTESTENABLE: u32 = 174;

    pub const D3DRS_STENCILENABLE: u32 = 52;
    pub const D3DRS_STENCILFAIL: u32 = 53;
    pub const D3DRS_STENCILZFAIL: u32 = 54;
    pub const D3DRS_STENCILPASS: u32 = 55;
    pub const D3DRS_STENCILFUNC: u32 = 56;
    pub const D3DRS_STENCILREF: u32 = 57;
    pub const D3DRS_STENCILMASK: u32 = 58;
    pub const D3DRS_STENCILWRITEMASK: u32 = 59;

    pub const D3DRS_TWOSIDEDSTENCILMODE: u32 = 185;
    pub const D3DRS_CCW_STENCILFAIL: u32 = 186;
    pub const D3DRS_CCW_STENCILZFAIL: u32 = 187;
    pub const D3DRS_CCW_STENCILPASS: u32 = 188;
    pub const D3DRS_CCW_STENCILFUNC: u32 = 189;

    // D3DSAMPLERSTATETYPE (subset).
    pub const D3DSAMP_ADDRESSU: u32 = 1;
    pub const D3DSAMP_ADDRESSV: u32 = 2;
    pub const D3DSAMP_ADDRESSW: u32 = 3;
    pub const D3DSAMP_BORDERCOLOR: u32 = 4;
    pub const D3DSAMP_MAGFILTER: u32 = 5;
    pub const D3DSAMP_MINFILTER: u32 = 6;
    pub const D3DSAMP_MIPFILTER: u32 = 7;
    pub const D3DSAMP_MAXMIPLEVEL: u32 = 9;
    pub const D3DSAMP_MAXANISOTROPY: u32 = 10;
    pub const D3DSAMP_SRGBTEXTURE: u32 = 11;

    // D3DTEXTUREADDRESS.
    pub const D3DTADDRESS_WRAP: u32 = 1;
    pub const D3DTADDRESS_MIRROR: u32 = 2;
    pub const D3DTADDRESS_CLAMP: u32 = 3;
    pub const D3DTADDRESS_BORDER: u32 = 4;
    pub const D3DTADDRESS_MIRRORONCE: u32 = 5;

    // D3DTEXTUREFILTERTYPE (subset).
    pub const D3DTEXF_NONE: u32 = 0;
    pub const D3DTEXF_POINT: u32 = 1;
    pub const D3DTEXF_LINEAR: u32 = 2;
    pub const D3DTEXF_ANISOTROPIC: u32 = 3;

    // Blend factors (subset).
    pub const D3DBLEND_ZERO: u32 = 1;
    pub const D3DBLEND_ONE: u32 = 2;
    pub const D3DBLEND_SRCALPHA: u32 = 5;
    pub const D3DBLEND_INVSRCALPHA: u32 = 6;
    pub const D3DBLEND_DESTALPHA: u32 = 7;
    pub const D3DBLEND_INVDESTALPHA: u32 = 8;
    pub const D3DBLEND_BOTHSRCALPHA: u32 = 12;
    pub const D3DBLEND_BOTHINVSRCALPHA: u32 = 13;
    pub const D3DBLEND_BLENDFACTOR: u32 = 14;
    pub const D3DBLEND_INVBLENDFACTOR: u32 = 15;

    // Cull modes.
    pub const D3DCULL_NONE: u32 = 1;
    pub const D3DCULL_CW: u32 = 2;
    pub const D3DCULL_CCW: u32 = 3;
}

#[cfg(test)]
mod tests {
    use super::{cmd, d3d9, guest_texture_linear_layout, AerogpuD3d9Executor, AerogpuFormat};

    #[test]
    fn d3d9_cull_mode_mapping_tracks_front_ccw() {
        let map = AerogpuD3d9Executor::map_d3d9_cull_mode;

        assert_eq!(map(0, false), Some(cmd::AerogpuCullMode::None as u32));
        assert_eq!(
            map(d3d9::D3DCULL_NONE, true),
            Some(cmd::AerogpuCullMode::None as u32)
        );

        assert_eq!(
            map(d3d9::D3DCULL_CW, false),
            Some(cmd::AerogpuCullMode::Front as u32)
        );
        assert_eq!(
            map(d3d9::D3DCULL_CW, true),
            Some(cmd::AerogpuCullMode::Back as u32)
        );

        assert_eq!(
            map(d3d9::D3DCULL_CCW, false),
            Some(cmd::AerogpuCullMode::Back as u32)
        );
        assert_eq!(
            map(d3d9::D3DCULL_CCW, true),
            Some(cmd::AerogpuCullMode::Front as u32)
        );

        assert_eq!(map(0xDEAD_BEEF, false), None);
    }

    #[test]
    fn guest_texture_linear_layout_rgba8_mipmapped_matches_expected_size() {
        // 4x4 RGBA8 with 2 mips and an explicit row pitch for mip0:
        // - mip0: row_pitch=16, rows=4 => 64 bytes
        // - mip1: tight pitch (2*4)=8, rows=2 => 16 bytes
        // Total: 80 bytes
        let layout = guest_texture_linear_layout(
            AerogpuFormat::R8G8B8A8Unorm as u32,
            4,
            4,
            2,
            1,
            16,
        )
        .expect("layout");
        assert_eq!(layout.mip_offsets, vec![0, 64]);
        assert_eq!(layout.layer_stride_bytes, 80);
        assert_eq!(layout.total_size_bytes, 80);
    }
}
