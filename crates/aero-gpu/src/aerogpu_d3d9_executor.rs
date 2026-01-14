use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::{Arc, OnceLock};

#[cfg(not(target_arch = "wasm32"))]
use std::sync::mpsc;

#[cfg(target_arch = "wasm32")]
use aero_d3d9::runtime::{ShaderCache as PersistentShaderCache, ShaderTranslationFlags};
use aero_d3d9::shader;
use aero_d3d9::shader_translate::{self, ShaderTranslateBackend};
use aero_d3d9::sm3::decode::TextureType;
use aero_d3d9::vertex::{StandardLocationMap, VertexDeclaration, VertexLocationMap};
use aero_protocol::aerogpu::aerogpu_cmd as cmd;
use aero_protocol::aerogpu::aerogpu_cmd::AEROGPU_COPY_FLAG_WRITEBACK_DST;
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::aerogpu_ring as ring;
use futures_intrusive::channel::shared::oneshot_channel;
#[cfg(target_arch = "wasm32")]
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::debug;
use wgpu::util::DeviceExt;

use crate::aerogpu_executor::{AllocEntry, AllocTable};
use crate::bc_decompress::{
    decompress_bc1_rgba8, decompress_bc2_rgba8, decompress_bc3_rgba8, decompress_bc7_rgba8,
};
use crate::guest_memory::{GuestMemory, GuestMemoryError};
use crate::protocol::{parse_cmd_stream, AeroGpuCmd, AeroGpuCmdStreamParseError};
use crate::shared_surface::{SharedSurfaceError, SharedSurfaceTable};
use crate::stats::GpuStats;
use crate::texture_manager::TextureRegion;
use crate::{
    expand_b5g5r5a1_unorm_to_rgba8, expand_b5g6r5_unorm_to_rgba8, pack_rgba8_to_b5g5r5a1_unorm,
    pack_rgba8_to_b5g6r5_unorm, readback_depth32f, readback_rgba8, readback_stencil8,
};

#[cfg(target_arch = "wasm32")]
fn compute_wgpu_caps_hash(device: &wgpu::Device, downlevel_flags: wgpu::DownlevelFlags) -> String {
    // This hash is included in the persistent shader cache key to avoid reusing translation output
    // across WebGPU capability changes. It does not need to match the JS-side
    // `computeWebGpuCapsHash`; it only needs to be stable for a given device/browser.
    const VERSION: &[u8] = b"aerogpu wgpu caps hash v1";

    let mut hasher = blake3::Hasher::new();
    hasher.update(VERSION);
    hasher.update(&device.features().bits().to_le_bytes());
    hasher.update(&downlevel_flags.bits().to_le_bytes());
    // `wgpu::Limits` is a large struct without `Serialize`; use a debug representation for a
    // stable-ish byte stream. Any change here just forces retranslation, which is safe.
    hasher.update(format!("{:?}", device.limits()).as_bytes());
    hasher.finalize().to_hex().to_string()
}

/// Minimal executor for the D3D9 UMD-produced `aerogpu_cmd.h` command stream.
///
/// This is intentionally a bring-up implementation: it focuses on enough
/// resource/state tracking to render basic D3D9Ex/DWM scenes, starting with a
/// deterministic triangle test.
pub struct AerogpuD3d9Executor {
    device: wgpu::Device,
    queue: wgpu::Queue,
    stats: Arc<GpuStats>,

    /// In-memory DXBC/token-stream -> WGSL cache.
    ///
    /// This uses the higher-level `aero_d3d9::shader_translate` entrypoint which tries the strict
    /// SM3 pipeline first and falls back to the legacy translator on unsupported features.
    shader_cache: shader_translate::ShaderCache,
    /// WASM-only persistent shader translation cache (IndexedDB/OPFS).
    #[cfg(target_arch = "wasm32")]
    persistent_shader_cache: PersistentShaderCache,
    /// Translation flags used for persistent shader cache lookups (wasm32 only).
    ///
    /// This includes a stable per-device capabilities hash so cached artifacts are not reused when
    /// WebGPU limits/features differ.
    #[cfg(target_arch = "wasm32")]
    persistent_shader_cache_flags: ShaderTranslationFlags,

    resources: HashMap<u32, Resource>,
    /// D3D9Ex shared surface import/export bookkeeping (EXPORT/IMPORT_SHARED_SURFACE).
    ///
    /// This table also tracks all resource handle aliasing so imported handles and refcounting
    /// behave consistently across textures and buffers.
    shared_surfaces: SharedSurfaceTable,
    shaders: HashMap<u32, Shader>,
    input_layouts: HashMap<u32, InputLayout>,

    constants_buffer: wgpu::Buffer,
    /// Whether translated vertex shaders should apply the D3D9 half-pixel center convention.
    ///
    /// When enabled, the shader translator emits an extra uniform bind group and nudges the final
    /// clip-space position by (-1/viewport_width, +1/viewport_height) * w to emulate D3D9's
    /// viewport transform bias.
    half_pixel_center: bool,
    half_pixel_bind_group_layout: Option<wgpu::BindGroupLayout>,
    half_pixel_uniform_buffer: Option<wgpu::Buffer>,
    half_pixel_bind_group: Option<wgpu::BindGroup>,
    /// Cached viewport dimensions (width/height) used for the last half-pixel uniform upload.
    ///
    /// This is tracked per-context (see [`ContextState`]) and swapped in `switch_context`.
    half_pixel_last_viewport_dims: Option<(f32, f32)>,

    dummy_texture_view: wgpu::TextureView,
    dummy_cube_texture_view: wgpu::TextureView,
    dummy_1d_texture_view: wgpu::TextureView,
    dummy_3d_texture_view: wgpu::TextureView,
    downlevel_flags: wgpu::DownlevelFlags,
    bc_copy_to_buffer_supported: bool,

    constants_bind_group_layout: wgpu::BindGroupLayout,
    samplers_bind_group_layouts_vs: HashMap<u64, wgpu::BindGroupLayout>,
    samplers_bind_group_layouts_ps: HashMap<u64, wgpu::BindGroupLayout>,
    pipeline_layouts: HashMap<u128, wgpu::PipelineLayout>,
    constants_bind_group: wgpu::BindGroup,
    samplers_bind_group_vs: Option<wgpu::BindGroup>,
    samplers_bind_group_ps: Option<wgpu::BindGroup>,
    samplers_bind_group_key_vs: u64,
    samplers_bind_group_key_ps: u64,
    samplers_bind_groups_dirty: bool,
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
    presented_scanouts: HashMap<u32, u32>,

    triangle_fan_index_buffers: HashMap<u32, TriangleFanIndexBuffer>,

    contexts: HashMap<u32, ContextState>,
    current_context_id: u32,

    state: State,
    encoder: Option<wgpu::CommandEncoder>,
}

impl Drop for AerogpuD3d9Executor {
    fn drop(&mut self) {
        // Tests frequently create and destroy headless executors. Some wgpu backends/drivers are
        // sensitive to dropping a device while work is still in-flight, which can manifest as
        // intermittent SIGSEGVs in CI. Make teardown best-effort deterministic by flushing any
        // pending encoder and waiting for the device to go idle.
        if std::thread::panicking() {
            return;
        }

        if let Some(encoder) = self.encoder.take() {
            self.queue.submit(Some(encoder.finish()));
        }

        self.device.poll(wgpu::Maintain::Wait);
    }
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
    constants_bind_group: wgpu::BindGroup,
    samplers_bind_group_vs: Option<wgpu::BindGroup>,
    samplers_bind_group_ps: Option<wgpu::BindGroup>,
    samplers_bind_group_key_vs: u64,
    samplers_bind_group_key_ps: u64,
    samplers_bind_groups_dirty: bool,
    half_pixel_uniform_buffer: Option<wgpu::Buffer>,
    half_pixel_bind_group: Option<wgpu::BindGroup>,
    half_pixel_last_viewport_dims: Option<(f32, f32)>,
    samplers_vs: [Arc<wgpu::Sampler>; MAX_SAMPLERS],
    sampler_state_vs: [D3d9SamplerState; MAX_SAMPLERS],
    samplers_ps: [Arc<wgpu::Sampler>; MAX_SAMPLERS],
    sampler_state_ps: [D3d9SamplerState; MAX_SAMPLERS],
    state: State,
}

impl ContextState {
    fn new(
        device: &wgpu::Device,
        constants_bind_group_layout: &wgpu::BindGroupLayout,
        default_sampler: Arc<wgpu::Sampler>,
        half_pixel_bind_group_layout: Option<&wgpu::BindGroupLayout>,
    ) -> Self {
        let constants_buffer = create_constants_buffer(device);
        let constants_bind_group =
            create_constants_bind_group(device, constants_bind_group_layout, &constants_buffer);
        let (half_pixel_uniform_buffer, half_pixel_bind_group) =
            if let Some(layout) = half_pixel_bind_group_layout {
                let buffer = create_half_pixel_uniform_buffer(device);
                let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("aerogpu-d3d9.half_pixel.bind_group"),
                    layout,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: buffer.as_entire_binding(),
                    }],
                });
                (Some(buffer), Some(bind_group))
            } else {
                (None, None)
            };
        Self {
            constants_buffer,
            constants_bind_group,
            samplers_bind_group_vs: None,
            samplers_bind_group_ps: None,
            samplers_bind_group_key_vs: 0,
            samplers_bind_group_key_ps: 0,
            samplers_bind_groups_dirty: true,
            half_pixel_uniform_buffer,
            half_pixel_bind_group,
            half_pixel_last_viewport_dims: None,
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
#[allow(clippy::large_enum_variant)]
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
        view_cube: Option<wgpu::TextureView>,
        view_cube_srgb: Option<wgpu::TextureView>,
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
    /// Semantic→location mapping produced by `aero-d3d9` shader translation.
    ///
    /// When empty (e.g. legacy cached shader artifacts), callers should fall back to
    /// `StandardLocationMap` for the common semantics.
    semantic_locations: Vec<shader::SemanticLocation>,
    used_samplers_mask: u16,
    /// Per-stage sampler texture type requirements packed into a 2-bit-per-slot key.
    ///
    /// Encoding per sampler slot `s` (bits `2*s..2*s+1`):
    /// - 0: 2D (`texture_2d`)
    /// - 1: Cube (`texture_cube`)
    /// - 2: 3D (`texture_3d`)
    /// - 3: 1D (`texture_1d`)
    sampler_dim_key: u32,
}

/// Persisted shader reflection metadata stored alongside cached WGSL.
///
/// This is intentionally minimal: it includes only the fields the D3D9 executor needs to bind
/// vertex inputs and select the correct entry point without re-parsing DXBC on cache hit.
#[cfg(target_arch = "wasm32")]
const PERSISTENT_SHADER_REFLECTION_SCHEMA_VERSION: u32 = 2;

#[cfg(target_arch = "wasm32")]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistentShaderReflection {
    /// Version of this Rust-side reflection JSON schema.
    ///
    /// This is validated on cache hit to ensure older persisted blobs (from previous executor
    /// versions) are treated as stale/corrupt and trigger an invalidate+retranslate cycle.
    #[serde(default)]
    schema_version: u32,
    stage: PersistentShaderStage,
    entry_point: String,
    uses_semantic_locations: bool,
    used_samplers_mask: u16,
    /// Packed 2-bit-per-slot sampler dimension key (see `Shader::sampler_dim_key`).
    #[serde(default)]
    sampler_dim_key: u32,
    #[serde(default)]
    semantic_locations: Vec<shader::SemanticLocation>,
}

#[cfg(target_arch = "wasm32")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum PersistentShaderStage {
    Vertex,
    Pixel,
}

#[cfg(target_arch = "wasm32")]
impl PersistentShaderStage {
    fn from_stage(stage: shader::ShaderStage) -> Self {
        match stage {
            shader::ShaderStage::Vertex => Self::Vertex,
            shader::ShaderStage::Pixel => Self::Pixel,
        }
    }

    fn to_stage(self) -> shader::ShaderStage {
        match self {
            Self::Vertex => shader::ShaderStage::Vertex,
            Self::Pixel => shader::ShaderStage::Pixel,
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn derive_sampler_masks_from_wgsl(wgsl: &str) -> (u16, u32) {
    // The D3D9 translator declares sampler bindings using stable names:
    //   var tex{s}: texture_{2d,cube,3d,1d}<f32>
    //   var samp{s}: sampler
    //
    // Persisted reflection includes sampler metadata used by the executor to:
    // - flush textures before draw
    // - select bind group layouts (view_dimension)
    //
    // When cached reflection is stale/corrupt, the masks can become inconsistent with the cached
    // WGSL. Derive metadata from WGSL declarations for validation on persistent cache hits.
    let mut used = 0u16;
    let mut sampler_dim_key = 0u32;

    for line in wgsl.lines() {
        let Some(pos) = line.find("var tex") else {
            continue;
        };
        let rest = &line[pos + "var tex".len()..];
        let mut digits_end = 0usize;
        for (i, ch) in rest.char_indices() {
            if ch.is_ascii_digit() {
                digits_end = i + ch.len_utf8();
            } else {
                break;
            }
        }
        if digits_end == 0 {
            continue;
        }
        let Ok(idx) = rest[..digits_end].parse::<u32>() else {
            continue;
        };
        if idx >= MAX_SAMPLERS as u32 {
            continue;
        }
        let bit = 1u16 << idx;
        used |= bit;
        // Match the encoding described in `Shader::sampler_dim_key`.
        let dim_code = if line.contains("texture_cube<") {
            1u32
        } else if line.contains("texture_3d<") {
            2u32
        } else if line.contains("texture_1d<") {
            3u32
        } else {
            0u32
        };
        let shift = idx * 2;
        sampler_dim_key &= !(0b11 << shift);
        sampler_dim_key |= dim_code << shift;
    }

    (used, sampler_dim_key)
}

#[cfg(target_arch = "wasm32")]
fn parse_wgsl_attr_u32(line: &str, attr: &str) -> Option<u32> {
    let pat = format!("@{attr}(");
    let start = line.find(&pat)? + pat.len();
    let end = line[start..].find(')')? + start;
    line[start..end].trim().parse::<u32>().ok()
}

#[cfg(target_arch = "wasm32")]
fn parse_wgsl_uniform_var_name(line: &str) -> Option<&str> {
    let pos = line.find("var<uniform>")?;
    let rest = line[pos + "var<uniform>".len()..].trim_start();
    let mut end = 0usize;
    for (i, ch) in rest.char_indices() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            end = i + ch.len_utf8();
        } else {
            break;
        }
    }
    (end != 0).then_some(&rest[..end])
}

#[cfg(target_arch = "wasm32")]
fn wgsl_has_expected_entry_point(wgsl: &str, stage: shader::ShaderStage) -> bool {
    let (attr, fn_prefix) = match stage {
        shader::ShaderStage::Vertex => ("@vertex", "fn vs_main("),
        shader::ShaderStage::Pixel => ("@fragment", "fn fs_main("),
    };

    let lines: Vec<&str> = wgsl.lines().collect();
    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if !trimmed.starts_with(attr) {
            continue;
        }
        // Support one-line formatting: `@vertex fn vs_main(...)`.
        //
        // Avoid false positives from comments like `@vertex // fn vs_main(` by requiring the
        // function signature to appear before any `//` comment marker.
        if let Some(fn_pos) = trimmed.find(fn_prefix) {
            let comment_pos = trimmed.find("//").unwrap_or(trimmed.len());
            if fn_pos < comment_pos {
                return true;
            }
        }
        let mut next = idx + 1;
        while next < lines.len() {
            let next_trimmed = lines[next].trim();
            if next_trimmed.is_empty() {
                next += 1;
                continue;
            }
            // Allow additional attribute lines between the stage attribute and entry point.
            // Attributes in WGSL apply to the immediately following item, regardless of newline
            // placement.
            if next_trimmed.starts_with('@') {
                next += 1;
                continue;
            }
            break;
        }
        if next < lines.len() && lines[next].trim_start().starts_with(fn_prefix) {
            return true;
        }
    }
    false
}

#[cfg(target_arch = "wasm32")]
fn validate_wgsl_binding_contract(
    wgsl: &str,
    stage: shader::ShaderStage,
    half_pixel_center: bool,
) -> Result<(), String> {
    let mut has_constants = false;
    let mut has_constants_i = false;
    let mut has_constants_b = false;
    let mut has_half_pixel = false;
    let sampler_group_expected = match stage {
        shader::ShaderStage::Vertex => 1u32,
        shader::ShaderStage::Pixel => 2u32,
    };

    let mut tex_slots = HashSet::<u32>::new();
    let mut samp_slots = HashSet::<u32>::new();

    for line in wgsl.lines() {
        let Some(group) = parse_wgsl_attr_u32(line, "group") else {
            continue;
        };
        let Some(binding) = parse_wgsl_attr_u32(line, "binding") else {
            continue;
        };

        if let Some(name) = parse_wgsl_uniform_var_name(line) {
            match name {
                "constants" => {
                    if group != 0 || binding != 0 {
                        return Err(format!(
                            "constants uniform has unexpected binding (@group({group}) @binding({binding})); expected @group(0) @binding(0)"
                        ));
                    }
                    has_constants = true;
                    continue;
                }
                "constants_i" => {
                    if group != 0 || binding != 1 {
                        return Err(format!(
                            "constants_i uniform has unexpected binding (@group({group}) @binding({binding})); expected @group(0) @binding(1)"
                        ));
                    }
                    has_constants_i = true;
                    continue;
                }
                "constants_b" => {
                    if group != 0 || binding != 2 {
                        return Err(format!(
                            "constants_b uniform has unexpected binding (@group({group}) @binding({binding})); expected @group(0) @binding(2)"
                        ));
                    }
                    has_constants_b = true;
                    continue;
                }
                "half_pixel" => {
                    has_half_pixel = true;
                    if group != 3 || binding != 0 {
                        return Err(format!(
                            "half_pixel uniform has unexpected binding (@group({group}) @binding({binding})); expected @group(3) @binding(0)"
                        ));
                    }
                    continue;
                }
                other => {
                    return Err(format!(
                        "WGSL declares unexpected uniform binding '{other}' (@group({group}) @binding({binding}))"
                    ));
                }
            }
        }

        if let Some(pos) = line.find("var tex") {
            let rest = &line[pos + "var tex".len()..];
            let mut digits_end = 0usize;
            for (i, ch) in rest.char_indices() {
                if ch.is_ascii_digit() {
                    digits_end = i + ch.len_utf8();
                } else {
                    break;
                }
            }
            if digits_end == 0 {
                return Err(format!(
                    "WGSL declares unexpected texture binding name (@group({group}) @binding({binding}))"
                ));
            }
            let idx: u32 = rest[..digits_end]
                .parse()
                .map_err(|_| "failed to parse sampler index for tex binding".to_string())?;
            if idx >= MAX_SAMPLERS as u32 {
                return Err(format!(
                    "WGSL declares out-of-range texture binding tex{idx} (MAX_SAMPLERS={MAX_SAMPLERS})"
                ));
            }
            if group != sampler_group_expected {
                return Err(format!(
                    "tex{idx} has unexpected @group({group}); expected @group({sampler_group_expected})"
                ));
            }
            let expected_binding = idx * 2;
            if binding != expected_binding {
                return Err(format!(
                    "tex{idx} has unexpected @binding({binding}); expected @binding({expected_binding})"
                ));
            }
            let has_known_type = line.contains("texture_2d<f32>")
                || line.contains("texture_cube<f32>")
                || line.contains("texture_3d<f32>")
                || line.contains("texture_1d<f32>");
            if !has_known_type {
                return Err(format!(
                    "tex{idx} has unexpected type; expected texture_{{2d,cube,3d,1d}}<f32>"
                ));
            }
            if !tex_slots.insert(idx) {
                return Err(format!("WGSL declares tex{idx} more than once"));
            }
            continue;
        }

        if let Some(pos) = line.find("var samp") {
            let rest = &line[pos + "var samp".len()..];
            let mut digits_end = 0usize;
            for (i, ch) in rest.char_indices() {
                if ch.is_ascii_digit() {
                    digits_end = i + ch.len_utf8();
                } else {
                    break;
                }
            }
            if digits_end == 0 {
                return Err(format!(
                    "WGSL declares unexpected sampler binding name (@group({group}) @binding({binding}))"
                ));
            }
            let idx: u32 = rest[..digits_end]
                .parse()
                .map_err(|_| "failed to parse sampler index for samp binding".to_string())?;
            if idx >= MAX_SAMPLERS as u32 {
                return Err(format!(
                    "WGSL declares out-of-range sampler binding samp{idx} (MAX_SAMPLERS={MAX_SAMPLERS})"
                ));
            }
            if group != sampler_group_expected {
                return Err(format!(
                    "samp{idx} has unexpected @group({group}); expected @group({sampler_group_expected})"
                ));
            }
            let expected_binding = idx * 2 + 1;
            if binding != expected_binding {
                return Err(format!(
                    "samp{idx} has unexpected @binding({binding}); expected @binding({expected_binding})"
                ));
            }
            if !line.contains(": sampler;") {
                return Err(format!("samp{idx} has unexpected type; expected sampler"));
            }
            if !samp_slots.insert(idx) {
                return Err(format!("WGSL declares samp{idx} more than once"));
            }
            continue;
        }

        return Err(format!(
            "WGSL declares unexpected resource binding (@group({group}) @binding({binding}))"
        ));
    }

    if !has_constants {
        return Err("WGSL is missing the expected constants uniform binding".into());
    }
    if !has_constants_i {
        return Err("WGSL is missing the expected constants_i uniform binding".into());
    }
    if !has_constants_b {
        return Err("WGSL is missing the expected constants_b uniform binding".into());
    }
    if stage == shader::ShaderStage::Vertex && half_pixel_center && !has_half_pixel {
        return Err(
            "half_pixel_center is enabled but WGSL is missing the half_pixel uniform binding"
                .into(),
        );
    }
    if stage == shader::ShaderStage::Vertex && !half_pixel_center && has_half_pixel {
        return Err("WGSL declares half_pixel uniform but half_pixel_center is disabled".into());
    }
    if stage != shader::ShaderStage::Vertex && has_half_pixel {
        return Err("WGSL declares half_pixel uniform in a non-vertex shader".into());
    }
    if tex_slots != samp_slots {
        return Err(format!(
            "WGSL sampler declarations are inconsistent: textures={:?} samplers={:?}",
            tex_slots, samp_slots
        ));
    }

    Ok(())
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
const CONSTANTS_REGION_SIZE_BYTES: u64 = 512 * 16;
const CONSTANTS_FLOATS_OFFSET_BYTES: u64 = 0;
const CONSTANTS_INTS_OFFSET_BYTES: u64 = 512 * 16;
const CONSTANTS_BOOLS_OFFSET_BYTES: u64 = 512 * 16 * 2;
const CONSTANTS_BUFFER_SIZE_BYTES: usize = 512 * 16 * 3;
const HALF_PIXEL_UNIFORM_SIZE_BYTES: usize = 16;
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

fn create_constants_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("aerogpu-d3d9.constants_bind_group_layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(CONSTANTS_REGION_SIZE_BYTES),
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(CONSTANTS_REGION_SIZE_BYTES),
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(CONSTANTS_REGION_SIZE_BYTES),
                },
                count: None,
            },
        ],
    })
}

fn create_samplers_bind_group_layout(
    device: &wgpu::Device,
    visibility: wgpu::ShaderStages,
    used_samplers_mask: u16,
    sampler_dim_key: u32,
) -> wgpu::BindGroupLayout {
    // Matches `aero-d3d9` token stream shader translation binding contract:
    // - group(1): VS samplers
    // - group(2): PS samplers
    //
    // And bindings derived from sampler register index:
    //   texture binding = 2*s
    //   sampler binding = 2*s + 1
    let mut entries = Vec::with_capacity(used_samplers_mask.count_ones() as usize * 2);
    for slot in 0..MAX_SAMPLERS {
        if (used_samplers_mask & (1u16 << slot)) == 0 {
            continue;
        }
        let tex_binding = slot as u32 * 2;
        let samp_binding = tex_binding + 1;
        let dim_code = (sampler_dim_key >> (slot as u32 * 2)) & 0b11;
        let view_dimension = match dim_code {
            1 => wgpu::TextureViewDimension::Cube,
            2 => wgpu::TextureViewDimension::D3,
            3 => wgpu::TextureViewDimension::D1,
            _ => wgpu::TextureViewDimension::D2,
        };
        entries.push(wgpu::BindGroupLayoutEntry {
            binding: tex_binding,
            visibility,
            ty: wgpu::BindingType::Texture {
                multisampled: false,
                view_dimension,
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
            },
            count: None,
        });
        entries.push(wgpu::BindGroupLayoutEntry {
            binding: samp_binding,
            visibility,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        });
    }

    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("aerogpu-d3d9.samplers_bind_group_layout"),
        entries: &entries,
    })
}

fn create_half_pixel_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("aerogpu-d3d9.half_pixel.bind_group_layout"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: wgpu::BufferSize::new(HALF_PIXEL_UNIFORM_SIZE_BYTES as u64),
            },
            count: None,
        }],
    })
}

fn samplers_bind_group_layout_key(used_samplers_mask: u16, sampler_dim_key: u32) -> u64 {
    (u64::from(used_samplers_mask) << 32) | u64::from(sampler_dim_key)
}

fn sampler_layout_key(vs_sampler_layout_key: u64, ps_sampler_layout_key: u64) -> u128 {
    u128::from(vs_sampler_layout_key) | (u128::from(ps_sampler_layout_key) << 64)
}

fn create_constants_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    constants_buffer: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("aerogpu-d3d9.constants_bind_group"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: constants_buffer,
                    offset: CONSTANTS_FLOATS_OFFSET_BYTES,
                    size: wgpu::BufferSize::new(CONSTANTS_REGION_SIZE_BYTES),
                }),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: constants_buffer,
                    offset: CONSTANTS_INTS_OFFSET_BYTES,
                    size: wgpu::BufferSize::new(CONSTANTS_REGION_SIZE_BYTES),
                }),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: constants_buffer,
                    offset: CONSTANTS_BOOLS_OFFSET_BYTES,
                    size: wgpu::BufferSize::new(CONSTANTS_REGION_SIZE_BYTES),
                }),
            },
        ],
    })
}

fn create_half_pixel_uniform_buffer(device: &wgpu::Device) -> wgpu::Buffer {
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("aerogpu-d3d9.half_pixel.uniform"),
        contents: &[0u8; HALF_PIXEL_UNIFORM_SIZE_BYTES],
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
    // The D3D9 shader translator's `fs_main` signature has evolved over time (struct names, empty
    // input handling, etc). Keep alpha-test injection robust by pattern-matching the signature
    // instead of hard-coding specific type names.
    //
    // Expected forms:
    // - `@fragment\nfn fs_main(input: FsIn) -> FsOut {\n`
    // - `@fragment\nfn fs_main() -> FsOut {\n`
    // - fixed-function: `@fragment\nfn fs_main(input: FragmentIn) -> vec4<f32> {\n`
    //
    // We'll rewrite the original entry point into `fs_main_inner` (removing the `@fragment`
    // attribute), then append a new `@fragment fn fs_main(...)` wrapper that performs alpha test.
    const MARKER: &str = "@fragment\nfn fs_main";

    let Some(sig_start) = base.find(MARKER) else {
        return Err(AerogpuD3d9Error::ShaderTranslation(
            "alpha-test WGSL injection failed: could not locate fs_main".into(),
        ));
    };
    let sig_rest = &base[sig_start + MARKER.len()..];
    let Some(open_brace_rel) = sig_rest.find("{\n") else {
        return Err(AerogpuD3d9Error::ShaderTranslation(
            "alpha-test WGSL injection failed: unrecognized fs_main signature".into(),
        ));
    };

    let sig_len = MARKER.len() + open_brace_rel + 2; // include "{\n"
    let old_sig = &base[sig_start..sig_start + sig_len];
    let suffix = &old_sig[MARKER.len()..]; // "(...) -> ... {\n"

    let new_sig = format!("fn fs_main_inner{suffix}");
    let wrapper_sig = format!("fn fs_main{suffix}");

    let call_expr = {
        let params = suffix
            .strip_prefix('(')
            .and_then(|s| s.split_once(')'))
            .map(|(params, _rest)| params)
            .ok_or_else(|| {
                AerogpuD3d9Error::ShaderTranslation(
                    "alpha-test WGSL injection failed: unrecognized fs_main parameters".into(),
                )
            })?;
        let params = params.trim();
        if params.is_empty() {
            "fs_main_inner()".to_owned()
        } else {
            let mut names = Vec::new();
            for part in params.split(',') {
                let part = part.trim();
                let Some((before_colon, _ty)) = part.split_once(':') else {
                    continue;
                };
                let name = before_colon.split_whitespace().last().unwrap_or("").trim();
                if !name.is_empty() {
                    names.push(name.to_owned());
                }
            }
            format!("fs_main_inner({})", names.join(", "))
        }
    };

    let alpha_expr = {
        let Some((_before_arrow, after_arrow)) = suffix.split_once("->") else {
            return Err(AerogpuD3d9Error::ShaderTranslation(
                "alpha-test WGSL injection failed: unrecognized fs_main return type".into(),
            ));
        };
        let return_ty = after_arrow
            .split('{')
            .next()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                AerogpuD3d9Error::ShaderTranslation(
                    "alpha-test WGSL injection failed: unrecognized fs_main return type".into(),
                )
            })?;
        if return_ty.contains("vec4") {
            // Fixed-function shaders return a bare vec4.
            "out.a"
        } else {
            // SM2/SM3 shaders return a struct with `oC0`.
            "out.oC0.a"
        }
    };

    let mut out = base.replacen(old_sig, &new_sig, 1);
    out.push_str("\n@fragment\n");
    out.push_str(&wrapper_sig);
    out.push_str(&format!("  let out = {call_expr};\n"));
    out.push_str(&format!("  let a: f32 = clamp({alpha_expr}, 0.0, 1.0);\n"));
    out.push_str(&format!(
        "  let alpha_ref: f32 = f32({}u) / 255.0;\n",
        alpha_test_ref
    ));

    let passes = match alpha_test_func {
        1 => "false",            // D3DCMP_NEVER
        2 => "(a < alpha_ref)",  // D3DCMP_LESS
        3 => "(a == alpha_ref)", // D3DCMP_EQUAL
        4 => "(a <= alpha_ref)", // D3DCMP_LESSEQUAL
        5 => "(a > alpha_ref)",  // D3DCMP_GREATER
        6 => "(a != alpha_ref)", // D3DCMP_NOTEQUAL
        7 => "(a >= alpha_ref)", // D3DCMP_GREATEREQUAL
        8 => "true",             // D3DCMP_ALWAYS
        _ => "true",
    };
    out.push_str(&format!("  if !({}) {{\n    discard;\n  }}\n", passes));
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
    /// Encodes the bind group layout requirements for vertex+pixel sampler slots.
    ///
    /// Pipeline layout key = (vs_layout_key as u128) | ((ps_layout_key as u128) << 64)
    samplers_layout_key: u128,
    alpha_test_enable: bool,
    alpha_test_func: u32,
    alpha_test_ref: u8,
    vertex_buffers: Vec<crate::pipeline_key::VertexBufferLayoutKey>,
    color_formats: Vec<Option<wgpu::TextureFormat>>,
    /// Per color attachment: true if the bound render target has no alpha channel (opaque alpha).
    ///
    /// Examples:
    /// - `X8R8G8B8`/`X8B8G8R8` style formats (protocol `*X8*`)
    /// - `R5G6B5` (protocol `B5G6R5Unorm`)
    ///
    /// These formats are mapped to wgpu formats that *do* have an alpha channel (RGBA/BGRA), but
    /// D3D9 semantics require that alpha writes are ignored and alpha reads behave as opaque.
    ///
    /// This needs to be part of the cache key to avoid reusing a pipeline with a mismatched color
    /// write mask between alpha-less and alpha-bearing render targets of the same wgpu format.
    opaque_alpha_mask: Vec<bool>,
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

/// Configuration for [`AerogpuD3d9Executor`].
#[derive(Debug, Clone, Copy, Default)]
pub struct AerogpuD3d9ExecutorConfig {
    /// Enable the D3D9 half-pixel center convention in translated vertex shaders.
    ///
    /// See `aero-d3d9`'s `WgslOptions::half_pixel_center` for details.
    pub half_pixel_center: bool,
}

impl AerogpuD3d9Executor {
    /// Create a headless executor suitable for tests.
    pub async fn new_headless() -> Result<Self, AerogpuD3d9Error> {
        Self::new_headless_with_config(AerogpuD3d9ExecutorConfig::default()).await
    }

    /// Like [`AerogpuD3d9Executor::new_headless`], but allows configuring translation flags.
    pub async fn new_headless_with_config(
        config: AerogpuD3d9ExecutorConfig,
    ) -> Result<Self, AerogpuD3d9Error> {
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

        // Avoid wgpu's GL backend on Linux: wgpu-hal's GLES pipeline reflection can panic for some
        // shader pipelines (observed in CI sandboxes), which turns these tests into hard failures.
        //
        // Prefer "native" backends across platforms; on Linux this means Vulkan.
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..Default::default()
        });

        // Prefer a fallback adapter first so headless CI can run without a hardware GPU.
        let adapter = match instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: true,
            })
            .await
        {
            Some(adapter) => Some(adapter),
            None => {
                instance
                    .request_adapter(&wgpu::RequestAdapterOptions {
                        power_preference: wgpu::PowerPreference::HighPerformance,
                        compatible_surface: None,
                        force_fallback_adapter: false,
                    })
                    .await
            }
        }
        .ok_or(AerogpuD3d9Error::AdapterNotFound)?;

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

        Ok(Self::new_with_config(
            device,
            queue,
            downlevel_flags,
            Arc::new(GpuStats::new()),
            config,
        ))
    }

    pub fn new(
        device: wgpu::Device,
        queue: wgpu::Queue,
        downlevel_flags: wgpu::DownlevelFlags,
        stats: Arc<GpuStats>,
    ) -> Self {
        Self::new_with_config(
            device,
            queue,
            downlevel_flags,
            stats,
            AerogpuD3d9ExecutorConfig::default(),
        )
    }

    pub fn new_with_config(
        device: wgpu::Device,
        queue: wgpu::Queue,
        downlevel_flags: wgpu::DownlevelFlags,
        stats: Arc<GpuStats>,
        config: AerogpuD3d9ExecutorConfig,
    ) -> Self {
        #[cfg(target_arch = "wasm32")]
        let persistent_shader_cache_flags = aero_d3d9::runtime::ShaderTranslationFlags::new(
            config.half_pixel_center,
            Some(compute_wgpu_caps_hash(&device, downlevel_flags)),
        );

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

        let dummy_cube_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("aerogpu-d3d9.dummy_cube_texture"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 6,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        for face in 0..6u32 {
            queue.write_texture(
                wgpu::ImageCopyTexture {
                    texture: &dummy_cube_texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: face,
                    },
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
        }
        let dummy_cube_texture_view =
            dummy_cube_texture.create_view(&wgpu::TextureViewDescriptor {
                dimension: Some(wgpu::TextureViewDimension::Cube),
                base_array_layer: 0,
                array_layer_count: Some(6),
                ..Default::default()
            });

        let dummy_1d_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("aerogpu-d3d9.dummy_1d_texture"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D1,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &dummy_1d_texture,
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
        let dummy_1d_texture_view = dummy_1d_texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D1),
            ..Default::default()
        });

        let dummy_3d_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("aerogpu-d3d9.dummy_3d_texture"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D3,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &dummy_3d_texture,
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
        let dummy_3d_texture_view = dummy_3d_texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D3),
            ..Default::default()
        });

        let bc_copy_to_buffer_supported = bc_copy_to_buffer_supported(&device, &queue);

        let constants_bind_group_layout = create_constants_bind_group_layout(&device);
        let half_pixel_bind_group_layout = config
            .half_pixel_center
            .then(|| create_half_pixel_bind_group_layout(&device));

        let mut samplers_bind_group_layouts_vs = HashMap::new();
        let mut samplers_bind_group_layouts_ps = HashMap::new();
        let vs_bgl0 =
            create_samplers_bind_group_layout(&device, wgpu::ShaderStages::VERTEX, 0u16, 0u32);
        let ps_bgl0 =
            create_samplers_bind_group_layout(&device, wgpu::ShaderStages::FRAGMENT, 0u16, 0u32);

        let mut pipeline_layouts = HashMap::new();
        let pipeline_layout0 =
            if let Some(half_pixel_layout) = half_pixel_bind_group_layout.as_ref() {
                device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("aerogpu-d3d9.pipeline_layout"),
                    bind_group_layouts: &[
                        &constants_bind_group_layout,
                        &vs_bgl0,
                        &ps_bgl0,
                        half_pixel_layout,
                    ],
                    push_constant_ranges: &[],
                })
            } else {
                device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("aerogpu-d3d9.pipeline_layout"),
                    bind_group_layouts: &[&constants_bind_group_layout, &vs_bgl0, &ps_bgl0],
                    push_constant_ranges: &[],
                })
            };

        samplers_bind_group_layouts_vs.insert(0, vs_bgl0);
        samplers_bind_group_layouts_ps.insert(0, ps_bgl0);
        pipeline_layouts.insert(0, pipeline_layout0);
        let constants_bind_group =
            create_constants_bind_group(&device, &constants_bind_group_layout, &constants_buffer);

        let (half_pixel_uniform_buffer, half_pixel_bind_group) =
            if let Some(half_pixel_layout) = half_pixel_bind_group_layout.as_ref() {
                let buffer = create_half_pixel_uniform_buffer(&device);
                let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("aerogpu-d3d9.half_pixel.bind_group"),
                    layout: half_pixel_layout,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: buffer.as_entire_binding(),
                    }],
                });
                (Some(buffer), Some(bind_group))
            } else {
                (None, None)
            };

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
            stats,
            shader_cache: shader_translate::ShaderCache::new(shader::WgslOptions {
                half_pixel_center: config.half_pixel_center,
            }),
            #[cfg(target_arch = "wasm32")]
            persistent_shader_cache: PersistentShaderCache::new(),
            #[cfg(target_arch = "wasm32")]
            persistent_shader_cache_flags,
            resources: HashMap::new(),
            shared_surfaces: SharedSurfaceTable::default(),
            shaders: HashMap::new(),
            input_layouts: HashMap::new(),
            constants_buffer,
            half_pixel_center: config.half_pixel_center,
            half_pixel_bind_group_layout,
            half_pixel_uniform_buffer,
            half_pixel_bind_group,
            half_pixel_last_viewport_dims: None,
            dummy_texture_view,
            dummy_cube_texture_view,
            dummy_1d_texture_view,
            dummy_3d_texture_view,
            downlevel_flags,
            bc_copy_to_buffer_supported,
            constants_bind_group_layout,
            samplers_bind_group_layouts_vs,
            samplers_bind_group_layouts_ps,
            pipeline_layouts,
            constants_bind_group,
            samplers_bind_group_vs: None,
            samplers_bind_group_ps: None,
            samplers_bind_group_key_vs: 0,
            samplers_bind_group_key_ps: 0,
            samplers_bind_groups_dirty: true,
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
            presented_scanouts: HashMap::new(),
            triangle_fan_index_buffers: HashMap::new(),
            contexts: HashMap::new(),
            current_context_id: 0,
            state: create_default_state(),
            encoder: None,
        }
    }

    /// Configure the optional device/backend fingerprint used to partition persistent shader cache
    /// keys on wasm.
    ///
    /// On non-wasm targets this is a no-op (persistent caching is not supported).
    pub fn set_shader_cache_caps_hash(&mut self, caps_hash: Option<String>) -> Option<String> {
        #[cfg(target_arch = "wasm32")]
        {
            if let Some(stable_fingerprint) = caps_hash {
                // Combine the stable backend+adapter fingerprint (provided by the wasm frontend)
                // with a hash of relevant wgpu capabilities/limits. This avoids reusing cached
                // WGSL across incompatible backend/compiler/capability sets.
                let wgpu_caps_hash = compute_wgpu_caps_hash(&self.device, self.downlevel_flags);
                let wgpu_caps_short = wgpu_caps_hash.get(..16).unwrap_or(&wgpu_caps_hash);
                self.persistent_shader_cache_flags.caps_hash =
                    Some(format!("{stable_fingerprint}-{wgpu_caps_short}"));
            }
            self.persistent_shader_cache_flags.caps_hash.clone()
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = caps_hash;
            None
        }
    }

    pub fn reset(&mut self) {
        self.shader_cache = shader_translate::ShaderCache::new(shader::WgslOptions {
            half_pixel_center: self.half_pixel_center,
        });
        #[cfg(target_arch = "wasm32")]
        {
            self.persistent_shader_cache = PersistentShaderCache::new();
            // Reset clears the per-session persistence disable flag. If persistence is still
            // unavailable (missing APIs, quota issues, etc) it will be re-disabled on the next
            // lookup.
            self.stats.set_d3d9_shader_cache_disabled(
                self.persistent_shader_cache.is_persistent_disabled(),
            );
        }
        self.resources.clear();
        self.shared_surfaces.clear();
        self.shaders.clear();
        self.input_layouts.clear();
        self.presented_scanouts.clear();
        self.pipelines.clear();
        self.alpha_test_pixel_shaders.clear();
        self.samplers_bind_group_layouts_vs.clear();
        self.samplers_bind_group_layouts_ps.clear();
        self.pipeline_layouts.clear();
        self.clear_pipelines.clear();
        self.clear_depth_pipelines.clear();
        self.triangle_fan_index_buffers.clear();
        self.contexts.clear();
        self.current_context_id = 0;
        self.samplers_bind_group_vs = None;
        self.samplers_bind_group_ps = None;
        self.samplers_bind_groups_dirty = true;
        self.sampler_state_vs = std::array::from_fn(|_| D3d9SamplerState::default());
        self.sampler_state_ps = std::array::from_fn(|_| D3d9SamplerState::default());
        self.sampler_cache.clear();
        let default_sampler = create_default_sampler(&self.device, self.downlevel_flags);
        self.sampler_cache
            .insert(D3d9SamplerState::default(), default_sampler.clone());
        self.samplers_vs = std::array::from_fn(|_| default_sampler.clone());
        self.samplers_ps = std::array::from_fn(|_| default_sampler.clone());
        self.state = create_default_state();
        self.half_pixel_last_viewport_dims = None;
        self.encoder = None;

        // Avoid leaking constants across resets; the next draw will rewrite what it needs.
        self.queue.write_buffer(
            &self.constants_buffer,
            0,
            &[0u8; CONSTANTS_BUFFER_SIZE_BYTES],
        );
        if let Some(buf) = self.half_pixel_uniform_buffer.as_ref() {
            self.queue
                .write_buffer(buf, 0, &[0u8; HALF_PIXEL_UNIFORM_SIZE_BYTES]);
        }
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
        crate::wgpu_async::receive_oneshot_with_wgpu_poll(&self.device, receiver)
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

    pub async fn execute_cmd_stream_for_context_async(
        &mut self,
        context_id: u32,
        bytes: &[u8],
    ) -> Result<(), AerogpuD3d9Error> {
        let stream = parse_cmd_stream(bytes)?;

        // Without guest memory, WRITEBACK_DST cannot be committed. Reject early to avoid partially
        // executing the stream.
        let writeback_at = stream.cmds.iter().position(|cmd| match cmd {
            AeroGpuCmd::CopyBuffer { flags, .. } | AeroGpuCmd::CopyTexture2d { flags, .. } => {
                (flags & AEROGPU_COPY_FLAG_WRITEBACK_DST) != 0
            }
            _ => false,
        });
        if let Some(at) = writeback_at {
            return Err(AerogpuD3d9Error::Validation(format!(
                "WRITEBACK_DST requires guest memory (call execute_cmd_stream_with_guest_memory_for_context_async); first WRITEBACK_DST at packet {at}"
            )));
        }

        self.switch_context(context_id);
        let mut pending_writebacks = Vec::new();
        let mut ctx = SubmissionCtx {
            guest_memory: None,
            alloc_table: None,
        };
        for cmd in stream.cmds {
            if let Err(err) = self
                .execute_cmd_async(cmd, &mut ctx, &mut pending_writebacks)
                .await
            {
                self.encoder = None;
                self.queue.submit([]);
                return Err(err);
            }
        }
        self.flush()?;
        debug_assert!(
            pending_writebacks.is_empty(),
            "pending writebacks should be impossible without guest memory"
        );
        Ok(())
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
        self.execute_cmd_stream_with_ctx_async(
            context_id,
            bytes,
            SubmissionCtx {
                guest_memory: Some(guest_memory),
                alloc_table,
            },
        )
        .await
    }

    async fn execute_cmd_stream_with_ctx_async(
        &mut self,
        context_id: u32,
        bytes: &[u8],
        mut ctx: SubmissionCtx<'_>,
    ) -> Result<(), AerogpuD3d9Error> {
        let stream = parse_cmd_stream(bytes)?;
        self.switch_context(context_id);
        let mut pending_writebacks = Vec::new();

        for cmd in stream.cmds {
            if let Err(err) = self
                .execute_cmd_async(cmd, &mut ctx, &mut pending_writebacks)
                .await
            {
                self.encoder = None;
                self.queue.submit([]);
                return Err(err);
            }
        }

        self.flush()?;
        if !pending_writebacks.is_empty() {
            let Some(guest_memory) = ctx.guest_memory.take() else {
                return Err(AerogpuD3d9Error::Validation(
                    "WRITEBACK_DST requires guest memory".into(),
                ));
            };
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
            ContextState::new(
                &self.device,
                &self.constants_bind_group_layout,
                default_sampler,
                self.half_pixel_bind_group_layout.as_ref(),
            )
        };

        std::mem::swap(&mut self.constants_buffer, &mut next.constants_buffer);
        std::mem::swap(
            &mut self.constants_bind_group,
            &mut next.constants_bind_group,
        );
        std::mem::swap(
            &mut self.samplers_bind_group_vs,
            &mut next.samplers_bind_group_vs,
        );
        std::mem::swap(
            &mut self.samplers_bind_group_ps,
            &mut next.samplers_bind_group_ps,
        );
        std::mem::swap(
            &mut self.samplers_bind_group_key_vs,
            &mut next.samplers_bind_group_key_vs,
        );
        std::mem::swap(
            &mut self.samplers_bind_group_key_ps,
            &mut next.samplers_bind_group_key_ps,
        );
        std::mem::swap(
            &mut self.samplers_bind_groups_dirty,
            &mut next.samplers_bind_groups_dirty,
        );
        std::mem::swap(
            &mut self.half_pixel_uniform_buffer,
            &mut next.half_pixel_uniform_buffer,
        );
        std::mem::swap(
            &mut self.half_pixel_bind_group,
            &mut next.half_pixel_bind_group,
        );
        std::mem::swap(
            &mut self.half_pixel_last_viewport_dims,
            &mut next.half_pixel_last_viewport_dims,
        );
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

            // Shader creation uses the persistent shader cache on wasm, which requires async IO.
            // Reject synchronous execution to avoid silently bypassing persistence.
            let shader_at = stream
                .cmds
                .iter()
                .position(|cmd| matches!(cmd, AeroGpuCmd::CreateShaderDxbc { .. }));
            if let Some(at) = shader_at {
                return Err(AerogpuD3d9Error::Validation(format!(
                    "CREATE_SHADER_DXBC requires async execution on wasm (call execute_cmd_stream_for_context_async or execute_cmd_stream_with_guest_memory_for_context_async); first CREATE_SHADER_DXBC at packet {at}"
                )));
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
            Err(AerogpuD3d9Error::Validation(
                "WRITEBACK_DST requires async execution on wasm (call execute_cmd_stream_with_guest_memory_for_context_async)"
                    .into(),
            ))
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
        let resolved = match self.shared_surfaces.resolve_cmd_handle(handle) {
            Ok(resolved) => resolved,
            // Preserve existing executor behavior: treat references to destroyed/reserved handles
            // as unknown resources.
            Err(_) => return Err(AerogpuD3d9Error::UnknownResource(handle)),
        };
        if self.resources.contains_key(&resolved) {
            Ok(resolved)
        } else {
            Err(AerogpuD3d9Error::UnknownResource(handle))
        }
    }

    fn handle_in_use(&self, handle: u32) -> bool {
        self.shared_surfaces.contains_handle(handle)
            || self.resources.contains_key(&handle)
            || self.shaders.contains_key(&handle)
            || self.input_layouts.contains_key(&handle)
    }

    fn invalidate_bind_groups(&mut self) {
        self.samplers_bind_group_vs = None;
        self.samplers_bind_group_ps = None;
        self.samplers_bind_group_key_vs = 0;
        self.samplers_bind_group_key_ps = 0;
        self.samplers_bind_groups_dirty = true;
        for ctx in self.contexts.values_mut() {
            ctx.samplers_bind_group_vs = None;
            ctx.samplers_bind_group_ps = None;
            ctx.samplers_bind_group_key_vs = 0;
            ctx.samplers_bind_group_key_ps = 0;
            ctx.samplers_bind_groups_dirty = true;
        }
    }

    fn destroy_resource_handle(&mut self, handle: u32) {
        if handle == 0 {
            return;
        }
        let Some((underlying, last_ref)) = self.shared_surfaces.destroy_handle(handle) else {
            return;
        };

        // Texture bindings (and therefore bind groups) may reference the destroyed handle. Drop
        // cached bind groups so subsequent draws re-resolve handles against the updated alias
        // table.
        self.invalidate_bind_groups();

        if !last_ref {
            return;
        }

        self.resources.remove(&underlying);
        self.presented_scanouts.retain(|_, v| *v != underlying);
    }

    fn create_shader_dxbc_in_memory(
        &mut self,
        shader_handle: u32,
        expected_stage: shader::ShaderStage,
        dxbc_bytes: &[u8],
    ) -> Result<(), AerogpuD3d9Error> {
        let cached = self
            .shader_cache
            .get_or_translate(dxbc_bytes)
            .map_err(|e| AerogpuD3d9Error::ShaderTranslation(e.to_string()))?;
        let key = xxhash_rust::xxh3::xxh3_64(dxbc_bytes);
        match cached.source {
            shader_translate::ShaderCacheLookupSource::Memory => {
                self.stats.inc_d3d9_shader_cache_memory_hits();
            }
            shader_translate::ShaderCacheLookupSource::Translated => {
                self.stats.inc_d3d9_shader_translate_calls();
                if cached.backend == ShaderTranslateBackend::LegacyFallback {
                    self.stats.inc_d3d9_shader_sm3_fallbacks();
                    debug!(
                        shader_handle,
                        reason = cached.fallback_reason.as_deref().unwrap_or("<unknown>"),
                        "SM3 shader translation failed; falling back to legacy D3D9 translator"
                    );
                }
            }
        }

        let bytecode_stage = cached.version.stage;
        if expected_stage != bytecode_stage {
            return Err(AerogpuD3d9Error::ShaderStageMismatch {
                shader_handle,
                expected: expected_stage,
                actual: bytecode_stage,
            });
        }

        // Shader translation can surface sampler texture types via SM3 `dcl_*` declarations.
        //
        // The executor can satisfy non-2D sampler bindings by falling back to dummy texture views
        // for unsupported resource types, but still validate the declared texture type encoding to
        // avoid accepting unknown dimensions.
        for (&sampler, &ty) in &cached.sampler_texture_types {
            if matches!(
                ty,
                TextureType::Texture1D
                    | TextureType::Texture2D
                    | TextureType::Texture3D
                    | TextureType::TextureCube
            ) {
                continue;
            }
            return Err(AerogpuD3d9Error::ShaderTranslation(format!(
                "unsupported sampler texture type {ty:?} (s{sampler})"
            )));
        }

        let wgsl = cached.wgsl.clone();
        let module = self
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("aerogpu-d3d9.shader"),
                source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(wgsl.as_str())),
            });

        let mut used_samplers_mask = 0u16;
        let mut sampler_dim_key = 0u32;
        for &s in &cached.used_samplers {
            if (s as usize) < MAX_SAMPLERS {
                used_samplers_mask |= 1u16 << s;
                let dim_code = match cached
                    .sampler_texture_types
                    .get(&s)
                    .copied()
                    .unwrap_or(TextureType::Texture2D)
                {
                    TextureType::TextureCube => 1,
                    TextureType::Texture3D => 2,
                    TextureType::Texture1D => 3,
                    _ => 0,
                };
                sampler_dim_key |= dim_code << (u32::from(s) * 2);
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
                entry_point: cached.entry_point,
                uses_semantic_locations: cached.uses_semantic_locations
                    && bytecode_stage == shader::ShaderStage::Vertex,
                semantic_locations: cached.semantic_locations.clone(),
                used_samplers_mask,
                sampler_dim_key,
            },
        );
        Ok(())
    }

    #[cfg(target_arch = "wasm32")]
    async fn create_shader_dxbc_persistent(
        &mut self,
        shader_handle: u32,
        expected_stage: shader::ShaderStage,
        dxbc_bytes: &[u8],
    ) -> Result<(), AerogpuD3d9Error> {
        let flags = self.persistent_shader_cache_flags.clone();

        // At most one invalidation+retranslate retry for corruption defense.
        let mut invalidated_once = false;

        loop {
            let stats = self.stats.clone();
            let half_pixel_center = flags.half_pixel_center;
            let (artifact, source) = self
                .persistent_shader_cache
                .get_or_translate_with_source(dxbc_bytes, flags.clone(), move || async move {
                    let translated = shader_translate::translate_d3d9_shader_to_wgsl(
                        dxbc_bytes,
                        shader::WgslOptions { half_pixel_center },
                    )
                    .map_err(|e| e.to_string())?;

                    for (&sampler, &ty) in &translated.sampler_texture_types {
                        if matches!(
                            ty,
                            TextureType::Texture1D
                                | TextureType::Texture2D
                                | TextureType::Texture3D
                                | TextureType::TextureCube
                        ) {
                            continue;
                        }
                        return Err(format!(
                            "unsupported sampler texture type {ty:?} (s{sampler})"
                        ));
                    }

                    if translated.backend == ShaderTranslateBackend::LegacyFallback {
                        stats.inc_d3d9_shader_sm3_fallbacks();
                        debug!(
                            shader_handle,
                            reason = translated.fallback_reason.as_deref().unwrap_or("<unknown>"),
                            "SM3 shader translation failed; falling back to legacy D3D9 translator"
                        );
                    }

                    let mut used_samplers_mask = 0u16;
                    let mut sampler_dim_key = 0u32;
                    for &s in &translated.used_samplers {
                        if (s as usize) < MAX_SAMPLERS {
                            used_samplers_mask |= 1u16 << s;
                            let dim_code = match translated
                                .sampler_texture_types
                                .get(&s)
                                .copied()
                                .unwrap_or(TextureType::Texture2D)
                            {
                                TextureType::TextureCube => 1,
                                TextureType::Texture3D => 2,
                                TextureType::Texture1D => 3,
                                _ => 0,
                            };
                            sampler_dim_key |= dim_code << (u32::from(s) * 2);
                        } else {
                            debug!(
                                shader_handle,
                                sampler = s,
                                "shader uses out-of-range sampler index"
                            );
                        }
                    }

                    let reflection = PersistentShaderReflection {
                        schema_version: PERSISTENT_SHADER_REFLECTION_SCHEMA_VERSION,
                        stage: PersistentShaderStage::from_stage(translated.version.stage),
                        entry_point: translated.entry_point.to_string(),
                        uses_semantic_locations: translated.uses_semantic_locations,
                        used_samplers_mask,
                        sampler_dim_key,
                        semantic_locations: translated.semantic_locations.clone(),
                    };
                    let reflection = serde_json::to_value(reflection).map_err(|e| e.to_string())?;

                    Ok(aero_d3d9::runtime::PersistedShaderArtifact {
                        wgsl: translated.wgsl,
                        reflection,
                    })
                })
                .await
                .map_err(|err| {
                    AerogpuD3d9Error::ShaderTranslation(
                        err.as_string().unwrap_or_else(|| format!("{err:?}")),
                    )
                })?;

            // If the JS persistent cache is unavailable (missing APIs, quota/permission errors,
            // serialization issues, etc.) the shader cache disables persistence for the remainder
            // of the session. Expose this via stats so browser harnesses can detect when
            // persistence isn't active.
            self.stats.set_d3d9_shader_cache_disabled(
                self.persistent_shader_cache.is_persistent_disabled(),
            );

            let aero_d3d9::runtime::PersistedShaderArtifact { wgsl, reflection } = artifact;

            let reflection: PersistentShaderReflection = match serde_json::from_value(reflection) {
                Ok(v) => v,
                Err(err) => {
                    debug!(
                        ?err,
                        "cached shader reflection is malformed; invalidating and retranslating"
                    );
                    if !invalidated_once {
                        invalidated_once = true;
                        let _ = self
                            .persistent_shader_cache
                            .invalidate(dxbc_bytes, flags.clone())
                            .await;
                        self.stats.set_d3d9_shader_cache_disabled(
                            self.persistent_shader_cache.is_persistent_disabled(),
                        );
                        continue;
                    }
                    return self.create_shader_dxbc_in_memory(
                        shader_handle,
                        expected_stage,
                        dxbc_bytes,
                    );
                }
            };

            if reflection.schema_version != PERSISTENT_SHADER_REFLECTION_SCHEMA_VERSION {
                debug!(
                    shader_handle,
                    schema_version = reflection.schema_version,
                    expected = PERSISTENT_SHADER_REFLECTION_SCHEMA_VERSION,
                    "cached shader reflection schema version is unsupported; invalidating and retranslating"
                );
                if !invalidated_once {
                    invalidated_once = true;
                    let _ = self
                        .persistent_shader_cache
                        .invalidate(dxbc_bytes, flags.clone())
                        .await;
                    self.stats.set_d3d9_shader_cache_disabled(
                        self.persistent_shader_cache.is_persistent_disabled(),
                    );
                    continue;
                }
                return self.create_shader_dxbc_in_memory(
                    shader_handle,
                    expected_stage,
                    dxbc_bytes,
                );
            }

            let bytecode_stage = reflection.stage.to_stage();
            if expected_stage != bytecode_stage {
                // Stage mismatches can be caused either by:
                // - a guest bug (CREATE_SHADER_DXBC stage doesn't match the DXBC bytecode), or
                // - stale/corrupt cached reflection metadata (Persistent hit).
                //
                // Avoid invalidating the cache on guest bugs, but do attempt a single
                // invalidate+retranslate cycle when the persistent cache returns mismatched stage
                // metadata.
                if source == aero_d3d9::runtime::ShaderCacheSource::Persistent {
                    let stage_matches_expected = match aero_d3d9::dxbc::extract_shader_bytecode(
                        dxbc_bytes,
                    ) {
                        Ok(token_stream) => {
                            let first_token = if token_stream.len() >= 4 {
                                Some(u32::from_le_bytes([
                                    token_stream[0],
                                    token_stream[1],
                                    token_stream[2],
                                    token_stream[3],
                                ]))
                            } else {
                                None
                            };
                            let actual_stage =
                                first_token.and_then(|token| match token & 0xFFFF_0000 {
                                    0xFFFE_0000 => Some(shader::ShaderStage::Vertex),
                                    0xFFFF_0000 => Some(shader::ShaderStage::Pixel),
                                    _ => None,
                                });
                            match actual_stage {
                                Some(stage) => stage == expected_stage,
                                None => {
                                    debug!(
                                        shader_handle,
                                        ?first_token,
                                        "failed to decode shader stage token while validating cached shader stage; invalidating and retranslating"
                                    );
                                    true
                                }
                            }
                        }
                        Err(err) => {
                            debug!(
                                shader_handle,
                                ?err,
                                "failed to extract shader bytecode while validating cached shader stage; invalidating and retranslating"
                            );
                            true
                        }
                    };
                    if stage_matches_expected {
                        debug!(
                            shader_handle,
                            expected = ?expected_stage,
                            cached = ?bytecode_stage,
                            "cached shader stage metadata is incorrect; invalidating and retranslating"
                        );
                        if !invalidated_once {
                            invalidated_once = true;
                            let _ = self
                                .persistent_shader_cache
                                .invalidate(dxbc_bytes, flags.clone())
                                .await;
                            self.stats.set_d3d9_shader_cache_disabled(
                                self.persistent_shader_cache.is_persistent_disabled(),
                            );
                            continue;
                        }
                        return self.create_shader_dxbc_in_memory(
                            shader_handle,
                            expected_stage,
                            dxbc_bytes,
                        );
                    }
                }

                return Err(AerogpuD3d9Error::ShaderStageMismatch {
                    shader_handle,
                    expected: expected_stage,
                    actual: bytecode_stage,
                });
            }

            let expected_entry_point = match bytecode_stage {
                shader::ShaderStage::Vertex => "vs_main",
                shader::ShaderStage::Pixel => "fs_main",
            };
            let entry_point: &'static str = match reflection.entry_point.as_str() {
                entry_point if entry_point == expected_entry_point => expected_entry_point,
                other => {
                    debug!(
                        shader_handle,
                        stage = ?bytecode_stage,
                        entry_point = other,
                        expected = expected_entry_point,
                        "cached shader entry point does not match stage; invalidating and retranslating"
                    );
                    if !invalidated_once {
                        invalidated_once = true;
                        let _ = self
                            .persistent_shader_cache
                            .invalidate(dxbc_bytes, flags.clone())
                            .await;
                        self.stats.set_d3d9_shader_cache_disabled(
                            self.persistent_shader_cache.is_persistent_disabled(),
                        );
                        continue;
                    }
                    return self.create_shader_dxbc_in_memory(
                        shader_handle,
                        expected_stage,
                        dxbc_bytes,
                    );
                }
            };

            if source == aero_d3d9::runtime::ShaderCacheSource::Persistent {
                let expected_stage_attr = match bytecode_stage {
                    shader::ShaderStage::Vertex => "@vertex",
                    shader::ShaderStage::Pixel => "@fragment",
                };
                let expected_fn_sig = match bytecode_stage {
                    shader::ShaderStage::Vertex => "fn vs_main(",
                    shader::ShaderStage::Pixel => "fn fs_main(",
                };
                if !wgsl_has_expected_entry_point(wgsl.as_str(), bytecode_stage) {
                    debug!(
                        shader_handle,
                        expected_stage_attr,
                        expected_fn_sig,
                        "cached WGSL is missing expected shader entry point; invalidating and retranslating"
                    );
                    if !invalidated_once {
                        invalidated_once = true;
                        let _ = self
                            .persistent_shader_cache
                            .invalidate(dxbc_bytes, flags.clone())
                            .await;
                        self.stats.set_d3d9_shader_cache_disabled(
                            self.persistent_shader_cache.is_persistent_disabled(),
                        );
                        continue;
                    }
                    return self.create_shader_dxbc_in_memory(
                        shader_handle,
                        expected_stage,
                        dxbc_bytes,
                    );
                }

                if let Err(reason) = validate_wgsl_binding_contract(
                    wgsl.as_str(),
                    bytecode_stage,
                    flags.half_pixel_center,
                ) {
                    debug!(
                        shader_handle,
                        %reason,
                        "cached WGSL does not match expected binding contract; invalidating and retranslating"
                    );
                    if !invalidated_once {
                        invalidated_once = true;
                        let _ = self
                            .persistent_shader_cache
                            .invalidate(dxbc_bytes, flags.clone())
                            .await;
                        self.stats.set_d3d9_shader_cache_disabled(
                            self.persistent_shader_cache.is_persistent_disabled(),
                        );
                        continue;
                    }
                    return self.create_shader_dxbc_in_memory(
                        shader_handle,
                        expected_stage,
                        dxbc_bytes,
                    );
                }

                let (wgsl_used_samplers_mask, wgsl_sampler_dim_key) =
                    derive_sampler_masks_from_wgsl(wgsl.as_str());
                if wgsl_used_samplers_mask != reflection.used_samplers_mask
                    || wgsl_sampler_dim_key != reflection.sampler_dim_key
                {
                    debug!(
                        shader_handle,
                        expected_used = reflection.used_samplers_mask,
                        expected_dim_key = reflection.sampler_dim_key,
                        derived_used = wgsl_used_samplers_mask,
                        derived_dim_key = wgsl_sampler_dim_key,
                        "cached shader sampler metadata does not match WGSL; invalidating and retranslating"
                    );
                    if !invalidated_once {
                        invalidated_once = true;
                        let _ = self
                            .persistent_shader_cache
                            .invalidate(dxbc_bytes, flags.clone())
                            .await;
                        self.stats.set_d3d9_shader_cache_disabled(
                            self.persistent_shader_cache.is_persistent_disabled(),
                        );
                        continue;
                    }
                    return self.create_shader_dxbc_in_memory(
                        shader_handle,
                        expected_stage,
                        dxbc_bytes,
                    );
                }

                // Validate semantic location metadata for vertex shaders. Corrupt semantic mappings
                // can cause incorrect vertex attribute binding or spurious draw-time errors.
                if bytecode_stage == shader::ShaderStage::Vertex {
                    let mut reason = None::<String>;
                    if reflection.uses_semantic_locations
                        && reflection.semantic_locations.is_empty()
                    {
                        reason = Some(
                            "usesSemanticLocations is true but semanticLocations is empty".into(),
                        );
                    } else if !reflection.uses_semantic_locations
                        && !reflection.semantic_locations.is_empty()
                    {
                        reason = Some(
                            "usesSemanticLocations is false but semanticLocations is non-empty"
                                .into(),
                        );
                    } else if !reflection.semantic_locations.is_empty() {
                        let max_vertex_attributes =
                            self.device.limits().max_vertex_attributes.max(1);
                        let mut loc_to_sem =
                            HashMap::<u32, (aero_d3d9::vertex::DeclUsage, u8)>::new();
                        let mut sem_to_loc =
                            HashMap::<(aero_d3d9::vertex::DeclUsage, u8), u32>::new();
                        for loc in &reflection.semantic_locations {
                            if loc.location >= max_vertex_attributes {
                                reason = Some(format!(
                                    "semanticLocations contains out-of-range @location({}) (maxVertexAttributes={})",
                                    loc.location, max_vertex_attributes
                                ));
                                break;
                            }

                            let semantic = (loc.usage, loc.usage_index);
                            if let Some(prev) = loc_to_sem.insert(loc.location, semantic) {
                                if prev != semantic {
                                    reason = Some(format!(
                                        "semanticLocations maps multiple semantics to @location({}): {:?}{} and {:?}{}",
                                        loc.location, prev.0, prev.1, semantic.0, semantic.1
                                    ));
                                    break;
                                }
                            }
                            if let Some(prev_loc) = sem_to_loc.insert(semantic, loc.location) {
                                if prev_loc != loc.location {
                                    reason = Some(format!(
                                        "semanticLocations maps {:?}{} to multiple locations: {} and {}",
                                        semantic.0, semantic.1, prev_loc, loc.location
                                    ));
                                    break;
                                }
                            }
                        }
                    }

                    if let Some(reason) = reason {
                        debug!(
                            shader_handle,
                            %reason,
                            "cached shader semantic location metadata is invalid; invalidating and retranslating"
                        );
                        if !invalidated_once {
                            invalidated_once = true;
                            let _ = self
                                .persistent_shader_cache
                                .invalidate(dxbc_bytes, flags.clone())
                                .await;
                            self.stats.set_d3d9_shader_cache_disabled(
                                self.persistent_shader_cache.is_persistent_disabled(),
                            );
                            continue;
                        }
                        return self.create_shader_dxbc_in_memory(
                            shader_handle,
                            expected_stage,
                            dxbc_bytes,
                        );
                    }
                }
            }

            // Optional: validate cached WGSL on persistent hit to guard against corruption/staleness.
            let module = if source == aero_d3d9::runtime::ShaderCacheSource::Persistent {
                self.device.push_error_scope(wgpu::ErrorFilter::Validation);
                let module = self
                    .device
                    .create_shader_module(wgpu::ShaderModuleDescriptor {
                        label: Some("aerogpu-d3d9.shader.cached"),
                        source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(wgsl.as_str())),
                    });
                self.device.poll(wgpu::Maintain::Poll);
                let err = self.device.pop_error_scope().await;
                if let Some(err) = err {
                    debug!(
                        ?err,
                        "cached WGSL failed wgpu validation; invalidating and retranslating"
                    );
                    if !invalidated_once {
                        invalidated_once = true;
                        let _ = self
                            .persistent_shader_cache
                            .invalidate(dxbc_bytes, flags.clone())
                            .await;
                        self.stats.set_d3d9_shader_cache_disabled(
                            self.persistent_shader_cache.is_persistent_disabled(),
                        );
                        continue;
                    }
                    return self.create_shader_dxbc_in_memory(
                        shader_handle,
                        expected_stage,
                        dxbc_bytes,
                    );
                }
                module
            } else {
                self.device
                    .create_shader_module(wgpu::ShaderModuleDescriptor {
                        label: Some("aerogpu-d3d9.shader"),
                        source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(wgsl.as_str())),
                    })
            };

            match source {
                aero_d3d9::runtime::ShaderCacheSource::Memory => {
                    self.stats.inc_d3d9_shader_cache_memory_hits();
                }
                aero_d3d9::runtime::ShaderCacheSource::Persistent => {
                    self.stats.inc_d3d9_shader_cache_persistent_hits();
                }
                aero_d3d9::runtime::ShaderCacheSource::Translated => {
                    // Only count this as a *persistent* cache miss when persistence is actually
                    // available. If the persistent cache is disabled/unavailable (missing APIs,
                    // quota issues, etc), `ShaderCacheSource::Translated` can still be returned.
                    if !self.persistent_shader_cache.is_persistent_disabled() {
                        self.stats.inc_d3d9_shader_cache_persistent_misses();
                    }
                    self.stats.inc_d3d9_shader_translate_calls();
                }
            }

            let key = xxhash_rust::xxh3::xxh3_64(dxbc_bytes);
            self.shaders.insert(
                shader_handle,
                Shader {
                    stage: bytecode_stage,
                    key,
                    module,
                    wgsl,
                    entry_point,
                    uses_semantic_locations: reflection.uses_semantic_locations
                        && bytecode_stage == shader::ShaderStage::Vertex,
                    semantic_locations: reflection.semantic_locations.clone(),
                    used_samplers_mask: reflection.used_samplers_mask,
                    sampler_dim_key: reflection.sampler_dim_key,
                },
            );
            return Ok(());
        }
    }

    async fn execute_cmd_async(
        &mut self,
        cmd: AeroGpuCmd<'_>,
        ctx: &mut SubmissionCtx<'_>,
        pending_writebacks: &mut Vec<PendingWriteback>,
    ) -> Result<(), AerogpuD3d9Error> {
        #[cfg(target_arch = "wasm32")]
        match cmd {
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

                self.create_shader_dxbc_persistent(shader_handle, expected_stage, dxbc_bytes)
                    .await
            }
            other => self.execute_cmd(other, ctx, pending_writebacks),
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            self.execute_cmd(cmd, ctx, pending_writebacks)
        }
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
            | AeroGpuCmd::SetShaderResourceBuffers { .. }
            | AeroGpuCmd::SetUnorderedAccessBuffers { .. }
            | AeroGpuCmd::Dispatch { .. }
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

                if self.shared_surfaces.contains_handle(buffer_handle) {
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
                    // Respect the guest-provided usage flags. The WebGL2 backend in particular has
                    // stricter downlevel limits for some usages (e.g. INDEX buffers), so avoid
                    // over-allocating capabilities on buffers that are only ever used as vertex
                    // buffers.
                    let mut buffer_usage =
                        wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC;
                    if (usage_flags & cmd::AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER) != 0 {
                        buffer_usage |= wgpu::BufferUsages::VERTEX;
                    }
                    if (usage_flags & cmd::AEROGPU_RESOURCE_USAGE_INDEX_BUFFER) != 0 {
                        buffer_usage |= wgpu::BufferUsages::INDEX;
                    }
                    if (usage_flags & cmd::AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER) != 0 {
                        buffer_usage |= wgpu::BufferUsages::UNIFORM;
                    }
                    if (usage_flags & cmd::AEROGPU_RESOURCE_USAGE_STORAGE) != 0 {
                        buffer_usage |= wgpu::BufferUsages::STORAGE;
                    }

                    let shadow_len = usize::try_from(size_bytes).map_err(|_| {
                        AerogpuD3d9Error::Validation(format!(
                            "CREATE_BUFFER: size_bytes is too large for host shadow copy (size_bytes={size_bytes})"
                        ))
                    })?;

                    // Reserve the handle before inserting the resource so we don't overwrite an
                    // underlying resource kept alive by shared-surface aliases.
                    self.shared_surfaces
                        .register_handle(buffer_handle)
                        .map_err(|_| AerogpuD3d9Error::ResourceHandleInUse(buffer_handle))?;

                    let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("aerogpu-d3d9.buffer"),
                        size: size_bytes,
                        usage: buffer_usage,
                        mapped_at_creation: false,
                    });
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
                // The D3D9 executor currently supports:
                // - regular 2D textures via `CREATE_TEXTURE2D` with `array_layers=1`
                // - cube textures via `CREATE_TEXTURE2D` with `array_layers=6`
                //
                // Other array texture sizes are not supported yet.
                if array_layers != 1 && array_layers != 6 {
                    return Err(AerogpuD3d9Error::Validation(format!(
                        "CREATE_TEXTURE2D: array_layers is not supported (array_layers={array_layers})"
                    )));
                }
                // Cube texture views require square dimensions. D3D9 cube textures are always
                // square; reject invalid shapes early to avoid wgpu validation errors when creating
                // the cube view.
                if array_layers == 6 && width != height {
                    return Err(AerogpuD3d9Error::Validation(format!(
                        "CREATE_TEXTURE2D: cube textures require width == height (width={width}, height={height})"
                    )));
                }
                // WebGPU validation requires `mip_level_count` to be within the possible chain
                // length for the given dimensions.
                let max_dim = width.max(height);
                let max_mip_levels = 32u32.saturating_sub(max_dim.leading_zeros());
                if mip_levels > max_mip_levels {
                    return Err(AerogpuD3d9Error::Validation(format!(
                        "CREATE_TEXTURE2D: mip_levels too large for dimensions (width={width}, height={height}, mip_levels={mip_levels}, max_mip_levels={max_mip_levels})"
                    )));
                }
                if aerogpu_format_bc(format_raw).is_some()
                    && (usage_flags
                        & (cmd::AEROGPU_RESOURCE_USAGE_RENDER_TARGET
                            | cmd::AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL
                            | cmd::AEROGPU_RESOURCE_USAGE_SCANOUT))
                        != 0
                {
                    return Err(AerogpuD3d9Error::Validation(format!(
                        "CREATE_TEXTURE2D: BC formats cannot be used with RENDER_TARGET/DEPTH_STENCIL/SCANOUT usage flags (handle={texture_handle})"
                    )));
                }
                let mapped_format = map_aerogpu_format(format_raw)?;
                let format = match mapped_format {
                    // Allow BC formats to fall back to CPU decompression + RGBA8 uploads when the
                    // device can't sample BC textures (e.g. wgpu GL/WebGL2 paths), or when the
                    // texture's mip chain isn't compatible with wgpu/WebGPU's BC dimension
                    // requirements (mip levels that are at least one full block in size must be
                    // block-aligned).
                    wgpu::TextureFormat::Bc1RgbaUnorm
                    | wgpu::TextureFormat::Bc2RgbaUnorm
                    | wgpu::TextureFormat::Bc3RgbaUnorm
                    | wgpu::TextureFormat::Bc7RgbaUnorm => {
                        let bc_supported = self
                            .device
                            .features()
                            .contains(wgpu::Features::TEXTURE_COMPRESSION_BC);

                        if bc_supported
                            && crate::wgpu_bc_texture_dimensions_compatible(
                                width, height, mip_levels,
                            )
                        {
                            mapped_format
                        } else {
                            wgpu::TextureFormat::Rgba8Unorm
                        }
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
                        row_pitch_bytes,
                        size_bytes: required,
                    })
                };

                if self.shared_surfaces.contains_handle(texture_handle) {
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

                    // Reserve the handle before inserting the resource so we don't overwrite an
                    // underlying resource kept alive by shared-surface aliases.
                    self.shared_surfaces
                        .register_handle(texture_handle)
                        .map_err(|_| AerogpuD3d9Error::ResourceHandleInUse(texture_handle))?;

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
                    let view_cube = if array_layers == 6 {
                        Some(texture.create_view(&wgpu::TextureViewDescriptor {
                            dimension: Some(wgpu::TextureViewDimension::Cube),
                            base_array_layer: 0,
                            array_layer_count: Some(6),
                            ..Default::default()
                        }))
                    } else {
                        None
                    };
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
                    let view_cube_srgb = if self
                        .downlevel_flags
                        .contains(wgpu::DownlevelFlags::VIEW_FORMATS)
                        && array_layers == 6
                    {
                        match format {
                            wgpu::TextureFormat::Rgba8Unorm => {
                                Some(texture.create_view(&wgpu::TextureViewDescriptor {
                                    format: Some(wgpu::TextureFormat::Rgba8UnormSrgb),
                                    dimension: Some(wgpu::TextureViewDimension::Cube),
                                    base_array_layer: 0,
                                    array_layer_count: Some(6),
                                    ..Default::default()
                                }))
                            }
                            wgpu::TextureFormat::Bgra8Unorm => {
                                Some(texture.create_view(&wgpu::TextureViewDescriptor {
                                    format: Some(wgpu::TextureFormat::Bgra8UnormSrgb),
                                    dimension: Some(wgpu::TextureViewDimension::Cube),
                                    base_array_layer: 0,
                                    array_layer_count: Some(6),
                                    ..Default::default()
                                }))
                            }
                            wgpu::TextureFormat::Bc1RgbaUnorm => {
                                Some(texture.create_view(&wgpu::TextureViewDescriptor {
                                    format: Some(wgpu::TextureFormat::Bc1RgbaUnormSrgb),
                                    dimension: Some(wgpu::TextureViewDimension::Cube),
                                    base_array_layer: 0,
                                    array_layer_count: Some(6),
                                    ..Default::default()
                                }))
                            }
                            wgpu::TextureFormat::Bc2RgbaUnorm => {
                                Some(texture.create_view(&wgpu::TextureViewDescriptor {
                                    format: Some(wgpu::TextureFormat::Bc2RgbaUnormSrgb),
                                    dimension: Some(wgpu::TextureViewDimension::Cube),
                                    base_array_layer: 0,
                                    array_layer_count: Some(6),
                                    ..Default::default()
                                }))
                            }
                            wgpu::TextureFormat::Bc3RgbaUnorm => {
                                Some(texture.create_view(&wgpu::TextureViewDescriptor {
                                    format: Some(wgpu::TextureFormat::Bc3RgbaUnormSrgb),
                                    dimension: Some(wgpu::TextureViewDimension::Cube),
                                    base_array_layer: 0,
                                    array_layer_count: Some(6),
                                    ..Default::default()
                                }))
                            }
                            wgpu::TextureFormat::Bc7RgbaUnorm => {
                                Some(texture.create_view(&wgpu::TextureViewDescriptor {
                                    format: Some(wgpu::TextureFormat::Bc7RgbaUnormSrgb),
                                    dimension: Some(wgpu::TextureViewDimension::Cube),
                                    base_array_layer: 0,
                                    array_layer_count: Some(6),
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
                            view_cube,
                            view_cube_srgb,
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
                        width,
                        height,
                        mip_level_count,
                        array_layers,
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
                        let min_row_pitch = block.row_pitch_bytes(*width)?;
                        let mip0_row_pitch = if *row_pitch_bytes != 0 {
                            (*row_pitch_bytes).max(min_row_pitch)
                        } else {
                            min_row_pitch
                        };
                        let layout = guest_texture_linear_layout(
                            *format_raw,
                            *width,
                            *height,
                            *mip_level_count,
                            *array_layers,
                            mip0_row_pitch,
                        )?;
                        let total_size = layout.total_size_bytes;
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
                            let end_usize =
                                off.checked_add(len).ok_or(AerogpuD3d9Error::Validation(
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
                            mip_level_count,
                            array_layers,
                            row_pitch_bytes,
                            ..
                        } => {
                            let base_width = *width;
                            let base_height = *height;
                            let mip_level_count = *mip_level_count;
                            let array_layers = *array_layers;
                            let mip0_row_pitch_bytes = *row_pitch_bytes;

                            let src_block = aerogpu_format_texel_block_info(*format_raw)?;
                            let bc_format = aerogpu_format_bc(*format_raw);
                            let dst_is_bc = matches!(
                                *format,
                                wgpu::TextureFormat::Bc1RgbaUnorm
                                    | wgpu::TextureFormat::Bc2RgbaUnorm
                                    | wgpu::TextureFormat::Bc3RgbaUnorm
                                    | wgpu::TextureFormat::Bc7RgbaUnorm
                            );

                            // `UPLOAD_RESOURCE` offsets are expressed in terms of the guest linear
                            // texture layout used for both guest-backed and host-backed textures.
                            // This includes all mip levels (packed sequentially).
                            //
                            // Map the incoming byte offset into a specific mip level so host-backed
                            // mipmapped textures can be updated via `UPLOAD_RESOURCE`.
                            let min_row_pitch_mip0 = src_block.row_pitch_bytes(base_width)?;
                            let mip0_layout_row_pitch = if mip0_row_pitch_bytes != 0 {
                                mip0_row_pitch_bytes.max(min_row_pitch_mip0)
                            } else {
                                min_row_pitch_mip0
                            };
                            let layout = guest_texture_linear_layout(
                                *format_raw,
                                base_width,
                                base_height,
                                mip_level_count,
                                array_layers,
                                mip0_layout_row_pitch,
                            )?;
                            let end_global = offset_bytes
                                .checked_add(size_bytes)
                                .ok_or(AerogpuD3d9Error::UploadOutOfBounds(resource_handle))?;
                            if end_global > layout.total_size_bytes {
                                return Err(AerogpuD3d9Error::UploadOutOfBounds(resource_handle));
                            }

                            let layer_stride = layout.layer_stride_bytes;
                            let layer = if layer_stride != 0 {
                                offset_bytes / layer_stride
                            } else {
                                return Err(AerogpuD3d9Error::Validation(
                                    "UPLOAD_RESOURCE: texture layer stride is 0".into(),
                                ));
                            };
                            if layer >= u64::from(array_layers) {
                                return Err(AerogpuD3d9Error::UploadOutOfBounds(resource_handle));
                            }
                            let layer_base = layer.checked_mul(layer_stride).ok_or_else(|| {
                                AerogpuD3d9Error::Validation(
                                    "UPLOAD_RESOURCE: texture layer offset overflow".into(),
                                )
                            })?;
                            let in_layer_offset = offset_bytes
                                .checked_sub(layer_base)
                                .ok_or(AerogpuD3d9Error::UploadOutOfBounds(resource_handle))?;
                            let in_layer_end = end_global
                                .checked_sub(layer_base)
                                .ok_or(AerogpuD3d9Error::UploadOutOfBounds(resource_handle))?;

                            // Find the mip level containing `in_layer_offset` and ensure the
                            // upload doesn't cross mip boundaries (we don't support multi-mip
                            // uploads in a single command).
                            let mut upload_mip_level: Option<u32> = None;
                            let mut mip_base: u64 = 0;
                            let mut _mip_end: u64 = 0;
                            for mip in 0..mip_level_count {
                                let start =
                                    *layout.mip_offsets.get(mip as usize).ok_or_else(|| {
                                        AerogpuD3d9Error::Validation(
                                            "UPLOAD_RESOURCE: mip layout out of bounds".into(),
                                        )
                                    })?;
                                let end = if mip + 1 < mip_level_count {
                                    *layout.mip_offsets.get((mip + 1) as usize).ok_or_else(
                                        || {
                                            AerogpuD3d9Error::Validation(
                                                "UPLOAD_RESOURCE: mip layout out of bounds".into(),
                                            )
                                        },
                                    )?
                                } else {
                                    layout.layer_stride_bytes
                                };
                                if in_layer_offset >= start && in_layer_offset < end {
                                    if in_layer_end > end {
                                        return Err(AerogpuD3d9Error::UploadNotSupported(
                                            resource_handle,
                                        ));
                                    }
                                    upload_mip_level = Some(mip);
                                    mip_base = start;
                                    _mip_end = end;
                                    break;
                                }
                            }
                            let upload_mip_level = upload_mip_level
                                .ok_or(AerogpuD3d9Error::UploadOutOfBounds(resource_handle))?;
                            debug_assert!(
                                in_layer_offset >= mip_base && in_layer_offset < _mip_end
                            );

                            // NOTE: Some wgpu backends (notably the GL path on some drivers) have
                            // been observed to mishandle `copy_buffer_to_texture` into cube texture
                            // subresources. To keep UPLOAD_RESOURCE reliable, we route cube uploads
                            // through an intermediate 2D texture and then `copy_texture_to_texture`
                            // into the cube subresource.
                            let prefer_intermediate_upload = array_layers == 6;

                            let mip_w = mip_extent(base_width, upload_mip_level);
                            let mip_h = mip_extent(base_height, upload_mip_level);
                            let mip_row_pitch_bytes = if upload_mip_level == 0 {
                                mip0_row_pitch_bytes
                            } else {
                                0u32
                            };
                            let width = &mip_w;
                            let height = &mip_h;
                            let row_pitch_bytes = &mip_row_pitch_bytes;
                            let offset_bytes = in_layer_offset
                                .checked_sub(mip_base)
                                .ok_or(AerogpuD3d9Error::UploadOutOfBounds(resource_handle))?;

                            let upload_origin_z: u32 = layer.try_into().map_err(|_| {
                                AerogpuD3d9Error::Validation(
                                    "UPLOAD_RESOURCE: array layer out of range".into(),
                                )
                            })?;

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
                            let pixel_row_pitch_u64 = u64::from(expected_row_pitch);
                            let units_per_row = width.div_ceil(src_block.block_width);

                            let mut upload_segment =
                                |segment_offset_bytes: u64,
                                 segment_size_bytes: u64,
                                 segment_data: &[u8]|
                                 -> Result<(), AerogpuD3d9Error> {
                                    if segment_size_bytes == 0 {
                                        return Ok(());
                                    }

                                    let x_bytes = segment_offset_bytes % src_pitch_u64;
                                    let y_row = segment_offset_bytes / src_pitch_u64;
                                    if y_row >= u64::from(total_rows) {
                                        return Err(AerogpuD3d9Error::UploadOutOfBounds(
                                            resource_handle,
                                        ));
                                    }
                                    let origin_y_row: u32 = y_row.try_into().map_err(|_| {
                                        AerogpuD3d9Error::Validation(
                                            "UPLOAD_RESOURCE: origin_y out of range".into(),
                                        )
                                    })?;

                                    let full_rows = x_bytes == 0
                                        && segment_size_bytes.is_multiple_of(src_pitch_u64);

                                    let (
                                        origin_x_texels,
                                        origin_y_texels,
                                        copy_w_texels,
                                        copy_h_texels,
                                        buffer_rows,
                                        src_bpr_bytes,
                                        segment_data,
                                    ) = if full_rows {
                                        let buffer_rows =
                                            u32::try_from(segment_size_bytes / src_pitch_u64)
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
                                            height.checked_sub(origin_y_texels).ok_or_else(
                                                || {
                                                    AerogpuD3d9Error::Validation(
                                                        "UPLOAD_RESOURCE: origin_y out of bounds"
                                                            .into(),
                                                    )
                                                },
                                            )?
                                        } else {
                                            buffer_rows
                                                .checked_mul(src_block.block_height)
                                                .ok_or_else(|| {
                                                    AerogpuD3d9Error::Validation(
                                                        "UPLOAD_RESOURCE: copy height overflow"
                                                            .into(),
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
                                            segment_data,
                                        )
                                    } else {
                                        // Single-row upload. Note: `segment_size_bytes` may include
                                        // end-of-row padding bytes when the texture's row pitch is
                                        // larger than its texel bytes-per-row. Those bytes do not map
                                        // to any texels and are therefore ignored.
                                        if x_bytes.saturating_add(segment_size_bytes)
                                            > src_pitch_u64
                                        {
                                            return Err(AerogpuD3d9Error::UploadNotSupported(
                                                resource_handle,
                                            ));
                                        }

                                        let used_size_bytes = if x_bytes >= pixel_row_pitch_u64 {
                                            0
                                        } else {
                                            segment_size_bytes.min(pixel_row_pitch_u64 - x_bytes)
                                        };
                                        if used_size_bytes == 0 {
                                            return Ok(());
                                        }

                                        if !x_bytes.is_multiple_of(bytes_per_unit)
                                            || !used_size_bytes.is_multiple_of(bytes_per_unit)
                                        {
                                            return Err(AerogpuD3d9Error::UploadNotSupported(
                                                resource_handle,
                                            ));
                                        }

                                        let x_unit = u32::try_from(x_bytes / bytes_per_unit)
                                            .map_err(|_| {
                                                AerogpuD3d9Error::UploadNotSupported(
                                                    resource_handle,
                                                )
                                            })?;

                                        let units_to_copy =
                                            u32::try_from(used_size_bytes / bytes_per_unit)
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
                                                    "UPLOAD_RESOURCE: origin_x out of bounds"
                                                        .into(),
                                                )
                                            })?
                                        } else {
                                            units_to_copy
                                                .checked_mul(src_block.block_width)
                                                .ok_or_else(|| {
                                                    AerogpuD3d9Error::Validation(
                                                        "UPLOAD_RESOURCE: copy width overflow"
                                                            .into(),
                                                    )
                                                })?
                                        };

                                        let end_row = origin_y_row + 1;
                                        let copy_h_texels = if end_row == total_rows {
                                            height.checked_sub(origin_y_texels).ok_or_else(
                                                || {
                                                    AerogpuD3d9Error::Validation(
                                                        "UPLOAD_RESOURCE: origin_y out of bounds"
                                                            .into(),
                                                    )
                                                },
                                            )?
                                        } else {
                                            src_block.block_height
                                        };

                                        let row_len_bytes = units_to_copy
                                            .checked_mul(src_block.bytes_per_block)
                                            .ok_or_else(|| {
                                                AerogpuD3d9Error::Validation(
                                                    "UPLOAD_RESOURCE: row byte size overflow"
                                                        .into(),
                                                )
                                            })?;

                                        let row_len_usize: usize =
                                            row_len_bytes.try_into().map_err(|_| {
                                                AerogpuD3d9Error::Validation(
                                                    "UPLOAD_RESOURCE: row byte size out of range"
                                                        .into(),
                                                )
                                            })?;
                                        let segment_data =
                                            segment_data.get(..row_len_usize).ok_or_else(|| {
                                                AerogpuD3d9Error::Validation(
                                                    "UPLOAD_RESOURCE: texture upload out of bounds"
                                                        .into(),
                                                )
                                            })?;

                                        (
                                            origin_x_texels,
                                            origin_y_texels,
                                            copy_w_texels,
                                            copy_h_texels,
                                            1u32,
                                            row_len_bytes,
                                            segment_data,
                                        )
                                    };

                                    let needs_16bit_expand = matches!(
                                        *format_raw,
                                        x if x == AerogpuFormat::B5G6R5Unorm as u32
                                            || x == AerogpuFormat::B5G5R5A1Unorm as u32
                                    );

                                    let mut upload_copy = |bytes: &[u8],
                                                           bytes_per_row: u32,
                                                           rows_per_image: u32|
                                     -> Result<(), AerogpuD3d9Error> {
                                        let staging = self.device.create_buffer_init(
                                            &wgpu::util::BufferInitDescriptor {
                                                label: Some("aerogpu-d3d9.upload_resource_staging"),
                                                contents: bytes,
                                                usage: wgpu::BufferUsages::COPY_SRC,
                                            },
                                        );
                                        let encoder = encoder_opt
                                            .as_mut()
                                            .expect("encoder exists for upload_resource");

                                        if prefer_intermediate_upload {
                                            let upload_tex = self.device.create_texture(
                                                &wgpu::TextureDescriptor {
                                                    label: Some(
                                                        "aerogpu-d3d9.upload_resource_intermediate",
                                                    ),
                                                    size: wgpu::Extent3d {
                                                        width: copy_w_texels,
                                                        height: copy_h_texels,
                                                        depth_or_array_layers: 1,
                                                    },
                                                    mip_level_count: 1,
                                                    sample_count: 1,
                                                    dimension: wgpu::TextureDimension::D2,
                                                    format: *format,
                                                    usage: wgpu::TextureUsages::COPY_DST
                                                        | wgpu::TextureUsages::COPY_SRC,
                                                    view_formats: &[],
                                                },
                                            );
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
                                                    texture: &upload_tex,
                                                    mip_level: 0,
                                                    origin: wgpu::Origin3d::ZERO,
                                                    aspect: wgpu::TextureAspect::All,
                                                },
                                                wgpu::Extent3d {
                                                    width: copy_w_texels,
                                                    height: copy_h_texels,
                                                    depth_or_array_layers: 1,
                                                },
                                            );
                                            encoder.copy_texture_to_texture(
                                                wgpu::ImageCopyTexture {
                                                    texture: &upload_tex,
                                                    mip_level: 0,
                                                    origin: wgpu::Origin3d::ZERO,
                                                    aspect: wgpu::TextureAspect::All,
                                                },
                                                wgpu::ImageCopyTexture {
                                                    texture,
                                                    mip_level: upload_mip_level,
                                                    origin: wgpu::Origin3d {
                                                        x: origin_x_texels,
                                                        y: origin_y_texels,
                                                        z: upload_origin_z,
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
                                                    mip_level: upload_mip_level,
                                                    origin: wgpu::Origin3d {
                                                        x: origin_x_texels,
                                                        y: origin_y_texels,
                                                        z: upload_origin_z,
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
                                    };

                                    if let (Some(bc), false) = (bc_format, dst_is_bc) {
                                        // CPU BC fallback upload into RGBA8.
                                        let tight_row_bytes =
                                            src_block.row_pitch_bytes(copy_w_texels)?;

                                        let bc_bytes: Vec<u8> = if full_rows {
                                            if src_bpr_bytes == tight_row_bytes {
                                                segment_data.to_vec()
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
                                                            &segment_data[src_start
                                                                ..src_start + tight_usize],
                                                        );
                                                }
                                                packed
                                            }
                                        } else {
                                            // Single-row segment is already tight (caller may have
                                            // trimmed padding bytes).
                                            segment_data.to_vec()
                                        };

                                        let rgba = match bc {
                                            BcFormat::Bc1 => decompress_bc1_rgba8(
                                                copy_w_texels,
                                                copy_h_texels,
                                                &bc_bytes,
                                            ),
                                            BcFormat::Bc2 => decompress_bc2_rgba8(
                                                copy_w_texels,
                                                copy_h_texels,
                                                &bc_bytes,
                                            ),
                                            BcFormat::Bc3 => decompress_bc3_rgba8(
                                                copy_w_texels,
                                                copy_h_texels,
                                                &bc_bytes,
                                            ),
                                            BcFormat::Bc7 => decompress_bc7_rgba8(
                                                copy_w_texels,
                                                copy_h_texels,
                                                &bc_bytes,
                                            ),
                                        };

                                        let unpadded_bpr =
                                            copy_w_texels.checked_mul(4).ok_or_else(|| {
                                                AerogpuD3d9Error::Validation(
                                                    "UPLOAD_RESOURCE: bytes_per_row overflow"
                                                        .into(),
                                                )
                                            })?;
                                        let padded_bpr = align_to(
                                            unpadded_bpr,
                                            wgpu::COPY_BYTES_PER_ROW_ALIGNMENT,
                                        );
                                        let bytes = if padded_bpr == unpadded_bpr {
                                            rgba
                                        } else {
                                            let height_usize: usize =
                                                copy_h_texels.try_into().map_err(|_| {
                                                    AerogpuD3d9Error::Validation(
                                                        "UPLOAD_RESOURCE: height out of range"
                                                            .into(),
                                                    )
                                                })?;
                                            let padded_usize: usize = padded_bpr as usize;
                                            let unpadded_usize: usize = unpadded_bpr as usize;
                                            let mut padded = vec![0u8; padded_usize * height_usize];
                                            for row in 0..height_usize {
                                                let src_start = row * unpadded_usize;
                                                let dst_start = row * padded_usize;
                                                padded[dst_start..dst_start + unpadded_usize]
                                                    .copy_from_slice(
                                                        &rgba
                                                            [src_start..src_start + unpadded_usize],
                                                    );
                                            }
                                            padded
                                        };

                                        upload_copy(&bytes, padded_bpr, copy_h_texels)?;
                                        Ok(())
                                    } else if needs_16bit_expand {
                                        // CPU expansion: 16-bit packed -> RGBA8
                                        let src_row_bytes =
                                            copy_w_texels.checked_mul(2).ok_or_else(|| {
                                                AerogpuD3d9Error::Validation(
                                                    "UPLOAD_RESOURCE: 16-bit row byte size overflow"
                                                        .into(),
                                                )
                                            })?;
                                        let unpadded_bpr =
                                            copy_w_texels.checked_mul(4).ok_or_else(|| {
                                                AerogpuD3d9Error::Validation(
                                                    "UPLOAD_RESOURCE: bytes_per_row overflow"
                                                        .into(),
                                                )
                                            })?;
                                        let bytes_per_row = align_to(
                                            unpadded_bpr,
                                            wgpu::COPY_BYTES_PER_ROW_ALIGNMENT,
                                        );

                                        let height_usize: usize =
                                            copy_h_texels.try_into().map_err(|_| {
                                                AerogpuD3d9Error::Validation(
                                                    "UPLOAD_RESOURCE: height out of range".into(),
                                                )
                                            })?;
                                        let bytes_per_row_usize = bytes_per_row as usize;
                                        let unpadded_usize = unpadded_bpr as usize;
                                        let src_pitch_usize = src_pitch as usize;
                                        let src_row_usize = src_row_bytes as usize;

                                        let mut bytes =
                                            vec![0u8; bytes_per_row_usize * height_usize];
                                        for row in 0..height_usize {
                                            let src_start = if full_rows {
                                                row.checked_mul(src_pitch_usize).ok_or_else(
                                                    || {
                                                        AerogpuD3d9Error::Validation(
                                                            "UPLOAD_RESOURCE: texture row offset overflow"
                                                                .into(),
                                                        )
                                                    },
                                                )?
                                            } else {
                                                0
                                            };
                                            let src_end = src_start
                                                .checked_add(src_row_usize)
                                                .ok_or_else(|| {
                                                    AerogpuD3d9Error::Validation(
                                                        "UPLOAD_RESOURCE: texture row offset overflow"
                                                            .into(),
                                                    )
                                                })?;
                                            let src = segment_data
                                                .get(src_start..src_end)
                                                .ok_or_else(|| {
                                                    AerogpuD3d9Error::Validation(
                                                        "UPLOAD_RESOURCE: texture upload out of bounds"
                                                            .into(),
                                                    )
                                                })?;

                                            let dst_start = row * bytes_per_row_usize;
                                            let dst_end = dst_start + unpadded_usize;
                                            let dst = bytes
                                                .get_mut(dst_start..dst_end)
                                                .ok_or_else(|| {
                                                    AerogpuD3d9Error::Validation(
                                                        "UPLOAD_RESOURCE: staging out of bounds"
                                                            .into(),
                                                    )
                                                })?;

                                            match *format_raw {
                                                x if x == AerogpuFormat::B5G6R5Unorm as u32 => {
                                                    expand_b5g6r5_unorm_to_rgba8(src, dst);
                                                }
                                                x if x == AerogpuFormat::B5G5R5A1Unorm as u32 => {
                                                    expand_b5g5r5a1_unorm_to_rgba8(src, dst);
                                                }
                                                _ => {
                                                    unreachable!("needs_16bit_expand checked above")
                                                }
                                            }
                                        }

                                        upload_copy(&bytes, bytes_per_row, copy_h_texels)?;
                                        Ok(())
                                    } else {
                                        // Direct upload.
                                        let bytes_per_row = align_to(
                                            src_bpr_bytes,
                                            wgpu::COPY_BYTES_PER_ROW_ALIGNMENT,
                                        );

                                        let bytes: Vec<u8> = if full_rows {
                                            if bytes_per_row != src_bpr_bytes {
                                                let src_bpr_usize: usize = src_bpr_bytes as usize;
                                                let dst_bpr_usize: usize = bytes_per_row as usize;
                                                let rows_usize: usize = buffer_rows as usize;
                                                let mut staging =
                                                    vec![0u8; dst_bpr_usize * rows_usize];
                                                for row in 0..rows_usize {
                                                    let src_start = row * src_bpr_usize;
                                                    let dst_start = row * dst_bpr_usize;
                                                    staging[dst_start..dst_start + src_bpr_usize]
                                                        .copy_from_slice(
                                                            &segment_data[src_start
                                                                ..src_start + src_bpr_usize],
                                                        );
                                                }
                                                staging
                                            } else {
                                                segment_data.to_vec()
                                            }
                                        } else if bytes_per_row != src_bpr_bytes {
                                            let mut staging = vec![0u8; bytes_per_row as usize];
                                            staging[..src_bpr_bytes as usize]
                                                .copy_from_slice(segment_data);
                                            staging
                                        } else {
                                            segment_data.to_vec()
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

                                        upload_copy(&bytes, bytes_per_row, buffer_rows)?;
                                        Ok(())
                                    }
                                };

                            let mut cursor: usize = 0;
                            let mut cur_offset = offset_bytes;
                            let mut remaining = size_bytes;
                            while remaining != 0 {
                                if (cur_offset % src_pitch_u64) == 0 {
                                    let full_rows_bytes =
                                        (remaining / src_pitch_u64) * src_pitch_u64;
                                    if full_rows_bytes != 0 {
                                        let full_rows_usize: usize =
                                            full_rows_bytes.try_into().map_err(|_| {
                                                AerogpuD3d9Error::Validation(
                                                    "UPLOAD_RESOURCE: segment size out of range"
                                                        .into(),
                                                )
                                            })?;
                                        let end = cursor.checked_add(full_rows_usize).ok_or_else(
                                            || {
                                                AerogpuD3d9Error::Validation(
                                                    "UPLOAD_RESOURCE: segment size overflow".into(),
                                                )
                                            },
                                        )?;
                                        let segment = data.get(cursor..end).ok_or_else(|| {
                                            AerogpuD3d9Error::Validation(
                                                "UPLOAD_RESOURCE: segment out of bounds".into(),
                                            )
                                        })?;
                                        upload_segment(cur_offset, full_rows_bytes, segment)?;
                                        cursor = end;
                                        cur_offset = cur_offset
                                            .checked_add(full_rows_bytes)
                                            .ok_or_else(|| {
                                                AerogpuD3d9Error::Validation(
                                                    "UPLOAD_RESOURCE: offset overflow".into(),
                                                )
                                            })?;
                                        remaining -= full_rows_bytes;
                                        continue;
                                    }
                                }

                                let x_bytes = cur_offset % src_pitch_u64;
                                let row_remaining =
                                    src_pitch_u64.checked_sub(x_bytes).ok_or_else(|| {
                                        AerogpuD3d9Error::Validation(
                                            "UPLOAD_RESOURCE: row remaining underflow".into(),
                                        )
                                    })?;
                                let seg_size_bytes = remaining.min(row_remaining);
                                debug_assert!(seg_size_bytes != 0);
                                let seg_size_usize: usize =
                                    seg_size_bytes.try_into().map_err(|_| {
                                        AerogpuD3d9Error::Validation(
                                            "UPLOAD_RESOURCE: segment size out of range".into(),
                                        )
                                    })?;
                                let end = cursor.checked_add(seg_size_usize).ok_or_else(|| {
                                    AerogpuD3d9Error::Validation(
                                        "UPLOAD_RESOURCE: segment size overflow".into(),
                                    )
                                })?;
                                let segment = data.get(cursor..end).ok_or_else(|| {
                                    AerogpuD3d9Error::Validation(
                                        "UPLOAD_RESOURCE: segment out of bounds".into(),
                                    )
                                })?;
                                upload_segment(cur_offset, seg_size_bytes, segment)?;
                                cursor = end;
                                cur_offset =
                                    cur_offset.checked_add(seg_size_bytes).ok_or_else(|| {
                                        AerogpuD3d9Error::Validation(
                                            "UPLOAD_RESOURCE: offset overflow".into(),
                                        )
                                    })?;
                                remaining -= seg_size_bytes;
                            }

                            Ok(())
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
                        (src_format_raw, src_format, src_w, src_h, src_mips, src_layers),
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
                    if src_format != dst_format {
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

                    // Block-compressed (BC) textures have additional copy alignment requirements.
                    //
                    // IMPORTANT: We validate based on the *guest* format (`*_format_raw`) rather
                    // than the wgpu texture format because BC textures may be stored as RGBA8 when
                    // `TEXTURE_COMPRESSION_BC` is not enabled (CPU decompression fallback).
                    let texel_block = aerogpu_format_texel_block_info(src_format_raw)?;
                    validate_copy_region_alignment(
                        texel_block,
                        CopyRegion2d {
                            origin_x: src_x,
                            origin_y: src_y,
                            width,
                            height,
                            mip_width: src_mip_w,
                            mip_height: src_mip_h,
                        },
                        "COPY_TEXTURE2D: src",
                    )?;
                    validate_copy_region_alignment(
                        texel_block,
                        CopyRegion2d {
                            origin_x: dst_x,
                            origin_y: dst_y,
                            width,
                            height,
                            mip_width: dst_mip_w,
                            mip_height: dst_mip_h,
                        },
                        "COPY_TEXTURE2D: dst",
                    )?;

                    // WebGPU copy operations for BC formats require the copy extents themselves to
                    // be block-aligned. For mip edges we allow the guest to specify the logical
                    // width/height, then round up to the containing texel-blocks for the actual
                    // WebGPU copy (the padded texels are not addressable by sampling).
                    let dst_is_bc = matches!(
                        dst_format,
                        wgpu::TextureFormat::Bc1RgbaUnorm
                            | wgpu::TextureFormat::Bc2RgbaUnorm
                            | wgpu::TextureFormat::Bc3RgbaUnorm
                            | wgpu::TextureFormat::Bc7RgbaUnorm
                    );
                    let copy_width = if dst_is_bc {
                        align_to(width, texel_block.block_width)
                    } else {
                        width
                    };
                    let copy_height = if dst_is_bc {
                        align_to(height, texel_block.block_height)
                    } else {
                        height
                    };

                    let dst_writeback_plan = if writeback {
                        let dst_backing = dst_backing.ok_or_else(|| {
                            AerogpuD3d9Error::Validation(
                                "COPY_TEXTURE2D: WRITEBACK_DST requires guest-backed dst".into(),
                            )
                        })?;
                        let bc_format = aerogpu_format_bc(dst_format_raw);
                        if bc_format.is_some() && !dst_is_bc {
                            return Err(AerogpuD3d9Error::Validation(
                                "COPY_TEXTURE2D: WRITEBACK_DST is not supported for BC textures when TEXTURE_COMPRESSION_BC is not enabled"
                                    .into(),
                            ));
                        }
                        if bc_format.is_some() && dst_is_bc && !self.bc_copy_to_buffer_supported {
                            return Err(AerogpuD3d9Error::Validation(
                                "COPY_TEXTURE2D: WRITEBACK_DST is not supported for BC textures on this backend"
                                    .into(),
                            ));
                        }
                        if ctx.guest_memory.is_none() {
                            return Err(AerogpuD3d9Error::MissingGuestMemory(dst_texture));
                        }

                        let block = aerogpu_format_texel_block_info(dst_format_raw)?;
                        let copy_w_units = width.div_ceil(block.block_width);
                        let copy_h_units = block.rows_per_image(height);
                        let guest_unit_bytes = block.bytes_per_block;
                        let guest_unpadded_bpr =
                            copy_w_units.checked_mul(guest_unit_bytes).ok_or_else(|| {
                                AerogpuD3d9Error::Validation(
                                    "COPY_TEXTURE2D: bytes_per_row overflow".into(),
                                )
                            })?;
                        let (host_unit_bytes, host_unpadded_bpr) = if dst_is_bc {
                            (block.bytes_per_block, guest_unpadded_bpr)
                        } else {
                            let host_bpp = bytes_per_pixel(dst_format);
                            let host_unpadded_bpr =
                                width.checked_mul(host_bpp).ok_or_else(|| {
                                    AerogpuD3d9Error::Validation(
                                        "COPY_TEXTURE2D: bytes_per_row overflow".into(),
                                    )
                                })?;
                            (host_bpp, host_unpadded_bpr)
                        };
                        let padded_bpr =
                            align_to(host_unpadded_bpr, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);

                        let (dst_subresource_offset_bytes, dst_subresource_row_pitch_bytes) =
                            if dst_mips == 1 && dst_layers == 1 {
                                // Single-subresource: allow padded row pitch (the legacy path).
                                (0u64, dst_backing.row_pitch_bytes)
                            } else {
                                let layout = guest_texture_linear_layout(
                                    dst_format_raw,
                                    dst_w,
                                    dst_h,
                                    dst_mips,
                                    dst_layers,
                                    dst_backing.row_pitch_bytes,
                                )?;
                                debug_assert_eq!(layout.total_size_bytes, dst_backing.size_bytes);

                                let layer_off = layout
                                    .layer_stride_bytes
                                    .checked_mul(dst_array_layer as u64)
                                    .ok_or_else(|| {
                                        AerogpuD3d9Error::Validation(
                                            "COPY_TEXTURE2D: dst subresource overflow".into(),
                                        )
                                    })?;
                                let in_layer_off = *layout
                                    .mip_offsets
                                    .get(dst_mip_level as usize)
                                    .ok_or_else(|| {
                                        AerogpuD3d9Error::Validation(
                                            "COPY_TEXTURE2D: dst mip index out of bounds".into(),
                                        )
                                    })?;
                                let in_layer_end = if dst_mip_level + 1 < dst_mips {
                                    *layout
                                        .mip_offsets
                                        .get((dst_mip_level + 1) as usize)
                                        .ok_or_else(|| {
                                            AerogpuD3d9Error::Validation(
                                                "COPY_TEXTURE2D: dst mip index out of bounds"
                                                    .into(),
                                            )
                                        })?
                                } else {
                                    layout.layer_stride_bytes
                                };
                                let sub_end =
                                    layer_off.checked_add(in_layer_end).ok_or_else(|| {
                                        AerogpuD3d9Error::Validation(
                                            "COPY_TEXTURE2D: dst subresource overflow".into(),
                                        )
                                    })?;
                                if sub_end > dst_backing.size_bytes {
                                    return Err(AerogpuD3d9Error::Validation(
                                        "COPY_TEXTURE2D: dst subresource out of bounds".into(),
                                    ));
                                }

                                let offset =
                                    layer_off.checked_add(in_layer_off).ok_or_else(|| {
                                        AerogpuD3d9Error::Validation(
                                            "COPY_TEXTURE2D: dst subresource overflow".into(),
                                        )
                                    })?;

                                let dst_block = aerogpu_format_texel_block_info(dst_format_raw)?;
                                let mip_w = mip_extent(dst_w, dst_mip_level);
                                let row_pitch = if dst_mip_level == 0 {
                                    dst_backing.row_pitch_bytes
                                } else {
                                    dst_block.row_pitch_bytes(mip_w)?
                                };

                                (offset, row_pitch)
                            };

                        let dst_x_units = if dst_x.is_multiple_of(block.block_width) {
                            dst_x / block.block_width
                        } else {
                            return Err(AerogpuD3d9Error::Validation(
                                "COPY_TEXTURE2D: BC dst_x must be block-aligned".into(),
                            ));
                        };
                        let dst_y_units = if dst_y.is_multiple_of(block.block_height) {
                            dst_y / block.block_height
                        } else {
                            return Err(AerogpuD3d9Error::Validation(
                                "COPY_TEXTURE2D: BC dst_y must be block-aligned".into(),
                            ));
                        };
                        let dst_x_bytes = (dst_x_units as u64)
                            .checked_mul(guest_unit_bytes as u64)
                            .ok_or_else(|| {
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
                            dst_x: dst_x_units,
                            dst_y: dst_y_units,
                            height: copy_h_units,
                            format_raw: dst_format_raw,
                            is_x8: is_x8_format(dst_format_raw),
                            guest_bytes_per_pixel: guest_unit_bytes,
                            host_bytes_per_pixel: host_unit_bytes,
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
                            width: copy_width,
                            height: copy_height,
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
                                width: copy_width,
                                height: copy_height,
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

                self.create_shader_dxbc_in_memory(shader_handle, expected_stage, dxbc_bytes)
            }
            AeroGpuCmd::DestroyShader { shader_handle } => {
                self.shaders.remove(&shader_handle);
                Ok(())
            }
            AeroGpuCmd::BindShaders { vs, ps, .. } => {
                self.state.vs = vs;
                self.state.ps = ps;
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

                let offset = (CONSTANTS_FLOATS_OFFSET_BYTES + stage_base)
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
            AeroGpuCmd::SetShaderConstantsI {
                stage,
                start_register,
                vec4_count,
                data,
                ..
            } => {
                if data.is_empty() {
                    return Ok(());
                }

                let stage_base = match stage {
                    s if s == cmd::AerogpuShaderStage::Vertex as u32 => 0u64,
                    s if s == cmd::AerogpuShaderStage::Pixel as u32 => 256u64 * 16,
                    _ => {
                        return Err(AerogpuD3d9Error::Validation(format!(
                            "SET_SHADER_CONSTANTS_I: unsupported stage {stage}"
                        )));
                    }
                };

                let end_register = start_register.checked_add(vec4_count).ok_or_else(|| {
                    AerogpuD3d9Error::Validation(
                        "SET_SHADER_CONSTANTS_I: register range overflow".into(),
                    )
                })?;
                if end_register > 256 {
                    return Err(AerogpuD3d9Error::Validation(format!(
                        "SET_SHADER_CONSTANTS_I: register range out of bounds (start_register={start_register} vec4_count={vec4_count})"
                    )));
                }

                let offset = (CONSTANTS_INTS_OFFSET_BYTES + stage_base)
                    .checked_add(start_register as u64 * 16)
                    .ok_or_else(|| {
                        AerogpuD3d9Error::Validation(
                            "SET_SHADER_CONSTANTS_I: register offset overflow".into(),
                        )
                    })?;
                let end_offset = offset.checked_add(data.len() as u64).ok_or_else(|| {
                    AerogpuD3d9Error::Validation(
                        "SET_SHADER_CONSTANTS_I: data length overflow".into(),
                    )
                })?;
                if end_offset > CONSTANTS_BUFFER_SIZE_BYTES as u64 {
                    return Err(AerogpuD3d9Error::Validation(format!(
                        "SET_SHADER_CONSTANTS_I: upload out of bounds (end_offset={end_offset} buffer_size={})",
                        CONSTANTS_BUFFER_SIZE_BYTES
                    )));
                }

                self.ensure_encoder();
                let staging = self
                    .device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("aerogpu-d3d9.constants_i_staging"),
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
            AeroGpuCmd::SetShaderConstantsB {
                stage,
                start_register,
                bool_count,
                data,
                ..
            } => {
                if data.is_empty() {
                    return Ok(());
                }

                let stage_base = match stage {
                    s if s == cmd::AerogpuShaderStage::Vertex as u32 => 0u64,
                    s if s == cmd::AerogpuShaderStage::Pixel as u32 => 256u64 * 16,
                    _ => {
                        return Err(AerogpuD3d9Error::Validation(format!(
                            "SET_SHADER_CONSTANTS_B: unsupported stage {stage}"
                        )));
                    }
                };

                let end_register = start_register.checked_add(bool_count).ok_or_else(|| {
                    AerogpuD3d9Error::Validation(
                        "SET_SHADER_CONSTANTS_B: register range overflow".into(),
                    )
                })?;
                if end_register > 256 {
                    return Err(AerogpuD3d9Error::Validation(format!(
                        "SET_SHADER_CONSTANTS_B: register range out of bounds (start_register={start_register} bool_count={bool_count})"
                    )));
                }

                let offset = (CONSTANTS_BOOLS_OFFSET_BYTES + stage_base)
                    .checked_add(start_register as u64 * 16)
                    .ok_or_else(|| {
                        AerogpuD3d9Error::Validation(
                            "SET_SHADER_CONSTANTS_B: register offset overflow".into(),
                        )
                    })?;
                let end_offset = offset.checked_add(data.len() as u64).ok_or_else(|| {
                    AerogpuD3d9Error::Validation(
                        "SET_SHADER_CONSTANTS_B: data length overflow".into(),
                    )
                })?;
                if end_offset > CONSTANTS_BUFFER_SIZE_BYTES as u64 {
                    return Err(AerogpuD3d9Error::Validation(format!(
                        "SET_SHADER_CONSTANTS_B: upload out of bounds (end_offset={end_offset} buffer_size={})",
                        CONSTANTS_BUFFER_SIZE_BYTES
                    )));
                }

                self.ensure_encoder();
                let staging = self
                    .device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("aerogpu-d3d9.constants_b_staging"),
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
                let width = f32::from_bits(width_f32);
                let height = f32::from_bits(height_f32);
                self.state.viewport = Some(ViewportState {
                    x: f32::from_bits(x_f32),
                    y: f32::from_bits(y_f32),
                    width,
                    height,
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
                ..
            } => {
                let slot_idx = slot as usize;
                if slot_idx >= MAX_SAMPLERS {
                    return Ok(());
                }

                match shader_stage {
                    s if s == cmd::AerogpuShaderStage::Vertex as u32 => {
                        self.state.textures_vs[slot_idx] = texture;
                        self.samplers_bind_groups_dirty = true;
                    }
                    s if s == cmd::AerogpuShaderStage::Pixel as u32 => {
                        self.state.textures_ps[slot_idx] = texture;
                        self.samplers_bind_groups_dirty = true;
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
                self.shared_surfaces
                    .export(resource_handle, share_token)
                    .map_err(|e| match e {
                        SharedSurfaceError::TokenRetired(token) => {
                            AerogpuD3d9Error::ShareTokenRetired(token)
                        }
                        SharedSurfaceError::TokenAlreadyExported {
                            share_token,
                            existing,
                            new,
                        } => AerogpuD3d9Error::ShareTokenAlreadyExported {
                            share_token,
                            existing,
                            new,
                        },
                        SharedSurfaceError::UnknownHandle(handle) => {
                            AerogpuD3d9Error::UnknownResource(handle)
                        }
                        other => AerogpuD3d9Error::Validation(other.to_string()),
                    })?;
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
                if self.shaders.contains_key(&out_resource_handle)
                    || self.input_layouts.contains_key(&out_resource_handle)
                {
                    return Err(AerogpuD3d9Error::ResourceHandleInUse(out_resource_handle));
                }

                let Some(underlying) = self.shared_surfaces.lookup_token(share_token) else {
                    return Err(AerogpuD3d9Error::UnknownShareToken(share_token));
                };
                match self.resources.get(&underlying) {
                    Some(Resource::Texture2d { .. }) => {}
                    Some(Resource::Buffer { .. }) => {
                        return Err(AerogpuD3d9Error::Validation(format!(
                            "IMPORT_SHARED_SURFACE: token refers to a non-texture resource (share_token=0x{share_token:016X})"
                        )));
                    }
                    None => return Err(AerogpuD3d9Error::UnknownShareToken(share_token)),
                }

                let existed = self.shared_surfaces.contains_handle(out_resource_handle);
                self.shared_surfaces
                    .import(out_resource_handle, share_token)
                    .map_err(|e| match e {
                        SharedSurfaceError::UnknownToken(token) => {
                            AerogpuD3d9Error::UnknownShareToken(token)
                        }
                        SharedSurfaceError::TokenRefersToDestroyed { share_token, .. } => {
                            AerogpuD3d9Error::UnknownShareToken(share_token)
                        }
                        SharedSurfaceError::AliasAlreadyBound {
                            alias,
                            existing,
                            new,
                        } => AerogpuD3d9Error::SharedSurfaceAliasAlreadyBound {
                            alias,
                            existing,
                            new,
                        },
                        SharedSurfaceError::HandleStillInUse(handle) => {
                            AerogpuD3d9Error::ResourceHandleInUse(handle)
                        }
                        other => AerogpuD3d9Error::Validation(other.to_string()),
                    })?;

                if !existed {
                    // Bringing a previously-unknown alias handle into existence changes how
                    // `SetTexture` bindings resolve. Drop any cached bind groups (including for
                    // other contexts) so subsequent draws re-resolve handles against the updated
                    // alias table.
                    self.invalidate_bind_groups();
                }

                Ok(())
            }
            AeroGpuCmd::ReleaseSharedSurface { share_token } => {
                self.shared_surfaces.release_token(share_token);
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
            let rows_usize: usize = rows
                .try_into()
                .map_err(|_| AerogpuD3d9Error::Validation("texture rows out of range".into()))?;
            let total = dst_pitch_usize.checked_mul(rows_usize).ok_or_else(|| {
                AerogpuD3d9Error::Validation("texture upload staging size overflow".into())
            })?;
            let mut out = vec![0u8; total];

            for row in 0..rows {
                let src_off = u64::from(src_row_pitch_bytes)
                    .checked_mul(u64::from(row))
                    .ok_or_else(|| {
                        AerogpuD3d9Error::Validation("texture backing overflow".into())
                    })?;
                let src_gpa = base_gpa.checked_add(src_off).ok_or_else(|| {
                    AerogpuD3d9Error::Validation("texture backing overflow".into())
                })?;
                if src_gpa.checked_add(u64::from(row_len_bytes)).is_none() {
                    return Err(AerogpuD3d9Error::Validation(
                        "texture backing overflow".into(),
                    ));
                }

                let dst_start = (row as usize).checked_mul(dst_pitch_usize).ok_or_else(|| {
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
                let sub_size = in_layer_end.checked_sub(in_layer_off).ok_or_else(|| {
                    AerogpuD3d9Error::Validation("texture backing overflow".into())
                })?;
                let sub_start = layer_off.checked_add(in_layer_off).ok_or_else(|| {
                    AerogpuD3d9Error::Validation("texture backing overflow".into())
                })?;
                let sub_end = sub_start.checked_add(sub_size).ok_or_else(|| {
                    AerogpuD3d9Error::Validation("texture backing overflow".into())
                })?;

                // Skip untouched subresources to avoid reading uninitialized guest memory.
                if !ranges
                    .iter()
                    .any(|r| r.start < sub_end && r.end > sub_start)
                {
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
                            AerogpuD3d9Error::Validation(
                                "texture bytes_per_row out of range".into(),
                            )
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
                    let dst_unpadded_usize: usize = dst_unpadded_bpr.try_into().map_err(|_| {
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
                        let dst_start = row_usize.checked_mul(dst_bpr_usize).ok_or_else(|| {
                            AerogpuD3d9Error::Validation(
                                "texture upload staging size overflow".into(),
                            )
                        })?;
                        let dst_end =
                            dst_start.checked_add(dst_unpadded_usize).ok_or_else(|| {
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
                        if dst_is_bc {
                            align_to(mip_w, src_block.block_width)
                        } else {
                            mip_w
                        },
                        if dst_is_bc {
                            align_to(mip_h, src_block.block_height)
                        } else {
                            mip_h
                        },
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
        let (_, color_is_opaque_alpha, depth_format) = self.render_target_formats()?;
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

        // wgpu supports "gapped" render target arrays (e.g. `[None, Some(rt1)]`) when drawing, but
        // `LoadOp::Clear` on a pass with no draws does not reliably clear attachments after the
        // first gap across all backends. Work around this by clearing each non-null render target
        // in its own render pass when a gap precedes later bound RTVs.
        //
        // This matches D3D9/D3D10/11 semantics, where `Clear` affects all bound render targets,
        // including those in higher MRT slots.
        let mut seen_gap = false;
        let mut has_gap_followed_by_target = false;
        for attachment in &color_attachments {
            if attachment.is_none() {
                seen_gap = true;
                continue;
            }
            if seen_gap {
                has_gap_followed_by_target = true;
                break;
            }
        }
        if clear_color_enabled && has_gap_followed_by_target {
            for (idx, attachment) in color_attachments.into_iter().enumerate() {
                let Some(view) = attachment else {
                    continue;
                };
                let clear_color_for_rt = if color_is_opaque_alpha.get(idx).copied().unwrap_or(false)
                {
                    clear_color_opaque
                } else {
                    clear_color
                };
                let color_attachments_single = [Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(clear_color_for_rt),
                        store: wgpu::StoreOp::Store,
                    },
                })];

                let depth_attachment =
                    depth_stencil.map(|view| wgpu::RenderPassDepthStencilAttachment {
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
                    label: Some("aerogpu-d3d9.clear.gapped"),
                    color_attachments: &color_attachments_single,
                    depth_stencil_attachment: depth_attachment,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
            }
            return Ok(());
        }

        let mut color_attachments_out = Vec::with_capacity(color_attachments.len());
        for (idx, attachment) in color_attachments.into_iter().enumerate() {
            let Some(view) = attachment else {
                color_attachments_out.push(None);
                continue;
            };
            let clear_color_for_rt = if color_is_opaque_alpha.get(idx).copied().unwrap_or(false) {
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
                        let opaque_alpha = is_opaque_alpha_format(*format_raw);
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
                        targets.push((
                            underlying,
                            out_format,
                            *width,
                            *height,
                            opaque_alpha,
                            use_srgb_view,
                        ))
                    }
                    _ => return Err(AerogpuD3d9Error::UnknownResource(handle)),
                }
            }

            let mut current_opaque_alpha = false;
            for (underlying, format, width, height, opaque_alpha, use_srgb_view) in targets {
                let Some((x, y, w, h)) =
                    clamp_scissor_rect(scissor.0, scissor.1, scissor.2, scissor.3, width, height)
                else {
                    continue;
                };

                self.ensure_clear_pipeline(format);
                let pipeline = self.clear_pipeline(format);
                if opaque_alpha != current_opaque_alpha {
                    let src = if opaque_alpha {
                        &staging_opaque
                    } else {
                        &staging_normal
                    };
                    encoder.copy_buffer_to_buffer(src, 0, &self.clear_color_buffer, 0, 32);
                    current_opaque_alpha = opaque_alpha;
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
                    // Depth-only pass: avoid dummy color targets and avoid `@builtin(frag_depth)`
                    // so this stays compatible across wgpu backends (some Vulkan software stacks
                    // have been observed to crash when using frag_depth on Depth32Float).
                    color_attachments: &[],
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
                    // Prefer setting the depth value via clip-space Z rather than `@builtin(frag_depth)`.
                    // This avoids backend-specific frag_depth issues and enables depth-only pipelines.
                    entry_point: "vs_depth",
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[],
                },
                fragment: None,
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

    fn ensure_triangle_fan_index_buffer(
        &mut self,
        vertex_count: u32,
    ) -> Result<(), AerogpuD3d9Error> {
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
        let (
            vs_key,
            ps_key,
            vs_uses_semantic_locations,
            vs_semantic_locations,
            vs_used_mask,
            ps_used_mask,
            vs_sampler_dim_key,
            ps_sampler_dim_key,
        ) = {
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
            (
                vs.key,
                ps.key,
                vs.uses_semantic_locations,
                vs.semantic_locations.clone(),
                vs.used_samplers_mask,
                ps.used_samplers_mask,
                vs.sampler_dim_key,
                ps.sampler_dim_key,
            )
        };
        for slot in 0..MAX_SAMPLERS {
            let bit = 1u16 << slot;
            if (vs_used_mask & bit) != 0 {
                let tex_handle = textures_vs[slot];
                if tex_handle != 0 {
                    self.flush_texture_binding_if_dirty(Some(encoder), tex_handle, ctx)?;
                }
            }
            if (ps_used_mask & bit) != 0 {
                let tex_handle = textures_ps[slot];
                if tex_handle != 0 {
                    self.flush_texture_binding_if_dirty(Some(encoder), tex_handle, ctx)?;
                }
            }
        }
        let vs_samplers_layout_key =
            samplers_bind_group_layout_key(vs_used_mask, vs_sampler_dim_key);
        let ps_samplers_layout_key =
            samplers_bind_group_layout_key(ps_used_mask, ps_sampler_dim_key);
        self.ensure_sampler_bind_groups(vs_samplers_layout_key, ps_samplers_layout_key);
        let (color_formats, color_is_opaque_alpha, depth_format) = self.render_target_formats()?;
        let depth_has_stencil =
            matches!(depth_format, Some(wgpu::TextureFormat::Depth24PlusStencil8));
        let vertex_buffers = {
            let layout = self
                .input_layouts
                .get(&layout_handle)
                .ok_or(AerogpuD3d9Error::UnknownInputLayout(layout_handle))?;
            self.vertex_buffer_layouts(layout, vs_uses_semantic_locations, &vs_semantic_locations)?
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
                let opaque_alpha = color_is_opaque_alpha.get(idx).copied().unwrap_or(false);
                fmt.map(|format| {
                    let mut write_mask = map_color_write_mask(
                        self.state
                            .blend_state
                            .color_write_mask
                            .get(idx)
                            .copied()
                            .unwrap_or(0xF),
                    );
                    if opaque_alpha {
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
        let (pipeline_alpha_enable, pipeline_alpha_func, pipeline_alpha_ref) = if alpha_test_enable
        {
            (true, alpha_test_func, alpha_test_ref)
        } else {
            (false, 0, 0)
        };

        let pipeline_key = PipelineCacheKey {
            vs: vs_key,
            ps: ps_key,
            samplers_layout_key: sampler_layout_key(vs_samplers_layout_key, ps_samplers_layout_key),
            alpha_test_enable: pipeline_alpha_enable,
            alpha_test_func: pipeline_alpha_func,
            alpha_test_ref: pipeline_alpha_ref,
            vertex_buffers: vertex_buffer_keys,
            color_formats: color_formats.clone(),
            opaque_alpha_mask: color_is_opaque_alpha.clone(),
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

            self.ensure_pipeline_layout(vs_samplers_layout_key, ps_samplers_layout_key);
            let layout_key = sampler_layout_key(vs_samplers_layout_key, ps_samplers_layout_key);

            let pipeline = {
                let pipeline_layout = self
                    .pipeline_layouts
                    .get(&layout_key)
                    .expect("ensure_pipeline_layout should populate pipeline layout");
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
                self.device
                    .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                        label: Some("aerogpu-d3d9.pipeline"),
                        layout: Some(pipeline_layout),
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
                            stencil: if depth_has_stencil
                                && self.state.depth_stencil_state.stencil_enable
                            {
                                let cw_face = wgpu::StencilFaceState {
                                    compare: map_compare_func(
                                        self.state.depth_stencil_state.stencil_func,
                                    ),
                                    fail_op: map_stencil_op(
                                        self.state.depth_stencil_state.stencil_fail_op,
                                    ),
                                    depth_fail_op: map_stencil_op(
                                        self.state.depth_stencil_state.stencil_depth_fail_op,
                                    ),
                                    pass_op: map_stencil_op(
                                        self.state.depth_stencil_state.stencil_pass_op,
                                    ),
                                };
                                let ccw_face = wgpu::StencilFaceState {
                                    compare: map_compare_func(
                                        self.state.depth_stencil_state.ccw_stencil_func,
                                    ),
                                    fail_op: map_stencil_op(
                                        self.state.depth_stencil_state.ccw_stencil_fail_op,
                                    ),
                                    depth_fail_op: map_stencil_op(
                                        self.state.depth_stencil_state.ccw_stencil_depth_fail_op,
                                    ),
                                    pass_op: map_stencil_op(
                                        self.state.depth_stencil_state.ccw_stencil_pass_op,
                                    ),
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
                                    read_mask: self.state.depth_stencil_state.stencil_read_mask
                                        as u32,
                                    write_mask: self.state.depth_stencil_state.stencil_write_mask
                                        as u32,
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

        let constants_bind_group = &self.constants_bind_group;
        let samplers_bind_group_vs = self
            .samplers_bind_group_vs
            .as_ref()
            .expect("ensure_sampler_bind_groups initializes VS sampler bind group");
        let samplers_bind_group_ps = self
            .samplers_bind_group_ps
            .as_ref()
            .expect("ensure_sampler_bind_groups initializes PS sampler bind group");

        // When half-pixel mode is enabled, keep the viewport inverse dimensions uniform in sync
        // with the *effective* viewport used for this draw (after clamping to render target
        // bounds). This also covers the default viewport case where the guest never explicitly
        // sets one (D3D9 defaults the viewport to the full render target).
        if self.half_pixel_center {
            if let Some(buf) = self.half_pixel_uniform_buffer.as_ref() {
                let (vp_w, vp_h) = if let Some(vp) = viewport.as_ref() {
                    (vp.width, vp.height)
                } else {
                    (rt_w as f32, rt_h as f32)
                };
                if vp_w > 0.0 && vp_h > 0.0 {
                    let dims = (vp_w, vp_h);
                    if self.half_pixel_last_viewport_dims != Some(dims) {
                        let inv_w = 1.0 / vp_w;
                        let inv_h = 1.0 / vp_h;
                        let mut data = [0u8; HALF_PIXEL_UNIFORM_SIZE_BYTES];
                        data[0..4].copy_from_slice(&inv_w.to_le_bytes());
                        data[4..8].copy_from_slice(&inv_h.to_le_bytes());
                        // Remaining bytes are padding.

                        let staging =
                            self.device
                                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                                    label: Some("aerogpu-d3d9.half_pixel.viewport_staging"),
                                    contents: &data,
                                    usage: wgpu::BufferUsages::COPY_SRC,
                                });
                        encoder.copy_buffer_to_buffer(
                            &staging,
                            0,
                            buf,
                            0,
                            HALF_PIXEL_UNIFORM_SIZE_BYTES as u64,
                        );
                        self.half_pixel_last_viewport_dims = Some(dims);
                    }
                }
            }
        }

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

        // wgpu/WebGPU vertex fetch is robust and does not allow reading before the bound vertex
        // buffer offset, even if the final address would still be within the underlying buffer.
        //
        // D3D9 allows this pattern via a negative `base_vertex` combined with a positive stream
        // offset (i.e., indices can reference vertices "before" the stream offset). To preserve
        // D3D9 semantics we compute a per-draw vertex index bias that shifts all vertex indices to
        // be non-negative and adjust the bound vertex buffer offsets accordingly.
        let (draw, swizzle_vertex_start, swizzle_vertex_end, vertex_index_bias) = match draw {
            DrawParams::NonIndexed {
                vertex_count,
                first_vertex,
                ..
            } => (
                draw,
                first_vertex,
                first_vertex.saturating_add(vertex_count),
                0i64,
            ),
            DrawParams::Indexed {
                index_count,
                first_index,
                base_vertex,
                instance_count,
                first_instance,
            } => {
                if base_vertex >= 0 && d3dcolor_offsets_by_stream.is_empty() {
                    // Fast path: no D3DCOLOR conversion and non-negative base_vertex means all vertex
                    // indices are >= 0 and within the bound vertex buffer slice, so no per-draw index
                    // scanning is needed.
                    (draw, 0, 0, 0)
                } else {
                    let index_binding =
                        index_binding.ok_or(AerogpuD3d9Error::MissingIndexBuffer)?;
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
                        (draw, 0, 0, 0)
                    } else {
                        let base = base_vertex as i64;
                        let min_v_i64 = base.saturating_add(min_index as i64);
                        let max_v_excl_i64 =
                            base.saturating_add(max_index as i64).saturating_add(1);

                        // Bias vertex indices so the minimum becomes >= 0.
                        let bias_i64 = if min_v_i64 < 0 {
                            min_v_i64.saturating_abs()
                        } else {
                            0
                        };

                        let effective_base_vertex_i64 = base.saturating_add(bias_i64);
                        let effective_base_vertex: i32 =
                            effective_base_vertex_i64.try_into().map_err(|_| {
                                AerogpuD3d9Error::Validation(
                                    "indexed draw base_vertex out of range after bias".into(),
                                )
                            })?;

                        let clamp_u32 = |v: i64| -> u32 {
                            if v <= 0 {
                                0
                            } else if v >= u32::MAX as i64 {
                                u32::MAX
                            } else {
                                v as u32
                            }
                        };
                        let swizzle_start = clamp_u32(min_v_i64.saturating_add(bias_i64));
                        let swizzle_end = clamp_u32(max_v_excl_i64.saturating_add(bias_i64));

                        (
                            DrawParams::Indexed {
                                index_count,
                                instance_count,
                                first_index,
                                base_vertex: effective_base_vertex,
                                first_instance,
                            },
                            swizzle_start,
                            swizzle_end,
                            bias_i64,
                        )
                    }
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
                let base_offset = if vertex_index_bias != 0 {
                    let bias_u64 = u64::try_from(vertex_index_bias).map_err(|_| {
                        AerogpuD3d9Error::Validation("vertex index bias out of range".into())
                    })?;
                    let delta_bytes = bias_u64.checked_mul(stride).ok_or_else(|| {
                        AerogpuD3d9Error::Validation("vertex index bias overflow".into())
                    })?;
                    (binding.offset_bytes as u64).checked_sub(delta_bytes).ok_or_else(|| {
                        AerogpuD3d9Error::Validation(format!(
                            "vertex buffer offset underflow after base vertex bias (handle={} offset_bytes=0x{:x} bias_vertices={bias_u64} stride=0x{:x})",
                            binding.buffer, binding.offset_bytes, stride
                        ))
                    })?
                } else {
                    binding.offset_bytes as u64
                };
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
                        binding.buffer,
                        end_usize,
                        shadow.len()
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
        pass.set_bind_group(0, constants_bind_group, &[]);
        pass.set_bind_group(1, samplers_bind_group_vs, &[]);
        pass.set_bind_group(2, samplers_bind_group_ps, &[]);
        if let Some(half_pixel_bg) = self.half_pixel_bind_group.as_ref() {
            pass.set_bind_group(3, half_pixel_bg, &[]);
        }

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
                let stride = binding.stride_bytes as u64;
                let vb_offset = if vertex_index_bias != 0 {
                    let bias_u64 = u64::try_from(vertex_index_bias).map_err(|_| {
                        AerogpuD3d9Error::Validation("vertex index bias out of range".into())
                    })?;
                    let delta_bytes = bias_u64.checked_mul(stride).ok_or_else(|| {
                        AerogpuD3d9Error::Validation("vertex index bias overflow".into())
                    })?;
                    (binding.offset_bytes as u64).checked_sub(delta_bytes).ok_or_else(|| {
                        AerogpuD3d9Error::Validation(format!(
                            "vertex buffer offset underflow after base vertex bias (handle={} offset_bytes=0x{:x} bias_vertices={bias_u64} stride=0x{:x})",
                            binding.buffer, binding.offset_bytes, stride
                        ))
                    })?
                } else {
                    binding.offset_bytes as u64
                };
                pass.set_vertex_buffer(wgpu_slot, buffer.slice(vb_offset..));
            }
        }

        match draw {
            DrawParams::NonIndexed {
                vertex_count,
                instance_count,
                first_vertex,
                first_instance,
            } => {
                if let (
                    Some((_vertex_count, index_count, base_vertex, first_instance, instance_count)),
                    Some(fan_index),
                ) = (triangle_fan_nonindexed_plan, triangle_fan_index_buffer)
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
        let module = Arc::new(
            self.device
                .create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("aerogpu-d3d9.shader.alpha_test"),
                    source: wgpu::ShaderSource::Wgsl(wgsl.into()),
                }),
        );
        self.alpha_test_pixel_shaders.insert(key, module.clone());
        Ok(module)
    }

    fn ensure_pipeline_layout(&mut self, vs_layout_key: u64, ps_layout_key: u64) {
        if !self
            .samplers_bind_group_layouts_vs
            .contains_key(&vs_layout_key)
        {
            let used_samplers_mask = (vs_layout_key >> 32) as u16;
            let sampler_dim_key = vs_layout_key as u32;
            let layout = create_samplers_bind_group_layout(
                &self.device,
                wgpu::ShaderStages::VERTEX,
                used_samplers_mask,
                sampler_dim_key,
            );
            self.samplers_bind_group_layouts_vs
                .insert(vs_layout_key, layout);
        }
        if !self
            .samplers_bind_group_layouts_ps
            .contains_key(&ps_layout_key)
        {
            let used_samplers_mask = (ps_layout_key >> 32) as u16;
            let sampler_dim_key = ps_layout_key as u32;
            let layout = create_samplers_bind_group_layout(
                &self.device,
                wgpu::ShaderStages::FRAGMENT,
                used_samplers_mask,
                sampler_dim_key,
            );
            self.samplers_bind_group_layouts_ps
                .insert(ps_layout_key, layout);
        }

        let key = sampler_layout_key(vs_layout_key, ps_layout_key);
        if self.pipeline_layouts.contains_key(&key) {
            return;
        }

        let pipeline_layout = {
            let vs_bgl = self
                .samplers_bind_group_layouts_vs
                .get(&vs_layout_key)
                .expect("VS sampler bind group layout should be present");
            let ps_bgl = self
                .samplers_bind_group_layouts_ps
                .get(&ps_layout_key)
                .expect("PS sampler bind group layout should be present");

            if let Some(half_pixel_layout) = self.half_pixel_bind_group_layout.as_ref() {
                self.device
                    .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                        label: Some("aerogpu-d3d9.pipeline_layout"),
                        bind_group_layouts: &[
                            &self.constants_bind_group_layout,
                            vs_bgl,
                            ps_bgl,
                            half_pixel_layout,
                        ],
                        push_constant_ranges: &[],
                    })
            } else {
                self.device
                    .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                        label: Some("aerogpu-d3d9.pipeline_layout"),
                        bind_group_layouts: &[&self.constants_bind_group_layout, vs_bgl, ps_bgl],
                        push_constant_ranges: &[],
                    })
            }
        };
        self.pipeline_layouts.insert(key, pipeline_layout);
    }

    fn ensure_sampler_bind_groups(&mut self, vs_layout_key: u64, ps_layout_key: u64) {
        // If the shader's sampler requirements changed (used slots and/or texture type), we must
        // recreate the bind groups with a matching bind group layout.
        if self.samplers_bind_group_key_vs != vs_layout_key {
            self.samplers_bind_group_vs = None;
            self.samplers_bind_groups_dirty = true;
            self.samplers_bind_group_key_vs = vs_layout_key;
        }
        if self.samplers_bind_group_key_ps != ps_layout_key {
            self.samplers_bind_group_ps = None;
            self.samplers_bind_groups_dirty = true;
            self.samplers_bind_group_key_ps = ps_layout_key;
        }

        if !self.samplers_bind_groups_dirty
            && self.samplers_bind_group_vs.is_some()
            && self.samplers_bind_group_ps.is_some()
        {
            return;
        }

        // D3D9 has separate sampler namespaces for vertex and pixel shaders. Model this with
        // separate bind groups:
        // - group(1): VS textures/samplers
        // - group(2): PS textures/samplers
        //
        // Binding numbers are derived from sampler register index:
        // - texture binding = 2*s
        // - sampler binding = 2*s + 1
        let srgb_enabled = |states: &Vec<u32>| -> bool {
            states
                .get(d3d9::D3DSAMP_SRGBTEXTURE as usize)
                .copied()
                .unwrap_or(0)
                != 0
        };

        self.ensure_pipeline_layout(vs_layout_key, ps_layout_key);

        let vs_used_mask = (vs_layout_key >> 32) as u16;
        let vs_sampler_dim_key = vs_layout_key as u32;
        let ps_used_mask = (ps_layout_key >> 32) as u16;
        let ps_sampler_dim_key = ps_layout_key as u32;

        let samplers_bind_group_vs = {
            let layout = self
                .samplers_bind_group_layouts_vs
                .get(&vs_layout_key)
                .expect("layout was inserted above");

            let mut entries: Vec<wgpu::BindGroupEntry> =
                Vec::with_capacity(vs_used_mask.count_ones() as usize * 2);
            for slot in 0..MAX_SAMPLERS {
                if (vs_used_mask & (1u16 << slot)) == 0 {
                    continue;
                }
                let tex_binding = slot as u32 * 2;
                let samp_binding = tex_binding + 1;
                let tex_handle = self.state.textures_vs[slot];
                let srgb_texture = srgb_enabled(&self.state.sampler_states_vs[slot]);
                let sampler = self.samplers_vs[slot].as_ref();
                let dim_code = (vs_sampler_dim_key >> (slot as u32 * 2)) & 0b11;

                let view: &wgpu::TextureView = match dim_code {
                    2 => &self.dummy_3d_texture_view,
                    3 => &self.dummy_1d_texture_view,
                    _ => {
                        let requires_cube = dim_code == 1;
                        if tex_handle == 0 {
                            if requires_cube {
                                &self.dummy_cube_texture_view
                            } else {
                                &self.dummy_texture_view
                            }
                        } else {
                            let underlying = self.resolve_resource_handle(tex_handle).ok();
                            match underlying.and_then(|h| self.resources.get(&h)) {
                                Some(Resource::Texture2d {
                                    view,
                                    view_srgb,
                                    view_cube,
                                    view_cube_srgb,
                                    array_layers,
                                    ..
                                }) => {
                                    if requires_cube {
                                        if srgb_texture {
                                            view_cube_srgb
                                                .as_ref()
                                                .or(view_cube.as_ref())
                                                .unwrap_or(&self.dummy_cube_texture_view)
                                        } else {
                                            view_cube
                                                .as_ref()
                                                .unwrap_or(&self.dummy_cube_texture_view)
                                        }
                                    } else if *array_layers != 1 {
                                        // A cube texture's default view is `D2Array`, which cannot be
                                        // bound to a `texture_2d<f32>` binding. Treat mismatched bindings
                                        // as unbound.
                                        &self.dummy_texture_view
                                    } else if srgb_texture {
                                        view_srgb.as_ref().unwrap_or(view)
                                    } else {
                                        view
                                    }
                                }
                                _ => {
                                    if requires_cube {
                                        &self.dummy_cube_texture_view
                                    } else {
                                        &self.dummy_texture_view
                                    }
                                }
                            }
                        }
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
                label: Some("aerogpu-d3d9.samplers_bind_group_vs"),
                layout,
                entries: &entries,
            })
        };

        let samplers_bind_group_ps = {
            let layout = self
                .samplers_bind_group_layouts_ps
                .get(&ps_layout_key)
                .expect("layout was inserted above");

            let mut entries: Vec<wgpu::BindGroupEntry> =
                Vec::with_capacity(ps_used_mask.count_ones() as usize * 2);
            for slot in 0..MAX_SAMPLERS {
                if (ps_used_mask & (1u16 << slot)) == 0 {
                    continue;
                }
                let tex_binding = slot as u32 * 2;
                let samp_binding = tex_binding + 1;
                let tex_handle = self.state.textures_ps[slot];
                let srgb_texture = srgb_enabled(&self.state.sampler_states_ps[slot]);
                let sampler = self.samplers_ps[slot].as_ref();
                let dim_code = (ps_sampler_dim_key >> (slot as u32 * 2)) & 0b11;

                let view: &wgpu::TextureView = match dim_code {
                    2 => &self.dummy_3d_texture_view,
                    3 => &self.dummy_1d_texture_view,
                    _ => {
                        let requires_cube = dim_code == 1;
                        if tex_handle == 0 {
                            if requires_cube {
                                &self.dummy_cube_texture_view
                            } else {
                                &self.dummy_texture_view
                            }
                        } else {
                            let underlying = self.resolve_resource_handle(tex_handle).ok();
                            match underlying.and_then(|h| self.resources.get(&h)) {
                                Some(Resource::Texture2d {
                                    view,
                                    view_srgb,
                                    view_cube,
                                    view_cube_srgb,
                                    array_layers,
                                    ..
                                }) => {
                                    if requires_cube {
                                        if srgb_texture {
                                            view_cube_srgb
                                                .as_ref()
                                                .or(view_cube.as_ref())
                                                .unwrap_or(&self.dummy_cube_texture_view)
                                        } else {
                                            view_cube
                                                .as_ref()
                                                .unwrap_or(&self.dummy_cube_texture_view)
                                        }
                                    } else if *array_layers != 1 {
                                        &self.dummy_texture_view
                                    } else if srgb_texture {
                                        view_srgb.as_ref().unwrap_or(view)
                                    } else {
                                        view
                                    }
                                }
                                _ => {
                                    if requires_cube {
                                        &self.dummy_cube_texture_view
                                    } else {
                                        &self.dummy_texture_view
                                    }
                                }
                            }
                        }
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
                label: Some("aerogpu-d3d9.samplers_bind_group_ps"),
                layout,
                entries: &entries,
            })
        };

        self.samplers_bind_group_vs = Some(samplers_bind_group_vs);
        self.samplers_bind_group_ps = Some(samplers_bind_group_ps);
        self.samplers_bind_groups_dirty = false;
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

        let sampler = Arc::new(create_wgpu_sampler(
            &self.device,
            self.downlevel_flags,
            &state,
        ));
        self.sampler_cache.insert(state, sampler.clone());
        sampler
    }

    fn canonicalize_sampler_state(&self, state: D3d9SamplerState) -> D3d9SamplerState {
        Self::canonicalize_sampler_state_for_caps(
            self.device.features(),
            self.downlevel_flags,
            state,
        )
    }

    fn canonicalize_sampler_state_for_caps(
        device_features: wgpu::Features,
        downlevel_flags: wgpu::DownlevelFlags,
        mut state: D3d9SamplerState,
    ) -> D3d9SamplerState {
        let border_supported =
            device_features.contains(wgpu::Features::ADDRESS_MODE_CLAMP_TO_BORDER);

        let mut uses_border = false;
        for addr in [
            &mut state.address_u,
            &mut state.address_v,
            &mut state.address_w,
        ] {
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
            downlevel_flags.contains(wgpu::DownlevelFlags::ANISOTROPIC_FILTERING);
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
                slot, state_id, value, "ignoring sampler state with unknown shader stage"
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
            self.samplers_bind_groups_dirty = true;
        }
    }

    fn vertex_buffer_layouts(
        &self,
        input_layout: &InputLayout,
        uses_semantic_locations: bool,
        semantic_locations: &[shader::SemanticLocation],
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
        let semantic_location_map = semantic_locations
            .iter()
            .map(|s| ((s.usage, s.usage_index), s.location))
            .collect::<HashMap<_, _>>();
        let mut seen_locations = HashMap::<u32, (aero_d3d9::vertex::DeclUsage, u8)>::new();

        // Map declaration elements to shader locations.
        for (i, e) in input_layout.decl.elements.iter().enumerate() {
            let Some(&slot) = stream_to_slot.get(&e.stream) else {
                continue;
            };
            let fmt = map_decl_type_to_vertex_format(e.ty)?;
            let shader_location = if uses_semantic_locations {
                if !semantic_locations.is_empty() {
                    // Prefer the semantic mapping produced by shader translation.
                    let Some(&loc) = semantic_location_map.get(&(e.usage, e.usage_index)) else {
                        // Element not declared by the shader; ignore it to avoid unnecessary
                        // `maxVertexAttributes` pressure and to tolerate declarations that contain
                        // unused semantics.
                        continue;
                    };
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
                    // Legacy fallback for cached shaders that don't provide semantic location
                    // metadata. Use the standard mapping, but skip unsupported semantics (which are
                    // necessarily unused by the cached shader, otherwise translation would have
                    // failed).
                    let Ok(loc) = location_map.location_for(e.usage, e.usage_index) else {
                        continue;
                    };
                    if let Some((prev_usage, prev_index)) =
                        seen_locations.insert(loc, (e.usage, e.usage_index))
                    {
                        return Err(AerogpuD3d9Error::VertexDeclaration(format!(
                            "vertex declaration maps multiple elements to WGSL @location({loc}): {prev_usage:?}{prev_index} and {:?}{}",
                            e.usage, e.usage_index
                        )));
                    }
                    loc
                }
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

    fn render_target_formats(&self) -> Result<RenderTargetFormats, AerogpuD3d9Error> {
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
        let mut color_is_opaque_alpha = Vec::new();
        for slot in 0..rt.color_count.min(8) as usize {
            let handle = rt.colors[slot];
            if handle == 0 {
                colors.push(None);
                color_is_opaque_alpha.push(false);
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
                    color_is_opaque_alpha.push(is_opaque_alpha_format(*format_raw));
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

        Ok((colors, color_is_opaque_alpha, depth))
    }
}

type RenderTargetFormats = (
    Vec<Option<wgpu::TextureFormat>>,
    Vec<bool>,
    Option<wgpu::TextureFormat>,
);

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

fn bc_copy_to_buffer_supported(device: &wgpu::Device, queue: &wgpu::Queue) -> bool {
    if !device
        .features()
        .contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
    {
        return false;
    }

    // On wasm32, buffer mapping completion is delivered via the JS event loop, so we avoid doing
    // a synchronous probe at construction time. The D3D9 executor already requires async execution
    // for WRITEBACK_DST on wasm.
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (device, queue);
        false
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        // Use a single BC1 4x4 block with non-zero bytes so a broken copy path doesn't appear to
        // succeed accidentally.
        let bc1_block: [u8; 8] = [0x00, 0xF8, 0x00, 0xF8, 0x00, 0x00, 0x00, 0x00];

        let bytes_per_row = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let mut upload = vec![0u8; bytes_per_row as usize];
        upload[..bc1_block.len()].copy_from_slice(&bc1_block);

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("aerogpu-d3d9.bc_copy_to_buffer_probe_tex"),
            size: wgpu::Extent3d {
                width: 4,
                height: 4,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Bc1RgbaUnorm,
            usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let upload_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("aerogpu-d3d9.bc_copy_to_buffer_probe_upload"),
            contents: &upload,
            usage: wgpu::BufferUsages::COPY_SRC,
        });
        let readback_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aerogpu-d3d9.bc_copy_to_buffer_probe_readback"),
            size: bytes_per_row as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("aerogpu-d3d9.bc_copy_to_buffer_probe_encoder"),
        });
        encoder.copy_buffer_to_texture(
            wgpu::ImageCopyBuffer {
                buffer: &upload_buf,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(bytes_per_row),
                    rows_per_image: Some(1),
                },
            },
            wgpu::ImageCopyTexture {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: 4,
                height: 4,
                depth_or_array_layers: 1,
            },
        );
        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &readback_buf,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(bytes_per_row),
                    rows_per_image: Some(1),
                },
            },
            wgpu::Extent3d {
                width: 4,
                height: 4,
                depth_or_array_layers: 1,
            },
        );

        queue.submit([encoder.finish()]);

        let slice = readback_buf.slice(..);
        let (sender, receiver) = mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |res| {
            let _ = sender.send(res);
        });
        device.poll(wgpu::Maintain::Wait);

        let map_res = match receiver.recv() {
            Ok(res) => res,
            Err(_) => return false,
        };
        if map_res.is_err() {
            return false;
        }

        let mapped = slice.get_mapped_range();
        let ok = mapped
            .get(..bc1_block.len())
            .is_some_and(|s| s == bc1_block.as_slice());
        drop(mapped);
        readback_buf.unmap();
        ok
    }
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

#[derive(Debug, Clone, Copy)]
struct CopyRegion2d {
    origin_x: u32,
    origin_y: u32,
    width: u32,
    height: u32,
    mip_width: u32,
    mip_height: u32,
}

fn validate_copy_region_alignment(
    block: TexelBlockInfo,
    region: CopyRegion2d,
    label: &str,
) -> Result<(), AerogpuD3d9Error> {
    // Uncompressed formats have no special alignment constraints.
    if block.block_width == 1 && block.block_height == 1 {
        return Ok(());
    }

    let CopyRegion2d {
        origin_x,
        origin_y,
        width,
        height,
        mip_width,
        mip_height,
    } = region;

    // Block-compressed copies must use block-aligned origins, and block-aligned extents unless the
    // region reaches the edge of the mip.
    if !origin_x.is_multiple_of(block.block_width) || !origin_y.is_multiple_of(block.block_height) {
        return Err(AerogpuD3d9Error::Validation(format!(
            "{label}: BC copy origin must be {}x{}-aligned (origin=({origin_x},{origin_y}))",
            block.block_width, block.block_height
        )));
    }

    let end_x = origin_x
        .checked_add(width)
        .ok_or_else(|| AerogpuD3d9Error::Validation(format!("{label}: copy region overflow")))?;
    let end_y = origin_y
        .checked_add(height)
        .ok_or_else(|| AerogpuD3d9Error::Validation(format!("{label}: copy region overflow")))?;

    if !width.is_multiple_of(block.block_width) && end_x != mip_width {
        return Err(AerogpuD3d9Error::Validation(format!(
            "{label}: BC copy width must be a multiple of {} unless it reaches the mip edge (origin_x={origin_x} width={width} mip_width={mip_width})",
            block.block_width
        )));
    }

    if !height.is_multiple_of(block.block_height) && end_y != mip_height {
        return Err(AerogpuD3d9Error::Validation(format!(
            "{label}: BC copy height must be a multiple of {} unless it reaches the mip edge (origin_y={origin_y} height={height} mip_height={mip_height})",
            block.block_height
        )));
    }

    Ok(())
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
        x if x == AerogpuFormat::B5G6R5Unorm as u32 || x == AerogpuFormat::B5G5R5A1Unorm as u32 => {
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
        x if x == AerogpuFormat::BC1RgbaUnorm as u32
            || x == AerogpuFormat::BC1RgbaUnormSrgb as u32 =>
        {
            Some(BcFormat::Bc1)
        }
        x if x == AerogpuFormat::BC2RgbaUnorm as u32
            || x == AerogpuFormat::BC2RgbaUnormSrgb as u32 =>
        {
            Some(BcFormat::Bc2)
        }
        x if x == AerogpuFormat::BC3RgbaUnorm as u32
            || x == AerogpuFormat::BC3RgbaUnormSrgb as u32 =>
        {
            Some(BcFormat::Bc3)
        }
        x if x == AerogpuFormat::BC7RgbaUnorm as u32
            || x == AerogpuFormat::BC7RgbaUnormSrgb as u32 =>
        {
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

fn is_opaque_alpha_format(format_raw: u32) -> bool {
    // Formats with no alpha channel must behave as if alpha is always 1.0:
    // - writes to alpha are ignored
    // - reads observe opaque alpha
    //
    // Note: the executor stores some of these as RGBA8/BGRA8 because wgpu doesn't expose the
    // underlying packed formats (e.g. RGB565). We therefore need to explicitly enforce the
    // "opaque alpha" semantics in pipeline setup.
    is_x8_format(format_raw) || format_raw == AerogpuFormat::B5G6R5Unorm as u32
}

fn force_opaque_alpha_rgba8(pixels: &mut [u8]) {
    for alpha in pixels.iter_mut().skip(3).step_by(4) {
        *alpha = 0xFF;
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

pub(crate) fn map_aerogpu_format(format: u32) -> Result<wgpu::TextureFormat, AerogpuD3d9Error> {
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
        x if x == AerogpuFormat::B5G6R5Unorm as u32 || x == AerogpuFormat::B5G5R5A1Unorm as u32 => {
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

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    #[cfg(not(target_os = "linux"))]
    use super::D3d9SamplerState;
    use super::{
        build_alpha_test_wgsl_variant, cmd, d3d9, guest_texture_linear_layout, AerogpuD3d9Error,
        AerogpuD3d9Executor, AerogpuFormat,
    };

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct BlockExpectation {
        block_width: u32,
        block_height: u32,
        bytes_per_block: u32,
        is_bc: bool,
    }

    macro_rules! format_block_expectations {
        ($($variant:ident => $expectation:expr,)+) => {
            // Keep the list of protocol formats and the expectation table in sync by generating
            // both from the same source of truth.
            //
            // The match is intentionally exhaustive (no `_ => ...`), so adding a new protocol enum
            // variant forces this test to be updated.
            const ALL_PROTOCOL_FORMATS: &[AerogpuFormat] = &[
                $(AerogpuFormat::$variant,)+
            ];

            fn expected_block(format: AerogpuFormat) -> Option<BlockExpectation> {
                match format {
                    $(AerogpuFormat::$variant => $expectation,)+
                }
            }
        };
    }

    format_block_expectations! {
        Invalid => None,

        B8G8R8A8Unorm => Some(BlockExpectation { block_width: 1, block_height: 1, bytes_per_block: 4, is_bc: false }),
        B8G8R8X8Unorm => Some(BlockExpectation { block_width: 1, block_height: 1, bytes_per_block: 4, is_bc: false }),
        R8G8B8A8Unorm => Some(BlockExpectation { block_width: 1, block_height: 1, bytes_per_block: 4, is_bc: false }),
        R8G8B8X8Unorm => Some(BlockExpectation { block_width: 1, block_height: 1, bytes_per_block: 4, is_bc: false }),

        B5G6R5Unorm => Some(BlockExpectation { block_width: 1, block_height: 1, bytes_per_block: 2, is_bc: false }),
        B5G5R5A1Unorm => Some(BlockExpectation { block_width: 1, block_height: 1, bytes_per_block: 2, is_bc: false }),

        B8G8R8A8UnormSrgb => Some(BlockExpectation { block_width: 1, block_height: 1, bytes_per_block: 4, is_bc: false }),
        B8G8R8X8UnormSrgb => Some(BlockExpectation { block_width: 1, block_height: 1, bytes_per_block: 4, is_bc: false }),
        R8G8B8A8UnormSrgb => Some(BlockExpectation { block_width: 1, block_height: 1, bytes_per_block: 4, is_bc: false }),
        R8G8B8X8UnormSrgb => Some(BlockExpectation { block_width: 1, block_height: 1, bytes_per_block: 4, is_bc: false }),

        D24UnormS8Uint => Some(BlockExpectation { block_width: 1, block_height: 1, bytes_per_block: 4, is_bc: false }),
        D32Float => Some(BlockExpectation { block_width: 1, block_height: 1, bytes_per_block: 4, is_bc: false }),

        BC1RgbaUnorm => Some(BlockExpectation { block_width: 4, block_height: 4, bytes_per_block: 8, is_bc: true }),
        BC1RgbaUnormSrgb => Some(BlockExpectation { block_width: 4, block_height: 4, bytes_per_block: 8, is_bc: true }),
        BC2RgbaUnorm => Some(BlockExpectation { block_width: 4, block_height: 4, bytes_per_block: 16, is_bc: true }),
        BC2RgbaUnormSrgb => Some(BlockExpectation { block_width: 4, block_height: 4, bytes_per_block: 16, is_bc: true }),
        BC3RgbaUnorm => Some(BlockExpectation { block_width: 4, block_height: 4, bytes_per_block: 16, is_bc: true }),
        BC3RgbaUnormSrgb => Some(BlockExpectation { block_width: 4, block_height: 4, bytes_per_block: 16, is_bc: true }),
        BC7RgbaUnorm => Some(BlockExpectation { block_width: 4, block_height: 4, bytes_per_block: 16, is_bc: true }),
        BC7RgbaUnormSrgb => Some(BlockExpectation { block_width: 4, block_height: 4, bytes_per_block: 16, is_bc: true }),
    }

    #[test]
    fn aerogpu_format_texel_block_info_covers_all_protocol_formats() {
        for &format in ALL_PROTOCOL_FORMATS {
            let got = super::aerogpu_format_texel_block_info(format as u32);
            match expected_block(format) {
                Some(exp) => {
                    let info = got.unwrap_or_else(|err| {
                        panic!(
                            "aerogpu_format_texel_block_info should accept {format:?} ({}), got error: {err:?}",
                            format as u32
                        )
                    });
                    assert_eq!(info.block_width, exp.block_width, "format={format:?}");
                    assert_eq!(info.block_height, exp.block_height, "format={format:?}");
                    assert_eq!(
                        info.bytes_per_block, exp.bytes_per_block,
                        "format={format:?}"
                    );
                }
                None => {
                    assert!(
                        matches!(got, Err(AerogpuD3d9Error::UnsupportedFormat(_))),
                        "expected UnsupportedFormat for {format:?}, got {got:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn aerogpu_format_bc_detection_matches_formats() {
        for &format in ALL_PROTOCOL_FORMATS {
            let got = super::aerogpu_format_bc(format as u32);
            let expected = expected_block(format).map(|e| e.is_bc).unwrap_or(false);
            assert_eq!(got.is_some(), expected, "format={format:?}");
        }
    }

    fn assert_naga_valid_wgsl(wgsl: &str) {
        let module = naga::front::wgsl::parse_str(wgsl).expect("injected WGSL should parse");
        let mut validator = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        validator
            .validate(&module)
            .expect("injected WGSL should validate");
    }

    #[test]
    fn alpha_test_wgsl_injection_supports_sm3_fsin_fsout() {
        let base = concat!(
            "struct FsIn { @location(0) v0: vec4<f32>, };\n",
            "struct FsOut { @location(0) oC0: vec4<f32>, };\n",
            "\n",
            "@fragment\n",
            "fn fs_main(input: FsIn) -> FsOut {\n",
            "  var out: FsOut;\n",
            "  out.oC0 = vec4<f32>(1.0, 0.0, 0.0, 0.25);\n",
            "  return out;\n",
            "}\n",
        );

        let injected = build_alpha_test_wgsl_variant(base, 5, 128).expect("injection should work");
        assert!(injected.contains("fn fs_main_inner(input: FsIn) -> FsOut {"));
        assert!(injected.contains("@fragment\nfn fs_main(input: FsIn) -> FsOut {"));
        assert!(injected.contains("discard;"));
        assert_naga_valid_wgsl(&injected);
    }

    #[test]
    fn alpha_test_wgsl_injection_supports_sm3_no_input() {
        let base = concat!(
            "struct FsOut { @location(0) oC0: vec4<f32>, };\n",
            "\n",
            "@fragment\n",
            "fn fs_main() -> FsOut {\n",
            "  var out: FsOut;\n",
            "  out.oC0 = vec4<f32>(0.0, 1.0, 0.0, 0.75);\n",
            "  return out;\n",
            "}\n",
        );

        let injected = build_alpha_test_wgsl_variant(base, 5, 128).expect("injection should work");
        assert!(injected.contains("fn fs_main_inner() -> FsOut {"));
        assert!(injected.contains("@fragment\nfn fs_main() -> FsOut {"));
        assert!(injected.contains("discard;"));
        assert_naga_valid_wgsl(&injected);
    }

    #[test]
    fn alpha_test_wgsl_injection_still_supports_legacy_psinput_psoutput() {
        let base = concat!(
            "struct PsInput { @location(0) v0: vec4<f32>, };\n",
            "struct PsOutput { @location(0) oC0: vec4<f32>, };\n",
            "\n",
            "@fragment\n",
            "fn fs_main(input: PsInput) -> PsOutput {\n",
            "  var out: PsOutput;\n",
            "  out.oC0 = vec4<f32>(0.0, 0.0, 1.0, 0.25);\n",
            "  return out;\n",
            "}\n",
        );

        let injected = build_alpha_test_wgsl_variant(base, 5, 128).expect("injection should work");
        assert!(injected.contains("fn fs_main_inner(input: PsInput) -> PsOutput {"));
        assert!(injected.contains("@fragment\nfn fs_main(input: PsInput) -> PsOutput {"));
        assert!(injected.contains("discard;"));
        assert_naga_valid_wgsl(&injected);
    }

    #[test]
    fn is_x8_format_includes_srgb_variants() {
        assert!(super::is_x8_format(AerogpuFormat::B8G8R8X8Unorm as u32));
        assert!(super::is_x8_format(AerogpuFormat::R8G8B8X8Unorm as u32));
        assert!(super::is_x8_format(AerogpuFormat::B8G8R8X8UnormSrgb as u32));
        assert!(super::is_x8_format(AerogpuFormat::R8G8B8X8UnormSrgb as u32));

        assert!(!super::is_x8_format(
            AerogpuFormat::B8G8R8A8UnormSrgb as u32
        ));
        assert!(!super::is_x8_format(
            AerogpuFormat::R8G8B8A8UnormSrgb as u32
        ));
    }

    #[test]
    fn opaque_alpha_formats_include_x8_and_b5g6r5() {
        assert!(super::is_opaque_alpha_format(
            AerogpuFormat::B8G8R8X8Unorm as u32
        ));
        assert!(super::is_opaque_alpha_format(
            AerogpuFormat::R8G8B8X8UnormSrgb as u32
        ));
        assert!(super::is_opaque_alpha_format(
            AerogpuFormat::B5G6R5Unorm as u32
        ));

        assert!(!super::is_opaque_alpha_format(
            AerogpuFormat::B8G8R8A8Unorm as u32
        ));
        assert!(!super::is_opaque_alpha_format(
            AerogpuFormat::B5G5R5A1Unorm as u32
        ));
    }

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
        let layout =
            guest_texture_linear_layout(AerogpuFormat::R8G8B8A8Unorm as u32, 4, 4, 2, 1, 16)
                .expect("layout");
        assert_eq!(layout.mip_offsets, vec![0, 64]);
        assert_eq!(layout.layer_stride_bytes, 80);
        assert_eq!(layout.total_size_bytes, 80);
    }

    #[test]
    #[cfg(all(not(target_arch = "wasm32"), not(target_os = "linux")))]
    fn d3d9_sampler_maxanisotropy_affects_sampler_only_when_anisotropic_supported() {
        // This logic is purely about sampler canonicalization + caching keys; avoid creating a real
        // wgpu device here so we don't depend on system GPU backends during unit tests.
        let device_features = wgpu::Features::empty();
        let no_aniso = wgpu::DownlevelFlags::empty();
        let aniso_supported = wgpu::DownlevelFlags::ANISOTROPIC_FILTERING;

        // MAXANISOTROPY should have no effect unless anisotropic filtering is actually requested.
        let non_aniso = super::D3d9SamplerState {
            max_anisotropy: 16,
            ..Default::default()
        };
        let default_key = AerogpuD3d9Executor::canonicalize_sampler_state_for_caps(
            device_features,
            no_aniso,
            super::D3d9SamplerState::default(),
        );
        let non_aniso_key = AerogpuD3d9Executor::canonicalize_sampler_state_for_caps(
            device_features,
            no_aniso,
            non_aniso,
        );
        assert_eq!(
            default_key, non_aniso_key,
            "MAXANISOTROPY should not affect the canonical sampler state when anisotropic filtering is not requested"
        );

        let aniso_2 = super::D3d9SamplerState {
            min_filter: d3d9::D3DTEXF_ANISOTROPIC,
            mag_filter: d3d9::D3DTEXF_ANISOTROPIC,
            mip_filter: d3d9::D3DTEXF_LINEAR,
            max_anisotropy: 2,
            ..Default::default()
        };

        let aniso_16 = super::D3d9SamplerState {
            max_anisotropy: 16,
            ..aniso_2
        };

        // When anisotropy isn't supported, the canonical state should squash MAXANISOTROPY.
        let key_2_no_aniso = AerogpuD3d9Executor::canonicalize_sampler_state_for_caps(
            device_features,
            no_aniso,
            aniso_2,
        );
        let key_16_no_aniso = AerogpuD3d9Executor::canonicalize_sampler_state_for_caps(
            device_features,
            no_aniso,
            aniso_16,
        );
        assert_eq!(key_2_no_aniso, key_16_no_aniso);
        assert_eq!(key_2_no_aniso.max_anisotropy, 1);

        // When anisotropy is supported *and* requested, MAXANISOTROPY should influence the key.
        let key_2_supported = AerogpuD3d9Executor::canonicalize_sampler_state_for_caps(
            device_features,
            aniso_supported,
            aniso_2,
        );
        let key_16_supported = AerogpuD3d9Executor::canonicalize_sampler_state_for_caps(
            device_features,
            aniso_supported,
            aniso_16,
        );
        assert_ne!(key_2_supported, key_16_supported);
        assert_eq!(key_2_supported.max_anisotropy, 2);
        assert_eq!(key_16_supported.max_anisotropy, 16);
    }
}
