use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;

use aero_gpu::bindings::bind_group_cache::{
    BindGroupCache, BindGroupCacheEntry, BufferId, TextureViewId,
};
use aero_gpu::bindings::layout_cache::BindGroupLayoutCache;
use aero_gpu::bindings::samplers::SamplerCache;
use aero_gpu::bindings::CacheStats;
use aero_gpu::guest_memory::{GuestMemory, GuestMemoryError};
use aero_gpu::pipeline_cache::{PipelineCache, PipelineCacheConfig};
use aero_gpu::pipeline_key::{ColorTargetKey, PipelineLayoutKey, RenderPipelineKey, ShaderHash};
use aero_gpu::GpuCapabilities;
use aero_protocol::aerogpu::aerogpu_cmd::{
    decode_cmd_copy_buffer_le, decode_cmd_copy_texture2d_le,
    decode_cmd_create_input_layout_blob_le, decode_cmd_create_shader_dxbc_payload_le,
    decode_cmd_set_vertex_buffers_bindings_le, decode_cmd_upload_resource_payload_le,
    AerogpuCmdOpcode, AerogpuCmdStreamHeader, AerogpuCmdStreamIter, AEROGPU_CLEAR_COLOR,
    AEROGPU_CLEAR_DEPTH, AEROGPU_CLEAR_STENCIL, AEROGPU_COPY_FLAG_WRITEBACK_DST,
    AEROGPU_RASTERIZER_FLAG_DEPTH_CLIP_DISABLE, AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER,
    AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL, AEROGPU_RESOURCE_USAGE_INDEX_BUFFER,
    AEROGPU_RESOURCE_USAGE_RENDER_TARGET, AEROGPU_RESOURCE_USAGE_SCANOUT,
    AEROGPU_RESOURCE_USAGE_TEXTURE, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::aerogpu_ring::{AerogpuAllocEntry, AEROGPU_ALLOC_FLAG_READONLY};
use anyhow::{anyhow, bail, Context, Result};

use crate::binding_model::{MAX_SAMPLER_SLOTS, MAX_TEXTURE_SLOTS};
use crate::input_layout::{
    fnv1a_32, map_layout_to_shader_locations_compact, InputLayoutBinding, InputLayoutDesc,
    VertexBufferLayoutOwned, VsInputSignatureElement, MAX_INPUT_SLOTS,
};
use crate::{
    parse_signatures, translate_sm4_module_to_wgsl, DxbcFile, ShaderReflection, ShaderTranslation,
    Sm4Program,
};

use super::bindings::{BindingState, BoundConstantBuffer, BoundSampler, ShaderStage};
use super::pipeline_layout_cache::PipelineLayoutCache;
use super::reflection_bindings;

const DEFAULT_MAX_VERTEX_SLOTS: usize = MAX_INPUT_SLOTS as usize;
// D3D11 exposes 128 SRV slots per stage. Our shader translation keeps the D3D register index as the
// WGSL/WebGPU binding number (samplers live at an offset), so the executor must accept and track
// slots up to 127 even if only a smaller subset is used by a given shader.
const DEFAULT_MAX_TEXTURE_SLOTS: usize = MAX_TEXTURE_SLOTS as usize;
const DEFAULT_MAX_SAMPLER_SLOTS: usize = MAX_SAMPLER_SLOTS as usize;
// D3D10/11 exposes 14 constant buffer slots (0..13) per shader stage.
const DEFAULT_MAX_CONSTANT_BUFFER_SLOTS: usize = 14;
const LEGACY_CONSTANTS_SIZE_BYTES: u64 = 4096 * 16;

// Opcode constants from `aerogpu_cmd.h` (via the canonical `aero-protocol` enum).
const OPCODE_NOP: u32 = AerogpuCmdOpcode::Nop as u32;
const OPCODE_DEBUG_MARKER: u32 = AerogpuCmdOpcode::DebugMarker as u32;

const OPCODE_CREATE_BUFFER: u32 = AerogpuCmdOpcode::CreateBuffer as u32;
const OPCODE_CREATE_TEXTURE2D: u32 = AerogpuCmdOpcode::CreateTexture2d as u32;
const OPCODE_DESTROY_RESOURCE: u32 = AerogpuCmdOpcode::DestroyResource as u32;
const OPCODE_RESOURCE_DIRTY_RANGE: u32 = AerogpuCmdOpcode::ResourceDirtyRange as u32;
const OPCODE_UPLOAD_RESOURCE: u32 = AerogpuCmdOpcode::UploadResource as u32;
const OPCODE_COPY_BUFFER: u32 = AerogpuCmdOpcode::CopyBuffer as u32;
const OPCODE_COPY_TEXTURE2D: u32 = AerogpuCmdOpcode::CopyTexture2d as u32;

const OPCODE_CREATE_SHADER_DXBC: u32 = AerogpuCmdOpcode::CreateShaderDxbc as u32;
const OPCODE_DESTROY_SHADER: u32 = AerogpuCmdOpcode::DestroyShader as u32;
const OPCODE_BIND_SHADERS: u32 = AerogpuCmdOpcode::BindShaders as u32;
const OPCODE_SET_SHADER_CONSTANTS_F: u32 = AerogpuCmdOpcode::SetShaderConstantsF as u32;

const OPCODE_CREATE_INPUT_LAYOUT: u32 = AerogpuCmdOpcode::CreateInputLayout as u32;
const OPCODE_DESTROY_INPUT_LAYOUT: u32 = AerogpuCmdOpcode::DestroyInputLayout as u32;
const OPCODE_SET_INPUT_LAYOUT: u32 = AerogpuCmdOpcode::SetInputLayout as u32;

const OPCODE_SET_BLEND_STATE: u32 = AerogpuCmdOpcode::SetBlendState as u32;
const OPCODE_SET_DEPTH_STENCIL_STATE: u32 = AerogpuCmdOpcode::SetDepthStencilState as u32;
const OPCODE_SET_RASTERIZER_STATE: u32 = AerogpuCmdOpcode::SetRasterizerState as u32;

const OPCODE_SET_RENDER_TARGETS: u32 = AerogpuCmdOpcode::SetRenderTargets as u32;
const OPCODE_SET_VIEWPORT: u32 = AerogpuCmdOpcode::SetViewport as u32;
const OPCODE_SET_SCISSOR: u32 = AerogpuCmdOpcode::SetScissor as u32;

const OPCODE_SET_VERTEX_BUFFERS: u32 = AerogpuCmdOpcode::SetVertexBuffers as u32;
const OPCODE_SET_INDEX_BUFFER: u32 = AerogpuCmdOpcode::SetIndexBuffer as u32;
const OPCODE_SET_PRIMITIVE_TOPOLOGY: u32 = AerogpuCmdOpcode::SetPrimitiveTopology as u32;
const OPCODE_SET_TEXTURE: u32 = AerogpuCmdOpcode::SetTexture as u32;
const OPCODE_SET_SAMPLER_STATE: u32 = AerogpuCmdOpcode::SetSamplerState as u32;
const OPCODE_SET_RENDER_STATE: u32 = AerogpuCmdOpcode::SetRenderState as u32;
const OPCODE_CREATE_SAMPLER: u32 = AerogpuCmdOpcode::CreateSampler as u32;
const OPCODE_DESTROY_SAMPLER: u32 = AerogpuCmdOpcode::DestroySampler as u32;
const OPCODE_SET_SAMPLERS: u32 = AerogpuCmdOpcode::SetSamplers as u32;
const OPCODE_SET_CONSTANT_BUFFERS: u32 = AerogpuCmdOpcode::SetConstantBuffers as u32;

const OPCODE_CLEAR: u32 = AerogpuCmdOpcode::Clear as u32;
const OPCODE_DRAW: u32 = AerogpuCmdOpcode::Draw as u32;
const OPCODE_DRAW_INDEXED: u32 = AerogpuCmdOpcode::DrawIndexed as u32;

const OPCODE_PRESENT: u32 = AerogpuCmdOpcode::Present as u32;
const OPCODE_PRESENT_EX: u32 = AerogpuCmdOpcode::PresentEx as u32;

const OPCODE_EXPORT_SHARED_SURFACE: u32 = AerogpuCmdOpcode::ExportSharedSurface as u32;
const OPCODE_IMPORT_SHARED_SURFACE: u32 = AerogpuCmdOpcode::ImportSharedSurface as u32;
const OPCODE_RELEASE_SHARED_SURFACE: u32 = AerogpuCmdOpcode::ReleaseSharedSurface as u32;

const OPCODE_FLUSH: u32 = AerogpuCmdOpcode::Flush as u32;

const DEFAULT_BIND_GROUP_CACHE_CAPACITY: usize = 4096;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AerogpuCmdCacheStats {
    pub samplers: CacheStats,
    pub bind_group_layouts: CacheStats,
    pub pipeline_layouts: CacheStats,
    pub bind_groups: CacheStats,
}

#[derive(Debug, Clone, Default)]
pub struct ExecuteReport {
    pub commands: u32,
    pub unknown_opcodes: u32,
    pub presents: Vec<PresentEvent>,
}

#[derive(Debug, Clone)]
pub struct PresentEvent {
    pub scanout_id: u32,
    pub flags: u32,
    pub d3d9_present_flags: Option<u32>,
    pub presented_render_target: Option<u32>,
}

/// Shared surface bookkeeping for `EXPORT_SHARED_SURFACE` / `IMPORT_SHARED_SURFACE`.
///
/// This is the host-side equivalent of the Win7 KMD/UMD shared-resource protocol:
/// - `EXPORT` associates a stable `share_token` with an existing resource handle.
/// - `IMPORT` creates a new handle aliasing the exported resource.
/// - `RELEASE_SHARED_SURFACE` removes the token mapping and retires it (imports must fail).
/// - `DESTROY_RESOURCE` decrements refcounts and destroys the underlying resource only when the
///   final handle (original or alias) is released.
#[derive(Debug, Default)]
struct SharedSurfaceTable {
    /// `share_token -> underlying resource handle`.
    by_token: HashMap<u64, u32>,
    /// Tokens that were released (or otherwise removed) and must not be reused.
    retired_tokens: HashSet<u64>,
    /// `handle -> underlying resource handle`.
    ///
    /// - Original resources are stored as `handle -> handle`
    /// - Imported aliases are stored as `alias_handle -> underlying_handle`
    handles: HashMap<u32, u32>,
    /// `underlying handle -> refcount`.
    refcounts: HashMap<u32, u32>,
}

impl SharedSurfaceTable {
    fn retire_tokens_for_underlying(&mut self, underlying: u32) {
        let to_retire: Vec<u64> = self
            .by_token
            .iter()
            .filter_map(|(k, v)| (*v == underlying).then_some(*k))
            .collect();
        for token in to_retire {
            self.by_token.remove(&token);
            self.retired_tokens.insert(token);
        }
    }

    fn clear(&mut self) {
        self.by_token.clear();
        self.retired_tokens.clear();
        self.handles.clear();
        self.refcounts.clear();
    }

    fn register_handle(&mut self, handle: u32) {
        if handle == 0 {
            return;
        }
        if self.handles.contains_key(&handle) {
            return;
        }
        self.handles.insert(handle, handle);
        *self.refcounts.entry(handle).or_insert(0) += 1;
    }

    fn resolve_handle(&self, handle: u32) -> u32 {
        self.handles.get(&handle).copied().unwrap_or(handle)
    }

    fn export(&mut self, resource_handle: u32, share_token: u64) -> Result<()> {
        if resource_handle == 0 {
            bail!("EXPORT_SHARED_SURFACE: invalid resource handle 0");
        }
        if share_token == 0 {
            bail!("EXPORT_SHARED_SURFACE: invalid share_token 0");
        }
        if self.retired_tokens.contains(&share_token) {
            bail!(
                "EXPORT_SHARED_SURFACE: share_token 0x{share_token:016X} was previously released"
            );
        }
        let underlying = self
            .handles
            .get(&resource_handle)
            .copied()
            .ok_or_else(|| anyhow!("EXPORT_SHARED_SURFACE: unknown resource handle {resource_handle}"))?;

        if let Some(&existing) = self.by_token.get(&share_token) {
            if existing != underlying {
                bail!(
                    "EXPORT_SHARED_SURFACE: share_token 0x{share_token:016X} already exported (existing={existing} new={underlying})"
                );
            }
            return Ok(());
        }

        self.by_token.insert(share_token, underlying);
        Ok(())
    }

    fn import(&mut self, out_handle: u32, share_token: u64) -> Result<()> {
        if out_handle == 0 {
            bail!("IMPORT_SHARED_SURFACE: invalid out_resource_handle 0");
        }
        if share_token == 0 {
            bail!("IMPORT_SHARED_SURFACE: invalid share_token 0");
        }
        let Some(&underlying) = self.by_token.get(&share_token) else {
            bail!(
                "IMPORT_SHARED_SURFACE: unknown share_token 0x{share_token:016X} (not exported)"
            );
        };

        if !self.refcounts.contains_key(&underlying) {
            bail!(
                "IMPORT_SHARED_SURFACE: share_token 0x{share_token:016X} refers to destroyed handle {underlying}"
            );
        }

        if let Some(&existing) = self.handles.get(&out_handle) {
            if existing != underlying {
                bail!(
                    "IMPORT_SHARED_SURFACE: out_resource_handle {out_handle} already bound (existing={existing} new={underlying})"
                );
            }
            return Ok(());
        }

        self.handles.insert(out_handle, underlying);
        *self.refcounts.entry(underlying).or_insert(0) += 1;
        Ok(())
    }

    fn release_token(&mut self, share_token: u64) {
        if share_token == 0 {
            return;
        }
        self.by_token.remove(&share_token);
        self.retired_tokens.insert(share_token);
    }

    /// Releases a handle (original or alias). Returns `(underlying_handle, last_ref)` if tracked.
    fn destroy_handle(&mut self, handle: u32) -> Option<(u32, bool)> {
        if handle == 0 {
            return None;
        }

        let underlying = self.handles.remove(&handle)?;
        let Some(count) = self.refcounts.get_mut(&underlying) else {
            // Table invariant broken (handle tracked but no refcount entry). Treat as last-ref so
            // callers can clean up the underlying resource instead of leaking it.
            self.retire_tokens_for_underlying(underlying);
            return Some((underlying, true));
        };

        *count = count.saturating_sub(1);
        if *count != 0 {
            return Some((underlying, false));
        }

        self.refcounts.remove(&underlying);
        self.retire_tokens_for_underlying(underlying);
        Some((underlying, true))
    }
}

struct CmdStreamCtx<'a, 'b> {
    iter: &'b mut core::iter::Peekable<AerogpuCmdStreamIter<'a>>,
    cursor: &'b mut usize,
    bytes: &'a [u8],
    size: usize,
}

#[derive(Debug, Clone, Copy)]
struct Viewport {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    min_depth: f32,
    max_depth: f32,
}

#[derive(Debug, Clone, Copy)]
struct Scissor {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

#[derive(Debug, Clone, Copy)]
struct ResourceBacking {
    alloc_id: u32,
    offset_bytes: u64,
}

#[derive(Debug, Clone, Copy)]
struct TextureWritebackPlan {
    base_gpa: u64,
    row_pitch: u64,
    padded_bytes_per_row: u32,
    unpadded_bytes_per_row: u32,
    height: u32,
    is_x8: bool,
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
struct BufferResource {
    buffer: wgpu::Buffer,
    size: u64,
    gpu_size: u64,
    backing: Option<ResourceBacking>,
    dirty: Option<Range<u64>>,
}

impl BufferResource {
    fn mark_dirty(&mut self, range: Range<u64>) {
        let alignment = wgpu::COPY_BUFFER_ALIGNMENT;
        debug_assert!(alignment.is_power_of_two());

        let start = range.start.min(self.size);
        let end = range.end.min(self.size);
        if start >= end {
            return;
        }

        let start = start & !(alignment - 1);
        let end = end.saturating_add(alignment - 1) & !(alignment - 1);
        let end = end.min(self.size);
        if start >= end {
            return;
        }

        let range = start..end;
        self.dirty = Some(match self.dirty.take() {
            Some(existing) => existing.start.min(range.start)..existing.end.max(range.end),
            None => range,
        });
    }
}

#[derive(Debug, Clone, Copy)]
struct Texture2dDesc {
    width: u32,
    height: u32,
    mip_level_count: u32,
    array_layers: u32,
    format: wgpu::TextureFormat,
}

#[derive(Debug)]
struct Texture2dResource {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    desc: Texture2dDesc,
    format_u32: u32,
    backing: Option<ResourceBacking>,
    row_pitch_bytes: u32,
    dirty: bool,
    /// CPU shadow for textures updated via `UPLOAD_RESOURCE`.
    ///
    /// The command stream expresses uploads as a linear byte range, but WebGPU uploads are 2D. For
    /// partial updates we patch into this shadow buffer and then re-upload the full texture.
    ///
    /// The shadow is invalidated when the texture is written by GPU operations (draw/clear/copy).
    host_shadow: Option<Vec<u8>>,
}

#[derive(Debug, Clone)]
struct ShaderResource {
    stage: ShaderStage,
    wgsl_hash: ShaderHash,
    /// Variant with depth clamp applied to `@builtin(position).z` (used when
    /// `DepthClipEnable = FALSE`).
    depth_clamp_wgsl_hash: Option<ShaderHash>,
    dxbc_hash_fnv1a64: u64,
    entry_point: &'static str,
    vs_input_signature: Vec<VsInputSignatureElement>,
    reflection: ShaderReflection,
    #[cfg(debug_assertions)]
    #[allow(dead_code)]
    wgsl_source: String,
}

#[derive(Debug, Clone, Copy)]
struct VertexBufferBinding {
    buffer: u32,
    stride_bytes: u32,
    offset_bytes: u64,
}

#[derive(Debug, Clone, Copy)]
struct IndexBufferBinding {
    buffer: u32,
    format: wgpu::IndexFormat,
    offset_bytes: u64,
}

#[derive(Debug, Clone)]
struct InputLayoutResource {
    layout: InputLayoutDesc,
    /// Sorted, deduplicated list of D3D input slots referenced by this layout.
    ///
    /// Used to make the per-layout mapping cache key depend only on slot strides that matter for
    /// this layout (bindings to unrelated slots should not invalidate the cache).
    used_slots: Vec<u32>,
    mapping_cache: HashMap<u64, BuiltVertexState>,
}

#[derive(Debug)]
struct ConstantBufferScratch {
    id: BufferId,
    buffer: wgpu::Buffer,
    size: u64,
}

#[derive(Debug, Default)]
struct AerogpuD3d11Resources {
    buffers: HashMap<u32, BufferResource>,
    textures: HashMap<u32, Texture2dResource>,
    samplers: HashMap<u32, aero_gpu::bindings::samplers::CachedSampler>,
    shaders: HashMap<u32, ShaderResource>,
    input_layouts: HashMap<u32, InputLayoutResource>,
}

#[derive(Debug)]
struct AerogpuD3d11State {
    render_targets: Vec<u32>,
    depth_stencil: Option<u32>,
    viewport: Option<Viewport>,
    scissor: Option<Scissor>,

    vertex_buffers: Vec<Option<VertexBufferBinding>>,
    index_buffer: Option<IndexBufferBinding>,
    primitive_topology: wgpu::PrimitiveTopology,

    vs: Option<u32>,
    ps: Option<u32>,
    cs: Option<u32>,
    input_layout: Option<u32>,
    // A small subset of pipeline state. Unsupported values are tolerated and
    // mapped onto sensible defaults.
    blend: Option<wgpu::BlendState>,
    color_write_mask: wgpu::ColorWrites,
    blend_constant: [f32; 4],
    sample_mask: u32,
    depth_enable: bool,
    depth_write_enable: bool,
    depth_compare: wgpu::CompareFunction,
    stencil_enable: bool,
    stencil_read_mask: u8,
    stencil_write_mask: u8,
    cull_mode: Option<wgpu::Face>,
    front_face: wgpu::FrontFace,
    scissor_enable: bool,
    depth_bias: i32,
    depth_clip_enabled: bool,
}

impl Default for AerogpuD3d11State {
    fn default() -> Self {
        Self {
            render_targets: Vec::new(),
            depth_stencil: None,
            viewport: None,
            scissor: None,
            vertex_buffers: vec![None; DEFAULT_MAX_VERTEX_SLOTS],
            index_buffer: None,
            primitive_topology: wgpu::PrimitiveTopology::TriangleList,
            vs: None,
            ps: None,
            cs: None,
            input_layout: None,
            blend: None,
            color_write_mask: wgpu::ColorWrites::ALL,
            blend_constant: [1.0; 4],
            sample_mask: 0xFFFF_FFFF,
            depth_enable: true,
            depth_write_enable: true,
            depth_compare: wgpu::CompareFunction::Less,
            stencil_enable: false,
            stencil_read_mask: 0xFF,
            stencil_write_mask: 0xFF,
            // D3D11 default rasterizer state when RS state object is NULL.
            cull_mode: Some(wgpu::Face::Back),
            front_face: wgpu::FrontFace::Cw,
            scissor_enable: false,
            depth_bias: 0,
            depth_clip_enabled: true,
        }
    }
}

pub struct AerogpuD3d11Executor {
    device: wgpu::Device,
    queue: wgpu::Queue,

    resources: AerogpuD3d11Resources,
    state: AerogpuD3d11State,
    shared_surfaces: SharedSurfaceTable,

    bindings: BindingState,
    legacy_constants: HashMap<ShaderStage, wgpu::Buffer>,

    cbuffer_scratch: HashMap<(ShaderStage, u32), ConstantBufferScratch>,
    next_scratch_buffer_id: u64,

    dummy_uniform: wgpu::Buffer,
    dummy_texture_view: wgpu::TextureView,

    sampler_cache: SamplerCache,
    default_sampler: aero_gpu::bindings::samplers::CachedSampler,

    bind_group_layout_cache: BindGroupLayoutCache,
    bind_group_cache: BindGroupCache<Arc<wgpu::BindGroup>>,
    pipeline_layout_cache: PipelineLayoutCache<Arc<wgpu::PipelineLayout>>,
    pipeline_cache: PipelineCache,

    /// Resources referenced by commands recorded into the current `wgpu::CommandEncoder`.
    ///
    /// `wgpu::Queue::write_*` operations are ordered relative to `queue.submit` calls, so when we
    /// perform an implicit guest-memory upload via `queue.write_*` while a render pass is active we
    /// must ensure that upload can safely be reordered before the eventual command-buffer submission.
    ///
    /// Tracking which handles were used by previously-recorded GPU commands lets the render-pass
    /// executor decide whether an implicit upload must end the pass (and force a submit) to
    /// preserve command stream ordering.
    encoder_used_buffers: HashSet<u32>,
    encoder_used_textures: HashSet<u32>,

    /// Tracks whether the in-flight command encoder has recorded any GPU work.
    ///
    /// This is used to avoid submitting empty command buffers when we need to
    /// flush the encoder to preserve ordering relative to `queue.write_*`
    /// uploads.
    encoder_has_commands: bool,
}

impl AerogpuD3d11Executor {
    pub async fn new_for_tests() -> Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let needs_runtime_dir = std::env::var("XDG_RUNTIME_DIR")
                .ok()
                .map(|v| v.is_empty())
                .unwrap_or(true);

            if needs_runtime_dir {
                let dir = std::env::temp_dir()
                    .join(format!("aero-d3d11-xdg-runtime-{}", std::process::id()));
                let _ = std::fs::create_dir_all(&dir);
                let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
                std::env::set_var("XDG_RUNTIME_DIR", &dir);
            }
        }

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            // Prefer GL on Linux CI to avoid crashes in some Vulkan software adapters.
            backends: if cfg!(target_os = "linux") {
                wgpu::Backends::GL
            } else {
                // Prefer "native" backends; this avoids noisy platform warnings from
                // initializing GL/WAYLAND stacks in headless CI environments.
                wgpu::Backends::PRIMARY
            },
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
            Some(adapter) => Some(adapter),
            None => {
                instance
                    .request_adapter(&wgpu::RequestAdapterOptions {
                        power_preference: wgpu::PowerPreference::LowPower,
                        compatible_surface: None,
                        force_fallback_adapter: false,
                    })
                    .await
            }
        }
        .ok_or_else(|| anyhow!("wgpu: no suitable adapter found"))?;

        let requested_features = super::negotiated_features(&adapter);
        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("aero-d3d11 aerogpu_cmd test device"),
                    required_features: requested_features,
                    required_limits: wgpu::Limits::downlevel_defaults(),
                },
                None,
            )
            .await
            .map_err(|e| anyhow!("wgpu: request_device failed: {e:?}"))?;

        let dummy_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("aerogpu_cmd dummy texture"),
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
        let dummy_texture_view = dummy_texture.create_view(&wgpu::TextureViewDescriptor::default());
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &dummy_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            // D3D treats unbound SRVs as (0, 0, 0, 1).
            &[0u8, 0u8, 0u8, 255u8],
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

        let dummy_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aerogpu_cmd dummy uniform buffer"),
            size: LEGACY_CONSTANTS_SIZE_BYTES,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: true,
        });
        {
            let mut mapped = dummy_uniform.slice(..).get_mapped_range_mut();
            mapped.fill(0);
        }
        dummy_uniform.unmap();

        let caps = GpuCapabilities::from_device(&device);
        let pipeline_cache = PipelineCache::new(PipelineCacheConfig::default(), caps);

        let mut sampler_cache = SamplerCache::new();
        let default_sampler = sampler_cache.get_or_create(
            &device,
            &wgpu::SamplerDescriptor {
                label: Some("aerogpu_cmd default sampler"),
                address_mode_u: wgpu::AddressMode::ClampToEdge,
                address_mode_v: wgpu::AddressMode::ClampToEdge,
                address_mode_w: wgpu::AddressMode::ClampToEdge,
                mag_filter: wgpu::FilterMode::Nearest,
                min_filter: wgpu::FilterMode::Nearest,
                mipmap_filter: wgpu::FilterMode::Nearest,
                ..Default::default()
            },
        );

        let mut legacy_constants = HashMap::new();
        for stage in [
            ShaderStage::Vertex,
            ShaderStage::Pixel,
            ShaderStage::Compute,
        ] {
            let buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("aerogpu_cmd legacy constants buffer"),
                size: LEGACY_CONSTANTS_SIZE_BYTES,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: true,
            });
            {
                let mut mapped = buf.slice(..).get_mapped_range_mut();
                mapped.fill(0);
            }
            buf.unmap();
            legacy_constants.insert(stage, buf);
        }

        let mut bindings = BindingState::default();
        for stage in [
            ShaderStage::Vertex,
            ShaderStage::Pixel,
            ShaderStage::Compute,
        ] {
            bindings.stage_mut(stage).set_constant_buffer(
                0,
                Some(BoundConstantBuffer {
                    buffer: legacy_constants_buffer_id(stage),
                    offset: 0,
                    size: None,
                }),
            );
            bindings.stage_mut(stage).clear_dirty();
        }

        Ok(Self {
            device,
            queue,
            resources: AerogpuD3d11Resources::default(),
            state: AerogpuD3d11State::default(),
            shared_surfaces: SharedSurfaceTable::default(),
            bindings,
            legacy_constants,
            cbuffer_scratch: HashMap::new(),
            next_scratch_buffer_id: 1u64 << 32,
            dummy_uniform,
            dummy_texture_view,
            sampler_cache,
            default_sampler,
            bind_group_layout_cache: BindGroupLayoutCache::new(),
            bind_group_cache: BindGroupCache::new(DEFAULT_BIND_GROUP_CACHE_CAPACITY),
            pipeline_layout_cache: PipelineLayoutCache::new(),
            pipeline_cache,
            encoder_used_buffers: HashSet::new(),
            encoder_used_textures: HashSet::new(),
            encoder_has_commands: false,
        })
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub fn reset(&mut self) {
        self.resources = AerogpuD3d11Resources::default();
        self.state = AerogpuD3d11State::default();
        self.shared_surfaces.clear();
        self.pipeline_cache.clear();
        self.cbuffer_scratch.clear();
        self.encoder_used_buffers.clear();
        self.encoder_used_textures.clear();
        self.next_scratch_buffer_id = 1u64 << 32;
        self.bindings = BindingState::default();
        for stage in [
            ShaderStage::Vertex,
            ShaderStage::Pixel,
            ShaderStage::Compute,
        ] {
            self.bindings.stage_mut(stage).set_constant_buffer(
                0,
                Some(BoundConstantBuffer {
                    buffer: legacy_constants_buffer_id(stage),
                    offset: 0,
                    size: None,
                }),
            );
            self.bindings.stage_mut(stage).clear_dirty();
        }
    }

    pub fn poll_wait(&self) {
        self.poll();
    }

    fn poll(&self) {
        #[cfg(not(target_arch = "wasm32"))]
        self.device.poll(wgpu::Maintain::Wait);

        #[cfg(target_arch = "wasm32")]
        self.device.poll(wgpu::Maintain::Poll);
    }

    pub fn cache_stats(&self) -> AerogpuCmdCacheStats {
        AerogpuCmdCacheStats {
            samplers: self.sampler_cache.stats(),
            bind_group_layouts: self.bind_group_layout_cache.stats(),
            pipeline_layouts: self.pipeline_layout_cache.stats(),
            bind_groups: self.bind_group_cache.stats(),
        }
    }

    pub fn texture_size(&self, texture_id: u32) -> Result<(u32, u32)> {
        let texture_id = self.shared_surfaces.resolve_handle(texture_id);
        let texture = self
            .resources
            .textures
            .get(&texture_id)
            .ok_or_else(|| anyhow!("unknown texture {texture_id}"))?;
        Ok((texture.desc.width, texture.desc.height))
    }

    pub async fn read_texture_rgba8(&self, texture_id: u32) -> Result<Vec<u8>> {
        let texture_id = self.shared_surfaces.resolve_handle(texture_id);
        let texture = self
            .resources
            .textures
            .get(&texture_id)
            .ok_or_else(|| anyhow!("unknown texture {texture_id}"))?;

        let width = texture.desc.width;
        let height = texture.desc.height;

        // Compute the buffer layout based on the source format. For block-compressed textures, the
        // copy layout uses block rows (4x4 blocks for BC formats).
        let (padded_bytes_per_row, rows_per_image, buffer_size, bc_readback_info) =
            match texture.desc.format {
                wgpu::TextureFormat::Rgba8Unorm
                | wgpu::TextureFormat::Rgba8UnormSrgb
                | wgpu::TextureFormat::Bgra8Unorm
                | wgpu::TextureFormat::Bgra8UnormSrgb => {
                    let bytes_per_pixel = 4u32;
                    let unpadded_bytes_per_row = width
                        .checked_mul(bytes_per_pixel)
                        .ok_or_else(|| anyhow!("read_texture_rgba8: bytes_per_row overflow"))?;
                    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
                    let padded_bytes_per_row = unpadded_bytes_per_row
                        .checked_add(align - 1)
                        .map(|v| v / align)
                        .and_then(|v| v.checked_mul(align))
                        .ok_or_else(|| anyhow!("read_texture_rgba8: padded bytes_per_row overflow"))?;
                    let buffer_size = (padded_bytes_per_row as u64)
                        .checked_mul(height as u64)
                        .ok_or_else(|| anyhow!("read_texture_rgba8: staging buffer size overflow"))?;
                    (
                        padded_bytes_per_row,
                        height,
                        buffer_size,
                        None::<(AerogpuBcFormat, u32, u32)>,
                    )
                }
                wgpu::TextureFormat::Bc1RgbaUnorm | wgpu::TextureFormat::Bc1RgbaUnormSrgb => {
                    let block_bytes = 8u32;
                    let blocks_w = width.div_ceil(4);
                    let blocks_h = height.div_ceil(4);
                    let tight_bpr = blocks_w
                        .checked_mul(block_bytes)
                        .ok_or_else(|| anyhow!("read_texture_rgba8: BC bytes_per_row overflow"))?;
                    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
                    let padded_bpr = tight_bpr
                        .checked_add(align - 1)
                        .map(|v| v / align)
                        .and_then(|v| v.checked_mul(align))
                        .ok_or_else(|| anyhow!("read_texture_rgba8: BC padded bytes_per_row overflow"))?;
                    let buffer_size = (padded_bpr as u64)
                        .checked_mul(blocks_h as u64)
                        .ok_or_else(|| anyhow!("read_texture_rgba8: BC staging buffer size overflow"))?;
                    (
                        padded_bpr,
                        blocks_h,
                        buffer_size,
                        Some((AerogpuBcFormat::Bc1, tight_bpr, blocks_h)),
                    )
                }
                wgpu::TextureFormat::Bc2RgbaUnorm | wgpu::TextureFormat::Bc2RgbaUnormSrgb => {
                    let block_bytes = 16u32;
                    let blocks_w = width.div_ceil(4);
                    let blocks_h = height.div_ceil(4);
                    let tight_bpr = blocks_w
                        .checked_mul(block_bytes)
                        .ok_or_else(|| anyhow!("read_texture_rgba8: BC bytes_per_row overflow"))?;
                    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
                    let padded_bpr = tight_bpr
                        .checked_add(align - 1)
                        .map(|v| v / align)
                        .and_then(|v| v.checked_mul(align))
                        .ok_or_else(|| anyhow!("read_texture_rgba8: BC padded bytes_per_row overflow"))?;
                    let buffer_size = (padded_bpr as u64)
                        .checked_mul(blocks_h as u64)
                        .ok_or_else(|| anyhow!("read_texture_rgba8: BC staging buffer size overflow"))?;
                    (
                        padded_bpr,
                        blocks_h,
                        buffer_size,
                        Some((AerogpuBcFormat::Bc2, tight_bpr, blocks_h)),
                    )
                }
                wgpu::TextureFormat::Bc3RgbaUnorm | wgpu::TextureFormat::Bc3RgbaUnormSrgb => {
                    let block_bytes = 16u32;
                    let blocks_w = width.div_ceil(4);
                    let blocks_h = height.div_ceil(4);
                    let tight_bpr = blocks_w
                        .checked_mul(block_bytes)
                        .ok_or_else(|| anyhow!("read_texture_rgba8: BC bytes_per_row overflow"))?;
                    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
                    let padded_bpr = tight_bpr
                        .checked_add(align - 1)
                        .map(|v| v / align)
                        .and_then(|v| v.checked_mul(align))
                        .ok_or_else(|| anyhow!("read_texture_rgba8: BC padded bytes_per_row overflow"))?;
                    let buffer_size = (padded_bpr as u64)
                        .checked_mul(blocks_h as u64)
                        .ok_or_else(|| anyhow!("read_texture_rgba8: BC staging buffer size overflow"))?;
                    (
                        padded_bpr,
                        blocks_h,
                        buffer_size,
                        Some((AerogpuBcFormat::Bc3, tight_bpr, blocks_h)),
                    )
                }
                wgpu::TextureFormat::Bc7RgbaUnorm | wgpu::TextureFormat::Bc7RgbaUnormSrgb => {
                    let block_bytes = 16u32;
                    let blocks_w = width.div_ceil(4);
                    let blocks_h = height.div_ceil(4);
                    let tight_bpr = blocks_w
                        .checked_mul(block_bytes)
                        .ok_or_else(|| anyhow!("read_texture_rgba8: BC bytes_per_row overflow"))?;
                    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
                    let padded_bpr = tight_bpr
                        .checked_add(align - 1)
                        .map(|v| v / align)
                        .and_then(|v| v.checked_mul(align))
                        .ok_or_else(|| anyhow!("read_texture_rgba8: BC padded bytes_per_row overflow"))?;
                    let buffer_size = (padded_bpr as u64)
                        .checked_mul(blocks_h as u64)
                        .ok_or_else(|| anyhow!("read_texture_rgba8: BC staging buffer size overflow"))?;
                    (
                        padded_bpr,
                        blocks_h,
                        buffer_size,
                        Some((AerogpuBcFormat::Bc7, tight_bpr, blocks_h)),
                    )
                }
                other => bail!("read_texture_rgba8 does not support format {other:?}"),
            };

        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aero-d3d11 aerogpu_cmd read_texture staging"),
            size: buffer_size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aero-d3d11 aerogpu_cmd read_texture encoder"),
            });

        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture: &texture.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &staging,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bytes_per_row),
                    rows_per_image: Some(rows_per_image),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );

        self.queue.submit([encoder.finish()]);

        let slice = staging.slice(..);
        let (sender, receiver) = futures_intrusive::channel::shared::oneshot_channel();
        slice.map_async(wgpu::MapMode::Read, move |v| {
            sender.send(v).ok();
        });
        self.poll();
        receiver
            .receive()
            .await
            .ok_or_else(|| anyhow!("wgpu: map_async dropped"))?
            .context("wgpu: map_async failed")?;

        let mapped = slice.get_mapped_range();
        let padded_bpr_usize: usize = padded_bytes_per_row
            .try_into()
            .map_err(|_| anyhow!("read_texture_rgba8: padded bytes_per_row out of range"))?;

        let out = if let Some((bc, tight_bpr, block_rows)) = bc_readback_info {
            // Extract tight BC bytes (strip `bytes_per_row` padding) and CPU-decompress to RGBA8 so
            // tests can validate BC textures even when compression features are enabled.
            let tight_bpr_usize: usize = tight_bpr
                .try_into()
                .map_err(|_| anyhow!("read_texture_rgba8: BC bytes_per_row out of range"))?;
            let block_rows_usize: usize = block_rows
                .try_into()
                .map_err(|_| anyhow!("read_texture_rgba8: BC rows out of range"))?;

            let mut bc_bytes = Vec::with_capacity(
                tight_bpr_usize
                    .checked_mul(block_rows_usize)
                    .ok_or_else(|| anyhow!("read_texture_rgba8: BC output size overflow"))?,
            );
            for row in 0..block_rows_usize {
                let start = row
                    .checked_mul(padded_bpr_usize)
                    .ok_or_else(|| anyhow!("read_texture_rgba8: BC row offset overflow"))?;
                let end = start
                    .checked_add(tight_bpr_usize)
                    .ok_or_else(|| anyhow!("read_texture_rgba8: BC row end overflow"))?;
                bc_bytes.extend_from_slice(
                    mapped
                        .get(start..end)
                        .ok_or_else(|| anyhow!("read_texture_rgba8: BC staging buffer too small"))?,
                );
            }

            match bc {
                AerogpuBcFormat::Bc1 => aero_gpu::decompress_bc1_rgba8(width, height, &bc_bytes),
                AerogpuBcFormat::Bc2 => aero_gpu::decompress_bc2_rgba8(width, height, &bc_bytes),
                AerogpuBcFormat::Bc3 => aero_gpu::decompress_bc3_rgba8(width, height, &bc_bytes),
                AerogpuBcFormat::Bc7 => aero_gpu::decompress_bc7_rgba8(width, height, &bc_bytes),
            }
        } else {
            let needs_bgra_swizzle = match texture.desc.format {
                wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Rgba8UnormSrgb => false,
                wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb => true,
                other => bail!(
                    "read_texture_rgba8 only supports RGBA/BGRA formats or BC formats (got {other:?})"
                ),
            };

            let bytes_per_pixel = 4u32;
            let unpadded_bytes_per_row = width
                .checked_mul(bytes_per_pixel)
                .ok_or_else(|| anyhow!("read_texture_rgba8: bytes_per_row overflow"))?;
            let out_len = (unpadded_bytes_per_row as u64)
                .checked_mul(height as u64)
                .ok_or_else(|| anyhow!("read_texture_rgba8: output size overflow"))?;
            let out_len_usize: usize = out_len
                .try_into()
                .map_err(|_| anyhow!("read_texture_rgba8: output size out of range"))?;
            let unpadded_bpr_usize: usize = unpadded_bytes_per_row
                .try_into()
                .map_err(|_| anyhow!("read_texture_rgba8: bytes_per_row out of range"))?;

            let mut out = Vec::with_capacity(out_len_usize);
            for row in 0..height as usize {
                let start = row
                    .checked_mul(padded_bpr_usize)
                    .ok_or_else(|| anyhow!("read_texture_rgba8: row offset overflow"))?;
                let end = start
                    .checked_add(unpadded_bpr_usize)
                    .ok_or_else(|| anyhow!("read_texture_rgba8: row end overflow"))?;
                out.extend_from_slice(
                    mapped
                        .get(start..end)
                        .ok_or_else(|| anyhow!("read_texture_rgba8: staging buffer too small"))?,
                );
            }

            if needs_bgra_swizzle {
                for px in out.chunks_exact_mut(4) {
                    px.swap(0, 2);
                }
            }

            out
        };

        drop(mapped);
        staging.unmap();

        Ok(out)
    }

    pub fn execute_cmd_stream(
        &mut self,
        stream_bytes: &[u8],
        allocs: Option<&[AerogpuAllocEntry]>,
        guest_mem: &mut dyn GuestMemory,
    ) -> Result<ExecuteReport> {
        #[cfg(target_arch = "wasm32")]
        {
            let iter = AerogpuCmdStreamIter::new(stream_bytes)
                .map_err(|e| anyhow!("aerogpu_cmd: invalid cmd stream: {e:?}"))?;
            let stream_size = iter.header().size_bytes as usize;
            let mut cursor = AerogpuCmdStreamHeader::SIZE_BYTES;
            let mut packet_index = 0usize;
            for next in iter {
                let packet = next.map_err(|err| {
                    anyhow!("aerogpu_cmd: invalid cmd header @0x{cursor:x}: {err:?}")
                })?;
                let cmd_size = packet.hdr.size_bytes as usize;
                let cmd_end = cursor
                    .checked_add(cmd_size)
                    .ok_or_else(|| anyhow!("aerogpu_cmd: cmd size overflow"))?;
                let cmd_bytes = stream_bytes.get(cursor..cmd_end).ok_or_else(|| {
                    anyhow!(
                        "aerogpu_cmd: cmd overruns stream: cursor=0x{cursor:x} cmd_size=0x{cmd_size:x} stream_size=0x{stream_size:x}"
                    )
                })?;

                match packet.hdr.opcode {
                    OPCODE_COPY_BUFFER => {
                        let cmd = decode_cmd_copy_buffer_le(cmd_bytes)
                            .map_err(|e| anyhow!("COPY_BUFFER: invalid payload: {e:?}"))?;
                        if (cmd.flags & AEROGPU_COPY_FLAG_WRITEBACK_DST) != 0 {
                            bail!(
                                "WRITEBACK_DST requires async execution on wasm (call execute_cmd_stream_async); first WRITEBACK_DST at packet {packet_index}"
                            );
                        }
                    }
                    OPCODE_COPY_TEXTURE2D => {
                        let cmd = decode_cmd_copy_texture2d_le(cmd_bytes)
                            .map_err(|e| anyhow!("COPY_TEXTURE2D: invalid payload: {e:?}"))?;
                        if (cmd.flags & AEROGPU_COPY_FLAG_WRITEBACK_DST) != 0 {
                            bail!(
                                "WRITEBACK_DST requires async execution on wasm (call execute_cmd_stream_async); first WRITEBACK_DST at packet {packet_index}"
                            );
                        }
                    }
                    _ => {}
                }

                cursor = cmd_end;
                packet_index += 1;
            }
        }

        let mut pending_writebacks = Vec::new();
        let report = self.execute_cmd_stream_inner(
            stream_bytes,
            allocs,
            guest_mem,
            &mut pending_writebacks,
        )?;

        if pending_writebacks.is_empty() {
            return Ok(report);
        }

        #[cfg(target_arch = "wasm32")]
        {
            bail!("WRITEBACK_DST requires async execution on wasm (call execute_cmd_stream_async)");
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            self.flush_pending_writebacks_blocking(pending_writebacks, guest_mem)?;
            Ok(report)
        }
    }

    /// WASM-friendly async variant of `execute_cmd_stream`.
    ///
    /// On WASM targets, `wgpu::Buffer::map_async` completion is delivered via the JS event loop,
    /// so synchronous waiting would deadlock. This method awaits writeback staging buffer maps
    /// when `AEROGPU_COPY_FLAG_WRITEBACK_DST` is used.
    pub async fn execute_cmd_stream_async(
        &mut self,
        stream_bytes: &[u8],
        allocs: Option<&[AerogpuAllocEntry]>,
        guest_mem: &mut dyn GuestMemory,
    ) -> Result<ExecuteReport> {
        let mut pending_writebacks = Vec::new();
        let report = self.execute_cmd_stream_inner(
            stream_bytes,
            allocs,
            guest_mem,
            &mut pending_writebacks,
        )?;
        if !pending_writebacks.is_empty() {
            self.flush_pending_writebacks_async(pending_writebacks, guest_mem)
                .await?;
        }
        Ok(report)
    }

    fn execute_cmd_stream_inner(
        &mut self,
        stream_bytes: &[u8],
        allocs: Option<&[AerogpuAllocEntry]>,
        guest_mem: &mut dyn GuestMemory,
        pending_writebacks: &mut Vec<PendingWriteback>,
    ) -> Result<ExecuteReport> {
        self.encoder_has_commands = false;
        let iter = AerogpuCmdStreamIter::new(stream_bytes)
            .map_err(|e| anyhow!("aerogpu_cmd: invalid cmd stream: {e:?}"))?;
        let stream_size = iter.header().size_bytes as usize;
        let mut iter = iter.peekable();

        let alloc_map = AllocTable::new(allocs)?;

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aerogpu_cmd encoder"),
            });

        let mut report = ExecuteReport::default();

        let result: Result<()> = (|| {
            let mut cursor = AerogpuCmdStreamHeader::SIZE_BYTES;
            while let Some(next) = iter.peek() {
                let (cmd_size, opcode) = match next {
                    Ok(packet) => (packet.hdr.size_bytes as usize, packet.hdr.opcode),
                    Err(err) => {
                        return Err(anyhow!(
                            "aerogpu_cmd: invalid cmd header @0x{cursor:x}: {err:?}"
                        ));
                    }
                };
                let cmd_end = cursor
                    .checked_add(cmd_size)
                    .ok_or_else(|| anyhow!("aerogpu_cmd: cmd size overflow"))?;
                let cmd_bytes = stream_bytes
                    .get(cursor..cmd_end)
                    .ok_or_else(|| {
                        anyhow!(
                            "aerogpu_cmd: cmd overruns stream: cursor=0x{cursor:x} cmd_size=0x{cmd_size:x} stream_size=0x{stream_size:x}"
                        )
                    })?;

                // Commands that need a render-pass boundary are handled by ending any
                // in-flight pass before processing the opcode.
                match opcode {
                    OPCODE_DRAW | OPCODE_DRAW_INDEXED => {
                        let mut stream = CmdStreamCtx {
                            iter: &mut iter,
                            cursor: &mut cursor,
                            bytes: stream_bytes,
                            size: stream_size,
                        };
                        self.exec_render_pass_load(
                            &mut encoder,
                            &mut stream,
                            &alloc_map,
                            guest_mem,
                            &mut report,
                        )?;
                        continue;
                    }
                    _ => {}
                }

                // Non-draw commands are processed directly.
                iter.next().expect("peeked Some").map_err(|err| {
                    anyhow!("aerogpu_cmd: invalid cmd header @0x{cursor:x}: {err:?}")
                })?;
                self.exec_non_draw_command(
                    &mut encoder,
                    opcode,
                    cmd_bytes,
                    &alloc_map,
                    guest_mem,
                    pending_writebacks,
                    &mut report,
                )?;

                report.commands = report.commands.saturating_add(1);
                cursor = cmd_end;
            }

            Ok(())
        })();

        match result {
            Ok(()) => {
                self.queue.submit([encoder.finish()]);
                self.encoder_has_commands = false;
                self.encoder_used_buffers.clear();
                self.encoder_used_textures.clear();
                Ok(report)
            }
            Err(err) => {
                // Drop partially-recorded work, but still flush `queue.write_*` uploads so they
                // don't remain queued indefinitely and reorder with later submissions.
                self.encoder_has_commands = false;
                self.encoder_used_buffers.clear();
                self.encoder_used_textures.clear();
                self.queue.submit([]);
                Err(err)
            }
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn flush_pending_writebacks_blocking(
        &self,
        pending: Vec<PendingWriteback>,
        guest_mem: &mut dyn GuestMemory,
    ) -> Result<()> {
        for writeback in pending {
            match writeback {
                PendingWriteback::Buffer {
                    staging,
                    dst_gpa,
                    size_bytes,
                } => {
                    let slice = staging.slice(..);
                    let state = std::sync::Arc::new((
                        std::sync::Mutex::new(None::<Result<(), wgpu::BufferAsyncError>>),
                        std::sync::Condvar::new(),
                    ));
                    let state_clone = state.clone();
                    slice.map_async(wgpu::MapMode::Read, move |res| {
                        let (lock, cv) = &*state_clone;
                        *lock.lock().unwrap() = Some(res);
                        cv.notify_one();
                    });
                    self.poll();

                    let (lock, cv) = &*state;
                    let mut guard = lock.lock().unwrap();
                    while guard.is_none() {
                        guard = cv.wait(guard).unwrap();
                    }
                    guard
                        .take()
                        .unwrap()
                        .map_err(|e| anyhow!("COPY_BUFFER: writeback map_async failed: {e:?}"))?;

                    let mapped = slice.get_mapped_range();
                    let len: usize = size_bytes
                        .try_into()
                        .map_err(|_| anyhow!("COPY_BUFFER: size_bytes out of range"))?;
                    guest_mem
                        .write(
                            dst_gpa,
                            mapped.get(..len).ok_or_else(|| {
                                anyhow!("COPY_BUFFER: writeback staging buffer too small")
                            })?,
                        )
                        .map_err(anyhow_guest_mem)?;
                    drop(mapped);
                    staging.unmap();
                }
                PendingWriteback::Texture2d { staging, plan } => {
                    let slice = staging.slice(..);
                    let state = std::sync::Arc::new((
                        std::sync::Mutex::new(None::<Result<(), wgpu::BufferAsyncError>>),
                        std::sync::Condvar::new(),
                    ));
                    let state_clone = state.clone();
                    slice.map_async(wgpu::MapMode::Read, move |res| {
                        let (lock, cv) = &*state_clone;
                        *lock.lock().unwrap() = Some(res);
                        cv.notify_one();
                    });
                    self.poll();

                    let (lock, cv) = &*state;
                    let mut guard = lock.lock().unwrap();
                    while guard.is_none() {
                        guard = cv.wait(guard).unwrap();
                    }
                    guard.take().unwrap().map_err(|e| {
                        anyhow!("COPY_TEXTURE2D: writeback map_async failed: {e:?}")
                    })?;

                    let padded_bpr_usize: usize =
                        plan.padded_bytes_per_row.try_into().map_err(|_| {
                            anyhow!("COPY_TEXTURE2D: padded bytes_per_row out of range")
                        })?;
                    let unpadded_bpr_usize: usize = plan
                        .unpadded_bytes_per_row
                        .try_into()
                        .map_err(|_| anyhow!("COPY_TEXTURE2D: bytes_per_row out of range"))?;
                    let mapped = slice.get_mapped_range();
                    let owned = if plan.is_x8 {
                        let mut bytes = mapped.to_vec();
                        for row in 0..plan.height as usize {
                            let start = row
                                .checked_mul(padded_bpr_usize)
                                .ok_or_else(|| anyhow!("COPY_TEXTURE2D: row offset overflow"))?;
                            let end = start
                                .checked_add(unpadded_bpr_usize)
                                .ok_or_else(|| anyhow!("COPY_TEXTURE2D: row end overflow"))?;
                            force_opaque_alpha_rgba8(bytes.get_mut(start..end).ok_or_else(
                                || anyhow!("COPY_TEXTURE2D: staging buffer too small"),
                            )?);
                        }
                        Some(bytes)
                    } else {
                        None
                    };
                    for row in 0..plan.height as u64 {
                        let src_start = row as usize * padded_bpr_usize;
                        let src_end =
                            src_start.checked_add(unpadded_bpr_usize).ok_or_else(|| {
                                anyhow!("COPY_TEXTURE2D: src row end overflows usize")
                            })?;
                        let row_bytes = match owned.as_ref() {
                            Some(bytes) => bytes.get(src_start..src_end),
                            None => mapped.get(src_start..src_end),
                        }
                        .ok_or_else(|| {
                            anyhow!("COPY_TEXTURE2D: writeback staging buffer too small")
                        })?;
                        let dst_gpa = plan
                            .base_gpa
                            .checked_add(row.checked_mul(plan.row_pitch).ok_or_else(|| {
                                anyhow!("COPY_TEXTURE2D: dst GPA overflow (row pitch mul)")
                            })?)
                            .ok_or_else(|| {
                                anyhow!("COPY_TEXTURE2D: dst GPA overflow (row pitch add)")
                            })?;
                        guest_mem
                            .write(dst_gpa, row_bytes)
                            .map_err(anyhow_guest_mem)?;
                    }
                    drop(mapped);
                    staging.unmap();
                }
            }
        }
        Ok(())
    }

    async fn flush_pending_writebacks_async(
        &self,
        pending: Vec<PendingWriteback>,
        guest_mem: &mut dyn GuestMemory,
    ) -> Result<()> {
        for writeback in pending {
            match writeback {
                PendingWriteback::Buffer {
                    staging,
                    dst_gpa,
                    size_bytes,
                } => {
                    let slice = staging.slice(..);
                    let (sender, receiver) = futures_intrusive::channel::shared::oneshot_channel();
                    slice.map_async(wgpu::MapMode::Read, move |res| {
                        sender.send(res).ok();
                    });
                    self.poll();
                    receiver
                        .receive()
                        .await
                        .ok_or_else(|| anyhow!("wgpu: map_async dropped"))?
                        .context("wgpu: map_async failed")?;

                    let mapped = slice.get_mapped_range();
                    let len: usize = size_bytes
                        .try_into()
                        .map_err(|_| anyhow!("COPY_BUFFER: size_bytes out of range"))?;
                    guest_mem
                        .write(
                            dst_gpa,
                            mapped.get(..len).ok_or_else(|| {
                                anyhow!("COPY_BUFFER: writeback staging buffer too small")
                            })?,
                        )
                        .map_err(anyhow_guest_mem)?;
                    drop(mapped);
                    staging.unmap();
                }
                PendingWriteback::Texture2d { staging, plan } => {
                    let slice = staging.slice(..);
                    let (sender, receiver) = futures_intrusive::channel::shared::oneshot_channel();
                    slice.map_async(wgpu::MapMode::Read, move |res| {
                        sender.send(res).ok();
                    });
                    self.poll();
                    receiver
                        .receive()
                        .await
                        .ok_or_else(|| anyhow!("wgpu: map_async dropped"))?
                        .context("wgpu: map_async failed")?;

                    let padded_bpr_usize: usize =
                        plan.padded_bytes_per_row.try_into().map_err(|_| {
                            anyhow!("COPY_TEXTURE2D: padded bytes_per_row out of range")
                        })?;
                    let unpadded_bpr_usize: usize = plan
                        .unpadded_bytes_per_row
                        .try_into()
                        .map_err(|_| anyhow!("COPY_TEXTURE2D: bytes_per_row out of range"))?;
                    let mapped = slice.get_mapped_range();
                    let owned = if plan.is_x8 {
                        let mut bytes = mapped.to_vec();
                        for row in 0..plan.height as usize {
                            let start = row
                                .checked_mul(padded_bpr_usize)
                                .ok_or_else(|| anyhow!("COPY_TEXTURE2D: row offset overflow"))?;
                            let end = start
                                .checked_add(unpadded_bpr_usize)
                                .ok_or_else(|| anyhow!("COPY_TEXTURE2D: row end overflow"))?;
                            force_opaque_alpha_rgba8(bytes.get_mut(start..end).ok_or_else(
                                || anyhow!("COPY_TEXTURE2D: staging buffer too small"),
                            )?);
                        }
                        Some(bytes)
                    } else {
                        None
                    };
                    for row in 0..plan.height as u64 {
                        let src_start = row as usize * padded_bpr_usize;
                        let src_end =
                            src_start.checked_add(unpadded_bpr_usize).ok_or_else(|| {
                                anyhow!("COPY_TEXTURE2D: src row end overflows usize")
                            })?;
                        let row_bytes = match owned.as_ref() {
                            Some(bytes) => bytes.get(src_start..src_end),
                            None => mapped.get(src_start..src_end),
                        }
                        .ok_or_else(|| {
                            anyhow!("COPY_TEXTURE2D: writeback staging buffer too small")
                        })?;
                        let dst_gpa = plan
                            .base_gpa
                            .checked_add(row.checked_mul(plan.row_pitch).ok_or_else(|| {
                                anyhow!("COPY_TEXTURE2D: dst GPA overflow (row pitch mul)")
                            })?)
                            .ok_or_else(|| {
                                anyhow!("COPY_TEXTURE2D: dst GPA overflow (row pitch add)")
                            })?;
                        guest_mem
                            .write(dst_gpa, row_bytes)
                            .map_err(anyhow_guest_mem)?;
                    }
                    drop(mapped);
                    staging.unmap();
                }
            }
        }
        Ok(())
    }

    fn submit_encoder(&mut self, encoder: &mut wgpu::CommandEncoder, label: &'static str) {
        let new_encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some(label) });
        let finished = std::mem::replace(encoder, new_encoder).finish();
        self.queue.submit([finished]);
        self.encoder_has_commands = false;
        self.encoder_used_buffers.clear();
        self.encoder_used_textures.clear();
    }

    fn submit_encoder_if_has_commands(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        label: &'static str,
    ) {
        if !self.encoder_has_commands {
            return;
        }
        self.submit_encoder(encoder, label);
    }

    #[allow(clippy::too_many_arguments)]
    fn exec_non_draw_command(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        opcode: u32,
        cmd_bytes: &[u8],
        allocs: &AllocTable,
        guest_mem: &mut dyn GuestMemory,
        pending_writebacks: &mut Vec<PendingWriteback>,
        report: &mut ExecuteReport,
    ) -> Result<()> {
        match opcode {
            OPCODE_NOP => Ok(()),
            OPCODE_DEBUG_MARKER => Ok(()),
            OPCODE_CREATE_BUFFER => self.exec_create_buffer(cmd_bytes, allocs),
            OPCODE_CREATE_TEXTURE2D => self.exec_create_texture2d(cmd_bytes, allocs),
            OPCODE_DESTROY_RESOURCE => self.exec_destroy_resource(cmd_bytes),
            OPCODE_RESOURCE_DIRTY_RANGE => self.exec_resource_dirty_range(cmd_bytes),
            OPCODE_UPLOAD_RESOURCE => self.exec_upload_resource(encoder, cmd_bytes),
            OPCODE_COPY_BUFFER => {
                self.exec_copy_buffer(encoder, cmd_bytes, allocs, guest_mem, pending_writebacks)
            }
            OPCODE_COPY_TEXTURE2D => {
                self.exec_copy_texture2d(encoder, cmd_bytes, allocs, guest_mem, pending_writebacks)
            }
            OPCODE_CREATE_SHADER_DXBC => self.exec_create_shader_dxbc(cmd_bytes),
            OPCODE_DESTROY_SHADER => self.exec_destroy_shader(cmd_bytes),
            OPCODE_BIND_SHADERS => self.exec_bind_shaders(cmd_bytes),
            OPCODE_SET_SHADER_CONSTANTS_F => self.exec_set_shader_constants_f(encoder, cmd_bytes),
            OPCODE_CREATE_INPUT_LAYOUT => self.exec_create_input_layout(cmd_bytes),
            OPCODE_DESTROY_INPUT_LAYOUT => self.exec_destroy_input_layout(cmd_bytes),
            OPCODE_SET_INPUT_LAYOUT => self.exec_set_input_layout(cmd_bytes),
            OPCODE_SET_RENDER_TARGETS => self.exec_set_render_targets(cmd_bytes),
            OPCODE_SET_VIEWPORT => self.exec_set_viewport(cmd_bytes),
            OPCODE_SET_SCISSOR => self.exec_set_scissor(cmd_bytes),
            OPCODE_SET_VERTEX_BUFFERS => self.exec_set_vertex_buffers(cmd_bytes),
            OPCODE_SET_INDEX_BUFFER => self.exec_set_index_buffer(cmd_bytes),
            OPCODE_SET_PRIMITIVE_TOPOLOGY => self.exec_set_primitive_topology(cmd_bytes),
            OPCODE_SET_TEXTURE => self.exec_set_texture(cmd_bytes),
            OPCODE_SET_SAMPLER_STATE => self.exec_set_sampler_state(cmd_bytes),
            OPCODE_CREATE_SAMPLER => self.exec_create_sampler(cmd_bytes),
            OPCODE_DESTROY_SAMPLER => self.exec_destroy_sampler(cmd_bytes),
            OPCODE_SET_SAMPLERS => self.exec_set_samplers(cmd_bytes),
            OPCODE_SET_CONSTANT_BUFFERS => self.exec_set_constant_buffers(cmd_bytes),
            OPCODE_CLEAR => self.exec_clear(encoder, cmd_bytes, allocs, guest_mem),
            OPCODE_PRESENT => self.exec_present(encoder, cmd_bytes, report),
            OPCODE_PRESENT_EX => self.exec_present_ex(encoder, cmd_bytes, report),
            OPCODE_EXPORT_SHARED_SURFACE => self.exec_export_shared_surface(cmd_bytes),
            OPCODE_IMPORT_SHARED_SURFACE => self.exec_import_shared_surface(cmd_bytes),
            OPCODE_RELEASE_SHARED_SURFACE => self.exec_release_shared_surface(cmd_bytes),
            OPCODE_FLUSH => self.exec_flush(encoder),
            // Known-but-ignored state that should not crash bring-up.
            OPCODE_SET_RENDER_STATE => Ok(()),
            OPCODE_SET_BLEND_STATE => self.exec_set_blend_state(cmd_bytes),
            OPCODE_SET_DEPTH_STENCIL_STATE => self.exec_set_depth_stencil_state(cmd_bytes),
            OPCODE_SET_RASTERIZER_STATE => self.exec_set_rasterizer_state(cmd_bytes),
            _ => {
                report.unknown_opcodes = report.unknown_opcodes.saturating_add(1);
                Ok(())
            }
        }
    }

    /// Execute a batch of draw/state commands inside a single render pass.
    ///
    /// This function is entered when the main stream parser sees a `DRAW`/`DRAW_INDEXED`
    /// opcode while no render pass is active. It begins a pass with `LoadOp::Load`,
    /// then continues consuming subsequent commands until a pass-ending opcode is
    /// reached (SET_RENDER_TARGETS, CLEAR, PRESENT, FLUSH, ...).
    #[allow(clippy::too_many_arguments)]
    fn exec_render_pass_load<'a>(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        stream: &mut CmdStreamCtx<'a, '_>,
        allocs: &AllocTable,
        guest_mem: &mut dyn GuestMemory,
        report: &mut ExecuteReport,
    ) -> Result<()> {
        if self.state.render_targets.is_empty() {
            bail!("aerogpu_cmd: draw without bound render target");
        }

        let render_targets = self.state.render_targets.clone();
        let depth_stencil = self.state.depth_stencil;
        for &handle in &render_targets {
            self.ensure_texture_uploaded(encoder, handle, allocs, guest_mem)?;
        }
        if let Some(handle) = depth_stencil {
            self.ensure_texture_uploaded(encoder, handle, allocs, guest_mem)?;
        }

        // Upload any dirty resources used by the current input assembler bindings.
        let mut ia_buffers: Vec<u32> = self
            .state
            .vertex_buffers
            .iter()
            .flatten()
            .map(|b| b.buffer)
            .collect();
        if let Some(ib) = self.state.index_buffer {
            ia_buffers.push(ib.buffer);
        }
        for handle in ia_buffers {
            self.ensure_buffer_uploaded(encoder, handle, allocs, guest_mem)?;
        }

        // The upcoming render pass will write to bound targets. Invalidate any CPU shadow copies so
        // that later partial `UPLOAD_RESOURCE` operations don't accidentally overwrite GPU-produced
        // contents.
        for &handle in &render_targets {
            if let Some(tex) = self.resources.textures.get_mut(&handle) {
                tex.host_shadow = None;
            }
        }
        if let Some(handle) = depth_stencil {
            if let Some(tex) = self.resources.textures.get_mut(&handle) {
                tex.host_shadow = None;
            }
        }
        let vs_handle = self
            .state
            .vs
            .ok_or_else(|| anyhow!("render draw without bound VS"))?;
        let ps_handle = self
            .state
            .ps
            .ok_or_else(|| anyhow!("render draw without bound PS"))?;
        // Clone shader metadata out of `self.resources` to avoid holding immutable borrows across
        // the rest of the render-pass recording path.
        let vs = self
            .resources
            .shaders
            .get(&vs_handle)
            .ok_or_else(|| anyhow!("unknown VS shader {vs_handle}"))?
            .clone();
        let ps = self
            .resources
            .shaders
            .get(&ps_handle)
            .ok_or_else(|| anyhow!("unknown PS shader {ps_handle}"))?
            .clone();
        if vs.stage != ShaderStage::Vertex {
            bail!("shader {vs_handle} is not a vertex shader");
        }
        if ps.stage != ShaderStage::Pixel {
            bail!("shader {ps_handle} is not a pixel shader");
        }

        let mut pipeline_bindings = reflection_bindings::build_pipeline_bindings_info(
            &self.device,
            &mut self.bind_group_layout_cache,
            [vs.reflection.bindings.as_slice(), ps.reflection.bindings.as_slice()],
        )?;

        // `PipelineLayoutKey` is used both for pipeline-layout caching and as part of the pipeline
        // cache key. Avoid cloning the underlying Vec by moving it out of `pipeline_bindings`.
        let layout_key =
            std::mem::replace(&mut pipeline_bindings.layout_key, PipelineLayoutKey::empty());

        let pipeline_layout = {
            let device = &self.device;
            let cache = &mut self.pipeline_layout_cache;
            cache.get_or_create_with(&layout_key, || {
                let layout_refs: Vec<&wgpu::BindGroupLayout> = pipeline_bindings
                    .group_layouts
                    .iter()
                    .map(|l| l.layout.as_ref())
                    .collect();
                Arc::new(device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("aerogpu_cmd pipeline layout"),
                    bind_group_layouts: &layout_refs,
                    push_constant_ranges: &[],
                }))
            })
        };

        // Ensure any guest-backed resources referenced by the current binding state are uploaded
        // before entering the render pass.
        self.ensure_bound_resources_uploaded(encoder, &pipeline_bindings, allocs, guest_mem)?;

        // `PipelineCache` returns a reference tied to the mutable borrow. Convert it to a raw
        // pointer so we can continue mutating unrelated executor state while the render pass is
        // alive.
        let (pipeline_ptr, wgpu_slot_to_d3d_slot) = {
            let (_pipeline_key, pipeline, wgpu_slot_to_d3d_slot) =
                get_or_create_render_pipeline_for_state(
                    &self.device,
                    &mut self.pipeline_cache,
                    pipeline_layout.as_ref(),
                    &mut self.resources,
                    &self.state,
                    layout_key,
                )?;
            (
                pipeline as *const wgpu::RenderPipeline,
                wgpu_slot_to_d3d_slot,
            )
        };
        let pipeline = unsafe { &*pipeline_ptr };

        // Create local texture views so we can continue mutating `self` while the render pass is
        // active.
        let mut color_views: Vec<wgpu::TextureView> =
            Vec::with_capacity(self.state.render_targets.len());
        for &tex_id in &self.state.render_targets {
            let tex = self
                .resources
                .textures
                .get(&tex_id)
                .ok_or_else(|| anyhow!("unknown render target texture {tex_id}"))?;
            color_views.push(
                tex.texture
                    .create_view(&wgpu::TextureViewDescriptor::default()),
            );
        }
        let depth_stencil_view: Option<(wgpu::TextureView, wgpu::TextureFormat)> =
            if let Some(ds_id) = self.state.depth_stencil {
                let tex = self
                    .resources
                    .textures
                    .get(&ds_id)
                    .ok_or_else(|| anyhow!("unknown depth stencil texture {ds_id}"))?;
                Some((
                    tex.texture
                        .create_view(&wgpu::TextureViewDescriptor::default()),
                    tex.desc.format,
                ))
            } else {
                None
            };

        let mut color_attachments: Vec<Option<wgpu::RenderPassColorAttachment<'_>>> =
            Vec::with_capacity(color_views.len());
        for view in &color_views {
            color_attachments.push(Some(wgpu::RenderPassColorAttachment {
                view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            }));
        }

        let depth_stencil_attachment = depth_stencil_view.as_ref().map(|(view, format)| {
            wgpu::RenderPassDepthStencilAttachment {
                view,
                depth_ops: texture_format_has_depth(*format).then_some(wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: texture_format_has_stencil(*format).then_some(wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                }),
            }
        });

        self.encoder_has_commands = true;
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("aerogpu_cmd render pass"),
            color_attachments: &color_attachments,
            depth_stencil_attachment,
            timestamp_writes: None,
            occlusion_query_set: None,
        });

        // Apply dynamic state once at pass start.
        let rt_dims = self
            .state
            .render_targets
            .first()
            .and_then(|rt| self.resources.textures.get(rt))
            .map(|tex| (tex.desc.width, tex.desc.height));

        if let Some(vp) = self.state.viewport {
            if vp.x.is_finite()
                && vp.y.is_finite()
                && vp.width.is_finite()
                && vp.height.is_finite()
                && vp.min_depth.is_finite()
                && vp.max_depth.is_finite()
            {
                if let Some((rt_w, rt_h)) = rt_dims {
                    let max_w = rt_w as f32;
                    let max_h = rt_h as f32;

                    let left = vp.x.max(0.0);
                    let top = vp.y.max(0.0);
                    let right = (vp.x + vp.width).max(0.0).min(max_w);
                    let bottom = (vp.y + vp.height).max(0.0).min(max_h);
                    let width = (right - left).max(0.0);
                    let height = (bottom - top).max(0.0);

                    if width > 0.0 && height > 0.0 {
                        let mut min_depth = vp.min_depth.clamp(0.0, 1.0);
                        let mut max_depth = vp.max_depth.clamp(0.0, 1.0);
                        if min_depth > max_depth {
                            std::mem::swap(&mut min_depth, &mut max_depth);
                        }
                        pass.set_viewport(left, top, width, height, min_depth, max_depth);
                    }
                } else {
                    pass.set_viewport(vp.x, vp.y, vp.width, vp.height, vp.min_depth, vp.max_depth);
                }
            }
        }

        if self.state.scissor_enable {
            if let Some(sc) = self.state.scissor {
                if let Some((rt_w, rt_h)) = rt_dims {
                    let x = sc.x.min(rt_w);
                    let y = sc.y.min(rt_h);
                    let width = sc.width.min(rt_w.saturating_sub(x));
                    let height = sc.height.min(rt_h.saturating_sub(y));
                    if width > 0 && height > 0 {
                        pass.set_scissor_rect(x, y, width, height);
                    }
                }
            }
        }

        pass.set_blend_constant(wgpu::Color {
            r: self.state.blend_constant[0] as f64,
            g: self.state.blend_constant[1] as f64,
            b: self.state.blend_constant[2] as f64,
            a: self.state.blend_constant[3] as f64,
        });

        pass.set_pipeline(pipeline);

        for (wgpu_slot, d3d_slot) in wgpu_slot_to_d3d_slot.iter().copied().enumerate() {
            let slot = d3d_slot as usize;
            let Some(vb) = self.state.vertex_buffers.get(slot).and_then(|v| *v) else {
                bail!("input layout requires vertex buffer slot {d3d_slot}");
            };
            // Safety: buffers are not created/destroyed while a render pass is active (the render
            // pass loop breaks on any resource-management opcode), so pointers into the buffer
            // table remain valid until `pass` is dropped.
            let (buf_ptr, buf_size): (*const wgpu::Buffer, u64) = {
                let buf = self
                    .resources
                    .buffers
                    .get(&vb.buffer)
                    .ok_or_else(|| anyhow!("unknown vertex buffer {}", vb.buffer))?;
                (&buf.buffer as *const wgpu::Buffer, buf.size)
            };
            if vb.offset_bytes > buf_size {
                bail!(
                    "vertex buffer {} offset {} out of bounds (size={})",
                    vb.buffer,
                    vb.offset_bytes,
                    buf_size
                );
            }
            let buf = unsafe { &*buf_ptr };
            pass.set_vertex_buffer(wgpu_slot as u32, buf.slice(vb.offset_bytes..));
        }
        if let Some(ib) = self.state.index_buffer {
            let (buf_ptr, buf_size): (*const wgpu::Buffer, u64) = {
                let buf = self
                    .resources
                    .buffers
                    .get(&ib.buffer)
                    .ok_or_else(|| anyhow!("unknown index buffer {}", ib.buffer))?;
                (&buf.buffer as *const wgpu::Buffer, buf.size)
            };
            if ib.offset_bytes > buf_size {
                bail!(
                    "index buffer {} offset {} out of bounds (size={})",
                    ib.buffer,
                    ib.offset_bytes,
                    buf_size
                );
            }
            let buf = unsafe { &*buf_ptr };
            pass.set_index_buffer(buf.slice(ib.offset_bytes..), ib.format);
        }

        // Bind groups referenced by `set_bind_group` must remain alive for the entire render pass.
        let mut bind_group_arena: Vec<Arc<wgpu::BindGroup>> = Vec::new();
        let mut current_bind_groups: Vec<Option<*const wgpu::BindGroup>> =
            vec![None; pipeline_bindings.group_layouts.len()];
        let mut bound_bind_groups: Vec<Option<*const wgpu::BindGroup>> =
            vec![None; pipeline_bindings.group_layouts.len()];

        let mut d3d_slot_to_wgpu_slot: Vec<Option<u32>> = vec![None; DEFAULT_MAX_VERTEX_SLOTS];
        let mut used_vertex_slots = [false; DEFAULT_MAX_VERTEX_SLOTS];
        for (wgpu_slot, &d3d_slot) in wgpu_slot_to_d3d_slot.iter().enumerate() {
            let slot_usize: usize = d3d_slot
                .try_into()
                .map_err(|_| anyhow!("wgpu vertex slot out of range"))?;
            if slot_usize < d3d_slot_to_wgpu_slot.len() {
                d3d_slot_to_wgpu_slot[slot_usize] = Some(wgpu_slot as u32);
                used_vertex_slots[slot_usize] = true;
            }
        }

        // Precompute which binding slots are actually referenced by the current shader pair so we
        // can avoid breaking the render pass for state updates that will not be read by any draw
        // in this pass.
        let mut used_textures_vs = vec![false; DEFAULT_MAX_TEXTURE_SLOTS];
        let mut used_textures_ps = vec![false; DEFAULT_MAX_TEXTURE_SLOTS];
        let mut used_textures_cs = vec![false; DEFAULT_MAX_TEXTURE_SLOTS];
        let mut used_cb_vs = vec![false; DEFAULT_MAX_CONSTANT_BUFFER_SLOTS];
        let mut used_cb_ps = vec![false; DEFAULT_MAX_CONSTANT_BUFFER_SLOTS];
        let mut used_cb_cs = vec![false; DEFAULT_MAX_CONSTANT_BUFFER_SLOTS];
        for (group_index, group_bindings) in pipeline_bindings.group_bindings.iter().enumerate() {
            let stage = group_index_to_stage(group_index as u32)?;
            for binding in group_bindings {
                match &binding.kind {
                    crate::BindingKind::ConstantBuffer { slot, .. } => {
                        let slot_usize = *slot as usize;
                        let used = match stage {
                            ShaderStage::Vertex => used_cb_vs.get_mut(slot_usize),
                            ShaderStage::Pixel => used_cb_ps.get_mut(slot_usize),
                            ShaderStage::Compute => used_cb_cs.get_mut(slot_usize),
                        };
                        if let Some(entry) = used {
                            *entry = true;
                        }
                    }
                    crate::BindingKind::Texture2D { slot } => {
                        let slot_usize = *slot as usize;
                        let used = match stage {
                            ShaderStage::Vertex => used_textures_vs.get_mut(slot_usize),
                            ShaderStage::Pixel => used_textures_ps.get_mut(slot_usize),
                            ShaderStage::Compute => used_textures_cs.get_mut(slot_usize),
                        };
                        if let Some(entry) = used {
                            *entry = true;
                        }
                    }
                    crate::BindingKind::Sampler { .. } => {}
                }
            }
        }

        // Tracks whether any previous draw in this render pass actually used the legacy constants
        // uniform buffer for a given stage. If it has, we cannot safely apply
        // `SET_SHADER_CONSTANTS_F` via `queue.write_buffer` without reordering it ahead of the
        // earlier draw commands.
        let mut legacy_constants_used = [false; 3];
        for &handle in &render_targets {
            self.encoder_used_textures.insert(handle);
        }
        if let Some(handle) = depth_stencil {
            self.encoder_used_textures.insert(handle);
        }

        loop {
            let Some(next) = stream.iter.peek() else {
                break;
            };
            let (cmd_size, opcode) = match next {
                Ok(packet) => (packet.hdr.size_bytes as usize, packet.hdr.opcode),
                Err(err) => {
                    return Err(anyhow!(
                        "aerogpu_cmd: invalid cmd header @0x{:x}: {err:?}",
                        *stream.cursor
                    ));
                }
            };

            match opcode {
                OPCODE_DRAW
                | OPCODE_DRAW_INDEXED
                | OPCODE_CREATE_BUFFER
                | OPCODE_CREATE_TEXTURE2D
                | OPCODE_DESTROY_RESOURCE
                | OPCODE_RESOURCE_DIRTY_RANGE
                | OPCODE_UPLOAD_RESOURCE
                | OPCODE_COPY_BUFFER
                | OPCODE_COPY_TEXTURE2D
                | OPCODE_CREATE_SAMPLER
                | OPCODE_DESTROY_SAMPLER
                | OPCODE_CREATE_SHADER_DXBC
                | OPCODE_DESTROY_SHADER
                | OPCODE_BIND_SHADERS
                | OPCODE_SET_SHADER_CONSTANTS_F
                | OPCODE_CREATE_INPUT_LAYOUT
                | OPCODE_DESTROY_INPUT_LAYOUT
                | OPCODE_SET_INPUT_LAYOUT
                | OPCODE_SET_RENDER_TARGETS
                | OPCODE_SET_BLEND_STATE
                | OPCODE_SET_DEPTH_STENCIL_STATE
                | OPCODE_SET_RASTERIZER_STATE
                | OPCODE_SET_VIEWPORT
                | OPCODE_SET_SCISSOR
                | OPCODE_SET_VERTEX_BUFFERS
                | OPCODE_SET_INDEX_BUFFER
                | OPCODE_SET_PRIMITIVE_TOPOLOGY
                | OPCODE_SET_TEXTURE
                | OPCODE_SET_SAMPLER_STATE
                | OPCODE_SET_RENDER_STATE
                | OPCODE_SET_SAMPLERS
                | OPCODE_SET_CONSTANT_BUFFERS
                | OPCODE_CLEAR
                | OPCODE_NOP
                | OPCODE_DEBUG_MARKER => {}
                _ => break, // leave the opcode for the outer loop
            }

            let cmd_end = (*stream.cursor)
                .checked_add(cmd_size)
                .ok_or_else(|| anyhow!("aerogpu_cmd: cmd size overflow"))?;
            let cmd_bytes = stream
                .bytes
                .get(*stream.cursor..cmd_end)
                .ok_or_else(|| {
                    anyhow!(
                        "aerogpu_cmd: cmd overruns stream: cursor=0x{:x} cmd_size=0x{:x} stream_size=0x{:x}",
                        *stream.cursor,
                        cmd_size,
                        stream.size
                    )
                })?;

            // Some binding/state updates can be applied inside a render pass only when they do not
            // require any implicit resource uploads (which must be encoded outside the pass).
            //
            // Pipeline-affecting state is only safe when the update is a no-op for the current
            // pipeline. If the update would change the pipeline, we must end the pass so the outer
            // loop can rebuild it.
            if opcode == OPCODE_BIND_SHADERS {
                // `struct aerogpu_cmd_bind_shaders` (24 bytes)
                if cmd_bytes.len() >= 20 {
                    let vs = read_u32_le(cmd_bytes, 8)?;
                    let ps = read_u32_le(cmd_bytes, 12)?;
                    let next_vs = if vs == 0 { None } else { Some(vs) };
                    let next_ps = if ps == 0 { None } else { Some(ps) };
                    if next_vs != self.state.vs || next_ps != self.state.ps {
                        break;
                    }
                } else {
                    break;
                }
            }

            if opcode == OPCODE_SET_INPUT_LAYOUT {
                // `struct aerogpu_cmd_set_input_layout` (16 bytes)
                if cmd_bytes.len() >= 12 {
                    let handle = read_u32_le(cmd_bytes, 8)?;
                    let next = if handle == 0 { None } else { Some(handle) };
                    if next != self.state.input_layout {
                        break;
                    }
                } else {
                    break;
                }
            }

            if opcode == OPCODE_DESTROY_INPUT_LAYOUT {
                // `DESTROY_INPUT_LAYOUT` clears the currently bound layout if it matches. That
                // affects pipeline creation, so we can only execute it inside the pass if it
                // doesn't touch the active layout.
                if cmd_bytes.len() < 16 {
                    break;
                }
                let handle = read_u32_le(cmd_bytes, 8)?;
                if self.state.input_layout == Some(handle) {
                    break;
                }
            }

            if opcode == OPCODE_SET_RENDER_TARGETS {
                // `struct aerogpu_cmd_set_render_targets` (48 bytes)
                if cmd_bytes.len() < 48 {
                    break;
                }
                let color_count = read_u32_le(cmd_bytes, 8)? as usize;
                let depth_stencil = read_u32_le(cmd_bytes, 12)?;
                if color_count > 8 {
                    break;
                }

                let mut colors = Vec::with_capacity(color_count);
                let mut seen_gap = false;
                let mut invalid_gap = false;
                for i in 0..color_count {
                    let tex_id = read_u32_le(cmd_bytes, 16 + i * 4)?;
                    if tex_id == 0 {
                        seen_gap = true;
                        continue;
                    }
                    if seen_gap {
                        invalid_gap = true;
                        break;
                    }
                    colors.push(tex_id);
                }
                if invalid_gap {
                    break;
                }

                let depth_stencil = if depth_stencil == 0 {
                    None
                } else {
                    Some(depth_stencil)
                };

                // Render targets cannot be changed inside a render pass; allow no-ops so redundant
                // binds don't force a restart.
                if colors != self.state.render_targets || depth_stencil != self.state.depth_stencil
                {
                    break;
                }
            }

            if opcode == OPCODE_CLEAR {
                // CLEAR requires ending the pass unless it is a no-op (flags == 0).
                if cmd_bytes.len() < 12 {
                    break;
                }
                let flags = read_u32_le(cmd_bytes, 8)?;
                if flags != 0 {
                    break;
                }
            }

            if opcode == OPCODE_UPLOAD_RESOURCE {
                // UPLOAD_RESOURCE can be applied inside an active render pass via `queue.write_*`
                // only when the upload can be reordered ahead of the eventual command-buffer
                // submission without changing the behavior of any previously recorded GPU work.
                if cmd_bytes.len() < 32 {
                    break;
                }
                let handle = self.shared_surfaces.resolve_handle(read_u32_le(cmd_bytes, 8)?);
                let size_bytes = read_u64_le(cmd_bytes, 24)?;
                if size_bytes != 0 {
                    if self.resources.buffers.contains_key(&handle) {
                        if self.encoder_used_buffers.contains(&handle) {
                            break;
                        }
                    } else if self.resources.textures.contains_key(&handle) {
                        if self.encoder_used_textures.contains(&handle) {
                            break;
                        }
                    } else {
                        // Unknown handle; treat as a no-op for robustness.
                    }
                }
            }

            if opcode == OPCODE_COPY_BUFFER {
                // COPY_BUFFER requires ending the pass, unless it is a no-op (size_bytes == 0).
                if cmd_bytes.len() < 48 {
                    break;
                }
                let size_bytes = read_u64_le(cmd_bytes, 32)?;
                if size_bytes != 0 {
                    break;
                }
            }

            if opcode == OPCODE_COPY_TEXTURE2D {
                // COPY_TEXTURE2D requires ending the pass, unless it is a no-op (width==0 ||
                // height==0).
                if cmd_bytes.len() < 64 {
                    break;
                }
                let width = read_u32_le(cmd_bytes, 48)?;
                let height = read_u32_le(cmd_bytes, 52)?;
                if width != 0 && height != 0 {
                    break;
                }
            }

            if opcode == OPCODE_DESTROY_RESOURCE {
                // `DESTROY_RESOURCE` can be applied mid-pass only when it does not affect any
                // resource currently used by the pass. Otherwise, we'd risk drawing with missing
                // resources (or destroying an active attachment).
                if cmd_bytes.len() < 16 {
                    break;
                }
                let handle = self.shared_surfaces.resolve_handle(read_u32_le(cmd_bytes, 8)?);
                let mut needs_break = false;

                if render_targets.contains(&handle) || depth_stencil.is_some_and(|ds| ds == handle)
                {
                    needs_break = true;
                }

                if !needs_break && self.resources.buffers.contains_key(&handle) {
                    for (slot, vb) in self.state.vertex_buffers.iter().enumerate() {
                        if used_vertex_slots.get(slot).is_some_and(|used| *used)
                            && vb.is_some_and(|vb| vb.buffer == handle)
                        {
                            needs_break = true;
                            break;
                        }
                    }
                    if !needs_break
                        && self
                            .state
                            .index_buffer
                            .is_some_and(|ib| ib.buffer == handle)
                    {
                        needs_break = true;
                    }
                    if !needs_break {
                        for (stage, used_slots) in [
                            (ShaderStage::Vertex, &used_cb_vs),
                            (ShaderStage::Pixel, &used_cb_ps),
                            (ShaderStage::Compute, &used_cb_cs),
                        ] {
                            let stage_bindings = self.bindings.stage(stage);
                            for (slot, used) in used_slots.iter().copied().enumerate() {
                                if !used {
                                    continue;
                                }
                                if stage_bindings
                                    .constant_buffer(slot as u32)
                                    .is_some_and(|cb| cb.buffer == handle)
                                {
                                    needs_break = true;
                                    break;
                                }
                            }
                            if needs_break {
                                break;
                            }
                        }
                    }
                }

                if !needs_break && self.resources.textures.contains_key(&handle) {
                    for (stage, used_slots) in [
                        (ShaderStage::Vertex, &used_textures_vs),
                        (ShaderStage::Pixel, &used_textures_ps),
                        (ShaderStage::Compute, &used_textures_cs),
                    ] {
                        let stage_bindings = self.bindings.stage(stage);
                        for (slot, used) in used_slots.iter().copied().enumerate() {
                            if !used {
                                continue;
                            }
                            if stage_bindings
                                .texture(slot as u32)
                                .is_some_and(|tex| tex.texture == handle)
                            {
                                needs_break = true;
                                break;
                            }
                        }
                        if needs_break {
                            break;
                        }
                    }
                }

                if needs_break {
                    break;
                }
            }

            if opcode == OPCODE_RESOURCE_DIRTY_RANGE {
                // `RESOURCE_DIRTY_RANGE` marks allocation-backed resources as requiring a
                // guest->GPU upload. It can be recorded inside an in-flight render pass only when
                // the resource is not currently used by the pass, otherwise we would draw with
                // stale data (or need to upload mid-pass).
                if cmd_bytes.len() < 32 {
                    break;
                }
                let handle = read_u32_le(cmd_bytes, 8)?;
                let mut needs_break = false;

                let buffer_backing = self
                    .resources
                    .buffers
                    .get(&handle)
                    .and_then(|buf| buf.backing);
                if buffer_backing.is_some() {
                    for (slot, vb) in self.state.vertex_buffers.iter().enumerate() {
                        if used_vertex_slots.get(slot).is_some_and(|used| *used)
                            && vb.is_some_and(|vb| vb.buffer == handle)
                        {
                            needs_break = true;
                            break;
                        }
                    }
                    if !needs_break
                        && self
                            .state
                            .index_buffer
                            .is_some_and(|ib| ib.buffer == handle)
                    {
                        needs_break = true;
                    }
                    if !needs_break {
                        for (stage, used_slots) in [
                            (ShaderStage::Vertex, &used_cb_vs),
                            (ShaderStage::Pixel, &used_cb_ps),
                            (ShaderStage::Compute, &used_cb_cs),
                        ] {
                            let stage_bindings = self.bindings.stage(stage);
                            for (slot, used) in used_slots.iter().copied().enumerate() {
                                if !used {
                                    continue;
                                }
                                if stage_bindings
                                    .constant_buffer(slot as u32)
                                    .is_some_and(|cb| cb.buffer == handle)
                                {
                                    needs_break = true;
                                    break;
                                }
                            }
                            if needs_break {
                                break;
                            }
                        }
                    }
                }

                let texture_backing = self
                    .resources
                    .textures
                    .get(&handle)
                    .and_then(|tex| tex.backing);
                if !needs_break && texture_backing.is_some() {
                    if render_targets.contains(&handle)
                        || depth_stencil.is_some_and(|ds| ds == handle)
                    {
                        needs_break = true;
                    }
                    if !needs_break {
                        for (stage, used_slots) in [
                            (ShaderStage::Vertex, &used_textures_vs),
                            (ShaderStage::Pixel, &used_textures_ps),
                            (ShaderStage::Compute, &used_textures_cs),
                        ] {
                            let stage_bindings = self.bindings.stage(stage);
                            for (slot, used) in used_slots.iter().copied().enumerate() {
                                if !used {
                                    continue;
                                }
                                if stage_bindings
                                    .texture(slot as u32)
                                    .is_some_and(|tex| tex.texture == handle)
                                {
                                    needs_break = true;
                                    break;
                                }
                            }
                            if needs_break {
                                break;
                            }
                        }
                    }
                }

                if needs_break {
                    break;
                }
            }

            if opcode == OPCODE_SET_SHADER_CONSTANTS_F {
                // `struct aerogpu_cmd_set_shader_constants_f` (24 bytes) + vec4 data.
                if cmd_bytes.len() < 24 {
                    break;
                }
                let stage_raw = read_u32_le(cmd_bytes, 8)?;
                let Some(stage) = ShaderStage::from_aerogpu_u32(stage_raw) else {
                    break;
                };
                let legacy_id = legacy_constants_buffer_id(stage);
                let stage_index = stage.as_bind_group_index() as usize;
                if (stage_index < legacy_constants_used.len() && legacy_constants_used[stage_index])
                    || self.encoder_used_buffers.contains(&legacy_id)
                {
                    break;
                }
            }

            if opcode == OPCODE_SET_PRIMITIVE_TOPOLOGY {
                // `struct aerogpu_cmd_set_primitive_topology` (16 bytes)
                if cmd_bytes.len() >= 12 {
                    let topology_u32 = read_u32_le(cmd_bytes, 8)?;
                    let next = match topology_u32 {
                        1 => wgpu::PrimitiveTopology::PointList,
                        2 => wgpu::PrimitiveTopology::LineList,
                        3 => wgpu::PrimitiveTopology::LineStrip,
                        4 => wgpu::PrimitiveTopology::TriangleList,
                        5 => wgpu::PrimitiveTopology::TriangleStrip,
                        6 => wgpu::PrimitiveTopology::TriangleList, // TriangleFan fallback
                        _ => break,
                    };
                    if next != self.state.primitive_topology {
                        break;
                    }
                } else {
                    break;
                }
            }

            if opcode == OPCODE_SET_BLEND_STATE {
                // `struct aerogpu_cmd_set_blend_state` (28 bytes minimum; extended in newer ABI
                // versions).
                if cmd_bytes.len() < 28 {
                    break;
                }
                let enable = read_u32_le(cmd_bytes, 8)? != 0;
                let src_factor = read_u32_le(cmd_bytes, 12)?;
                let dst_factor = read_u32_le(cmd_bytes, 16)?;
                let op = read_u32_le(cmd_bytes, 20)?;
                let write_mask = cmd_bytes[24];

                let next_write_mask = map_color_write_mask(write_mask);
                let src_factor_alpha = if cmd_bytes.len() >= 32 {
                    read_u32_le(cmd_bytes, 28)?
                } else {
                    src_factor
                };
                let dst_factor_alpha = if cmd_bytes.len() >= 36 {
                    read_u32_le(cmd_bytes, 32)?
                } else {
                    dst_factor
                };
                let op_alpha = if cmd_bytes.len() >= 40 {
                    read_u32_le(cmd_bytes, 36)?
                } else {
                    op
                };

                let next_blend = if !enable {
                    None
                } else {
                    let src = map_blend_factor(src_factor).unwrap_or(wgpu::BlendFactor::One);
                    let dst = map_blend_factor(dst_factor).unwrap_or(wgpu::BlendFactor::Zero);
                    let op = map_blend_op(op).unwrap_or(wgpu::BlendOperation::Add);

                    let src_a = map_blend_factor(src_factor_alpha).unwrap_or(src);
                    let dst_a = map_blend_factor(dst_factor_alpha).unwrap_or(dst);
                    let op_a = map_blend_op(op_alpha).unwrap_or(op);

                    Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: src,
                            dst_factor: dst,
                            operation: op,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: src_a,
                            dst_factor: dst_a,
                            operation: op_a,
                        },
                    })
                };

                if next_blend != self.state.blend || next_write_mask != self.state.color_write_mask
                {
                    break;
                }
            }

            if opcode == OPCODE_SET_DEPTH_STENCIL_STATE {
                use aero_protocol::aerogpu::aerogpu_cmd::AerogpuCmdSetDepthStencilState;

                if cmd_bytes.len() < std::mem::size_of::<AerogpuCmdSetDepthStencilState>() {
                    break;
                }

                // Depth-stencil state affects pipeline creation only when a depth attachment is
                // bound for this pass. If no depth buffer is bound, updates are always safe.
                if self.state.depth_stencil.is_some() {
                    let cmd: AerogpuCmdSetDepthStencilState = read_packed_unaligned(cmd_bytes)?;
                    let state = cmd.state;

                    let next_depth_enable = u32::from_le(state.depth_enable) != 0;
                    let next_depth_write_enable = u32::from_le(state.depth_write_enable) != 0;
                    let next_depth_func = u32::from_le(state.depth_func);
                    let next_stencil_enable = u32::from_le(state.stencil_enable) != 0;

                    let next_depth_compare =
                        map_compare_func(next_depth_func).unwrap_or(wgpu::CompareFunction::Always);
                    let next_depth_compare = if next_depth_enable {
                        next_depth_compare
                    } else {
                        wgpu::CompareFunction::Always
                    };
                    let next_depth_write_enabled = next_depth_enable && next_depth_write_enable;

                    let (next_stencil_read_mask, next_stencil_write_mask) = if next_stencil_enable {
                        (
                            state.stencil_read_mask as u32,
                            state.stencil_write_mask as u32,
                        )
                    } else {
                        (0, 0)
                    };

                    let current_depth_compare = if self.state.depth_enable {
                        self.state.depth_compare
                    } else {
                        wgpu::CompareFunction::Always
                    };
                    let current_depth_write_enabled =
                        self.state.depth_enable && self.state.depth_write_enable;
                    let (current_stencil_read_mask, current_stencil_write_mask) =
                        if self.state.stencil_enable {
                            (
                                self.state.stencil_read_mask as u32,
                                self.state.stencil_write_mask as u32,
                            )
                        } else {
                            (0, 0)
                        };

                    if current_depth_compare != next_depth_compare
                        || current_depth_write_enabled != next_depth_write_enabled
                        || current_stencil_read_mask != next_stencil_read_mask
                        || current_stencil_write_mask != next_stencil_write_mask
                    {
                        break;
                    }
                }
            }

            if opcode == OPCODE_SET_RASTERIZER_STATE {
                use aero_protocol::aerogpu::aerogpu_cmd::AerogpuCmdSetRasterizerState;

                if cmd_bytes.len() < std::mem::size_of::<AerogpuCmdSetRasterizerState>() {
                    break;
                }

                let cmd: AerogpuCmdSetRasterizerState = read_packed_unaligned(cmd_bytes)?;
                let state = cmd.state;

                let cull_mode_raw = u32::from_le(state.cull_mode);
                let front_ccw = u32::from_le(state.front_ccw) != 0;
                let next_depth_bias = i32::from_le(state.depth_bias);
                let flags = u32::from_le(state.flags);
                let next_depth_clip_enabled =
                    flags & AEROGPU_RASTERIZER_FLAG_DEPTH_CLIP_DISABLE == 0;

                let next_cull_mode = match cull_mode_raw {
                    0 => None,
                    1 => Some(wgpu::Face::Front),
                    2 => Some(wgpu::Face::Back),
                    _ => self.state.cull_mode,
                };
                let next_front_face = if front_ccw {
                    wgpu::FrontFace::Ccw
                } else {
                    wgpu::FrontFace::Cw
                };

                if next_cull_mode != self.state.cull_mode
                    || next_front_face != self.state.front_face
                {
                    break;
                }

                if self.state.depth_stencil.is_some() && next_depth_bias != self.state.depth_bias {
                    break;
                }

                let current_vs_hash = if self.state.depth_clip_enabled {
                    vs.wgsl_hash
                } else {
                    vs.depth_clamp_wgsl_hash.unwrap_or(vs.wgsl_hash)
                };
                let next_vs_hash = if next_depth_clip_enabled {
                    vs.wgsl_hash
                } else {
                    vs.depth_clamp_wgsl_hash.unwrap_or(vs.wgsl_hash)
                };
                if current_vs_hash != next_vs_hash {
                    break;
                }
            }
            //
            // In particular, allocation-backed textures start life `dirty=true` and are uploaded
            // lazily on first use. If a dirty texture is bound via SET_TEXTURE between draws, we
            // must end the current pass if the texture has already been referenced by previously
            // recorded GPU commands (because `queue.write_texture` would otherwise reorder the
            // upload ahead of those commands).
            if opcode == OPCODE_SET_TEXTURE {
                // `struct aerogpu_cmd_set_texture` (24 bytes)
                if cmd_bytes.len() >= 20 {
                    let stage_raw = read_u32_le(cmd_bytes, 8)?;
                    let slot = read_u32_le(cmd_bytes, 12)?;
                    let texture = read_u32_le(cmd_bytes, 16)?;
                    if texture != 0 {
                        let texture = self.shared_surfaces.resolve_handle(texture);
                        let Some(stage) = ShaderStage::from_aerogpu_u32(stage_raw) else {
                            break;
                        };
                        let used_slots = match stage {
                            ShaderStage::Vertex => &used_textures_vs,
                            ShaderStage::Pixel => &used_textures_ps,
                            ShaderStage::Compute => &used_textures_cs,
                        };
                        let slot_usize: usize = slot
                            .try_into()
                            .map_err(|_| anyhow!("SET_TEXTURE: slot out of range"))?;
                        if slot_usize < used_slots.len() && used_slots[slot_usize] {
                            if let Some(tex) = self.resources.textures.get(&texture) {
                                if tex.dirty
                                    && tex.backing.is_some()
                                    && self.encoder_used_textures.contains(&texture)
                                {
                                    break;
                                }
                            }
                        }
                    }
                }
            }

            if opcode == OPCODE_SET_VERTEX_BUFFERS {
                let Ok((cmd, bindings)) = decode_cmd_set_vertex_buffers_bindings_le(cmd_bytes)
                else {
                    break;
                };
                let start_slot = cmd.start_slot as usize;

                let mut needs_break = false;
                for (i, binding) in bindings.iter().copied().enumerate() {
                    let slot = start_slot.saturating_add(i);
                    if slot >= used_vertex_slots.len() || !used_vertex_slots[slot] {
                        continue;
                    }

                    let buffer = u32::from_le(binding.buffer);
                    if buffer == 0 {
                        // The current pipeline requires this slot; allow the outer loop to restart
                        // the pass so we don't accidentally keep using the previous vertex buffer.
                        needs_break = true;
                        break;
                    }
                    let buffer = self.shared_surfaces.resolve_handle(buffer);

                    let stride_bytes = u32::from_le(binding.stride_bytes);
                    let current_stride = self
                        .state
                        .vertex_buffers
                        .get(slot)
                        .and_then(|v| *v)
                        .map(|vb| vb.stride_bytes)
                        .unwrap_or(0);
                    if current_stride != stride_bytes {
                        // Vertex buffer stride affects the pipeline's vertex buffer layout.
                        needs_break = true;
                        break;
                    }

                    if let Some(buf) = self.resources.buffers.get(&buffer) {
                        if buf.backing.is_some()
                            && buf.dirty.is_some()
                            && self.encoder_used_buffers.contains(&buffer)
                        {
                            needs_break = true;
                            break;
                        }
                    }
                }
                if needs_break {
                    break;
                }
            }

            if opcode == OPCODE_SET_INDEX_BUFFER {
                // `struct aerogpu_cmd_set_index_buffer` (24 bytes)
                    if cmd_bytes.len() >= 12 {
                        let buffer = read_u32_le(cmd_bytes, 8)?;
                        if buffer != 0 {
                            let buffer = self.shared_surfaces.resolve_handle(buffer);
                            if let Some(buf) = self.resources.buffers.get(&buffer) {
                                if buf.backing.is_some()
                                    && buf.dirty.is_some()
                                    && self.encoder_used_buffers.contains(&buffer)
                                {
                                break;
                            }
                        }
                    }
                } else {
                    break;
                }
            }

            if opcode == OPCODE_SET_CONSTANT_BUFFERS {
                // `struct aerogpu_cmd_set_constant_buffers` (24 bytes) + N bindings.
                //
                // Setting constant buffers inside a render pass is only safe when the newly bound
                // buffers do not require any implicit guest->GPU uploads and do not need a scratch
                // copy for unaligned uniform offsets.
                if cmd_bytes.len() >= 24 {
                    let stage_raw = read_u32_le(cmd_bytes, 8)?;
                    let start_slot = read_u32_le(cmd_bytes, 12)?;
                    let buffer_count_u32 = read_u32_le(cmd_bytes, 16)?;
                    let buffer_count: usize = buffer_count_u32
                        .try_into()
                        .map_err(|_| anyhow!("SET_CONSTANT_BUFFERS: buffer_count out of range"))?;
                    let expected = 24usize
                        .checked_add(
                            buffer_count
                                .checked_mul(16)
                                .ok_or_else(|| anyhow!("SET_CONSTANT_BUFFERS: size overflow"))?,
                        )
                        .ok_or_else(|| anyhow!("SET_CONSTANT_BUFFERS: size overflow"))?;
                    if cmd_bytes.len() >= expected {
                        let uniform_align =
                            self.device.limits().min_uniform_buffer_offset_alignment as u64;
                        let Some(stage) = ShaderStage::from_aerogpu_u32(stage_raw) else {
                            break;
                        };
                        let used_slots = match stage {
                            ShaderStage::Vertex => &used_cb_vs,
                            ShaderStage::Pixel => &used_cb_ps,
                            ShaderStage::Compute => &used_cb_cs,
                        };
                        let mut needs_break = false;
                        for i in 0..buffer_count {
                            let slot = start_slot
                                .checked_add(i as u32)
                                .ok_or_else(|| anyhow!("SET_CONSTANT_BUFFERS: slot overflow"))?;
                            let slot_usize: usize = slot
                                .try_into()
                                .map_err(|_| anyhow!("SET_CONSTANT_BUFFERS: slot out of range"))?;
                            if slot_usize >= used_slots.len() || !used_slots[slot_usize] {
                                continue;
                            }

                            let base = 24 + i * 16;
                            let buffer = read_u32_le(cmd_bytes, base)?;
                            let offset_bytes = read_u32_le(cmd_bytes, base + 4)? as u64;
                            if offset_bytes != 0 && !offset_bytes.is_multiple_of(uniform_align) {
                                needs_break = true;
                                break;
                            }
                            if buffer == 0 || buffer == legacy_constants_buffer_id(stage) {
                                continue;
                            }
                            let buffer = self.shared_surfaces.resolve_handle(buffer);
                            if let Some(buf) = self.resources.buffers.get(&buffer) {
                                if buf.backing.is_some()
                                    && buf.dirty.is_some()
                                    && self.encoder_used_buffers.contains(&buffer)
                                {
                                    needs_break = true;
                                    break;
                                }
                            }
                        }
                        if needs_break {
                            break;
                        }
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            }

            stream.iter.next().expect("peeked Some").map_err(|err| {
                anyhow!(
                    "aerogpu_cmd: invalid cmd header @0x{:x}: {err:?}",
                    *stream.cursor
                )
            })?;

            match opcode {
                OPCODE_DRAW => {
                    if (self.state.sample_mask & 1) != 0 {
                        for group_index in 0..pipeline_bindings.group_layouts.len() {
                            if pipeline_bindings.group_bindings[group_index].is_empty() {
                                if current_bind_groups[group_index].is_none() {
                                    let entries: [BindGroupCacheEntry<'_>; 0] = [];
                                    let bg = self.bind_group_cache.get_or_create(
                                        &self.device,
                                        &pipeline_bindings.group_layouts[group_index],
                                        &entries,
                                    );
                                    let ptr = Arc::as_ptr(&bg);
                                    bind_group_arena.push(bg);
                                    current_bind_groups[group_index] = Some(ptr);
                                }
                            } else {
                                let stage = group_index_to_stage(group_index as u32)?;
                                let stage_bindings = self.bindings.stage_mut(stage);
                                if stage_bindings.is_dirty()
                                    || current_bind_groups[group_index].is_none()
                                {
                                    let provider = CmdExecutorBindGroupProvider {
                                        resources: &self.resources,
                                        legacy_constants: &self.legacy_constants,
                                        cbuffer_scratch: &self.cbuffer_scratch,
                                        dummy_uniform: &self.dummy_uniform,
                                        dummy_texture_view: &self.dummy_texture_view,
                                        default_sampler: &self.default_sampler,
                                        stage,
                                        stage_state: stage_bindings,
                                    };
                                    let bg = reflection_bindings::build_bind_group(
                                        &self.device,
                                        &mut self.bind_group_cache,
                                        &pipeline_bindings.group_layouts[group_index],
                                        &pipeline_bindings.group_bindings[group_index],
                                        &provider,
                                    )?;
                                    let ptr = Arc::as_ptr(&bg);
                                    bind_group_arena.push(bg);
                                    current_bind_groups[group_index] = Some(ptr);
                                    stage_bindings.clear_dirty();
                                }
                            }

                            let ptr =
                                current_bind_groups[group_index].expect("bind group built above");
                            if bound_bind_groups[group_index] != Some(ptr) {
                                let bg_ref = unsafe { &*ptr };
                                pass.set_bind_group(group_index as u32, bg_ref, &[]);
                                bound_bind_groups[group_index] = Some(ptr);
                            }
                        }
                        exec_draw(&mut pass, cmd_bytes)?;

                        if used_cb_vs.first().is_some_and(|v| *v)
                            && self
                                .bindings
                                .stage(ShaderStage::Vertex)
                                .constant_buffer(0)
                                .is_some_and(|cb| {
                                    cb.buffer == legacy_constants_buffer_id(ShaderStage::Vertex)
                                })
                        {
                            legacy_constants_used
                                [ShaderStage::Vertex.as_bind_group_index() as usize] = true;
                            self.encoder_used_buffers
                                .insert(legacy_constants_buffer_id(ShaderStage::Vertex));
                        }
                        if used_cb_ps.first().is_some_and(|v| *v)
                            && self
                                .bindings
                                .stage(ShaderStage::Pixel)
                                .constant_buffer(0)
                                .is_some_and(|cb| {
                                    cb.buffer == legacy_constants_buffer_id(ShaderStage::Pixel)
                                })
                        {
                            legacy_constants_used
                                [ShaderStage::Pixel.as_bind_group_index() as usize] = true;
                            self.encoder_used_buffers
                                .insert(legacy_constants_buffer_id(ShaderStage::Pixel));
                        }

                        for &d3d_slot in &wgpu_slot_to_d3d_slot {
                            let slot = d3d_slot as usize;
                            if let Some(vb) = self.state.vertex_buffers.get(slot).and_then(|v| *v) {
                                self.encoder_used_buffers.insert(vb.buffer);
                            }
                        }

                        for (group_index, group_bindings) in
                            pipeline_bindings.group_bindings.iter().enumerate()
                        {
                            let stage = group_index_to_stage(group_index as u32)?;
                            let stage_bindings = self.bindings.stage(stage);
                            for binding in group_bindings {
                                match &binding.kind {
                                    crate::BindingKind::Texture2D { slot } => {
                                        if let Some(tex) = stage_bindings.texture(*slot) {
                                            self.encoder_used_textures.insert(tex.texture);
                                        }
                                    }
                                    crate::BindingKind::ConstantBuffer { slot, .. } => {
                                        if let Some(cb) = stage_bindings.constant_buffer(*slot) {
                                            self.encoder_used_buffers.insert(cb.buffer);
                                        }
                                    }
                                    crate::BindingKind::Sampler { .. } => {}
                                }
                            }
                        }
                    }
                }
                OPCODE_DRAW_INDEXED => {
                    if self.state.index_buffer.is_none() {
                        bail!("DRAW_INDEXED without index buffer");
                    }
                    if (self.state.sample_mask & 1) != 0 {
                        for group_index in 0..pipeline_bindings.group_layouts.len() {
                            if pipeline_bindings.group_bindings[group_index].is_empty() {
                                if current_bind_groups[group_index].is_none() {
                                    let entries: [BindGroupCacheEntry<'_>; 0] = [];
                                    let bg = self.bind_group_cache.get_or_create(
                                        &self.device,
                                        &pipeline_bindings.group_layouts[group_index],
                                        &entries,
                                    );
                                    let ptr = Arc::as_ptr(&bg);
                                    bind_group_arena.push(bg);
                                    current_bind_groups[group_index] = Some(ptr);
                                }
                            } else {
                                let stage = group_index_to_stage(group_index as u32)?;
                                let stage_bindings = self.bindings.stage_mut(stage);
                                if stage_bindings.is_dirty()
                                    || current_bind_groups[group_index].is_none()
                                {
                                    let provider = CmdExecutorBindGroupProvider {
                                        resources: &self.resources,
                                        legacy_constants: &self.legacy_constants,
                                        cbuffer_scratch: &self.cbuffer_scratch,
                                        dummy_uniform: &self.dummy_uniform,
                                        dummy_texture_view: &self.dummy_texture_view,
                                        default_sampler: &self.default_sampler,
                                        stage,
                                        stage_state: stage_bindings,
                                    };
                                    let bg = reflection_bindings::build_bind_group(
                                        &self.device,
                                        &mut self.bind_group_cache,
                                        &pipeline_bindings.group_layouts[group_index],
                                        &pipeline_bindings.group_bindings[group_index],
                                        &provider,
                                    )?;
                                    let ptr = Arc::as_ptr(&bg);
                                    bind_group_arena.push(bg);
                                    current_bind_groups[group_index] = Some(ptr);
                                    stage_bindings.clear_dirty();
                                }
                            }

                            let ptr =
                                current_bind_groups[group_index].expect("bind group built above");
                            if bound_bind_groups[group_index] != Some(ptr) {
                                let bg_ref = unsafe { &*ptr };
                                pass.set_bind_group(group_index as u32, bg_ref, &[]);
                                bound_bind_groups[group_index] = Some(ptr);
                            }
                        }
                        exec_draw_indexed(&mut pass, cmd_bytes)?;

                        if used_cb_vs.first().is_some_and(|v| *v)
                            && self
                                .bindings
                                .stage(ShaderStage::Vertex)
                                .constant_buffer(0)
                                .is_some_and(|cb| {
                                    cb.buffer == legacy_constants_buffer_id(ShaderStage::Vertex)
                                })
                        {
                            legacy_constants_used
                                [ShaderStage::Vertex.as_bind_group_index() as usize] = true;
                            self.encoder_used_buffers
                                .insert(legacy_constants_buffer_id(ShaderStage::Vertex));
                        }
                        if used_cb_ps.first().is_some_and(|v| *v)
                            && self
                                .bindings
                                .stage(ShaderStage::Pixel)
                                .constant_buffer(0)
                                .is_some_and(|cb| {
                                    cb.buffer == legacy_constants_buffer_id(ShaderStage::Pixel)
                                })
                        {
                            legacy_constants_used
                                [ShaderStage::Pixel.as_bind_group_index() as usize] = true;
                            self.encoder_used_buffers
                                .insert(legacy_constants_buffer_id(ShaderStage::Pixel));
                        }

                        for &d3d_slot in &wgpu_slot_to_d3d_slot {
                            let slot = d3d_slot as usize;
                            if let Some(vb) = self.state.vertex_buffers.get(slot).and_then(|v| *v) {
                                self.encoder_used_buffers.insert(vb.buffer);
                            }
                        }
                        if let Some(ib) = self.state.index_buffer {
                            self.encoder_used_buffers.insert(ib.buffer);
                        }

                        for (group_index, group_bindings) in
                            pipeline_bindings.group_bindings.iter().enumerate()
                        {
                            let stage = group_index_to_stage(group_index as u32)?;
                            let stage_bindings = self.bindings.stage(stage);
                            for binding in group_bindings {
                                match &binding.kind {
                                    crate::BindingKind::Texture2D { slot } => {
                                        if let Some(tex) = stage_bindings.texture(*slot) {
                                            self.encoder_used_textures.insert(tex.texture);
                                        }
                                    }
                                    crate::BindingKind::ConstantBuffer { slot, .. } => {
                                        if let Some(cb) = stage_bindings.constant_buffer(*slot) {
                                            self.encoder_used_buffers.insert(cb.buffer);
                                        }
                                    }
                                    crate::BindingKind::Sampler { .. } => {}
                                }
                            }
                        }
                    }
                }
                OPCODE_SET_VIEWPORT => {
                    self.exec_set_viewport(cmd_bytes)?;
                    if let Some(vp) = self.state.viewport {
                        if vp.x.is_finite()
                            && vp.y.is_finite()
                            && vp.width.is_finite()
                            && vp.height.is_finite()
                            && vp.min_depth.is_finite()
                            && vp.max_depth.is_finite()
                        {
                            if let Some((rt_w, rt_h)) = rt_dims {
                                let max_w = rt_w as f32;
                                let max_h = rt_h as f32;

                                let left = vp.x.max(0.0);
                                let top = vp.y.max(0.0);
                                let right = (vp.x + vp.width).max(0.0).min(max_w);
                                let bottom = (vp.y + vp.height).max(0.0).min(max_h);
                                let width = (right - left).max(0.0);
                                let height = (bottom - top).max(0.0);

                                if width > 0.0 && height > 0.0 {
                                    let mut min_depth = vp.min_depth.clamp(0.0, 1.0);
                                    let mut max_depth = vp.max_depth.clamp(0.0, 1.0);
                                    if min_depth > max_depth {
                                        std::mem::swap(&mut min_depth, &mut max_depth);
                                    }
                                    pass.set_viewport(
                                        left, top, width, height, min_depth, max_depth,
                                    );
                                }
                            } else {
                                pass.set_viewport(
                                    vp.x,
                                    vp.y,
                                    vp.width,
                                    vp.height,
                                    vp.min_depth,
                                    vp.max_depth,
                                );
                            }
                        }
                    }
                }
                OPCODE_SET_SCISSOR => {
                    self.exec_set_scissor(cmd_bytes)?;
                    if self.state.scissor_enable {
                        if let Some(sc) = self.state.scissor {
                            if let Some((rt_w, rt_h)) = rt_dims {
                                let x = sc.x.min(rt_w);
                                let y = sc.y.min(rt_h);
                                let width = sc.width.min(rt_w.saturating_sub(x));
                                let height = sc.height.min(rt_h.saturating_sub(y));
                                if width > 0 && height > 0 {
                                    pass.set_scissor_rect(x, y, width, height);
                                }
                            }
                        }
                    }
                }
                OPCODE_SET_BLEND_STATE => {
                    self.exec_set_blend_state(cmd_bytes)?;
                    pass.set_blend_constant(wgpu::Color {
                        r: self.state.blend_constant[0] as f64,
                        g: self.state.blend_constant[1] as f64,
                        b: self.state.blend_constant[2] as f64,
                        a: self.state.blend_constant[3] as f64,
                    });
                }
                OPCODE_SET_SHADER_CONSTANTS_F => {
                    // This is only reachable when `legacy_constants_used` is false, meaning the
                    // legacy constants buffer is not referenced by any previously recorded draw.
                    // In that case, we can apply the update via `queue.write_buffer` without
                    // restarting the render pass.
                    if cmd_bytes.len() < 24 {
                        bail!(
                            "SET_SHADER_CONSTANTS_F: expected at least 24 bytes, got {}",
                            cmd_bytes.len()
                        );
                    }
                    let stage_raw = read_u32_le(cmd_bytes, 8)?;
                    let start_register = read_u32_le(cmd_bytes, 12)?;
                    let vec4_count = read_u32_le(cmd_bytes, 16)? as usize;
                    let byte_len = vec4_count
                        .checked_mul(16)
                        .ok_or_else(|| anyhow!("SET_SHADER_CONSTANTS_F: byte_len overflow"))?;
                    let expected = 24 + align4(byte_len);
                    // Forward-compat: allow this packet to grow by appending new fields after the
                    // data.
                    if cmd_bytes.len() < expected {
                        bail!(
                            "SET_SHADER_CONSTANTS_F: expected at least {expected} bytes, got {}",
                            cmd_bytes.len()
                        );
                    }
                    let data = &cmd_bytes[24..24 + byte_len];

                    let stage = ShaderStage::from_aerogpu_u32(stage_raw).ok_or_else(|| {
                        anyhow!("SET_SHADER_CONSTANTS_F: unknown shader stage {stage_raw}")
                    })?;
                    let dst = self
                        .legacy_constants
                        .get(&stage)
                        .expect("legacy constants buffer exists for every stage");

                    let offset_bytes = start_register as u64 * 16;
                    let end = offset_bytes + byte_len as u64;
                    if end > LEGACY_CONSTANTS_SIZE_BYTES {
                        bail!(
                            "SET_SHADER_CONSTANTS_F: write out of bounds (end={end}, buffer_size={LEGACY_CONSTANTS_SIZE_BYTES})"
                        );
                    }

                    self.queue.write_buffer(dst, offset_bytes, data);
                }
                OPCODE_SET_DEPTH_STENCIL_STATE => self.exec_set_depth_stencil_state(cmd_bytes)?,
                OPCODE_SET_RASTERIZER_STATE => {
                    self.exec_set_rasterizer_state(cmd_bytes)?;

                    if let Some((rt_w, rt_h)) = rt_dims {
                        if self.state.scissor_enable {
                            if let Some(sc) = self.state.scissor {
                                let x = sc.x.min(rt_w);
                                let y = sc.y.min(rt_h);
                                let width = sc.width.min(rt_w.saturating_sub(x));
                                let height = sc.height.min(rt_h.saturating_sub(y));
                                if width > 0 && height > 0 {
                                    pass.set_scissor_rect(x, y, width, height);
                                }
                            } else {
                                pass.set_scissor_rect(0, 0, rt_w, rt_h);
                            }
                        } else {
                            pass.set_scissor_rect(0, 0, rt_w, rt_h);
                        }
                    }
                }
                OPCODE_BIND_SHADERS => self.exec_bind_shaders(cmd_bytes)?,
                OPCODE_CREATE_BUFFER => self.exec_create_buffer(cmd_bytes, allocs)?,
                OPCODE_CREATE_TEXTURE2D => self.exec_create_texture2d(cmd_bytes, allocs)?,
                OPCODE_DESTROY_RESOURCE => self.exec_destroy_resource(cmd_bytes)?,
                OPCODE_CREATE_SHADER_DXBC => self.exec_create_shader_dxbc(cmd_bytes)?,
                OPCODE_DESTROY_SHADER => self.exec_destroy_shader(cmd_bytes)?,
                OPCODE_CREATE_INPUT_LAYOUT => self.exec_create_input_layout(cmd_bytes)?,
                OPCODE_DESTROY_INPUT_LAYOUT => self.exec_destroy_input_layout(cmd_bytes)?,
                OPCODE_SET_INPUT_LAYOUT => self.exec_set_input_layout(cmd_bytes)?,
                OPCODE_SET_RENDER_TARGETS => self.exec_set_render_targets(cmd_bytes)?,
                OPCODE_RESOURCE_DIRTY_RANGE => self.exec_resource_dirty_range(cmd_bytes)?,
                OPCODE_UPLOAD_RESOURCE => {
                    let (cmd, data) = decode_cmd_upload_resource_payload_le(cmd_bytes)
                        .map_err(|e| anyhow!("UPLOAD_RESOURCE: invalid payload: {e:?}"))?;
                    self.upload_resource_payload(
                        cmd.resource_handle,
                        cmd.offset_bytes,
                        cmd.size_bytes,
                        data,
                    )?;
                }
                OPCODE_COPY_BUFFER | OPCODE_COPY_TEXTURE2D => {}
                OPCODE_CLEAR => {}
                OPCODE_SET_VERTEX_BUFFERS => {
                    let Ok((cmd, bindings)) = decode_cmd_set_vertex_buffers_bindings_le(cmd_bytes)
                    else {
                        bail!("SET_VERTEX_BUFFERS: invalid payload");
                    };
                    let start_slot = cmd.start_slot as usize;
                    let buffer_count = cmd.buffer_count as usize;

                    for (i, binding) in bindings.iter().copied().enumerate() {
                        let slot = start_slot.saturating_add(i);
                        if slot >= used_vertex_slots.len() || !used_vertex_slots[slot] {
                            continue;
                        }
                        let buffer = u32::from_le(binding.buffer);
                        if buffer == 0 {
                            continue;
                        }
                        let needs_upload = self
                            .resources
                            .buffers
                            .get(&buffer)
                            .is_some_and(|buf| buf.backing.is_some() && buf.dirty.is_some());
                        if needs_upload && !self.encoder_used_buffers.contains(&buffer) {
                            self.upload_buffer_from_guest_memory(buffer, allocs, guest_mem)?;
                        }
                    }

                    self.exec_set_vertex_buffers(cmd_bytes)?;

                    for slot in start_slot..start_slot.saturating_add(buffer_count) {
                        let Some(wgpu_slot) = d3d_slot_to_wgpu_slot.get(slot).and_then(|v| *v)
                        else {
                            continue;
                        };
                        let d3d_slot = u32::try_from(slot)
                            .map_err(|_| anyhow!("SET_VERTEX_BUFFERS: slot out of range"))?;
                        let Some(vb) = self.state.vertex_buffers.get(slot).and_then(|v| *v) else {
                            bail!("input layout requires vertex buffer slot {d3d_slot}");
                        };
                        if vb.stride_bytes != u32::from_le(bindings[slot - start_slot].stride_bytes)
                        {
                            bail!(
                                "SET_VERTEX_BUFFERS: stride update requires restarting render pass"
                            );
                        }

                        let (buf_ptr, buf_size): (*const wgpu::Buffer, u64) = {
                            let buf =
                                self.resources.buffers.get(&vb.buffer).ok_or_else(|| {
                                    anyhow!("unknown vertex buffer {}", vb.buffer)
                                })?;
                            (&buf.buffer as *const wgpu::Buffer, buf.size)
                        };
                        if vb.offset_bytes > buf_size {
                            bail!(
                                "vertex buffer {} offset {} out of bounds (size={})",
                                vb.buffer,
                                vb.offset_bytes,
                                buf_size
                            );
                        }
                        let buf = unsafe { &*buf_ptr };
                        pass.set_vertex_buffer(wgpu_slot, buf.slice(vb.offset_bytes..));
                    }
                }
                OPCODE_SET_INDEX_BUFFER => {
                    if cmd_bytes.len() >= 12 {
                        let buffer = read_u32_le(cmd_bytes, 8)?;
                        if buffer != 0 {
                            let needs_upload =
                                self.resources.buffers.get(&buffer).is_some_and(|buf| {
                                    buf.backing.is_some() && buf.dirty.is_some()
                                });
                            if needs_upload && !self.encoder_used_buffers.contains(&buffer) {
                                self.upload_buffer_from_guest_memory(buffer, allocs, guest_mem)?;
                            }
                        }
                    }
                    self.exec_set_index_buffer(cmd_bytes)?;
                    if let Some(ib) = self.state.index_buffer {
                        let (buf_ptr, buf_size): (*const wgpu::Buffer, u64) = {
                            let buf = self
                                .resources
                                .buffers
                                .get(&ib.buffer)
                                .ok_or_else(|| anyhow!("unknown index buffer {}", ib.buffer))?;
                            (&buf.buffer as *const wgpu::Buffer, buf.size)
                        };
                        if ib.offset_bytes > buf_size {
                            bail!(
                                "index buffer {} offset {} out of bounds (size={})",
                                ib.buffer,
                                ib.offset_bytes,
                                buf_size
                            );
                        }
                        let buf = unsafe { &*buf_ptr };
                        pass.set_index_buffer(buf.slice(ib.offset_bytes..), ib.format);
                    }
                }
                OPCODE_SET_PRIMITIVE_TOPOLOGY => self.exec_set_primitive_topology(cmd_bytes)?,
                OPCODE_SET_TEXTURE => {
                    // Allow first-use uploads of allocation-backed textures inside a render pass by
                    // reordering the upload ahead of the pass submission. This is only safe when
                    // the texture has not been referenced by any previously recorded GPU commands
                    // in the current command encoder.
                    if cmd_bytes.len() >= 20 {
                        let stage_raw = read_u32_le(cmd_bytes, 8)?;
                        let slot = read_u32_le(cmd_bytes, 12)?;
                        let texture = read_u32_le(cmd_bytes, 16)?;
                        if texture != 0 {
                            let Some(stage) = ShaderStage::from_aerogpu_u32(stage_raw) else {
                                bail!("SET_TEXTURE: unknown shader stage {stage_raw}");
                            };
                            let used_slots = match stage {
                                ShaderStage::Vertex => &used_textures_vs,
                                ShaderStage::Pixel => &used_textures_ps,
                                ShaderStage::Compute => &used_textures_cs,
                            };
                            let slot_usize: usize = slot
                                .try_into()
                                .map_err(|_| anyhow!("SET_TEXTURE: slot out of range"))?;
                            if slot_usize < used_slots.len() && used_slots[slot_usize] {
                                let needs_upload = self
                                    .resources
                                    .textures
                                    .get(&texture)
                                    .is_some_and(|tex| tex.dirty && tex.backing.is_some());
                                if needs_upload && !self.encoder_used_textures.contains(&texture) {
                                    self.upload_texture_from_guest_memory(
                                        texture, allocs, guest_mem,
                                    )?;
                                }
                            }
                        }
                    }
                    self.exec_set_texture(cmd_bytes)?;
                }
                OPCODE_SET_SAMPLER_STATE => self.exec_set_sampler_state(cmd_bytes)?,
                OPCODE_SET_RENDER_STATE => {}
                OPCODE_CREATE_SAMPLER => self.exec_create_sampler(cmd_bytes)?,
                OPCODE_DESTROY_SAMPLER => self.exec_destroy_sampler(cmd_bytes)?,
                OPCODE_SET_SAMPLERS => self.exec_set_samplers(cmd_bytes)?,
                OPCODE_SET_CONSTANT_BUFFERS => {
                    // Allow first-use uploads of allocation-backed constant buffers inside a render
                    // pass by reordering the upload ahead of the pass submission. This is only safe
                    // when the buffer has not been referenced by any previously recorded GPU
                    // commands in the current command encoder.
                    if cmd_bytes.len() >= 24 {
                        let stage_raw = read_u32_le(cmd_bytes, 8)?;
                        let start_slot = read_u32_le(cmd_bytes, 12)?;
                        let buffer_count_u32 = read_u32_le(cmd_bytes, 16)?;
                        let buffer_count: usize = buffer_count_u32.try_into().map_err(|_| {
                            anyhow!("SET_CONSTANT_BUFFERS: buffer_count out of range")
                        })?;
                        let expected =
                            24usize
                                .checked_add(buffer_count.checked_mul(16).ok_or_else(|| {
                                    anyhow!("SET_CONSTANT_BUFFERS: size overflow")
                                })?)
                                .ok_or_else(|| anyhow!("SET_CONSTANT_BUFFERS: size overflow"))?;
                        if cmd_bytes.len() >= expected {
                            if let Some(stage) = ShaderStage::from_aerogpu_u32(stage_raw) {
                                let used_slots = match stage {
                                    ShaderStage::Vertex => &used_cb_vs,
                                    ShaderStage::Pixel => &used_cb_ps,
                                    ShaderStage::Compute => &used_cb_cs,
                                };
                                for i in 0..buffer_count {
                                    let slot =
                                        start_slot.checked_add(i as u32).ok_or_else(|| {
                                            anyhow!("SET_CONSTANT_BUFFERS: slot overflow")
                                        })?;
                                    let slot_usize: usize = slot.try_into().map_err(|_| {
                                        anyhow!("SET_CONSTANT_BUFFERS: slot out of range")
                                    })?;
                                    if slot_usize >= used_slots.len() || !used_slots[slot_usize] {
                                        continue;
                                    }

                                    let base = 24 + i * 16;
                                    let buffer = read_u32_le(cmd_bytes, base)?;
                                    if buffer == 0 || buffer == legacy_constants_buffer_id(stage) {
                                        continue;
                                    }
                                    let needs_upload =
                                        self.resources.buffers.get(&buffer).is_some_and(|buf| {
                                            buf.backing.is_some() && buf.dirty.is_some()
                                        });
                                    if needs_upload && !self.encoder_used_buffers.contains(&buffer)
                                    {
                                        self.upload_buffer_from_guest_memory(
                                            buffer, allocs, guest_mem,
                                        )?;
                                    }
                                }
                            }
                        }
                    }
                    self.exec_set_constant_buffers(cmd_bytes)?;
                }
                OPCODE_NOP | OPCODE_DEBUG_MARKER => {}
                _ => {}
            }

            report.commands = report.commands.saturating_add(1);
            *stream.cursor = cmd_end;
        }

        drop(pass);
        Ok(())
    }

    fn exec_create_buffer(&mut self, cmd_bytes: &[u8], allocs: &AllocTable) -> Result<()> {
        // struct aerogpu_cmd_create_buffer (40 bytes)
        if cmd_bytes.len() < 40 {
            bail!(
                "CREATE_BUFFER: expected at least 40 bytes, got {}",
                cmd_bytes.len()
            );
        }
        let buffer_handle = read_u32_le(cmd_bytes, 8)?;
        let usage_flags = read_u32_le(cmd_bytes, 12)?;
        let size_bytes = read_u64_le(cmd_bytes, 16)?;
        let backing_alloc_id = read_u32_le(cmd_bytes, 24)?;
        let backing_offset_bytes = read_u32_le(cmd_bytes, 28)?;

        if buffer_handle == 0 {
            bail!("CREATE_BUFFER: buffer_handle 0 is reserved");
        }
        if size_bytes == 0 {
            bail!("CREATE_BUFFER: size_bytes must be > 0");
        }

        if let Some(&existing) = self.shared_surfaces.handles.get(&buffer_handle) {
            if existing != buffer_handle {
                bail!(
                    "CREATE_BUFFER: buffer_handle {buffer_handle} is already an alias (underlying={existing})"
                );
            }
        }

        let usage = map_buffer_usage_flags(usage_flags);
        let gpu_size = align_copy_buffer_size(size_bytes)?;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aerogpu buffer"),
            size: gpu_size,
            usage,
            mapped_at_creation: false,
        });

        let backing = if backing_alloc_id != 0 {
            allocs.validate_range(backing_alloc_id, backing_offset_bytes as u64, size_bytes)?;
            Some(ResourceBacking {
                alloc_id: backing_alloc_id,
                offset_bytes: backing_offset_bytes as u64,
            })
        } else {
            None
        };

        let mut res = BufferResource {
            buffer,
            size: size_bytes,
            gpu_size,
            backing,
            dirty: None,
        };
        if res.backing.is_some() {
            res.mark_dirty(0..size_bytes);
        }

        self.resources.buffers.insert(buffer_handle, res);
        self.shared_surfaces.register_handle(buffer_handle);
        Ok(())
    }

    fn exec_create_texture2d(&mut self, cmd_bytes: &[u8], allocs: &AllocTable) -> Result<()> {
        // struct aerogpu_cmd_create_texture2d (56 bytes)
        if cmd_bytes.len() < 56 {
            bail!(
                "CREATE_TEXTURE2D: expected at least 56 bytes, got {}",
                cmd_bytes.len()
            );
        }
        let texture_handle = read_u32_le(cmd_bytes, 8)?;
        let usage_flags = read_u32_le(cmd_bytes, 12)?;
        let format_u32 = read_u32_le(cmd_bytes, 16)?;
        let width = read_u32_le(cmd_bytes, 20)?;
        let height = read_u32_le(cmd_bytes, 24)?;
        let mip_levels = read_u32_le(cmd_bytes, 28)?;
        let array_layers = read_u32_le(cmd_bytes, 32)?;
        let row_pitch_bytes = read_u32_le(cmd_bytes, 36)?;
        let backing_alloc_id = read_u32_le(cmd_bytes, 40)?;
        let backing_offset_bytes = read_u32_le(cmd_bytes, 44)?;

        if texture_handle == 0 {
            bail!("CREATE_TEXTURE2D: texture_handle 0 is reserved");
        }
        if width == 0 || height == 0 {
            bail!("CREATE_TEXTURE2D: width/height must be non-zero");
        }
        if mip_levels == 0 || array_layers == 0 {
            bail!("CREATE_TEXTURE2D: mip_levels/array_layers must be >= 1");
        }
        if let Some(&existing) = self.shared_surfaces.handles.get(&texture_handle) {
            if existing != texture_handle {
                bail!(
                    "CREATE_TEXTURE2D: texture_handle {texture_handle} is already an alias (underlying={existing})"
                );
            }
        }

        let bc_enabled = self
            .device
            .features()
            .contains(wgpu::Features::TEXTURE_COMPRESSION_BC);
        let format = map_aerogpu_texture_format(format_u32, bc_enabled)?;
        let format_layout = aerogpu_texture_format_layout(format_u32)?;
        let usage = map_texture_usage_flags(usage_flags);
        let required_row_pitch = format_layout
            .bytes_per_row_tight(width)
            .context("CREATE_TEXTURE2D: compute required row_pitch")?;
        if row_pitch_bytes != 0 && row_pitch_bytes < required_row_pitch {
            bail!(
                "CREATE_TEXTURE2D: row_pitch_bytes {row_pitch_bytes} is smaller than required {required_row_pitch}"
            );
        }
        if backing_alloc_id != 0 && row_pitch_bytes == 0 {
            bail!("CREATE_TEXTURE2D: row_pitch_bytes is required for allocation-backed textures");
        }

        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("aerogpu texture2d"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: array_layers,
            },
            mip_level_count: mip_levels,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        let backing = if backing_alloc_id != 0 {
            // Validate that the allocation can hold all mips/layers using the guest UMD's canonical
            // packing:
            // - For each array layer: mip0..mipN tightly packed sequentially.
            // - mip0 uses the provided row_pitch_bytes (can include padding).
            // - mips > 0 use a tight row pitch based on format + mip width.
            let layout = compute_guest_texture_layout(
                format_u32,
                width,
                height,
                mip_levels,
                array_layers,
                row_pitch_bytes,
            )
            .context("CREATE_TEXTURE2D: compute guest layout")?;

            allocs.validate_range(
                backing_alloc_id,
                backing_offset_bytes as u64,
                layout.total_size,
            )?;
            Some(ResourceBacking {
                alloc_id: backing_alloc_id,
                offset_bytes: backing_offset_bytes as u64,
            })
        } else {
            None
        };

        self.resources.textures.insert(
            texture_handle,
            Texture2dResource {
                texture,
                view,
                desc: Texture2dDesc {
                    width,
                    height,
                    mip_level_count: mip_levels,
                    array_layers,
                    format,
                },
                format_u32,
                backing,
                row_pitch_bytes,
                dirty: backing.is_some(),
                host_shadow: None,
            },
        );
        self.shared_surfaces.register_handle(texture_handle);
        Ok(())
    }

    fn exec_destroy_resource(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        // struct aerogpu_cmd_destroy_resource (16 bytes)
        if cmd_bytes.len() < 16 {
            bail!(
                "DESTROY_RESOURCE: expected at least 16 bytes, got {}",
                cmd_bytes.len()
            );
        }
        let handle = read_u32_le(cmd_bytes, 8)?;

        if let Some((underlying, last_ref)) = self.shared_surfaces.destroy_handle(handle) {
            if last_ref {
                self.resources.buffers.remove(&underlying);
                self.resources.textures.remove(&underlying);
                self.encoder_used_buffers.remove(&underlying);
                self.encoder_used_textures.remove(&underlying);

                // Clean up bindings in state.
                self.state.render_targets.retain(|&rt| rt != underlying);
                if self.state.depth_stencil == Some(underlying) {
                    self.state.depth_stencil = None;
                }
                for slot in &mut self.state.vertex_buffers {
                    if slot.is_some_and(|b| b.buffer == underlying) {
                        *slot = None;
                    }
                }
                if self.state.index_buffer.is_some_and(|b| b.buffer == underlying) {
                    self.state.index_buffer = None;
                }
                for stage in [
                    ShaderStage::Vertex,
                    ShaderStage::Pixel,
                    ShaderStage::Compute,
                ] {
                    let stage_bindings = self.bindings.stage_mut(stage);
                    stage_bindings.clear_texture_handle(underlying);
                    stage_bindings.clear_constant_buffer_handle(underlying);
                }
            }
        } else {
            // Untracked handle; treat as a best-effort destroy (robustness).
            self.resources.buffers.remove(&handle);
            self.resources.textures.remove(&handle);
            self.encoder_used_buffers.remove(&handle);
            self.encoder_used_textures.remove(&handle);

            self.state.render_targets.retain(|&rt| rt != handle);
            if self.state.depth_stencil == Some(handle) {
                self.state.depth_stencil = None;
            }
            for slot in &mut self.state.vertex_buffers {
                if slot.is_some_and(|b| b.buffer == handle) {
                    *slot = None;
                }
            }
            if self.state.index_buffer.is_some_and(|b| b.buffer == handle) {
                self.state.index_buffer = None;
            }
            for stage in [
                ShaderStage::Vertex,
                ShaderStage::Pixel,
                ShaderStage::Compute,
            ] {
                let stage_bindings = self.bindings.stage_mut(stage);
                stage_bindings.clear_texture_handle(handle);
                stage_bindings.clear_constant_buffer_handle(handle);
            }
        }

        Ok(())
    }

    fn exec_export_shared_surface(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        // struct aerogpu_cmd_export_shared_surface (24 bytes)
        if cmd_bytes.len() < 24 {
            bail!(
                "EXPORT_SHARED_SURFACE: expected at least 24 bytes, got {}",
                cmd_bytes.len()
            );
        }
        let resource_handle = read_u32_le(cmd_bytes, 8)?;
        let share_token = read_u64_le(cmd_bytes, 16)?;
        self.shared_surfaces.export(resource_handle, share_token)
    }

    fn exec_import_shared_surface(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        // struct aerogpu_cmd_import_shared_surface (24 bytes)
        if cmd_bytes.len() < 24 {
            bail!(
                "IMPORT_SHARED_SURFACE: expected at least 24 bytes, got {}",
                cmd_bytes.len()
            );
        }
        let out_handle = read_u32_le(cmd_bytes, 8)?;
        let share_token = read_u64_le(cmd_bytes, 16)?;
        self.shared_surfaces.import(out_handle, share_token)
    }

    fn exec_release_shared_surface(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        // struct aerogpu_cmd_release_shared_surface (24 bytes)
        if cmd_bytes.len() < 24 {
            bail!(
                "RELEASE_SHARED_SURFACE: expected at least 24 bytes, got {}",
                cmd_bytes.len()
            );
        }
        let share_token = read_u64_le(cmd_bytes, 8)?;
        self.shared_surfaces.release_token(share_token);
        Ok(())
    }

    fn exec_resource_dirty_range(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        // struct aerogpu_cmd_resource_dirty_range (32 bytes)
        if cmd_bytes.len() < 32 {
            bail!(
                "RESOURCE_DIRTY_RANGE: expected at least 32 bytes, got {}",
                cmd_bytes.len()
            );
        }
        let handle = read_u32_le(cmd_bytes, 8)?;
        let offset = read_u64_le(cmd_bytes, 16)?;
        let size = read_u64_le(cmd_bytes, 24)?;
        let handle = self.shared_surfaces.resolve_handle(handle);

        if let Some(buf) = self.resources.buffers.get_mut(&handle) {
            let end = offset.saturating_add(size).min(buf.size);
            let start = offset.min(end);
            buf.mark_dirty(start..end);
        } else if let Some(tex) = self.resources.textures.get_mut(&handle) {
            tex.dirty = true;
        }
        Ok(())
    }

    fn exec_upload_resource(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        cmd_bytes: &[u8],
    ) -> Result<()> {
        let (cmd, data) = decode_cmd_upload_resource_payload_le(cmd_bytes)
            .map_err(|e| anyhow!("UPLOAD_RESOURCE: invalid payload: {e:?}"))?;
        let handle = self.shared_surfaces.resolve_handle(cmd.resource_handle);
        let offset = cmd.offset_bytes;
        let size = cmd.size_bytes;

        if size == 0 {
            return Ok(());
        }

        // Preserve command stream ordering relative to any previously encoded GPU work.
        if self.resources.buffers.contains_key(&handle) || self.resources.textures.contains_key(&handle) {
            self.submit_encoder_if_has_commands(
                encoder,
                "aerogpu_cmd encoder after UPLOAD_RESOURCE",
            );
        }

        self.upload_resource_payload(handle, offset, size, data)
    }

    fn upload_resource_payload(
        &mut self,
        handle: u32,
        offset: u64,
        size: u64,
        data: &[u8],
    ) -> Result<()> {
        if size == 0 {
            return Ok(());
        }
        let handle = self.shared_surfaces.resolve_handle(handle);

        if let Some((buffer_size, buffer_gpu_size)) = self
            .resources
            .buffers
            .get(&handle)
            .map(|buf| (buf.size, buf.gpu_size))
        {
            let alignment = wgpu::COPY_BUFFER_ALIGNMENT;
            if !offset.is_multiple_of(alignment) {
                bail!(
                    "UPLOAD_RESOURCE: buffer offset {offset} does not respect COPY_BUFFER_ALIGNMENT"
                );
            }
            if offset.saturating_add(size) > buffer_size {
                bail!("UPLOAD_RESOURCE: buffer upload out of bounds");
            }

            // `wgpu::Queue::write_buffer` requires the write size be a multiple of
            // `COPY_BUFFER_ALIGNMENT` (4). The AeroGPU command stream is byte-granular (e.g. index
            // buffers can be 3x u16 = 6 bytes), so we pad writes that reach the end of the buffer.
            let mut padded_tmp = Vec::new();
            let write_data: &[u8] = if !size.is_multiple_of(alignment) {
                if offset.saturating_add(size) != buffer_size {
                    bail!(
                        "UPLOAD_RESOURCE: unaligned buffer upload is only supported when writing to the end of the buffer"
                    );
                }
                let size_usize: usize = size
                    .try_into()
                    .map_err(|_| anyhow!("UPLOAD_RESOURCE: size_bytes out of range"))?;
                let padded = align4(size_usize);
                padded_tmp.resize(padded, 0);
                padded_tmp[..size_usize].copy_from_slice(data);

                let end = offset
                    .checked_add(padded as u64)
                    .ok_or_else(|| anyhow!("UPLOAD_RESOURCE: upload range overflows u64"))?;
                if end > buffer_gpu_size {
                    bail!("UPLOAD_RESOURCE: padded upload overruns wgpu buffer allocation");
                }

                &padded_tmp
            } else {
                data
            };

            {
                let buf = self
                    .resources
                    .buffers
                    .get(&handle)
                    .ok_or_else(|| anyhow!("UPLOAD_RESOURCE: unknown buffer {handle}"))?;
                self.queue.write_buffer(&buf.buffer, offset, write_data);
            }
            if let Some(buf_mut) = self.resources.buffers.get_mut(&handle) {
                // Uploaded data is now current on the GPU; clear dirty ranges.
                if let Some(dirty) = buf_mut.dirty.take() {
                    // If the dirty range extends outside the uploaded region, keep it.
                    let uploaded = offset..offset.saturating_add(size);
                    if dirty.start < uploaded.start || dirty.end > uploaded.end {
                        buf_mut.dirty = Some(dirty);
                    }
                }
            }
            return Ok(());
        }

        let Some((desc, format_u32, row_pitch_bytes, shadow_len)) =
            self.resources.textures.get(&handle).map(|tex| {
                (
                    tex.desc,
                    tex.format_u32,
                    tex.row_pitch_bytes,
                    tex.host_shadow.as_ref().map(|v| v.len()),
                )
            })
        else {
            return Ok(());
        };

        // Texture uploads are expressed as a linear byte range into mip0/layer0.
        //
        // WebGPU uploads are 2D; for partial updates we patch into a CPU shadow buffer and then
        // re-upload the full texture.
        let format_layout = aerogpu_texture_format_layout(format_u32)
            .context("UPLOAD_RESOURCE: unknown texture format")?;
        let bytes_per_row = if row_pitch_bytes != 0 {
            row_pitch_bytes
        } else {
            format_layout
                .bytes_per_row_tight(desc.width)
                .context("UPLOAD_RESOURCE: compute bytes_per_row")?
        };
        let rows = format_layout.rows(desc.height) as u64;
        let expected = (bytes_per_row as u64).saturating_mul(rows);

        let end = offset
            .checked_add(size)
            .ok_or_else(|| anyhow!("UPLOAD_RESOURCE: upload range overflows u64"))?;
        if end > expected {
            bail!("UPLOAD_RESOURCE: texture upload out of bounds");
        }

        let expected_usize: usize = expected
            .try_into()
            .map_err(|_| anyhow!("UPLOAD_RESOURCE: texture upload size out of range"))?;
        let offset_usize: usize = offset
            .try_into()
            .map_err(|_| anyhow!("UPLOAD_RESOURCE: offset out of range"))?;
        let end_usize: usize = end
            .try_into()
            .map_err(|_| anyhow!("UPLOAD_RESOURCE: end out of range"))?;

        if offset == 0 && size == expected {
            let tex = self
                .resources
                .textures
                .get_mut(&handle)
                .ok_or_else(|| anyhow!("UPLOAD_RESOURCE: unknown texture {handle}"))?;
            if format_layout.is_block_compressed() {
                let tight_bpr = format_layout
                    .bytes_per_row_tight(desc.width)
                    .context("UPLOAD_RESOURCE: compute BC tight bytes_per_row")?;
                if bytes_per_row < tight_bpr {
                    bail!("UPLOAD_RESOURCE: BC bytes_per_row too small");
                }
                let rows_u32: u32 = format_layout.rows(desc.height);
                let rows_usize: usize = rows_u32
                    .try_into()
                    .map_err(|_| anyhow!("UPLOAD_RESOURCE: BC rows out of range"))?;
                let src_bpr_usize: usize = bytes_per_row
                    .try_into()
                    .map_err(|_| anyhow!("UPLOAD_RESOURCE: BC bytes_per_row out of range"))?;
                let tight_bpr_usize: usize = tight_bpr
                    .try_into()
                    .map_err(|_| anyhow!("UPLOAD_RESOURCE: BC bytes_per_row out of range"))?;

                let mut tight = vec![
                    0u8;
                    tight_bpr_usize
                        .checked_mul(rows_usize)
                        .ok_or_else(|| anyhow!("UPLOAD_RESOURCE: BC data size overflow"))?
                ];
                for row in 0..rows_usize {
                    let src_start = row
                        .checked_mul(src_bpr_usize)
                        .ok_or_else(|| anyhow!("UPLOAD_RESOURCE: BC src row offset overflow"))?;
                    let dst_start = row
                        .checked_mul(tight_bpr_usize)
                        .ok_or_else(|| anyhow!("UPLOAD_RESOURCE: BC dst row offset overflow"))?;
                    tight[dst_start..dst_start + tight_bpr_usize].copy_from_slice(
                        data.get(src_start..src_start + tight_bpr_usize).ok_or_else(|| {
                            anyhow!("UPLOAD_RESOURCE: BC source too small for row")
                        })?,
                    );
                }

                let (bc, block_bytes) = match format_layout {
                    AerogpuTextureFormatLayout::BlockCompressed { bc, block_bytes } => {
                        (bc, block_bytes)
                    }
                    _ => unreachable!(),
                };
                let expected_len = desc.width.div_ceil(4) as usize
                    * desc.height.div_ceil(4) as usize
                    * (block_bytes as usize);
                if tight.len() != expected_len {
                    bail!(
                        "UPLOAD_RESOURCE: BC data length mismatch: expected {expected_len} bytes, got {}",
                        tight.len()
                    );
                }
                if bc_block_bytes(tex.desc.format).is_some() {
                    // Upload BC blocks directly.
                    write_texture_linear(
                        &self.queue,
                        &tex.texture,
                        tex.desc,
                        tight_bpr,
                        &tight,
                        false,
                    )?;
                } else {
                    // Fall back to RGBA8 + CPU decompression.
                    let rgba = match bc {
                        AerogpuBcFormat::Bc1 => {
                            aero_gpu::decompress_bc1_rgba8(desc.width, desc.height, &tight)
                        }
                        AerogpuBcFormat::Bc2 => {
                            aero_gpu::decompress_bc2_rgba8(desc.width, desc.height, &tight)
                        }
                        AerogpuBcFormat::Bc3 => {
                            aero_gpu::decompress_bc3_rgba8(desc.width, desc.height, &tight)
                        }
                        AerogpuBcFormat::Bc7 => {
                            aero_gpu::decompress_bc7_rgba8(desc.width, desc.height, &tight)
                        }
                    };

                    let rgba_bpr = desc.width.checked_mul(4).ok_or_else(|| {
                        anyhow!("UPLOAD_RESOURCE: decompressed bytes_per_row overflow")
                    })?;
                    write_texture_linear(
                        &self.queue,
                        &tex.texture,
                        tex.desc,
                        rgba_bpr,
                        &rgba,
                        false,
                    )?;
                }
            } else {
                write_texture_linear(
                    &self.queue,
                    &tex.texture,
                    tex.desc,
                    bytes_per_row,
                    data,
                    aerogpu_format_is_x8(tex.format_u32),
                )?;
            }
            tex.host_shadow = Some(data.to_vec());
            tex.dirty = false;
            return Ok(());
        }

        let shadow_len =
            shadow_len.ok_or_else(|| anyhow!("UPLOAD_RESOURCE: partial texture uploads require a prior full upload"))?;
        if shadow_len != expected_usize {
            bail!("UPLOAD_RESOURCE: internal shadow size mismatch");
        }

        let tex = self
            .resources
            .textures
            .get_mut(&handle)
            .ok_or_else(|| anyhow!("UPLOAD_RESOURCE: unknown texture {handle}"))?;
        let shadow = tex.host_shadow.as_mut().ok_or_else(|| {
            anyhow!("UPLOAD_RESOURCE: partial texture uploads require a prior full upload")
        })?;
        if shadow.len() != expected_usize {
            bail!("UPLOAD_RESOURCE: internal shadow size mismatch");
        }
        shadow[offset_usize..end_usize].copy_from_slice(data);

        if format_layout.is_block_compressed() {
            let tight_bpr = format_layout
                .bytes_per_row_tight(desc.width)
                .context("UPLOAD_RESOURCE: compute BC tight bytes_per_row")?;
            if bytes_per_row < tight_bpr {
                bail!("UPLOAD_RESOURCE: BC bytes_per_row too small");
            }
            let rows_u32: u32 = format_layout.rows(desc.height);
            let rows_usize: usize = rows_u32
                .try_into()
                .map_err(|_| anyhow!("UPLOAD_RESOURCE: BC rows out of range"))?;
            let src_bpr_usize: usize = bytes_per_row
                .try_into()
                .map_err(|_| anyhow!("UPLOAD_RESOURCE: BC bytes_per_row out of range"))?;
            let tight_bpr_usize: usize = tight_bpr
                .try_into()
                .map_err(|_| anyhow!("UPLOAD_RESOURCE: BC bytes_per_row out of range"))?;

            let mut tight = vec![
                0u8;
                tight_bpr_usize
                    .checked_mul(rows_usize)
                    .ok_or_else(|| anyhow!("UPLOAD_RESOURCE: BC data size overflow"))?
            ];
            for row in 0..rows_usize {
                let src_start = row
                    .checked_mul(src_bpr_usize)
                    .ok_or_else(|| anyhow!("UPLOAD_RESOURCE: BC src row offset overflow"))?;
                let dst_start = row
                    .checked_mul(tight_bpr_usize)
                    .ok_or_else(|| anyhow!("UPLOAD_RESOURCE: BC dst row offset overflow"))?;
                tight[dst_start..dst_start + tight_bpr_usize].copy_from_slice(
                    shadow
                        .get(src_start..src_start + tight_bpr_usize)
                        .ok_or_else(|| anyhow!("UPLOAD_RESOURCE: BC shadow too small for row"))?,
                );
            }

            let (bc, block_bytes) = match format_layout {
                AerogpuTextureFormatLayout::BlockCompressed { bc, block_bytes } => (bc, block_bytes),
                _ => unreachable!(),
            };
            let expected_len = desc.width.div_ceil(4) as usize
                * desc.height.div_ceil(4) as usize
                * (block_bytes as usize);
            if tight.len() != expected_len {
                bail!(
                    "UPLOAD_RESOURCE: BC data length mismatch: expected {expected_len} bytes, got {}",
                    tight.len()
                );
            }
            if bc_block_bytes(tex.desc.format).is_some() {
                write_texture_linear(
                    &self.queue,
                    &tex.texture,
                    tex.desc,
                    tight_bpr,
                    &tight,
                    false,
                )?;
            } else {
                let rgba = match bc {
                    AerogpuBcFormat::Bc1 => {
                        aero_gpu::decompress_bc1_rgba8(desc.width, desc.height, &tight)
                    }
                    AerogpuBcFormat::Bc2 => {
                        aero_gpu::decompress_bc2_rgba8(desc.width, desc.height, &tight)
                    }
                    AerogpuBcFormat::Bc3 => {
                        aero_gpu::decompress_bc3_rgba8(desc.width, desc.height, &tight)
                    }
                    AerogpuBcFormat::Bc7 => {
                        aero_gpu::decompress_bc7_rgba8(desc.width, desc.height, &tight)
                    }
                };

                let rgba_bpr = desc.width.checked_mul(4).ok_or_else(|| {
                    anyhow!("UPLOAD_RESOURCE: decompressed bytes_per_row overflow")
                })?;
                write_texture_linear(&self.queue, &tex.texture, tex.desc, rgba_bpr, &rgba, false)?;
            }
        } else {
            write_texture_linear(
                &self.queue,
                &tex.texture,
                tex.desc,
                bytes_per_row,
                shadow,
                aerogpu_format_is_x8(tex.format_u32),
            )?;
        }
        tex.dirty = false;
        Ok(())
    }

    fn exec_copy_buffer(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        cmd_bytes: &[u8],
        allocs: &AllocTable,
        guest_mem: &mut dyn GuestMemory,
        pending_writebacks: &mut Vec<PendingWriteback>,
    ) -> Result<()> {
        let cmd = decode_cmd_copy_buffer_le(cmd_bytes)
            .map_err(|e| anyhow!("COPY_BUFFER: invalid payload: {e:?}"))?;
        // `AerogpuCmdCopyBuffer` is `repr(C, packed)` (ABI mirror); copy out fields before use to
        // avoid taking references to packed fields.
        let dst_buffer = self.shared_surfaces.resolve_handle(cmd.dst_buffer);
        let src_buffer = self.shared_surfaces.resolve_handle(cmd.src_buffer);
        let dst_offset_bytes = cmd.dst_offset_bytes;
        let src_offset_bytes = cmd.src_offset_bytes;
        let size_bytes = cmd.size_bytes;
        let flags = cmd.flags;

        // WRITEBACK_DST requires an async executor on wasm (`execute_cmd_stream_async`), but the
        // copy + staging readback can be recorded here for both targets.
        let writeback = (flags & AEROGPU_COPY_FLAG_WRITEBACK_DST) != 0;
        if (flags & !AEROGPU_COPY_FLAG_WRITEBACK_DST) != 0 {
            bail!("COPY_BUFFER: unknown flags {flags:#x}");
        }
        if size_bytes == 0 {
            return Ok(());
        }
        if dst_buffer == 0 || src_buffer == 0 {
            bail!("COPY_BUFFER: resource handles must be non-zero");
        }
        if dst_buffer == src_buffer {
            bail!("COPY_BUFFER: src==dst is not supported");
        }

        let alignment = wgpu::COPY_BUFFER_ALIGNMENT;
        if dst_offset_bytes % alignment != 0 || src_offset_bytes % alignment != 0 {
            bail!(
                "COPY_BUFFER: offsets must be multiples of {alignment} (dst_offset_bytes={dst_offset_bytes} src_offset_bytes={src_offset_bytes})"
            );
        }

        let (src_size, src_gpu_size, dst_size, dst_gpu_size, dst_dirty, dst_backing) = {
            let src = self
                .resources
                .buffers
                .get(&src_buffer)
                .ok_or_else(|| anyhow!("COPY_BUFFER: unknown src buffer {src_buffer}"))?;
            let dst = self
                .resources
                .buffers
                .get(&dst_buffer)
                .ok_or_else(|| anyhow!("COPY_BUFFER: unknown dst buffer {dst_buffer}"))?;
            (
                src.size,
                src.gpu_size,
                dst.size,
                dst.gpu_size,
                dst.dirty.clone(),
                dst.backing,
            )
        };

        let src_end = src_offset_bytes
            .checked_add(size_bytes)
            .ok_or_else(|| anyhow!("COPY_BUFFER: src range overflows u64"))?;
        let dst_end = dst_offset_bytes
            .checked_add(size_bytes)
            .ok_or_else(|| anyhow!("COPY_BUFFER: dst range overflows u64"))?;
        if src_end > src_size {
            bail!(
                "COPY_BUFFER: src out of bounds: offset=0x{:x} size=0x{:x} buffer_size=0x{:x}",
                src_offset_bytes,
                size_bytes,
                src_size
            );
        }
        if dst_end > dst_size {
            bail!(
                "COPY_BUFFER: dst out of bounds: offset=0x{:x} size=0x{:x} buffer_size=0x{:x}",
                dst_offset_bytes,
                size_bytes,
                dst_size
            );
        }

        let mut copy_size_aligned = size_bytes;
        if size_bytes % alignment != 0 {
            if src_end != src_size || dst_end != dst_size {
                bail!(
                    "COPY_BUFFER: size_bytes must be a multiple of {alignment} unless copying to the end of both buffers (dst_offset_bytes={dst_offset_bytes} src_offset_bytes={src_offset_bytes} size_bytes={size_bytes} dst_size={dst_size} src_size={src_size})"
                );
            }
            copy_size_aligned = align_copy_buffer_size(size_bytes)?;
        }
        let src_end_aligned = src_offset_bytes
            .checked_add(copy_size_aligned)
            .ok_or_else(|| anyhow!("COPY_BUFFER: aligned src range overflows u64"))?;
        let dst_end_aligned = dst_offset_bytes
            .checked_add(copy_size_aligned)
            .ok_or_else(|| anyhow!("COPY_BUFFER: aligned dst range overflows u64"))?;
        if src_end_aligned > src_gpu_size || dst_end_aligned > dst_gpu_size {
            bail!("COPY_BUFFER: aligned copy range overruns wgpu buffer allocation");
        }

        let dst_writeback_gpa = if writeback {
            let dst_backing = dst_backing.ok_or_else(|| {
                anyhow!(
                    "COPY_BUFFER: WRITEBACK_DST requires dst buffer to be guest-backed (handle={dst_buffer})"
                )
            })?;
            let backing_offset = dst_backing
                .offset_bytes
                .checked_add(dst_offset_bytes)
                .ok_or_else(|| anyhow!("COPY_BUFFER: dst backing offset overflow"))?;
            Some(
                allocs
                    .validate_write_range(dst_backing.alloc_id, backing_offset, size_bytes)
                    .context("COPY_BUFFER: WRITEBACK_DST alloc table validation failed")?,
            )
        } else {
            None
        };

        // Ensure the source buffer reflects any CPU writes from guest memory before copying.
        self.ensure_buffer_uploaded(encoder, src_buffer, allocs, guest_mem)?;

        // If the destination is guest-backed and has pending uploads outside the copied region,
        // upload them now so untouched bytes remain correct.
        let needs_dst_upload = match dst_dirty.as_ref() {
            Some(dirty) if dst_backing.is_some() => {
                dirty.start < dst_offset_bytes || dirty.end > dst_end
            }
            _ => false,
        };
        if needs_dst_upload {
            self.ensure_buffer_uploaded(encoder, dst_buffer, allocs, guest_mem)?;
        }

        let mut staging: Option<wgpu::Buffer> = None;

        // Encode the copy.
        {
            let src = self
                .resources
                .buffers
                .get(&src_buffer)
                .ok_or_else(|| anyhow!("COPY_BUFFER: unknown src buffer {src_buffer}"))?;
            let dst = self
                .resources
                .buffers
                .get(&dst_buffer)
                .ok_or_else(|| anyhow!("COPY_BUFFER: unknown dst buffer {dst_buffer}"))?;

            encoder.copy_buffer_to_buffer(
                &src.buffer,
                src_offset_bytes,
                &dst.buffer,
                dst_offset_bytes,
                copy_size_aligned,
            );

            if writeback {
                let staging_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("aerogpu_cmd copy_buffer writeback staging"),
                    size: copy_size_aligned,
                    usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                encoder.copy_buffer_to_buffer(
                    &dst.buffer,
                    dst_offset_bytes,
                    &staging_buf,
                    0,
                    copy_size_aligned,
                );
                staging = Some(staging_buf);
            }
        }

        self.encoder_has_commands = true;
        self.encoder_used_buffers.insert(src_buffer);
        self.encoder_used_buffers.insert(dst_buffer);

        if let Some(dst_gpa) = dst_writeback_gpa {
            let Some(staging) = staging else {
                bail!("COPY_BUFFER: internal error: missing staging buffer for writeback");
            };
            pending_writebacks.push(PendingWriteback::Buffer {
                staging,
                dst_gpa,
                size_bytes,
            });
        }

        // The destination GPU buffer content has changed; discard any pending "dirty" ranges that
        // would otherwise cause us to overwrite the copy with stale guest-memory contents.
        if let Some(dst) = self.resources.buffers.get_mut(&dst_buffer) {
            dst.dirty = None;
        }

        Ok(())
    }

    fn exec_copy_texture2d(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        cmd_bytes: &[u8],
        allocs: &AllocTable,
        guest_mem: &mut dyn GuestMemory,
        pending_writebacks: &mut Vec<PendingWriteback>,
    ) -> Result<()> {
        let cmd = decode_cmd_copy_texture2d_le(cmd_bytes)
            .map_err(|e| anyhow!("COPY_TEXTURE2D: invalid payload: {e:?}"))?;
        // `AerogpuCmdCopyTexture2d` is `repr(C, packed)` (ABI mirror); copy out fields before use
        // to avoid taking references to packed fields.
        let dst_texture = self.shared_surfaces.resolve_handle(cmd.dst_texture);
        let src_texture = self.shared_surfaces.resolve_handle(cmd.src_texture);
        let dst_mip_level = cmd.dst_mip_level;
        let dst_array_layer = cmd.dst_array_layer;
        let src_mip_level = cmd.src_mip_level;
        let src_array_layer = cmd.src_array_layer;
        let dst_x = cmd.dst_x;
        let dst_y = cmd.dst_y;
        let src_x = cmd.src_x;
        let src_y = cmd.src_y;
        let width = cmd.width;
        let height = cmd.height;
        let flags = cmd.flags;

        // WRITEBACK_DST requires an async executor on wasm (`execute_cmd_stream_async`), but the
        // copy + staging readback can be recorded here for both targets.
        let writeback = (flags & AEROGPU_COPY_FLAG_WRITEBACK_DST) != 0;
        if (flags & !AEROGPU_COPY_FLAG_WRITEBACK_DST) != 0 {
            bail!("COPY_TEXTURE2D: unknown flags {flags:#x}");
        }
        if width == 0 || height == 0 {
            return Ok(());
        }
        if dst_texture == 0 || src_texture == 0 {
            bail!("COPY_TEXTURE2D: resource handles must be non-zero");
        }

        if writeback && (dst_mip_level != 0 || dst_array_layer != 0) {
            bail!(
                "COPY_TEXTURE2D: WRITEBACK_DST is only supported for dst_mip_level=0 and dst_array_layer=0 (got mip={} layer={})",
                dst_mip_level,
                dst_array_layer
            );
        }

        let mip_extent = |v: u32, level: u32| v.checked_shr(level).unwrap_or(0).max(1);

        let (
            src_desc,
            src_format_u32,
            dst_desc,
            dst_format_u32,
            dst_backing,
            dst_row_pitch_bytes,
            dst_dirty,
        ) =
            {
                let src =
                    self.resources.textures.get(&src_texture).ok_or_else(|| {
                        anyhow!("COPY_TEXTURE2D: unknown src texture {src_texture}")
                    })?;
                let dst =
                    self.resources.textures.get(&dst_texture).ok_or_else(|| {
                        anyhow!("COPY_TEXTURE2D: unknown dst texture {dst_texture}")
                    })?;
                (
                    src.desc,
                    src.format_u32,
                    dst.desc,
                    dst.format_u32,
                    dst.backing,
                    dst.row_pitch_bytes,
                    dst.dirty,
                )
            };

        if src_mip_level >= src_desc.mip_level_count {
            bail!(
                "COPY_TEXTURE2D: src_mip_level {src_mip_level} out of range (mip_levels={})",
                src_desc.mip_level_count
            );
        }
        if dst_mip_level >= dst_desc.mip_level_count {
            bail!(
                "COPY_TEXTURE2D: dst_mip_level {dst_mip_level} out of range (mip_levels={})",
                dst_desc.mip_level_count
            );
        }
        if src_array_layer >= src_desc.array_layers {
            bail!(
                "COPY_TEXTURE2D: src_array_layer {src_array_layer} out of range (array_layers={})",
                src_desc.array_layers
            );
        }
        if dst_array_layer >= dst_desc.array_layers {
            bail!(
                "COPY_TEXTURE2D: dst_array_layer {dst_array_layer} out of range (array_layers={})",
                dst_desc.array_layers
            );
        }

        if src_format_u32 != dst_format_u32 {
            bail!(
                "COPY_TEXTURE2D: format mismatch: src_format={src_format_u32} dst_format={dst_format_u32}"
            );
        }
        if src_desc.format != dst_desc.format {
            bail!(
                "COPY_TEXTURE2D: internal format mismatch: src={:?} dst={:?}",
                src_desc.format,
                dst_desc.format
            );
        }

        if writeback && !aerogpu_format_supports_writeback_dst(dst_format_u32) {
            bail!(
                "COPY_TEXTURE2D: WRITEBACK_DST is not supported for dst format {dst_format_u32} (only uncompressed 32bpp formats are supported)"
            );
        }

        let src_w = mip_extent(src_desc.width, src_mip_level);
        let src_h = mip_extent(src_desc.height, src_mip_level);
        let dst_w = mip_extent(dst_desc.width, dst_mip_level);
        let dst_h = mip_extent(dst_desc.height, dst_mip_level);

        let src_x_end = src_x
            .checked_add(width)
            .ok_or_else(|| anyhow!("COPY_TEXTURE2D: src_x+width overflows u32"))?;
        let src_y_end = src_y
            .checked_add(height)
            .ok_or_else(|| anyhow!("COPY_TEXTURE2D: src_y+height overflows u32"))?;
        let dst_x_end = dst_x
            .checked_add(width)
            .ok_or_else(|| anyhow!("COPY_TEXTURE2D: dst_x+width overflows u32"))?;
        let dst_y_end = dst_y
            .checked_add(height)
            .ok_or_else(|| anyhow!("COPY_TEXTURE2D: dst_y+height overflows u32"))?;

        if src_x_end > src_w || src_y_end > src_h {
            bail!("COPY_TEXTURE2D: src rect out of bounds");
        }
        if dst_x_end > dst_w || dst_y_end > dst_h {
            bail!("COPY_TEXTURE2D: dst rect out of bounds");
        }

        // WebGPU requires BC-compressed copies to be aligned to 4x4 blocks, except when the region
        // reaches the mip edge (partial blocks are only representable at the edge).
        if bc_block_bytes(src_desc.format).is_some() {
            if !src_x.is_multiple_of(4)
                || !src_y.is_multiple_of(4)
                || !dst_x.is_multiple_of(4)
                || !dst_y.is_multiple_of(4)
            {
                bail!(
                    "COPY_TEXTURE2D: BC copies require 4x4 block-aligned origins (src=({src_x},{src_y}) dst=({dst_x},{dst_y}))"
                );
            }
            if (!width.is_multiple_of(4) && (src_x_end != src_w || dst_x_end != dst_w))
                || (!height.is_multiple_of(4) && (src_y_end != src_h || dst_y_end != dst_h))
            {
                bail!(
                    "COPY_TEXTURE2D: BC copies require 4x4 block-aligned size unless the region reaches the mip edge (src_mip=({src_w},{src_h}) dst_mip=({dst_w},{dst_h}) src=({src_x},{src_y}) dst=({dst_x},{dst_y}) size=({width},{height}))"
                );
            }
        }

        // If the destination is guest-backed and dirty and we're only overwriting a sub-rectangle,
        // upload it now so the untouched pixels remain correct.
        let needs_dst_upload = dst_backing.is_some()
            && dst_dirty
            && dst_mip_level == 0
            && dst_array_layer == 0
            && !(dst_x == 0 && dst_y == 0 && width == dst_desc.width && height == dst_desc.height);

        let writeback_plan = if writeback {
            let dst_backing = dst_backing.ok_or_else(|| {
                anyhow!(
                    "COPY_TEXTURE2D: WRITEBACK_DST requires dst texture to be guest-backed (handle={dst_texture})"
                )
            })?;
            if dst_row_pitch_bytes == 0 {
                bail!("COPY_TEXTURE2D: WRITEBACK_DST requires non-zero dst row_pitch_bytes");
            }
            let row_pitch = dst_row_pitch_bytes as u64;
            let bytes_per_pixel = bytes_per_texel(dst_desc.format)? as u64;

            let dst_x_bytes = (dst_x as u64)
                .checked_mul(bytes_per_pixel)
                .ok_or_else(|| anyhow!("COPY_TEXTURE2D: dst_x byte offset overflow"))?;

            let row_bytes_u32 = width
                .checked_mul(bytes_per_pixel as u32)
                .ok_or_else(|| anyhow!("COPY_TEXTURE2D: bytes_per_row overflow"))?;
            let row_bytes = row_bytes_u32 as u64;
            if dst_x_bytes
                .checked_add(row_bytes)
                .ok_or_else(|| anyhow!("COPY_TEXTURE2D: dst row byte range overflow"))?
                > row_pitch
            {
                bail!("COPY_TEXTURE2D: dst row_pitch_bytes too small for writeback region");
            }

            let start_offset = dst_backing
                .offset_bytes
                .checked_add(
                    (dst_y as u64)
                        .checked_mul(row_pitch)
                        .ok_or_else(|| anyhow!("COPY_TEXTURE2D: dst_y row_pitch overflow"))?,
                )
                .and_then(|v| v.checked_add(dst_x_bytes))
                .ok_or_else(|| anyhow!("COPY_TEXTURE2D: dst backing offset overflow"))?;

            let last_row_start = start_offset
                .checked_add(
                    (height as u64)
                        .checked_sub(1)
                        .ok_or_else(|| anyhow!("COPY_TEXTURE2D: height underflow"))?
                        .checked_mul(row_pitch)
                        .ok_or_else(|| anyhow!("COPY_TEXTURE2D: last row offset overflow"))?,
                )
                .ok_or_else(|| anyhow!("COPY_TEXTURE2D: last row start overflow"))?;

            let end_offset = last_row_start
                .checked_add(row_bytes)
                .ok_or_else(|| anyhow!("COPY_TEXTURE2D: dst backing end overflow"))?;

            let validate_size = end_offset
                .checked_sub(start_offset)
                .ok_or_else(|| anyhow!("COPY_TEXTURE2D: dst backing size underflow"))?;

            let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
            let padded_bpr = row_bytes_u32
                .checked_add(align - 1)
                .map(|v| v / align)
                .and_then(|v| v.checked_mul(align))
                .ok_or_else(|| anyhow!("COPY_TEXTURE2D: padded bytes_per_row overflow"))?;

            let base_gpa = allocs
                .validate_write_range(dst_backing.alloc_id, start_offset, validate_size)
                .context("COPY_TEXTURE2D: WRITEBACK_DST alloc table validation failed")?;

            Some((
                TextureWritebackPlan {
                    base_gpa,
                    row_pitch,
                    padded_bytes_per_row: padded_bpr,
                    unpadded_bytes_per_row: row_bytes_u32,
                    height,
                    is_x8: aerogpu_format_is_x8(dst_format_u32),
                },
                padded_bpr as u64,
            ))
        } else {
            None
        };

        // Ensure the source texture reflects any CPU writes from guest memory before copying.
        self.ensure_texture_uploaded(encoder, src_texture, allocs, guest_mem)?;

        if needs_dst_upload {
            self.ensure_texture_uploaded(encoder, dst_texture, allocs, guest_mem)?;
        }

        let mut staging: Option<wgpu::Buffer> = None;
        {
            let src = self
                .resources
                .textures
                .get(&src_texture)
                .ok_or_else(|| anyhow!("COPY_TEXTURE2D: unknown src texture {src_texture}"))?;
            let dst = self
                .resources
                .textures
                .get(&dst_texture)
                .ok_or_else(|| anyhow!("COPY_TEXTURE2D: unknown dst texture {dst_texture}"))?;

            encoder.copy_texture_to_texture(
                wgpu::ImageCopyTexture {
                    texture: &src.texture,
                    mip_level: src_mip_level,
                    origin: wgpu::Origin3d {
                        x: src_x,
                        y: src_y,
                        z: src_array_layer,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::ImageCopyTexture {
                    texture: &dst.texture,
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

            if let Some((plan, padded_bpr_u64)) = writeback_plan.as_ref() {
                let buffer_size = padded_bpr_u64
                    .checked_mul(plan.height as u64)
                    .ok_or_else(|| anyhow!("COPY_TEXTURE2D: staging buffer size overflow"))?;
                let staging_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("aerogpu_cmd copy_texture2d writeback staging"),
                    size: buffer_size,
                    usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                encoder.copy_texture_to_buffer(
                    wgpu::ImageCopyTexture {
                        texture: &dst.texture,
                        mip_level: dst_mip_level,
                        origin: wgpu::Origin3d {
                            x: dst_x,
                            y: dst_y,
                            z: dst_array_layer,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::ImageCopyBuffer {
                        buffer: &staging_buf,
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
                staging = Some(staging_buf);
            }
        }
        self.encoder_has_commands = true;
        self.encoder_used_textures.insert(src_texture);
        self.encoder_used_textures.insert(dst_texture);

        if let Some((plan, _)) = writeback_plan {
            let Some(staging) = staging else {
                bail!("COPY_TEXTURE2D: internal error: missing staging buffer for writeback");
            };
            pending_writebacks.push(PendingWriteback::Texture2d { staging, plan });
        }

        // The destination GPU texture content has changed; discard any pending "dirty" marker that
        // would otherwise cause us to overwrite the copy with stale guest-memory contents.
        if let Some(dst) = self.resources.textures.get_mut(&dst_texture) {
            dst.dirty = false;
            dst.host_shadow = None;
        }

        Ok(())
    }

    fn exec_create_shader_dxbc(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        let (cmd, dxbc_bytes) = decode_cmd_create_shader_dxbc_payload_le(cmd_bytes)
            .map_err(|e| anyhow!("CREATE_SHADER_DXBC: invalid payload: {e:?}"))?;
        let shader_handle = cmd.shader_handle;
        let stage_u32 = cmd.stage;

        let stage = match stage_u32 {
            0 => ShaderStage::Vertex,
            1 => ShaderStage::Pixel,
            2 => ShaderStage::Compute,
            _ => bail!("CREATE_SHADER_DXBC: unknown shader stage {stage_u32}"),
        };

        let dxbc_hash_fnv1a64 = fnv1a64(dxbc_bytes);
        let dxbc = DxbcFile::parse(dxbc_bytes).context("DXBC parse failed")?;
        let program = Sm4Program::parse_from_dxbc(&dxbc).context("DXBC decode failed")?;
        let parsed_stage = match program.stage {
            crate::ShaderStage::Vertex => ShaderStage::Vertex,
            crate::ShaderStage::Pixel => ShaderStage::Pixel,
            crate::ShaderStage::Compute => ShaderStage::Compute,
            // Geometry/hull/domain stages are not represented in the AeroGPU command stream (WebGPU
            // does not expose them), but Win7 D3D11 applications may still create these shaders.
            //
            // Accept the create to keep the command stream robust, but ignore the shader since it
            // can never be bound (no GS/HS/DS slot in `AEROGPU_CMD_BIND_SHADERS`).
            crate::ShaderStage::Geometry
            | crate::ShaderStage::Hull
            | crate::ShaderStage::Domain => {
                return Ok(());
            }
            other => bail!("CREATE_SHADER_DXBC: unsupported DXBC shader stage {other:?}"),
        };
        if parsed_stage != stage {
            bail!("CREATE_SHADER_DXBC: stage mismatch (cmd={stage:?}, dxbc={parsed_stage:?})");
        }

        let signatures = parse_signatures(&dxbc).context("parse DXBC signatures")?;
        let (wgsl, reflection) = if signatures.isgn.is_some() && signatures.osgn.is_some() {
            let translated = try_translate_sm4_signature_driven(&dxbc, &program, &signatures)?;
            (translated.wgsl, translated.reflection)
        } else {
            (
                crate::wgsl_bootstrap::translate_sm4_to_wgsl_bootstrap(&program)
                    .context("DXBC->WGSL translation failed")?
                    .wgsl,
                ShaderReflection::default(),
            )
        };

        let entry_point = match stage {
            ShaderStage::Vertex => "vs_main",
            ShaderStage::Pixel => "fs_main",
            ShaderStage::Compute => "cs_main",
        };

        let (hash, _module) = self.pipeline_cache.get_or_create_shader_module(
            &self.device,
            map_pipeline_cache_stage(stage),
            &wgsl,
            Some("aerogpu_cmd shader"),
        );

        let vs_input_signature = if stage == ShaderStage::Vertex {
            extract_vs_input_signature(&signatures).context("extract VS input signature")?
        } else {
            Vec::new()
        };

        let depth_clamp_wgsl_hash = if stage == ShaderStage::Vertex {
            let clamped = wgsl_depth_clamp_variant(&wgsl);
            let (hash, _module) = self.pipeline_cache.get_or_create_shader_module(
                &self.device,
                map_pipeline_cache_stage(stage),
                &clamped,
                Some("aerogpu_cmd VS (depth clamp)"),
            );
            Some(hash)
        } else {
            None
        };

        #[cfg(debug_assertions)]
        let shader = ShaderResource {
            stage,
            wgsl_hash: hash,
            depth_clamp_wgsl_hash,
            dxbc_hash_fnv1a64,
            entry_point,
            vs_input_signature,
            reflection,
            wgsl_source: wgsl,
        };
        #[cfg(not(debug_assertions))]
        let shader = ShaderResource {
            stage,
            wgsl_hash: hash,
            depth_clamp_wgsl_hash,
            dxbc_hash_fnv1a64,
            entry_point,
            vs_input_signature,
            reflection,
        };

        self.resources.shaders.insert(shader_handle, shader);
        Ok(())
    }

    fn exec_destroy_shader(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        // struct aerogpu_cmd_destroy_shader (16 bytes)
        if cmd_bytes.len() < 16 {
            bail!(
                "DESTROY_SHADER: expected at least 16 bytes, got {}",
                cmd_bytes.len()
            );
        }
        let shader_handle = read_u32_le(cmd_bytes, 8)?;
        self.resources.shaders.remove(&shader_handle);
        Ok(())
    }

    fn exec_bind_shaders(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        // struct aerogpu_cmd_bind_shaders (24 bytes)
        if cmd_bytes.len() < 24 {
            bail!(
                "BIND_SHADERS: expected at least 24 bytes, got {}",
                cmd_bytes.len()
            );
        }
        let vs = read_u32_le(cmd_bytes, 8)?;
        let ps = read_u32_le(cmd_bytes, 12)?;
        let cs = read_u32_le(cmd_bytes, 16)?;

        self.state.vs = if vs == 0 { None } else { Some(vs) };
        self.state.ps = if ps == 0 { None } else { Some(ps) };
        self.state.cs = if cs == 0 { None } else { Some(cs) };
        self.bindings.mark_all_dirty();
        Ok(())
    }

    fn exec_set_shader_constants_f(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        cmd_bytes: &[u8],
    ) -> Result<()> {
        // struct aerogpu_cmd_set_shader_constants_f (24 bytes) + vec4 data.
        if cmd_bytes.len() < 24 {
            bail!(
                "SET_SHADER_CONSTANTS_F: expected at least 24 bytes, got {}",
                cmd_bytes.len()
            );
        }
        let stage_raw = read_u32_le(cmd_bytes, 8)?;
        let start_register = read_u32_le(cmd_bytes, 12)?;
        let vec4_count = read_u32_le(cmd_bytes, 16)? as usize;
        let byte_len = vec4_count
            .checked_mul(16)
            .ok_or_else(|| anyhow!("SET_SHADER_CONSTANTS_F: byte_len overflow"))?;
        let expected = 24 + align4(byte_len);
        // Forward-compat: allow this packet to grow by appending new fields after the data.
        if cmd_bytes.len() < expected {
            bail!(
                "SET_SHADER_CONSTANTS_F: expected at least {expected} bytes, got {}",
                cmd_bytes.len(),
            );
        }
        let data = &cmd_bytes[24..24 + byte_len];

        let stage = ShaderStage::from_aerogpu_u32(stage_raw)
            .ok_or_else(|| anyhow!("SET_SHADER_CONSTANTS_F: unknown shader stage {stage_raw}"))?;
        let dst = self
            .legacy_constants
            .get(&stage)
            .expect("legacy constants buffer exists for every stage");

        let offset_bytes = start_register as u64 * 16;
        let end = offset_bytes + byte_len as u64;
        if end > LEGACY_CONSTANTS_SIZE_BYTES {
            bail!(
                "SET_SHADER_CONSTANTS_F: write out of bounds (end={end}, buffer_size={LEGACY_CONSTANTS_SIZE_BYTES})"
            );
        }

        let staging_size = align4(byte_len) as u64;
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aerogpu_cmd constants staging"),
            size: staging_size,
            usage: wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: true,
        });
        {
            let mut mapped = staging.slice(..).get_mapped_range_mut();
            mapped[..byte_len].copy_from_slice(data);
            // Zero any padding so tooling never sees uninitialized bytes.
            for b in &mut mapped[byte_len..] {
                *b = 0;
            }
        }
        staging.unmap();

        encoder.copy_buffer_to_buffer(&staging, 0, dst, offset_bytes, byte_len as u64);
        self.encoder_has_commands = true;
        self.encoder_used_buffers
            .insert(legacy_constants_buffer_id(stage));
        Ok(())
    }

    fn exec_set_texture(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        // struct aerogpu_cmd_set_texture (24 bytes)
        // Forward-compat: allow this packet to grow by appending new fields after the existing
        // payload.
        if cmd_bytes.len() < 24 {
            bail!(
                "SET_TEXTURE: expected at least 24 bytes, got {}",
                cmd_bytes.len()
            );
        }
        let stage_raw = read_u32_le(cmd_bytes, 8)?;
        let slot = read_u32_le(cmd_bytes, 12)?;
        let texture = read_u32_le(cmd_bytes, 16)?;

        if slot as usize >= DEFAULT_MAX_TEXTURE_SLOTS {
            bail!(
                "SET_TEXTURE: slot out of supported range (slot={slot} max_slot={})",
                DEFAULT_MAX_TEXTURE_SLOTS - 1
            );
        }

        let stage = ShaderStage::from_aerogpu_u32(stage_raw)
            .ok_or_else(|| anyhow!("SET_TEXTURE: unknown shader stage {stage_raw}"))?;
        let texture = if texture == 0 {
            None
        } else {
            Some(self.shared_surfaces.resolve_handle(texture))
        };
        self.bindings.stage_mut(stage).set_texture(slot, texture);
        Ok(())
    }

    fn exec_set_sampler_state(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        // struct aerogpu_cmd_set_sampler_state (24 bytes)
        // Forward-compat: allow this packet to grow by appending new fields after the existing
        // payload.
        if cmd_bytes.len() < 24 {
            bail!(
                "SET_SAMPLER_STATE: expected at least 24 bytes, got {}",
                cmd_bytes.len()
            );
        }
        // Optional: sampler state translation is not implemented yet. The executor binds a
        // default sampler to all declared `s#` bindings.
        Ok(())
    }

    fn exec_create_sampler(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        // struct aerogpu_cmd_create_sampler (28 bytes)
        // Forward-compat: allow this packet to grow by appending new fields after the existing
        // payload.
        if cmd_bytes.len() < 28 {
            bail!(
                "CREATE_SAMPLER: expected at least 28 bytes, got {}",
                cmd_bytes.len()
            );
        }

        let sampler_handle = read_u32_le(cmd_bytes, 8)?;
        let filter_u32 = read_u32_le(cmd_bytes, 12)?;
        let address_u_u32 = read_u32_le(cmd_bytes, 16)?;
        let address_v_u32 = read_u32_le(cmd_bytes, 20)?;
        let address_w_u32 = read_u32_le(cmd_bytes, 24)?;

        let filter = match filter_u32 {
            0 => wgpu::FilterMode::Nearest,
            1 => wgpu::FilterMode::Linear,
            other => bail!("CREATE_SAMPLER: unknown filter {other}"),
        };
        let address = |v: u32| -> Result<wgpu::AddressMode> {
            Ok(match v {
                0 => wgpu::AddressMode::ClampToEdge,
                1 => wgpu::AddressMode::Repeat,
                2 => wgpu::AddressMode::MirrorRepeat,
                other => bail!("CREATE_SAMPLER: unknown address mode {other}"),
            })
        };

        let sampler = self.sampler_cache.get_or_create(
            &self.device,
            &wgpu::SamplerDescriptor {
                label: Some("aerogpu_cmd sampler"),
                address_mode_u: address(address_u_u32)?,
                address_mode_v: address(address_v_u32)?,
                address_mode_w: address(address_w_u32)?,
                mag_filter: filter,
                min_filter: filter,
                mipmap_filter: filter,
                ..Default::default()
            },
        );
        self.resources.samplers.insert(sampler_handle, sampler);
        Ok(())
    }

    fn exec_destroy_sampler(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        // struct aerogpu_cmd_destroy_sampler (16 bytes)
        // Forward-compat: allow this packet to grow by appending new fields after the existing
        // payload.
        if cmd_bytes.len() < 16 {
            bail!(
                "DESTROY_SAMPLER: expected at least 16 bytes, got {}",
                cmd_bytes.len()
            );
        }
        let sampler_handle = read_u32_le(cmd_bytes, 8)?;
        self.resources.samplers.remove(&sampler_handle);
        for stage in [
            ShaderStage::Vertex,
            ShaderStage::Pixel,
            ShaderStage::Compute,
        ] {
            self.bindings
                .stage_mut(stage)
                .clear_sampler_handle(sampler_handle);
        }
        Ok(())
    }

    fn exec_set_samplers(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        // struct aerogpu_cmd_set_samplers (24 bytes) + sampler handles.
        if cmd_bytes.len() < 24 {
            bail!(
                "SET_SAMPLERS: expected at least 24 bytes, got {}",
                cmd_bytes.len()
            );
        }
        let stage_raw = read_u32_le(cmd_bytes, 8)?;
        let start_slot_u32 = read_u32_le(cmd_bytes, 12)?;
        let sampler_count_u32 = read_u32_le(cmd_bytes, 16)?;

        let stage = ShaderStage::from_aerogpu_u32(stage_raw)
            .ok_or_else(|| anyhow!("SET_SAMPLERS: unknown shader stage {stage_raw}"))?;
        let start_slot: u32 = start_slot_u32;
        let sampler_count: usize = sampler_count_u32
            .try_into()
            .map_err(|_| anyhow!("SET_SAMPLERS: sampler_count out of range"))?;

        let end_slot = start_slot
            .checked_add(sampler_count_u32)
            .ok_or_else(|| anyhow!("SET_SAMPLERS: slot range overflow"))?;
        if end_slot as usize > DEFAULT_MAX_SAMPLER_SLOTS {
            bail!(
                "SET_SAMPLERS: slot range out of supported range (range={start_slot}..{end_slot} max_slot={})",
                DEFAULT_MAX_SAMPLER_SLOTS - 1
            );
        }

        let expected = 24 + sampler_count * 4;
        // Forward-compat: allow this packet to grow by appending new fields after `samplers[]`.
        if cmd_bytes.len() < expected {
            bail!(
                "SET_SAMPLERS: expected at least {expected} bytes, got {}",
                cmd_bytes.len(),
            );
        }

        for i in 0..sampler_count {
            let handle = read_u32_le(cmd_bytes, 24 + i * 4)?;
            let bound = if handle == 0 {
                None
            } else {
                Some(BoundSampler { sampler: handle })
            };
            let slot = start_slot
                .checked_add(i as u32)
                .ok_or_else(|| anyhow!("SET_SAMPLERS: slot overflow"))?;
            self.bindings.stage_mut(stage).set_sampler(slot, bound);
        }

        Ok(())
    }

    fn exec_set_constant_buffers(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        // struct aerogpu_cmd_set_constant_buffers (24 bytes) + bindings.
        if cmd_bytes.len() < 24 {
            bail!(
                "SET_CONSTANT_BUFFERS: expected at least 24 bytes, got {}",
                cmd_bytes.len()
            );
        }
        let stage_raw = read_u32_le(cmd_bytes, 8)?;
        let start_slot_u32 = read_u32_le(cmd_bytes, 12)?;
        let buffer_count_u32 = read_u32_le(cmd_bytes, 16)?;

        let stage = ShaderStage::from_aerogpu_u32(stage_raw)
            .ok_or_else(|| anyhow!("SET_CONSTANT_BUFFERS: unknown shader stage {stage_raw}"))?;
        let start_slot: u32 = start_slot_u32;
        let buffer_count: usize = buffer_count_u32
            .try_into()
            .map_err(|_| anyhow!("SET_CONSTANT_BUFFERS: buffer_count out of range"))?;

        let end_slot = start_slot
            .checked_add(buffer_count_u32)
            .ok_or_else(|| anyhow!("SET_CONSTANT_BUFFERS: slot range overflow"))?;
        if end_slot as usize > DEFAULT_MAX_CONSTANT_BUFFER_SLOTS {
            bail!(
                "SET_CONSTANT_BUFFERS: slot range out of supported range (range={start_slot}..{end_slot} max_slot={})",
                DEFAULT_MAX_CONSTANT_BUFFER_SLOTS - 1
            );
        }

        let expected = 24 + buffer_count * 16;
        // Forward-compat: allow this packet to grow by appending new fields after `bindings[]`.
        if cmd_bytes.len() < expected {
            bail!(
                "SET_CONSTANT_BUFFERS: expected at least {expected} bytes, got {}",
                cmd_bytes.len(),
            );
        }

        for i in 0..buffer_count {
            let base = 24 + i * 16;
            let buffer_raw = read_u32_le(cmd_bytes, base)?;
            let offset_bytes = read_u32_le(cmd_bytes, base + 4)?;
            let size_bytes = read_u32_le(cmd_bytes, base + 8)?;
            // reserved0 @ +12 ignored.

            let bound = if buffer_raw == 0 {
                None
            } else {
                let buffer = self.shared_surfaces.resolve_handle(buffer_raw);
                Some(BoundConstantBuffer {
                    buffer,
                    offset: offset_bytes as u64,
                    size: (size_bytes != 0).then_some(size_bytes as u64),
                })
            };
            let slot = start_slot
                .checked_add(i as u32)
                .ok_or_else(|| anyhow!("SET_CONSTANT_BUFFERS: slot overflow"))?;
            self.bindings
                .stage_mut(stage)
                .set_constant_buffer(slot, bound);
        }

        Ok(())
    }

    fn exec_create_input_layout(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        let (cmd, blob) = decode_cmd_create_input_layout_blob_le(cmd_bytes)
            .map_err(|e| anyhow!("CREATE_INPUT_LAYOUT: invalid payload: {e:?}"))?;
        let handle = cmd.input_layout_handle;

        let layout = InputLayoutDesc::parse(blob)
            .map_err(|e| anyhow!("CREATE_INPUT_LAYOUT: failed to parse ILAY blob: {e}"))?;
        let mut used_slots: Vec<u32> = layout.elements.iter().map(|e| e.input_slot).collect();
        used_slots.sort_unstable();
        used_slots.dedup();
        self.resources.input_layouts.insert(
            handle,
            InputLayoutResource {
                layout,
                used_slots,
                mapping_cache: HashMap::new(),
            },
        );
        Ok(())
    }

    fn exec_destroy_input_layout(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        if cmd_bytes.len() < 16 {
            bail!(
                "DESTROY_INPUT_LAYOUT: expected at least 16 bytes, got {}",
                cmd_bytes.len()
            );
        }
        let handle = read_u32_le(cmd_bytes, 8)?;
        self.resources.input_layouts.remove(&handle);
        if self.state.input_layout == Some(handle) {
            self.state.input_layout = None;
        }
        Ok(())
    }

    fn exec_set_input_layout(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        if cmd_bytes.len() < 16 {
            bail!(
                "SET_INPUT_LAYOUT: expected at least 16 bytes, got {}",
                cmd_bytes.len()
            );
        }
        let handle = read_u32_le(cmd_bytes, 8)?;
        self.state.input_layout = if handle == 0 { None } else { Some(handle) };
        Ok(())
    }

    fn exec_set_render_targets(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        // struct aerogpu_cmd_set_render_targets (48 bytes)
        if cmd_bytes.len() < 48 {
            bail!(
                "SET_RENDER_TARGETS: expected at least 48 bytes, got {}",
                cmd_bytes.len()
            );
        }
        let color_count = read_u32_le(cmd_bytes, 8)? as usize;
        let depth_stencil = read_u32_le(cmd_bytes, 12)?;
        if color_count > 8 {
            bail!("SET_RENDER_TARGETS: color_count out of range: {color_count}");
        }
        let mut colors = Vec::with_capacity(color_count);
        let mut seen_gap = false;
        for i in 0..color_count {
            let tex_id = read_u32_le(cmd_bytes, 16 + i * 4)?;
            if tex_id == 0 {
                seen_gap = true;
                continue;
            }
            if seen_gap {
                bail!("SET_RENDER_TARGETS: render target slot {i} is set after an earlier slot was unbound (gaps are not supported yet)");
            }
            colors.push(self.shared_surfaces.resolve_handle(tex_id));
        }
        self.state.render_targets = colors;
        self.state.depth_stencil = if depth_stencil == 0 {
            None
        } else {
            Some(self.shared_surfaces.resolve_handle(depth_stencil))
        };
        Ok(())
    }

    fn exec_set_viewport(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        // struct aerogpu_cmd_set_viewport (32 bytes)
        if cmd_bytes.len() < 32 {
            bail!(
                "SET_VIEWPORT: expected at least 32 bytes, got {}",
                cmd_bytes.len()
            );
        }
        let x = f32::from_bits(read_u32_le(cmd_bytes, 8)?);
        let y = f32::from_bits(read_u32_le(cmd_bytes, 12)?);
        let width = f32::from_bits(read_u32_le(cmd_bytes, 16)?);
        let height = f32::from_bits(read_u32_le(cmd_bytes, 20)?);
        let min_depth = f32::from_bits(read_u32_le(cmd_bytes, 24)?);
        let max_depth = f32::from_bits(read_u32_le(cmd_bytes, 28)?);
        self.state.viewport = Some(Viewport {
            x,
            y,
            width,
            height,
            min_depth,
            max_depth,
        });
        Ok(())
    }

    fn exec_set_scissor(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        // struct aerogpu_cmd_set_scissor (24 bytes)
        if cmd_bytes.len() < 24 {
            bail!(
                "SET_SCISSOR: expected at least 24 bytes, got {}",
                cmd_bytes.len()
            );
        }
        let x = read_i32_le(cmd_bytes, 8)?;
        let y = read_i32_le(cmd_bytes, 12)?;
        let w = read_i32_le(cmd_bytes, 16)?;
        let h = read_i32_le(cmd_bytes, 20)?;
        if w <= 0 || h <= 0 {
            self.state.scissor = None;
            return Ok(());
        }
        let left = x.max(0);
        let top = y.max(0);
        let right = x.saturating_add(w).max(0);
        let bottom = y.saturating_add(h).max(0);
        if right <= left || bottom <= top {
            self.state.scissor = None;
            return Ok(());
        }
        self.state.scissor = Some(Scissor {
            x: left as u32,
            y: top as u32,
            width: (right - left) as u32,
            height: (bottom - top) as u32,
        });
        Ok(())
    }

    fn exec_set_vertex_buffers(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        let (cmd, bindings) = decode_cmd_set_vertex_buffers_bindings_le(cmd_bytes)
            .map_err(|e| anyhow!("SET_VERTEX_BUFFERS: invalid payload: {e:?}"))?;
        let start_slot = cmd.start_slot as usize;
        let buffer_count = cmd.buffer_count as usize;

        if start_slot + buffer_count > self.state.vertex_buffers.len() {
            bail!("SET_VERTEX_BUFFERS: slot range out of bounds");
        }

        for (i, binding) in bindings.iter().copied().enumerate() {
            let buffer_raw = u32::from_le(binding.buffer);
            let buffer = if buffer_raw == 0 {
                0
            } else {
                self.shared_surfaces.resolve_handle(buffer_raw)
            };
            let stride_bytes = u32::from_le(binding.stride_bytes);
            let offset_bytes = u64::from(u32::from_le(binding.offset_bytes));

            self.state.vertex_buffers[start_slot + i] = if buffer == 0 {
                None
            } else {
                Some(VertexBufferBinding {
                    buffer,
                    stride_bytes,
                    offset_bytes,
                })
            };
        }
        Ok(())
    }

    fn exec_set_index_buffer(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        // struct aerogpu_cmd_set_index_buffer (24 bytes)
        if cmd_bytes.len() < 24 {
            bail!(
                "SET_INDEX_BUFFER: expected at least 24 bytes, got {}",
                cmd_bytes.len()
            );
        }
        let buffer = read_u32_le(cmd_bytes, 8)?;
        let format_u32 = read_u32_le(cmd_bytes, 12)?;
        let offset_bytes = read_u32_le(cmd_bytes, 16)? as u64;

        if buffer == 0 {
            self.state.index_buffer = None;
            return Ok(());
        }
        let buffer = self.shared_surfaces.resolve_handle(buffer);

        let format = match format_u32 {
            0 => wgpu::IndexFormat::Uint16,
            1 => wgpu::IndexFormat::Uint32,
            _ => bail!("SET_INDEX_BUFFER: unknown index format {format_u32}"),
        };
        self.state.index_buffer = Some(IndexBufferBinding {
            buffer,
            format,
            offset_bytes,
        });
        Ok(())
    }

    fn exec_set_primitive_topology(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        // struct aerogpu_cmd_set_primitive_topology (16 bytes)
        if cmd_bytes.len() < 16 {
            bail!(
                "SET_PRIMITIVE_TOPOLOGY: expected at least 16 bytes, got {}",
                cmd_bytes.len()
            );
        }
        let topology_u32 = read_u32_le(cmd_bytes, 8)?;
        self.state.primitive_topology = match topology_u32 {
            1 => wgpu::PrimitiveTopology::PointList,
            2 => wgpu::PrimitiveTopology::LineList,
            3 => wgpu::PrimitiveTopology::LineStrip,
            4 => wgpu::PrimitiveTopology::TriangleList,
            5 => wgpu::PrimitiveTopology::TriangleStrip,
            // TriangleFan is not directly supported; fall back to TriangleList.
            6 => wgpu::PrimitiveTopology::TriangleList,
            other => bail!("SET_PRIMITIVE_TOPOLOGY: unknown topology {other}"),
        };
        Ok(())
    }
    fn exec_set_blend_state(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        // struct aerogpu_cmd_set_blend_state (28 bytes minimum; extended in newer ABI versions).
        if cmd_bytes.len() < 28 {
            bail!(
                "SET_BLEND_STATE: expected at least 28 bytes, got {}",
                cmd_bytes.len()
            );
        }
        let enable = read_u32_le(cmd_bytes, 8)? != 0;
        let src_factor = read_u32_le(cmd_bytes, 12)?;
        let dst_factor = read_u32_le(cmd_bytes, 16)?;
        let op = read_u32_le(cmd_bytes, 20)?;
        let write_mask = cmd_bytes[24];

        self.state.color_write_mask = map_color_write_mask(write_mask);

        // Optional extended fields (default when absent).
        let src_factor_alpha = if cmd_bytes.len() >= 32 {
            read_u32_le(cmd_bytes, 28)?
        } else {
            src_factor
        };
        let dst_factor_alpha = if cmd_bytes.len() >= 36 {
            read_u32_le(cmd_bytes, 32)?
        } else {
            dst_factor
        };
        let op_alpha = if cmd_bytes.len() >= 40 {
            read_u32_le(cmd_bytes, 36)?
        } else {
            op
        };

        let mut blend_constant = [1.0f32; 4];
        if cmd_bytes.len() >= 44 {
            blend_constant[0] = f32::from_bits(read_u32_le(cmd_bytes, 40)?);
        }
        if cmd_bytes.len() >= 48 {
            blend_constant[1] = f32::from_bits(read_u32_le(cmd_bytes, 44)?);
        }
        if cmd_bytes.len() >= 52 {
            blend_constant[2] = f32::from_bits(read_u32_le(cmd_bytes, 48)?);
        }
        if cmd_bytes.len() >= 56 {
            blend_constant[3] = f32::from_bits(read_u32_le(cmd_bytes, 52)?);
        }
        let sample_mask = if cmd_bytes.len() >= 60 {
            read_u32_le(cmd_bytes, 56)?
        } else {
            0xFFFF_FFFF
        };

        self.state.blend_constant = blend_constant;
        self.state.sample_mask = sample_mask;

        if !enable {
            self.state.blend = None;
            return Ok(());
        }

        let src = map_blend_factor(src_factor).unwrap_or(wgpu::BlendFactor::One);
        let dst = map_blend_factor(dst_factor).unwrap_or(wgpu::BlendFactor::Zero);
        let op = map_blend_op(op).unwrap_or(wgpu::BlendOperation::Add);

        let src_a = map_blend_factor(src_factor_alpha).unwrap_or(src);
        let dst_a = map_blend_factor(dst_factor_alpha).unwrap_or(dst);
        let op_a = map_blend_op(op_alpha).unwrap_or(op);

        self.state.blend = Some(wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: src,
                dst_factor: dst,
                operation: op,
            },
            alpha: wgpu::BlendComponent {
                src_factor: src_a,
                dst_factor: dst_a,
                operation: op_a,
            },
        });
        Ok(())
    }

    fn exec_set_depth_stencil_state(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        use aero_protocol::aerogpu::aerogpu_cmd::AerogpuCmdSetDepthStencilState;

        // struct aerogpu_cmd_set_depth_stencil_state (28 bytes)
        if cmd_bytes.len() < std::mem::size_of::<AerogpuCmdSetDepthStencilState>() {
            bail!(
                "SET_DEPTH_STENCIL_STATE: expected at least {} bytes, got {}",
                std::mem::size_of::<AerogpuCmdSetDepthStencilState>(),
                cmd_bytes.len()
            );
        }
        let cmd: AerogpuCmdSetDepthStencilState = read_packed_unaligned(cmd_bytes)?;
        let state = cmd.state;

        let depth_enable = u32::from_le(state.depth_enable) != 0;
        let depth_write_enable = u32::from_le(state.depth_write_enable) != 0;
        let depth_func = u32::from_le(state.depth_func);
        let stencil_enable = u32::from_le(state.stencil_enable) != 0;

        self.state.depth_enable = depth_enable;
        self.state.depth_write_enable = depth_write_enable;
        self.state.depth_compare =
            map_compare_func(depth_func).unwrap_or(wgpu::CompareFunction::Always);
        self.state.stencil_enable = stencil_enable;
        self.state.stencil_read_mask = state.stencil_read_mask;
        self.state.stencil_write_mask = state.stencil_write_mask;
        Ok(())
    }

    fn exec_set_rasterizer_state(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        use aero_protocol::aerogpu::aerogpu_cmd::AerogpuCmdSetRasterizerState;

        // struct aerogpu_cmd_set_rasterizer_state (32 bytes)
        if cmd_bytes.len() < std::mem::size_of::<AerogpuCmdSetRasterizerState>() {
            bail!(
                "SET_RASTERIZER_STATE: expected at least {} bytes, got {}",
                std::mem::size_of::<AerogpuCmdSetRasterizerState>(),
                cmd_bytes.len()
            );
        }
        let cmd: AerogpuCmdSetRasterizerState = read_packed_unaligned(cmd_bytes)?;
        let state = cmd.state;

        let cull_mode = u32::from_le(state.cull_mode);
        let front_ccw = u32::from_le(state.front_ccw) != 0;
        let scissor_enable = u32::from_le(state.scissor_enable) != 0;
        let depth_bias = i32::from_le(state.depth_bias);
        let flags = u32::from_le(state.flags);

        self.state.cull_mode = match cull_mode {
            0 => None,
            1 => Some(wgpu::Face::Front),
            2 => Some(wgpu::Face::Back),
            _ => self.state.cull_mode,
        };
        self.state.front_face = if front_ccw {
            wgpu::FrontFace::Ccw
        } else {
            wgpu::FrontFace::Cw
        };
        self.state.scissor_enable = scissor_enable;
        self.state.depth_bias = depth_bias;
        self.state.depth_clip_enabled = flags & AEROGPU_RASTERIZER_FLAG_DEPTH_CLIP_DISABLE == 0;
        Ok(())
    }

    fn exec_clear(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        cmd_bytes: &[u8],
        allocs: &AllocTable,
        guest_mem: &mut dyn GuestMemory,
    ) -> Result<()> {
        // struct aerogpu_cmd_clear (36 bytes)
        if cmd_bytes.len() < 36 {
            bail!("CLEAR: expected at least 36 bytes, got {}", cmd_bytes.len());
        }
        if self.state.render_targets.is_empty() && self.state.depth_stencil.is_none() {
            // Nothing bound; treat as no-op for robustness.
            return Ok(());
        }

        let flags = read_u32_le(cmd_bytes, 8)?;
        if flags == 0 {
            // Clearing with no flags set is a no-op; avoid forcing an otherwise-unnecessary render
            // pass boundary.
            return Ok(());
        }

        let render_targets = self.state.render_targets.clone();
        let depth_stencil = self.state.depth_stencil;
        for &handle in &render_targets {
            self.ensure_texture_uploaded(encoder, handle, allocs, guest_mem)?;
        }
        if let Some(handle) = depth_stencil {
            self.ensure_texture_uploaded(encoder, handle, allocs, guest_mem)?;
        }

        for &handle in &render_targets {
            self.encoder_used_textures.insert(handle);
        }
        if let Some(handle) = depth_stencil {
            self.encoder_used_textures.insert(handle);
        }

        let color = [
            f32::from_bits(read_u32_le(cmd_bytes, 12)?),
            f32::from_bits(read_u32_le(cmd_bytes, 16)?),
            f32::from_bits(read_u32_le(cmd_bytes, 20)?),
            f32::from_bits(read_u32_le(cmd_bytes, 24)?),
        ];
        let depth = f32::from_bits(read_u32_le(cmd_bytes, 28)?);
        let stencil = read_u32_le(cmd_bytes, 32)? as u32;

        // Clear writes modify the underlying textures; invalidate any CPU shadows.
        if flags & AEROGPU_CLEAR_COLOR != 0 {
            for &handle in &render_targets {
                if let Some(tex) = self.resources.textures.get_mut(&handle) {
                    tex.host_shadow = None;
                }
            }
        }
        if (flags & (AEROGPU_CLEAR_DEPTH | AEROGPU_CLEAR_STENCIL)) != 0 {
            if let Some(handle) = depth_stencil {
                if let Some(tex) = self.resources.textures.get_mut(&handle) {
                    tex.host_shadow = None;
                }
            }
        }

        self.encoder_has_commands = true;
        let (mut color_attachments, mut depth_stencil_attachment) =
            build_render_pass_attachments(&self.resources, &self.state, wgpu::LoadOp::Load)?;

        if flags & AEROGPU_CLEAR_COLOR != 0 {
            for att in &mut color_attachments {
                if let Some(att) = att.as_mut() {
                    att.ops.load = wgpu::LoadOp::Clear(wgpu::Color {
                        r: color[0] as f64,
                        g: color[1] as f64,
                        b: color[2] as f64,
                        a: color[3] as f64,
                    });
                }
            }
        }

        if let Some(ds) = depth_stencil_attachment.as_mut() {
            if flags & AEROGPU_CLEAR_DEPTH != 0 {
                if let Some(depth_ops) = ds.depth_ops.as_mut() {
                    depth_ops.load = wgpu::LoadOp::Clear(depth);
                }
            }
            if flags & AEROGPU_CLEAR_STENCIL != 0 {
                if let Some(stencil_ops) = ds.stencil_ops.as_mut() {
                    stencil_ops.load = wgpu::LoadOp::Clear(stencil);
                }
            }
        }

        let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("aerogpu_cmd clear pass"),
            color_attachments: &color_attachments,
            depth_stencil_attachment,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        Ok(())
    }

    fn exec_present(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        cmd_bytes: &[u8],
        report: &mut ExecuteReport,
    ) -> Result<()> {
        // struct aerogpu_cmd_present (16 bytes)
        if cmd_bytes.len() < 16 {
            bail!(
                "PRESENT: expected at least 16 bytes, got {}",
                cmd_bytes.len()
            );
        }
        let scanout_id = read_u32_le(cmd_bytes, 8)?;
        let flags = read_u32_le(cmd_bytes, 12)?;
        let presented_render_target = self
            .state
            .render_targets
            .first()
            .copied()
            .map(|h| self.shared_surfaces.resolve_handle(h));
        report.presents.push(PresentEvent {
            scanout_id,
            flags,
            d3d9_present_flags: None,
            presented_render_target,
        });
        self.submit_encoder(encoder, "aerogpu_cmd encoder after present");
        Ok(())
    }

    fn exec_present_ex(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        cmd_bytes: &[u8],
        report: &mut ExecuteReport,
    ) -> Result<()> {
        // struct aerogpu_cmd_present_ex (24 bytes)
        if cmd_bytes.len() < 24 {
            bail!(
                "PRESENT_EX: expected at least 24 bytes, got {}",
                cmd_bytes.len()
            );
        }
        let scanout_id = read_u32_le(cmd_bytes, 8)?;
        let flags = read_u32_le(cmd_bytes, 12)?;
        let d3d9_present_flags = read_u32_le(cmd_bytes, 16)?;
        let presented_render_target = self
            .state
            .render_targets
            .first()
            .copied()
            .map(|h| self.shared_surfaces.resolve_handle(h));
        report.presents.push(PresentEvent {
            scanout_id,
            flags,
            d3d9_present_flags: Some(d3d9_present_flags),
            presented_render_target,
        });
        self.submit_encoder(encoder, "aerogpu_cmd encoder after present_ex");
        Ok(())
    }

    fn exec_flush(&mut self, encoder: &mut wgpu::CommandEncoder) -> Result<()> {
        self.submit_encoder(encoder, "aerogpu_cmd encoder after flush");
        Ok(())
    }

    fn ensure_bound_resources_uploaded(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        pipeline_bindings: &reflection_bindings::PipelineBindingsInfo,
        allocs: &AllocTable,
        guest_mem: &mut dyn GuestMemory,
    ) -> Result<()> {
        let uniform_align = self.device.limits().min_uniform_buffer_offset_alignment as u64;
        let max_uniform_binding_size = self.device.limits().max_uniform_buffer_binding_size as u64;

        for (group_index, group_bindings) in pipeline_bindings.group_bindings.iter().enumerate() {
            let stage = group_index_to_stage(group_index as u32)?;
            for binding in group_bindings {
                match binding.kind {
                    crate::BindingKind::ConstantBuffer { slot, .. } => {
                        if let Some(cb) = self.bindings.stage(stage).constant_buffer(slot) {
                            if cb.buffer != legacy_constants_buffer_id(stage) {
                                self.ensure_buffer_uploaded(encoder, cb.buffer, allocs, guest_mem)?;
                            }
                        }
                    }
                    crate::BindingKind::Texture2D { slot } => {
                        if let Some(tex) = self.bindings.stage(stage).texture(slot) {
                            self.ensure_texture_uploaded(encoder, tex.texture, allocs, guest_mem)?;
                        }
                    }
                    crate::BindingKind::Sampler { .. } => {}
                }
            }
        }

        // WebGPU requires uniform buffer binding offsets to be aligned to
        // `min_uniform_buffer_offset_alignment`. D3D11 constant-buffer range binding does not have
        // this restriction, so for unaligned offsets we copy the bound range into an internal
        // scratch buffer and bind that at offset 0.
        for (group_index, group_bindings) in pipeline_bindings.group_bindings.iter().enumerate() {
            let stage = group_index_to_stage(group_index as u32)?;
            for binding in group_bindings {
                let crate::BindingKind::ConstantBuffer { slot, reg_count } = binding.kind else {
                    continue;
                };
                let Some(cb) = self.bindings.stage(stage).constant_buffer(slot) else {
                    continue;
                };

                let offset = cb.offset;
                if offset == 0 || offset % uniform_align == 0 {
                    continue;
                }

                // Resolve the source buffer and its size.
                let (src_ptr, src_size) = if cb.buffer == legacy_constants_buffer_id(stage) {
                    let buf = self
                        .legacy_constants
                        .get(&stage)
                        .expect("legacy constants buffer exists for every stage");
                    (buf as *const wgpu::Buffer, LEGACY_CONSTANTS_SIZE_BYTES)
                } else if let Some(buf) = self.resources.buffers.get(&cb.buffer) {
                    (&buf.buffer as *const wgpu::Buffer, buf.size)
                } else {
                    continue;
                };

                if offset >= src_size {
                    continue;
                }
                let mut size = cb.size.unwrap_or(src_size - offset);
                size = size.min(src_size - offset);
                let required_min = (reg_count as u64)
                    .saturating_mul(reflection_bindings::UNIFORM_BINDING_SIZE_ALIGN);
                if size < required_min {
                    continue;
                }
                if size > max_uniform_binding_size {
                    size = max_uniform_binding_size;
                    if size < required_min {
                        continue;
                    }
                }
                size -= size % reflection_bindings::UNIFORM_BINDING_SIZE_ALIGN;
                if size < required_min || size == 0 {
                    continue;
                }

                if !offset.is_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT)
                    || !size.is_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT)
                {
                    bail!(
                        "constant buffer scratch copy requires COPY_BUFFER_ALIGNMENT={} (stage={stage:?} slot={slot} offset={offset} size={size})",
                        wgpu::COPY_BUFFER_ALIGNMENT
                    );
                }

                let scratch = self.get_or_create_constant_buffer_scratch(stage, slot, size);
                let src = unsafe { &*src_ptr };
                encoder.copy_buffer_to_buffer(src, offset, &scratch.buffer, 0, size);
                self.encoder_has_commands = true;
                self.encoder_used_buffers.insert(cb.buffer);
            }
        }

        Ok(())
    }

    fn get_or_create_constant_buffer_scratch(
        &mut self,
        stage: ShaderStage,
        slot: u32,
        size: u64,
    ) -> &ConstantBufferScratch {
        let key = (stage, slot);
        let needs_new = self
            .cbuffer_scratch
            .get(&key)
            .map(|existing| existing.size < size)
            .unwrap_or(true);

        if needs_new {
            let id = BufferId(self.next_scratch_buffer_id);
            self.next_scratch_buffer_id = self.next_scratch_buffer_id.wrapping_add(1);

            let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("aerogpu_cmd constant buffer scratch"),
                size,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.cbuffer_scratch
                .insert(key, ConstantBufferScratch { id, buffer, size });
        }

        self.cbuffer_scratch
            .get(&key)
            .expect("scratch buffer inserted above")
    }

    fn ensure_buffer_uploaded(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        buffer_handle: u32,
        allocs: &AllocTable,
        guest_mem: &mut dyn GuestMemory,
    ) -> Result<()> {
        let needs_upload = self
            .resources
            .buffers
            .get(&buffer_handle)
            .is_some_and(|buf| buf.backing.is_some() && buf.dirty.is_some());
        if !needs_upload {
            return Ok(());
        }

        // Preserve command stream ordering relative to any previously encoded GPU work.
        self.submit_encoder_if_has_commands(
            encoder,
            "aerogpu_cmd encoder after implicit buffer upload",
        );
        self.upload_buffer_from_guest_memory(buffer_handle, allocs, guest_mem)
    }

    fn upload_buffer_from_guest_memory(
        &mut self,
        buffer_handle: u32,
        allocs: &AllocTable,
        guest_mem: &mut dyn GuestMemory,
    ) -> Result<()> {
        let (dirty, backing, buffer_size, buffer_gpu_size) = {
            let Some(buf) = self.resources.buffers.get(&buffer_handle) else {
                return Ok(());
            };
            let Some(backing) = buf.backing else {
                return Ok(());
            };
            let Some(dirty) = buf.dirty.clone() else {
                return Ok(());
            };
            (dirty, backing, buf.size, buf.gpu_size)
        };

        let dirty_len = dirty.end.saturating_sub(dirty.start);
        if dirty_len == 0 {
            return Ok(());
        }
        if !dirty.start.is_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT) {
            bail!(
                "buffer {buffer_handle} dirty range start {} does not respect COPY_BUFFER_ALIGNMENT",
                dirty.start
            );
        }

        allocs.validate_range(
            backing.alloc_id,
            backing.offset_bytes + dirty.start,
            dirty_len,
        )?;
        let gpa = allocs.gpa(backing.alloc_id)? + backing.offset_bytes + dirty.start;

        let Some(buf) = self.resources.buffers.get(&buffer_handle) else {
            return Ok(());
        };

        // Upload in chunks to avoid allocating massive temporary buffers for big resources.
        const CHUNK: usize = 64 * 1024;
        let mut offset = dirty.start;
        while offset < dirty.end {
            let remaining = (dirty.end - offset) as usize;
            let n = remaining.min(CHUNK);
            let mut tmp = vec![0u8; n];
            guest_mem
                .read(gpa + (offset - dirty.start), &mut tmp)
                .map_err(anyhow_guest_mem)?;

            let align = wgpu::COPY_BUFFER_ALIGNMENT as usize;
            let write_len = if !n.is_multiple_of(align) {
                if offset + n as u64 != dirty.end || dirty.end != buffer_size {
                    bail!("buffer {buffer_handle} upload is not COPY_BUFFER_ALIGNMENT-aligned");
                }
                let padded = align4(n);
                tmp.resize(padded, 0);
                padded
            } else {
                n
            };

            let end = offset
                .checked_add(write_len as u64)
                .ok_or_else(|| anyhow!("buffer upload range overflows u64"))?;
            if end > buffer_gpu_size {
                bail!("buffer upload overruns wgpu buffer allocation");
            }

            self.queue
                .write_buffer(&buf.buffer, offset, &tmp[..write_len]);
            offset += n as u64;
        }

        if let Some(buf_mut) = self.resources.buffers.get_mut(&buffer_handle) {
            buf_mut.dirty = None;
        }

        Ok(())
    }

    fn upload_texture_from_guest_memory(
        &mut self,
        texture_handle: u32,
        allocs: &AllocTable,
        guest_mem: &mut dyn GuestMemory,
    ) -> Result<()> {
        let (desc, format_u32, row_pitch_bytes, backing) = match self
            .resources
            .textures
            .get(&texture_handle)
        {
            Some(tex) if tex.dirty => (tex.desc, tex.format_u32, tex.row_pitch_bytes, tex.backing),
            _ => return Ok(()),
        };

        let Some(backing) = backing else {
            if let Some(tex) = self.resources.textures.get_mut(&texture_handle) {
                tex.dirty = false;
            }
            return Ok(());
        };

        let force_opaque_alpha = aerogpu_format_is_x8(format_u32);

        let format_layout = aerogpu_texture_format_layout(format_u32)?;
        let mip_extent = |v: u32, level: u32| v.checked_shr(level).unwrap_or(0).max(1);
        let mip_levels = desc.mip_level_count;
        let array_layers = desc.array_layers;

        let guest_layout = compute_guest_texture_layout(
            format_u32,
            desc.width,
            desc.height,
            mip_levels,
            array_layers,
            row_pitch_bytes,
        )
        .context("texture upload: compute guest layout")?;

        allocs.validate_range(
            backing.alloc_id,
            backing.offset_bytes,
            guest_layout.total_size,
        )?;
        let base_gpa = allocs
            .gpa(backing.alloc_id)?
            .checked_add(backing.offset_bytes)
            .ok_or_else(|| anyhow!("texture upload GPA overflow"))?;

        let queue = &self.queue;
        let Some(tex) = self.resources.textures.get_mut(&texture_handle) else {
            return Ok(());
        };

        // Avoid allocating `bytes_per_row * height` (and potentially a second repack buffer) for
        // large textures. We upload in row chunks, repacking only when required by WebGPU's
        // `COPY_BYTES_PER_ROW_ALIGNMENT`.
        const CHUNK_BYTES: usize = 256 * 1024;

        #[allow(clippy::too_many_arguments)]
        fn upload_subresource(
            queue: &wgpu::Queue,
            texture: &wgpu::Texture,
            format: wgpu::TextureFormat,
            mip_level: u32,
            array_layer: u32,
            width: u32,
            height: u32,
            bytes_per_row: u32,
            force_opaque_alpha: bool,
            gpa: u64,
            guest_mem: &mut dyn GuestMemory,
        ) -> Result<()> {
            let bpt = bytes_per_texel(format)?;
            let unpadded_bpr = width
                .checked_mul(bpt)
                .ok_or_else(|| anyhow!("texture upload bytes_per_row overflow"))?;
            if bytes_per_row < unpadded_bpr {
                bail!("texture upload bytes_per_row too small");
            }

            let aligned = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
            let height_usize: usize = height
                .try_into()
                .map_err(|_| anyhow!("texture upload height out of range"))?;
            let src_row_pitch = bytes_per_row as usize;

            let repack_padded_bpr = if height > 1 {
                let padded_bpr = unpadded_bpr
                    .checked_add(aligned - 1)
                    .map(|v| v / aligned)
                    .and_then(|v| v.checked_mul(aligned))
                    .ok_or_else(|| anyhow!("texture upload padded bytes_per_row overflow"))?;
                // If the guest row pitch isn't a valid WebGPU `bytes_per_row` (alignment) or contains
                // extra padding beyond what WebGPU requires, repack into the minimal aligned stride.
                (bytes_per_row != padded_bpr).then_some(padded_bpr)
            } else {
                None
            };

            if let Some(padded_bpr) = repack_padded_bpr {
                let padded_bpr_usize = padded_bpr as usize;
                let rows_per_chunk = (CHUNK_BYTES / padded_bpr_usize).max(1);

                let mut row_buf = vec![0u8; unpadded_bpr as usize];
                for y0 in (0..height_usize).step_by(rows_per_chunk) {
                    let rows = (height_usize - y0).min(rows_per_chunk);
                    let repacked_len = padded_bpr_usize
                        .checked_mul(rows)
                        .ok_or_else(|| anyhow!("texture upload chunk overflows usize"))?;
                    let mut repacked = vec![0u8; repacked_len];
                    for row in 0..rows {
                        let row_index = y0
                            .checked_add(row)
                            .ok_or_else(|| anyhow!("texture upload row index overflows usize"))?;
                        let row_offset = u64::try_from(row_index)
                            .ok()
                            .and_then(|v| v.checked_mul(u64::from(bytes_per_row)))
                            .ok_or_else(|| anyhow!("texture upload row offset overflows u64"))?;
                        let src_addr = gpa
                            .checked_add(row_offset)
                            .ok_or_else(|| anyhow!("texture upload address overflows u64"))?;
                        guest_mem
                            .read(src_addr, &mut row_buf)
                            .map_err(anyhow_guest_mem)?;
                        if force_opaque_alpha {
                            force_opaque_alpha_rgba8(&mut row_buf);
                        }
                        let dst_start = row * padded_bpr_usize;
                        repacked[dst_start..dst_start + row_buf.len()].copy_from_slice(&row_buf);
                    }

                    queue.write_texture(
                        wgpu::ImageCopyTexture {
                            texture,
                            mip_level,
                            origin: wgpu::Origin3d {
                                x: 0,
                                y: y0 as u32,
                                z: array_layer,
                            },
                            aspect: wgpu::TextureAspect::All,
                        },
                        &repacked,
                        wgpu::ImageDataLayout {
                            offset: 0,
                            bytes_per_row: Some(padded_bpr),
                            rows_per_image: Some(rows as u32),
                        },
                        wgpu::Extent3d {
                            width,
                            height: rows as u32,
                            depth_or_array_layers: 1,
                        },
                    );
                }
            } else {
                // `bytes_per_row` is already aligned (or the copy is a single row). Upload contiguous
                // chunks directly from guest memory.
                let rows_per_chunk = (CHUNK_BYTES / src_row_pitch).max(1);
                let mut tmp = vec![0u8; src_row_pitch * rows_per_chunk];
                for y0 in (0..height_usize).step_by(rows_per_chunk) {
                    let rows = (height_usize - y0).min(rows_per_chunk);
                    let byte_len = src_row_pitch
                        .checked_mul(rows)
                        .ok_or_else(|| anyhow!("texture upload chunk overflows usize"))?;
                    let tmp_slice = &mut tmp[..byte_len];
                    let row_offset = u64::try_from(y0)
                        .ok()
                        .and_then(|v| v.checked_mul(u64::from(bytes_per_row)))
                        .ok_or_else(|| anyhow!("texture upload row offset overflows u64"))?;
                    let src_addr = gpa
                        .checked_add(row_offset)
                        .ok_or_else(|| anyhow!("texture upload address overflows u64"))?;
                    guest_mem
                        .read(src_addr, tmp_slice)
                        .map_err(anyhow_guest_mem)?;
                    if force_opaque_alpha {
                        let unpadded_bpr_usize = unpadded_bpr as usize;
                        for row in 0..rows {
                            let start = row
                                .checked_mul(src_row_pitch)
                                .ok_or_else(|| anyhow!("texture upload row offset overflows usize"))?;
                            let end = start.checked_add(unpadded_bpr_usize).ok_or_else(|| {
                                anyhow!("texture upload row end overflows usize")
                            })?;
                            force_opaque_alpha_rgba8(tmp_slice.get_mut(start..end).ok_or_else(
                                || anyhow!("texture upload staging buffer too small"),
                            )?);
                        }
                    }

                    queue.write_texture(
                        wgpu::ImageCopyTexture {
                            texture,
                            mip_level,
                            origin: wgpu::Origin3d {
                                x: 0,
                                y: y0 as u32,
                                z: array_layer,
                            },
                            aspect: wgpu::TextureAspect::All,
                        },
                        tmp_slice,
                        wgpu::ImageDataLayout {
                            offset: 0,
                            bytes_per_row: Some(bytes_per_row),
                            rows_per_image: Some(rows as u32),
                        },
                        wgpu::Extent3d {
                            width,
                            height: rows as u32,
                            depth_or_array_layers: 1,
                        },
                    );
                }
            }

            Ok(())
        }

        #[allow(clippy::too_many_arguments)]
        fn upload_bc_subresource(
            queue: &wgpu::Queue,
            texture: &wgpu::Texture,
            mip_level: u32,
            array_layer: u32,
            width: u32,
            height: u32,
            bytes_per_row: u32,
            block_bytes: u32,
            gpa: u64,
            guest_mem: &mut dyn GuestMemory,
        ) -> Result<()> {
            let blocks_w = width.div_ceil(4);
            let blocks_h = height.div_ceil(4);
            let unpadded_bpr = blocks_w
                .checked_mul(block_bytes)
                .ok_or_else(|| anyhow!("texture upload: BC bytes_per_row overflow"))?;
            if bytes_per_row < unpadded_bpr {
                bail!("texture upload: BC bytes_per_row too small");
            }

            let aligned = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
            let blocks_h_usize: usize = blocks_h
                .try_into()
                .map_err(|_| anyhow!("texture upload: BC rows out of range"))?;
            let src_row_pitch = bytes_per_row as usize;

            let repack_padded_bpr = if blocks_h > 1 {
                let padded_bpr = unpadded_bpr
                    .checked_add(aligned - 1)
                    .map(|v| v / aligned)
                    .and_then(|v| v.checked_mul(aligned))
                    .ok_or_else(|| anyhow!("texture upload: BC padded bytes_per_row overflow"))?;
                (bytes_per_row != padded_bpr).then_some(padded_bpr)
            } else {
                None
            };

            if let Some(padded_bpr) = repack_padded_bpr {
                let padded_bpr_usize: usize = padded_bpr
                    .try_into()
                    .map_err(|_| anyhow!("texture upload: BC padded bytes_per_row out of range"))?;
                let rows_per_chunk = (CHUNK_BYTES / padded_bpr_usize).max(1);
                let row_len_usize: usize = unpadded_bpr
                    .try_into()
                    .map_err(|_| anyhow!("texture upload: BC bytes_per_row out of range"))?;

                let mut row_buf = vec![0u8; row_len_usize];
                for y0 in (0..blocks_h_usize).step_by(rows_per_chunk) {
                    let rows = (blocks_h_usize - y0).min(rows_per_chunk);
                    let repacked_len = padded_bpr_usize
                        .checked_mul(rows)
                        .ok_or_else(|| anyhow!("texture upload: BC chunk overflows usize"))?;
                    let mut repacked = vec![0u8; repacked_len];
                    for row in 0..rows {
                        let row_index = y0
                            .checked_add(row)
                            .ok_or_else(|| anyhow!("texture upload: BC row index overflows usize"))?;
                        let row_offset = u64::try_from(row_index)
                            .ok()
                            .and_then(|v| v.checked_mul(u64::from(bytes_per_row)))
                            .ok_or_else(|| anyhow!("texture upload: BC row offset overflows u64"))?;
                        let src_addr = gpa
                            .checked_add(row_offset)
                            .ok_or_else(|| anyhow!("texture upload: BC address overflows u64"))?;
                        guest_mem
                            .read(src_addr, &mut row_buf)
                            .map_err(anyhow_guest_mem)?;
                        let dst_start = row * padded_bpr_usize;
                        repacked[dst_start..dst_start + row_buf.len()].copy_from_slice(&row_buf);
                    }

                    let origin_y_texels =
                        (y0 as u32).checked_mul(4).ok_or_else(|| anyhow!("texture upload: BC origin.y overflow"))?;
                    let remaining_height = height
                        .checked_sub(origin_y_texels)
                        .ok_or_else(|| anyhow!("texture upload: BC origin exceeds mip height"))?;
                    let chunk_height_texels = if y0 + rows == blocks_h_usize {
                        remaining_height
                    } else {
                        (rows as u32)
                            .checked_mul(4)
                            .ok_or_else(|| anyhow!("texture upload: BC extent height overflow"))?
                    };

                    queue.write_texture(
                        wgpu::ImageCopyTexture {
                            texture,
                            mip_level,
                            origin: wgpu::Origin3d {
                                x: 0,
                                y: origin_y_texels,
                                z: array_layer,
                            },
                            aspect: wgpu::TextureAspect::All,
                        },
                        &repacked,
                        wgpu::ImageDataLayout {
                            offset: 0,
                            bytes_per_row: Some(padded_bpr),
                            rows_per_image: Some(rows as u32),
                        },
                        wgpu::Extent3d {
                            width,
                            height: chunk_height_texels,
                            depth_or_array_layers: 1,
                        },
                    );
                }
            } else {
                // `bytes_per_row` is already aligned (or the copy is a single block row).
                let rows_per_chunk = (CHUNK_BYTES / src_row_pitch).max(1);
                let mut tmp = vec![0u8; src_row_pitch * rows_per_chunk];
                for y0 in (0..blocks_h_usize).step_by(rows_per_chunk) {
                    let rows = (blocks_h_usize - y0).min(rows_per_chunk);
                    let byte_len = src_row_pitch
                        .checked_mul(rows)
                        .ok_or_else(|| anyhow!("texture upload: BC chunk overflows usize"))?;
                    let tmp_slice = &mut tmp[..byte_len];
                    let row_offset = u64::try_from(y0)
                        .ok()
                        .and_then(|v| v.checked_mul(u64::from(bytes_per_row)))
                        .ok_or_else(|| anyhow!("texture upload: BC row offset overflows u64"))?;
                    let src_addr = gpa
                        .checked_add(row_offset)
                        .ok_or_else(|| anyhow!("texture upload: BC address overflows u64"))?;
                    guest_mem
                        .read(src_addr, tmp_slice)
                        .map_err(anyhow_guest_mem)?;

                    let origin_y_texels =
                        (y0 as u32).checked_mul(4).ok_or_else(|| anyhow!("texture upload: BC origin.y overflow"))?;
                    let remaining_height = height
                        .checked_sub(origin_y_texels)
                        .ok_or_else(|| anyhow!("texture upload: BC origin exceeds mip height"))?;
                    let chunk_height_texels = if y0 + rows == blocks_h_usize {
                        remaining_height
                    } else {
                        (rows as u32)
                            .checked_mul(4)
                            .ok_or_else(|| anyhow!("texture upload: BC extent height overflow"))?
                    };

                    queue.write_texture(
                        wgpu::ImageCopyTexture {
                            texture,
                            mip_level,
                            origin: wgpu::Origin3d {
                                x: 0,
                                y: origin_y_texels,
                                z: array_layer,
                            },
                            aspect: wgpu::TextureAspect::All,
                        },
                        tmp_slice,
                        wgpu::ImageDataLayout {
                            offset: 0,
                            bytes_per_row: Some(bytes_per_row),
                            rows_per_image: Some(rows as u32),
                        },
                        wgpu::Extent3d {
                            width,
                            height: chunk_height_texels,
                            depth_or_array_layers: 1,
                        },
                    );
                }
            }

            Ok(())
        }

        for layer in 0..array_layers {
            let layer_offset = guest_layout
                .layer_stride
                .checked_mul(layer as u64)
                .ok_or_else(|| anyhow!("texture upload size overflow"))?;
            for level in 0..mip_levels {
                let level_width = mip_extent(desc.width, level);
                let level_height = mip_extent(desc.height, level);
                let level_row_pitch_u32 = *guest_layout
                    .mip_row_pitches
                    .get(level as usize)
                    .ok_or_else(|| anyhow!("texture upload: missing mip_row_pitches entry"))?;
                let level_rows_u32 = *guest_layout
                    .mip_rows
                    .get(level as usize)
                    .ok_or_else(|| anyhow!("texture upload: missing mip_rows entry"))?;
                let mip_offset = *guest_layout
                    .mip_offsets
                    .get(level as usize)
                    .ok_or_else(|| anyhow!("texture upload: missing mip_offsets entry"))?;

                let gpa = base_gpa
                    .checked_add(layer_offset)
                    .and_then(|v| v.checked_add(mip_offset))
                    .ok_or_else(|| anyhow!("texture upload GPA overflow"))?;

                if let AerogpuTextureFormatLayout::BlockCompressed { bc, block_bytes } =
                    format_layout
                {
                    let tight_bpr = format_layout
                        .bytes_per_row_tight(level_width)
                        .context("texture upload: compute BC tight bytes_per_row")?;
                    if level_row_pitch_u32 < tight_bpr {
                        bail!("texture upload bytes_per_row too small for BC data");
                    }

                    if bc_block_bytes(desc.format).is_some() {
                        // Upload BC blocks directly.
                        upload_bc_subresource(
                            queue,
                            &tex.texture,
                            level,
                            layer,
                            level_width,
                            level_height,
                            level_row_pitch_u32,
                            block_bytes,
                            gpa,
                            guest_mem,
                        )?;
                    } else {
                        // Read + repack into the tight BC layout expected by the decompressor.
                        let tight_bpr_usize: usize = tight_bpr
                            .try_into()
                            .map_err(|_| anyhow!("texture upload: BC bytes_per_row out of range"))?;
                        let rows_usize: usize = level_rows_u32
                            .try_into()
                            .map_err(|_| anyhow!("texture upload: BC rows out of range"))?;
                        let src_bpr_usize: usize = level_row_pitch_u32
                            .try_into()
                            .map_err(|_| anyhow!("texture upload: BC bytes_per_row out of range"))?;
                        let tight_len = tight_bpr_usize
                            .checked_mul(rows_usize)
                            .ok_or_else(|| anyhow!("texture upload: BC data size overflows usize"))?;

                        let bc_bytes = if level_row_pitch_u32 == tight_bpr {
                            let mut tmp = vec![0u8; tight_len];
                            guest_mem.read(gpa, &mut tmp).map_err(anyhow_guest_mem)?;
                            tmp
                        } else {
                            let mut tight = vec![0u8; tight_len];
                            let mut row_buf = vec![0u8; src_bpr_usize];
                            for row in 0..rows_usize {
                                let row_offset = (row as u64)
                                    .checked_mul(u64::from(level_row_pitch_u32))
                                    .ok_or_else(|| anyhow!("texture upload: BC row offset overflow"))?;
                                let addr = gpa
                                    .checked_add(row_offset)
                                    .ok_or_else(|| anyhow!("texture upload: BC row address overflow"))?;
                                guest_mem
                                    .read(addr, &mut row_buf)
                                    .map_err(anyhow_guest_mem)?;
                                let dst_start = row
                                    .checked_mul(tight_bpr_usize)
                                    .ok_or_else(|| anyhow!("texture upload: BC dst offset overflow"))?;
                                tight[dst_start..dst_start + tight_bpr_usize]
                                    .copy_from_slice(&row_buf[..tight_bpr_usize]);
                            }
                            tight
                        };

                        // Guard against panic in the decompressor (it asserts on length).
                        let expected_len = level_width
                            .div_ceil(4) as usize
                            * level_height.div_ceil(4) as usize
                            * (block_bytes as usize);
                        if bc_bytes.len() != expected_len {
                            bail!(
                                "texture upload: BC data length mismatch: expected {expected_len} bytes, got {}",
                                bc_bytes.len()
                            );
                        }

                        let rgba = match bc {
                            AerogpuBcFormat::Bc1 => {
                                aero_gpu::decompress_bc1_rgba8(level_width, level_height, &bc_bytes)
                            }
                            AerogpuBcFormat::Bc2 => {
                                aero_gpu::decompress_bc2_rgba8(level_width, level_height, &bc_bytes)
                            }
                            AerogpuBcFormat::Bc3 => {
                                aero_gpu::decompress_bc3_rgba8(level_width, level_height, &bc_bytes)
                            }
                            AerogpuBcFormat::Bc7 => {
                                aero_gpu::decompress_bc7_rgba8(level_width, level_height, &bc_bytes)
                            }
                        };

                        write_texture_subresource_linear(
                            queue,
                            &tex.texture,
                            Texture2dDesc {
                                width: level_width,
                                height: level_height,
                                mip_level_count: 1,
                                array_layers: 1,
                                format: desc.format,
                            },
                            level,
                            layer,
                            level_width
                                .checked_mul(4)
                                .ok_or_else(|| anyhow!("texture upload: decompressed bytes_per_row overflow"))?,
                            &rgba,
                            false,
                        )?;
                    }
                } else {
                    upload_subresource(
                        queue,
                        &tex.texture,
                        desc.format,
                        level,
                        layer,
                        level_width,
                        level_height,
                        level_row_pitch_u32,
                        force_opaque_alpha,
                        gpa,
                        guest_mem,
                    )?;
                }
            }
        }

        tex.dirty = false;
        Ok(())
    }

    fn ensure_texture_uploaded(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        texture_handle: u32,
        allocs: &AllocTable,
        guest_mem: &mut dyn GuestMemory,
    ) -> Result<()> {
        let backing = match self.resources.textures.get(&texture_handle) {
            Some(tex) if tex.dirty => tex.backing,
            _ => return Ok(()),
        };

        let Some(_backing) = backing else {
            if let Some(tex) = self.resources.textures.get_mut(&texture_handle) {
                tex.dirty = false;
            }
            return Ok(());
        };

        // Preserve command stream ordering relative to any previously encoded GPU work.
        self.submit_encoder_if_has_commands(
            encoder,
            "aerogpu_cmd encoder after implicit texture upload",
        );

        self.upload_texture_from_guest_memory(texture_handle, allocs, guest_mem)
    }
}

fn build_render_pass_attachments<'a>(
    resources: &'a AerogpuD3d11Resources,
    state: &'a AerogpuD3d11State,
    color_load: wgpu::LoadOp<wgpu::Color>,
) -> Result<(
    Vec<Option<wgpu::RenderPassColorAttachment<'a>>>,
    Option<wgpu::RenderPassDepthStencilAttachment<'a>>,
)> {
    let mut color_attachments = Vec::with_capacity(state.render_targets.len());
    for &tex_id in &state.render_targets {
        let tex = resources
            .textures
            .get(&tex_id)
            .ok_or_else(|| anyhow!("unknown render target texture {tex_id}"))?;
        color_attachments.push(Some(wgpu::RenderPassColorAttachment {
            view: &tex.view,
            resolve_target: None,
            ops: wgpu::Operations {
                load: color_load,
                store: wgpu::StoreOp::Store,
            },
        }));
    }

    let depth_stencil_attachment = match state.depth_stencil {
        None => None,
        Some(ds_id) => {
            let tex = resources
                .textures
                .get(&ds_id)
                .ok_or_else(|| anyhow!("unknown depth-stencil texture {ds_id}"))?;
            let format = tex.desc.format;
            if !texture_format_has_depth(format) && !texture_format_has_stencil(format) {
                bail!("render pass depth-stencil texture {ds_id} has non-depth format {format:?}");
            }
            Some(wgpu::RenderPassDepthStencilAttachment {
                view: &tex.view,
                depth_ops: texture_format_has_depth(format).then_some(wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: texture_format_has_stencil(format).then_some(wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                }),
            })
        }
    };

    Ok((color_attachments, depth_stencil_attachment))
}

#[derive(Debug, Clone)]
struct BuiltVertexState {
    vertex_buffers: Vec<VertexBufferLayoutOwned>,
    vertex_buffer_keys: Vec<aero_gpu::pipeline_key::VertexBufferLayoutKey>,
    /// WebGPU vertex buffer slot → D3D11 input slot.
    wgpu_slot_to_d3d_slot: Vec<u32>,
}

fn wgsl_depth_clamp_variant(wgsl: &str) -> String {
    // WebGPU requires `PrimitiveState::unclipped_depth` to implement depth clamp, but that feature
    // is optional. As a backend-agnostic fallback we rewrite the VS to clamp the clip-space z
    // component into D3D's legal range (0..w) after the position is assigned.
    //
    // This intentionally changes the WGSL source (and therefore its ShaderHash), so the shader and
    // pipeline cache can distinguish depth-clamped variants.
    let mut out = String::with_capacity(wgsl.len() + 96);
    for line in wgsl.lines() {
        out.push_str(line);
        out.push('\n');

        let trimmed = line.trim_start();
        if trimmed.starts_with("out.pos =") {
            let indent_len = line.len() - trimmed.len();
            out.push_str(&line[..indent_len]);
            out.push_str("out.pos.z = clamp(out.pos.z, 0.0, out.pos.w);\n");
        }
    }
    out
}

fn exec_draw<'a>(pass: &mut wgpu::RenderPass<'a>, cmd_bytes: &[u8]) -> Result<()> {
    // struct aerogpu_cmd_draw (24 bytes)
    if cmd_bytes.len() < 24 {
        bail!("DRAW: expected at least 24 bytes, got {}", cmd_bytes.len());
    }
    let vertex_count = read_u32_le(cmd_bytes, 8)?;
    let instance_count = read_u32_le(cmd_bytes, 12)?;
    let first_vertex = read_u32_le(cmd_bytes, 16)?;
    let first_instance = read_u32_le(cmd_bytes, 20)?;
    pass.draw(
        first_vertex..first_vertex.saturating_add(vertex_count),
        first_instance..first_instance.saturating_add(instance_count),
    );
    Ok(())
}

fn exec_draw_indexed<'a>(pass: &mut wgpu::RenderPass<'a>, cmd_bytes: &[u8]) -> Result<()> {
    // struct aerogpu_cmd_draw_indexed (28 bytes)
    if cmd_bytes.len() < 28 {
        bail!(
            "DRAW_INDEXED: expected at least 28 bytes, got {}",
            cmd_bytes.len()
        );
    }
    let index_count = read_u32_le(cmd_bytes, 8)?;
    let instance_count = read_u32_le(cmd_bytes, 12)?;
    let first_index = read_u32_le(cmd_bytes, 16)?;
    let base_vertex = read_i32_le(cmd_bytes, 20)?;
    let first_instance = read_u32_le(cmd_bytes, 24)?;
    pass.draw_indexed(
        first_index..first_index.saturating_add(index_count),
        base_vertex,
        first_instance..first_instance.saturating_add(instance_count),
    );
    Ok(())
}

struct CmdExecutorBindGroupProvider<'a> {
    resources: &'a AerogpuD3d11Resources,
    legacy_constants: &'a HashMap<ShaderStage, wgpu::Buffer>,
    cbuffer_scratch: &'a HashMap<(ShaderStage, u32), ConstantBufferScratch>,
    dummy_uniform: &'a wgpu::Buffer,
    dummy_texture_view: &'a wgpu::TextureView,
    default_sampler: &'a aero_gpu::bindings::samplers::CachedSampler,
    stage: ShaderStage,
    stage_state: &'a super::bindings::StageBindings,
}

impl reflection_bindings::BindGroupResourceProvider for CmdExecutorBindGroupProvider<'_> {
    fn constant_buffer(&self, slot: u32) -> Option<reflection_bindings::BufferBinding<'_>> {
        let bound = self.stage_state.constant_buffer(slot)?;

        if bound.buffer == legacy_constants_buffer_id(self.stage) {
            let buf = self
                .legacy_constants
                .get(&self.stage)
                .expect("legacy constants buffer exists for every stage");
            return Some(reflection_bindings::BufferBinding {
                id: BufferId(bound.buffer as u64),
                buffer: buf,
                offset: bound.offset,
                size: bound.size,
                total_size: LEGACY_CONSTANTS_SIZE_BYTES,
            });
        }

        let buf = self.resources.buffers.get(&bound.buffer)?;
        Some(reflection_bindings::BufferBinding {
            id: BufferId(bound.buffer as u64),
            buffer: &buf.buffer,
            offset: bound.offset,
            size: bound.size,
            total_size: buf.size,
        })
    }

    fn constant_buffer_scratch(&self, slot: u32) -> Option<(BufferId, &wgpu::Buffer)> {
        self.cbuffer_scratch
            .get(&(self.stage, slot))
            .map(|scratch| (scratch.id, &scratch.buffer))
    }

    fn texture2d(&self, slot: u32) -> Option<(TextureViewId, &wgpu::TextureView)> {
        let bound = self.stage_state.texture(slot)?;
        let tex = self.resources.textures.get(&bound.texture)?;
        Some((TextureViewId(bound.texture as u64), &tex.view))
    }

    fn sampler(&self, slot: u32) -> Option<&aero_gpu::bindings::samplers::CachedSampler> {
        let bound = self.stage_state.sampler(slot)?;
        self.resources.samplers.get(&bound.sampler)
    }

    fn dummy_uniform(&self) -> &wgpu::Buffer {
        self.dummy_uniform
    }

    fn dummy_texture_view(&self) -> &wgpu::TextureView {
        self.dummy_texture_view
    }

    fn default_sampler(&self) -> &aero_gpu::bindings::samplers::CachedSampler {
        self.default_sampler
    }
}

fn get_or_create_render_pipeline_for_state<'a>(
    device: &wgpu::Device,
    pipeline_cache: &'a mut PipelineCache,
    pipeline_layout: &wgpu::PipelineLayout,
    resources: &mut AerogpuD3d11Resources,
    state: &AerogpuD3d11State,
    layout_key: PipelineLayoutKey,
) -> Result<(RenderPipelineKey, &'a wgpu::RenderPipeline, Vec<u32>)> {
    let vs_handle = state
        .vs
        .ok_or_else(|| anyhow!("render draw without bound VS"))?;
    let ps_handle = state
        .ps
        .ok_or_else(|| anyhow!("render draw without bound PS"))?;
    let (
        vs_wgsl_hash,
        vs_depth_clamp_wgsl_hash,
        vs_dxbc_hash_fnv1a64,
        vs_entry_point,
        vs_input_signature,
    ) = {
        let vs = resources
            .shaders
            .get(&vs_handle)
            .ok_or_else(|| anyhow!("unknown VS shader {vs_handle}"))?;
        if vs.stage != ShaderStage::Vertex {
            bail!("shader {vs_handle} is not a vertex shader");
        }
        (
            vs.wgsl_hash,
            vs.depth_clamp_wgsl_hash,
            vs.dxbc_hash_fnv1a64,
            vs.entry_point,
            vs.vs_input_signature.clone(),
        )
    };
    let (ps_wgsl_hash, fs_entry_point) = {
        let ps = resources
            .shaders
            .get(&ps_handle)
            .ok_or_else(|| anyhow!("unknown PS shader {ps_handle}"))?;
        if ps.stage != ShaderStage::Pixel {
            bail!("shader {ps_handle} is not a pixel shader");
        }
        (ps.wgsl_hash, ps.entry_point)
    };

    let BuiltVertexState {
        vertex_buffers,
        vertex_buffer_keys,
        wgpu_slot_to_d3d_slot,
    } = build_vertex_buffers_for_pipeline(
        resources,
        state,
        vs_dxbc_hash_fnv1a64,
        &vs_input_signature,
    )?;

    let vertex_shader = if state.depth_clip_enabled {
        vs_wgsl_hash
    } else {
        vs_depth_clamp_wgsl_hash.unwrap_or(vs_wgsl_hash)
    };

    let mut color_targets = Vec::with_capacity(state.render_targets.len());
    let mut color_target_states = Vec::with_capacity(state.render_targets.len());
    for &rt in &state.render_targets {
        let tex = resources
            .textures
            .get(&rt)
            .ok_or_else(|| anyhow!("unknown render target texture {rt}"))?;
        let ct = wgpu::ColorTargetState {
            format: tex.desc.format,
            blend: state.blend,
            write_mask: state.color_write_mask,
        };
        color_targets.push(ColorTargetKey {
            format: ct.format,
            blend: ct.blend.map(Into::into),
            write_mask: ct.write_mask,
        });
        color_target_states.push(Some(ct));
    }

    let depth_stencil_state = if let Some(ds_id) = state.depth_stencil {
        let tex = resources
            .textures
            .get(&ds_id)
            .ok_or_else(|| anyhow!("unknown depth-stencil texture {ds_id}"))?;

        let depth_compare = if state.depth_enable {
            state.depth_compare
        } else {
            wgpu::CompareFunction::Always
        };
        let depth_write_enabled = state.depth_enable && state.depth_write_enable;

        let (read_mask, write_mask) = if state.stencil_enable {
            (
                state.stencil_read_mask as u32,
                state.stencil_write_mask as u32,
            )
        } else {
            (0, 0)
        };

        Some(wgpu::DepthStencilState {
            format: tex.desc.format,
            depth_write_enabled,
            depth_compare,
            stencil: wgpu::StencilState {
                front: wgpu::StencilFaceState::IGNORE,
                back: wgpu::StencilFaceState::IGNORE,
                read_mask,
                write_mask,
            },
            bias: wgpu::DepthBiasState {
                constant: state.depth_bias,
                slope_scale: 0.0,
                clamp: 0.0,
            },
        })
    } else {
        None
    };
    let depth_stencil_key = depth_stencil_state.as_ref().map(|ds| ds.clone().into());

    let key = RenderPipelineKey {
        vertex_shader,
        fragment_shader: ps_wgsl_hash,
        color_targets,
        depth_stencil: depth_stencil_key,
        primitive_topology: state.primitive_topology,
        cull_mode: state.cull_mode,
        front_face: state.front_face,
        vertex_buffers: vertex_buffer_keys,
        sample_count: 1,
        layout: layout_key,
    };

    let topology = state.primitive_topology;
    let cull_mode = state.cull_mode;
    let front_face = state.front_face;
    let depth_stencil_state_for_pipeline = depth_stencil_state.clone();

    let pipeline = pipeline_cache
        .get_or_create_render_pipeline(device, key.clone(), move |device, vs, fs| {
            let vb_layouts: Vec<wgpu::VertexBufferLayout<'_>> = vertex_buffers
                .iter()
                .map(VertexBufferLayoutOwned::as_wgpu)
                .collect();

            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("aerogpu_cmd render pipeline"),
                layout: Some(pipeline_layout),
                vertex: wgpu::VertexState {
                    module: vs,
                    entry_point: vs_entry_point,
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &vb_layouts,
                },
                fragment: Some(wgpu::FragmentState {
                    module: fs,
                    entry_point: fs_entry_point,
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &color_target_states,
                }),
                primitive: wgpu::PrimitiveState {
                    topology,
                    strip_index_format: None,
                    front_face,
                    cull_mode,
                    polygon_mode: wgpu::PolygonMode::Fill,
                    unclipped_depth: false,
                    conservative: false,
                },
                depth_stencil: depth_stencil_state_for_pipeline,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
            })
        })
        .map_err(|e| anyhow!("wgpu pipeline cache: {e:?}"))?;

    Ok((key, pipeline, wgpu_slot_to_d3d_slot))
}

fn build_vertex_buffers_for_pipeline(
    resources: &mut AerogpuD3d11Resources,
    state: &AerogpuD3d11State,
    vs_dxbc_hash_fnv1a64: u64,
    vs_signature: &[VsInputSignatureElement],
) -> Result<BuiltVertexState> {
    let Some(layout_handle) = state.input_layout else {
        bail!("draw without input layout");
    };
    let layout = resources
        .input_layouts
        .get_mut(&layout_handle)
        .ok_or_else(|| anyhow!("unknown input layout {layout_handle}"))?;

    let mut slot_strides = vec![0u32; MAX_INPUT_SLOTS as usize];
    for (slot, vb) in state
        .vertex_buffers
        .iter()
        .enumerate()
        .take(slot_strides.len())
    {
        if let Some(vb) = vb {
            slot_strides[slot] = vb.stride_bytes;
        }
    }

    let cache_key =
        hash_input_layout_mapping_key(vs_dxbc_hash_fnv1a64, &layout.used_slots, &slot_strides);
    if let Some(cached) = layout.mapping_cache.get(&cache_key) {
        return Ok(cached.clone());
    }

    let fallback_signature;
    let sig = if vs_signature.is_empty() {
        fallback_signature = build_fallback_vs_signature(&layout.layout);
        fallback_signature.as_slice()
    } else {
        vs_signature
    };

    let mapped = {
        let binding = InputLayoutBinding::new(&layout.layout, &slot_strides);
        map_layout_to_shader_locations_compact(&binding, sig)
            .map_err(|e| anyhow!("input layout mapping failed: {e}"))?
    };

    let mut keys: Vec<aero_gpu::pipeline_key::VertexBufferLayoutKey> =
        Vec::with_capacity(mapped.buffers.len());
    for vb in &mapped.buffers {
        let w = vb.as_wgpu();
        keys.push((&w).into());
    }

    let mut wgpu_slot_to_d3d_slot = vec![0u32; mapped.buffers.len()];
    for (d3d_slot, wgpu_slot) in &mapped.d3d_slot_to_wgpu_slot {
        wgpu_slot_to_d3d_slot[*wgpu_slot as usize] = *d3d_slot;
    }

    let built = BuiltVertexState {
        vertex_buffers: mapped.buffers,
        vertex_buffer_keys: keys,
        wgpu_slot_to_d3d_slot,
    };
    layout.mapping_cache.insert(cache_key, built.clone());
    Ok(built)
}

struct AllocTable {
    entries: HashMap<u32, AerogpuAllocEntry>,
    present: bool,
}

impl AllocTable {
    fn new(entries: Option<&[AerogpuAllocEntry]>) -> Result<Self> {
        let present = entries.is_some();
        let entries = entries.unwrap_or(&[]);
        let mut map = HashMap::new();
        for &e in entries {
            if e.alloc_id == 0 {
                bail!("alloc table entry has alloc_id=0");
            }
            if e.size_bytes == 0 {
                bail!("alloc table entry {} has size_bytes=0", e.alloc_id);
            }
            if e.gpa.checked_add(e.size_bytes).is_none() {
                bail!(
                    "alloc table entry {} overflows u64: gpa=0x{:x} size=0x{:x}",
                    e.alloc_id,
                    e.gpa,
                    e.size_bytes
                );
            }
            if map.insert(e.alloc_id, e).is_some() {
                bail!("duplicate alloc_id {} in alloc table", e.alloc_id);
            }
        }
        Ok(Self {
            entries: map,
            present,
        })
    }

    fn require_entry(&self, alloc_id: u32) -> Result<&AerogpuAllocEntry> {
        if !self.present {
            bail!(
                "submission is missing an allocation table required to resolve alloc_id={alloc_id}"
            );
        }
        self.entries
            .get(&alloc_id)
            .ok_or_else(|| anyhow!("allocation table does not contain alloc_id={alloc_id}"))
    }

    fn gpa(&self, alloc_id: u32) -> Result<u64> {
        Ok(self.require_entry(alloc_id)?.gpa)
    }

    fn validate_range(&self, alloc_id: u32, offset: u64, size: u64) -> Result<()> {
        if alloc_id == 0 {
            return Ok(());
        }
        let entry = self.require_entry(alloc_id)?;
        let end = offset
            .checked_add(size)
            .ok_or_else(|| anyhow!("alloc range overflow"))?;
        if end > entry.size_bytes {
            bail!(
                "alloc {} out of range: offset=0x{:x} size=0x{:x} alloc_size=0x{:x}",
                alloc_id,
                offset,
                size,
                entry.size_bytes
            );
        }
        Ok(())
    }

    fn validate_write_range(&self, alloc_id: u32, offset: u64, size: u64) -> Result<u64> {
        if alloc_id == 0 {
            bail!("alloc_id must be non-zero");
        }
        let entry = self.require_entry(alloc_id)?;
        if (entry.flags & AEROGPU_ALLOC_FLAG_READONLY) != 0 {
            bail!("alloc {alloc_id} is read-only");
        }
        self.validate_range(alloc_id, offset, size)?;
        entry
            .gpa
            .checked_add(offset)
            .ok_or_else(|| anyhow!("alloc {alloc_id} GPA overflows u64"))
    }
}

fn legacy_constants_buffer_id(stage: ShaderStage) -> u32 {
    0xFFFF_FF00
        | match stage {
            ShaderStage::Vertex => 0,
            ShaderStage::Pixel => 1,
            ShaderStage::Compute => 2,
        }
}

fn group_index_to_stage(group: u32) -> Result<ShaderStage> {
    match group {
        0 => Ok(ShaderStage::Vertex),
        1 => Ok(ShaderStage::Pixel),
        2 => Ok(ShaderStage::Compute),
        other => bail!("unsupported bind group index {other}"),
    }
}

fn map_pipeline_cache_stage(stage: ShaderStage) -> aero_gpu::pipeline_key::ShaderStage {
    match stage {
        ShaderStage::Vertex => aero_gpu::pipeline_key::ShaderStage::Vertex,
        ShaderStage::Pixel => aero_gpu::pipeline_key::ShaderStage::Fragment,
        ShaderStage::Compute => aero_gpu::pipeline_key::ShaderStage::Compute,
    }
}

fn extract_vs_input_signature(
    signatures: &crate::ShaderSignatures,
) -> Result<Vec<VsInputSignatureElement>> {
    let Some(isgn) = signatures.isgn.as_ref() else {
        return Ok(Vec::new());
    };
    // D3D semantics are case-insensitive, but the signature chunk stores the original string. The
    // aerogpu ILAY protocol only preserves a hash, so we canonicalize to ASCII uppercase to match
    // how the guest typically hashes semantic names.
    Ok(isgn
        .parameters
        .iter()
        .map(|p| VsInputSignatureElement {
            semantic_name_hash: fnv1a_32(p.semantic_name.to_ascii_uppercase().as_bytes()),
            semantic_index: p.semantic_index,
            input_register: p.register,
        })
        .collect())
}

fn build_fallback_vs_signature(layout: &InputLayoutDesc) -> Vec<VsInputSignatureElement> {
    let mut seen: HashMap<(u32, u32), u32> = HashMap::new();
    let mut out: Vec<VsInputSignatureElement> = Vec::new();

    for elem in &layout.elements {
        let key = (elem.semantic_name_hash, elem.semantic_index);
        if seen.contains_key(&key) {
            continue;
        }
        let reg = out.len() as u32;
        seen.insert(key, reg);
        out.push(VsInputSignatureElement {
            semantic_name_hash: key.0,
            semantic_index: key.1,
            input_register: reg,
        });
    }

    out
}

const FNV1A64_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
const FNV1A64_PRIME: u64 = 0x100000001b3;

fn hash_input_layout_mapping_key(
    vs_dxbc_hash_fnv1a64: u64,
    used_slots: &[u32],
    slot_strides: &[u32],
) -> u64 {
    let mut hash = FNV1A64_OFFSET_BASIS;
    fnv1a64_update(&mut hash, &vs_dxbc_hash_fnv1a64.to_le_bytes());
    fnv1a64_update(&mut hash, &(used_slots.len() as u32).to_le_bytes());
    for &slot in used_slots {
        fnv1a64_update(&mut hash, &slot.to_le_bytes());
        let stride = slot_strides.get(slot as usize).copied().unwrap_or(0);
        fnv1a64_update(&mut hash, &stride.to_le_bytes());
    }
    hash
}

fn fnv1a64_update(hash: &mut u64, bytes: &[u8]) {
    for &b in bytes {
        *hash ^= b as u64;
        *hash = hash.wrapping_mul(FNV1A64_PRIME);
    }
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = FNV1A64_OFFSET_BASIS;
    fnv1a64_update(&mut hash, bytes);
    hash
}

fn map_buffer_usage_flags(flags: u32) -> wgpu::BufferUsages {
    let mut usage = wgpu::BufferUsages::COPY_DST;
    if flags & AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER != 0 {
        usage |= wgpu::BufferUsages::VERTEX;
    }
    if flags & AEROGPU_RESOURCE_USAGE_INDEX_BUFFER != 0 {
        usage |= wgpu::BufferUsages::INDEX;
    }
    if flags & AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER != 0 {
        usage |= wgpu::BufferUsages::UNIFORM;
    }
    // Allow readback for tests / future host interop.
    usage |= wgpu::BufferUsages::COPY_SRC;
    usage
}

fn map_texture_usage_flags(flags: u32) -> wgpu::TextureUsages {
    let mut usage = wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC;
    if flags & AEROGPU_RESOURCE_USAGE_TEXTURE != 0 {
        usage |= wgpu::TextureUsages::TEXTURE_BINDING;
    }
    if flags & (AEROGPU_RESOURCE_USAGE_RENDER_TARGET | AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL) != 0 {
        usage |= wgpu::TextureUsages::RENDER_ATTACHMENT;
    }
    if flags & AEROGPU_RESOURCE_USAGE_SCANOUT != 0 {
        usage |= wgpu::TextureUsages::RENDER_ATTACHMENT;
    }
    usage
}

// `enum aerogpu_format` from `aerogpu_pci.h`.
//
// NOTE: ABI 1.2 adds sRGB + BC formats. The exact numeric values are part of the protocol and must
// remain stable once published.
const AEROGPU_FORMAT_B8G8R8A8_UNORM: u32 = AerogpuFormat::B8G8R8A8Unorm as u32;
const AEROGPU_FORMAT_B8G8R8X8_UNORM: u32 = AerogpuFormat::B8G8R8X8Unorm as u32;
const AEROGPU_FORMAT_R8G8B8A8_UNORM: u32 = AerogpuFormat::R8G8B8A8Unorm as u32;
const AEROGPU_FORMAT_R8G8B8X8_UNORM: u32 = AerogpuFormat::R8G8B8X8Unorm as u32;
const AEROGPU_FORMAT_D24_UNORM_S8_UINT: u32 = AerogpuFormat::D24UnormS8Uint as u32;
const AEROGPU_FORMAT_D32_FLOAT: u32 = AerogpuFormat::D32Float as u32;

// ABI 1.2 extensions (backwards-compatible).
const AEROGPU_FORMAT_B8G8R8A8_UNORM_SRGB: u32 = AerogpuFormat::B8G8R8A8UnormSrgb as u32;
const AEROGPU_FORMAT_B8G8R8X8_UNORM_SRGB: u32 = AerogpuFormat::B8G8R8X8UnormSrgb as u32;
const AEROGPU_FORMAT_R8G8B8A8_UNORM_SRGB: u32 = AerogpuFormat::R8G8B8A8UnormSrgb as u32;
const AEROGPU_FORMAT_R8G8B8X8_UNORM_SRGB: u32 = AerogpuFormat::R8G8B8X8UnormSrgb as u32;

const AEROGPU_FORMAT_BC1_RGBA_UNORM: u32 = AerogpuFormat::BC1RgbaUnorm as u32;
const AEROGPU_FORMAT_BC1_RGBA_UNORM_SRGB: u32 = AerogpuFormat::BC1RgbaUnormSrgb as u32;
const AEROGPU_FORMAT_BC2_RGBA_UNORM: u32 = AerogpuFormat::BC2RgbaUnorm as u32;
const AEROGPU_FORMAT_BC2_RGBA_UNORM_SRGB: u32 = AerogpuFormat::BC2RgbaUnormSrgb as u32;
const AEROGPU_FORMAT_BC3_RGBA_UNORM: u32 = AerogpuFormat::BC3RgbaUnorm as u32;
const AEROGPU_FORMAT_BC3_RGBA_UNORM_SRGB: u32 = AerogpuFormat::BC3RgbaUnormSrgb as u32;
const AEROGPU_FORMAT_BC7_RGBA_UNORM: u32 = AerogpuFormat::BC7RgbaUnorm as u32;
const AEROGPU_FORMAT_BC7_RGBA_UNORM_SRGB: u32 = AerogpuFormat::BC7RgbaUnormSrgb as u32;

fn map_aerogpu_texture_format(format_u32: u32, bc_enabled: bool) -> Result<wgpu::TextureFormat> {
    // BC formats are mapped to native BC textures when the device has
    // `TEXTURE_COMPRESSION_BC` enabled; otherwise we fall back to RGBA8 and CPU-decompress BC blocks
    // on upload.
    Ok(match format_u32 {
        AEROGPU_FORMAT_B8G8R8A8_UNORM | AEROGPU_FORMAT_B8G8R8X8_UNORM => {
            wgpu::TextureFormat::Bgra8Unorm
        }
        AEROGPU_FORMAT_R8G8B8A8_UNORM | AEROGPU_FORMAT_R8G8B8X8_UNORM => {
            wgpu::TextureFormat::Rgba8Unorm
        }
        AEROGPU_FORMAT_B8G8R8A8_UNORM_SRGB | AEROGPU_FORMAT_B8G8R8X8_UNORM_SRGB => {
            wgpu::TextureFormat::Bgra8UnormSrgb
        }
        AEROGPU_FORMAT_R8G8B8A8_UNORM_SRGB | AEROGPU_FORMAT_R8G8B8X8_UNORM_SRGB => {
            wgpu::TextureFormat::Rgba8UnormSrgb
        }

        // BC formats (fallback to RGBA8 when BC compression features are not enabled).
        AEROGPU_FORMAT_BC1_RGBA_UNORM => {
            if bc_enabled {
                wgpu::TextureFormat::Bc1RgbaUnorm
            } else {
                wgpu::TextureFormat::Rgba8Unorm
            }
        }
        AEROGPU_FORMAT_BC1_RGBA_UNORM_SRGB => {
            if bc_enabled {
                wgpu::TextureFormat::Bc1RgbaUnormSrgb
            } else {
                wgpu::TextureFormat::Rgba8UnormSrgb
            }
        }
        AEROGPU_FORMAT_BC2_RGBA_UNORM => {
            if bc_enabled {
                wgpu::TextureFormat::Bc2RgbaUnorm
            } else {
                wgpu::TextureFormat::Rgba8Unorm
            }
        }
        AEROGPU_FORMAT_BC2_RGBA_UNORM_SRGB => {
            if bc_enabled {
                wgpu::TextureFormat::Bc2RgbaUnormSrgb
            } else {
                wgpu::TextureFormat::Rgba8UnormSrgb
            }
        }
        AEROGPU_FORMAT_BC3_RGBA_UNORM => {
            if bc_enabled {
                wgpu::TextureFormat::Bc3RgbaUnorm
            } else {
                wgpu::TextureFormat::Rgba8Unorm
            }
        }
        AEROGPU_FORMAT_BC3_RGBA_UNORM_SRGB => {
            if bc_enabled {
                wgpu::TextureFormat::Bc3RgbaUnormSrgb
            } else {
                wgpu::TextureFormat::Rgba8UnormSrgb
            }
        }
        AEROGPU_FORMAT_BC7_RGBA_UNORM => {
            if bc_enabled {
                wgpu::TextureFormat::Bc7RgbaUnorm
            } else {
                wgpu::TextureFormat::Rgba8Unorm
            }
        }
        AEROGPU_FORMAT_BC7_RGBA_UNORM_SRGB => {
            if bc_enabled {
                wgpu::TextureFormat::Bc7RgbaUnormSrgb
            } else {
                wgpu::TextureFormat::Rgba8UnormSrgb
            }
        }

        AEROGPU_FORMAT_D24_UNORM_S8_UINT => wgpu::TextureFormat::Depth24PlusStencil8,
        AEROGPU_FORMAT_D32_FLOAT => wgpu::TextureFormat::Depth32Float,
        other => bail!("unsupported aerogpu texture format {other}"),
    })
}

fn aerogpu_format_is_x8(format_u32: u32) -> bool {
    matches!(
        format_u32,
        AEROGPU_FORMAT_B8G8R8X8_UNORM
            | AEROGPU_FORMAT_R8G8B8X8_UNORM
            | AEROGPU_FORMAT_B8G8R8X8_UNORM_SRGB
            | AEROGPU_FORMAT_R8G8B8X8_UNORM_SRGB
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AerogpuBcFormat {
    Bc1,
    Bc2,
    Bc3,
    Bc7,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AerogpuTextureFormatLayout {
    Uncompressed { bytes_per_texel: u32 },
    BlockCompressed {
        bc: AerogpuBcFormat,
        block_bytes: u32,
    },
}

impl AerogpuTextureFormatLayout {
    fn is_block_compressed(self) -> bool {
        matches!(self, Self::BlockCompressed { .. })
    }

    fn bytes_per_row_tight(self, width: u32) -> Result<u32> {
        if width == 0 {
            bail!("texture width must be non-zero");
        }
        Ok(match self {
            Self::Uncompressed { bytes_per_texel } => width
                .checked_mul(bytes_per_texel)
                .ok_or_else(|| anyhow!("texture bytes_per_row overflow"))?,
            Self::BlockCompressed { block_bytes, .. } => width
                .div_ceil(4)
                .checked_mul(block_bytes)
                .ok_or_else(|| anyhow!("texture bytes_per_row overflow"))?,
        })
    }

    fn rows(self, height: u32) -> u32 {
        match self {
            Self::Uncompressed { .. } => height,
            Self::BlockCompressed { .. } => height.div_ceil(4),
        }
    }
}

fn aerogpu_texture_format_layout(format_u32: u32) -> Result<AerogpuTextureFormatLayout> {
    Ok(match format_u32 {
        AEROGPU_FORMAT_B8G8R8A8_UNORM
        | AEROGPU_FORMAT_B8G8R8X8_UNORM
        | AEROGPU_FORMAT_R8G8B8A8_UNORM
        | AEROGPU_FORMAT_R8G8B8X8_UNORM
        | AEROGPU_FORMAT_B8G8R8A8_UNORM_SRGB
        | AEROGPU_FORMAT_B8G8R8X8_UNORM_SRGB
        | AEROGPU_FORMAT_R8G8B8A8_UNORM_SRGB
        | AEROGPU_FORMAT_R8G8B8X8_UNORM_SRGB
        | AEROGPU_FORMAT_D24_UNORM_S8_UINT
        | AEROGPU_FORMAT_D32_FLOAT => AerogpuTextureFormatLayout::Uncompressed {
            bytes_per_texel: 4,
        },

        AEROGPU_FORMAT_BC1_RGBA_UNORM | AEROGPU_FORMAT_BC1_RGBA_UNORM_SRGB => {
            AerogpuTextureFormatLayout::BlockCompressed {
                bc: AerogpuBcFormat::Bc1,
                block_bytes: 8,
            }
        }
        AEROGPU_FORMAT_BC2_RGBA_UNORM | AEROGPU_FORMAT_BC2_RGBA_UNORM_SRGB => {
            AerogpuTextureFormatLayout::BlockCompressed {
                bc: AerogpuBcFormat::Bc2,
                block_bytes: 16,
            }
        }
        AEROGPU_FORMAT_BC3_RGBA_UNORM | AEROGPU_FORMAT_BC3_RGBA_UNORM_SRGB => {
            AerogpuTextureFormatLayout::BlockCompressed {
                bc: AerogpuBcFormat::Bc3,
                block_bytes: 16,
            }
        }
        AEROGPU_FORMAT_BC7_RGBA_UNORM | AEROGPU_FORMAT_BC7_RGBA_UNORM_SRGB => {
            AerogpuTextureFormatLayout::BlockCompressed {
                bc: AerogpuBcFormat::Bc7,
                block_bytes: 16,
            }
        }

        other => bail!("unsupported aerogpu texture format {other}"),
    })
}

#[derive(Debug)]
struct AerogpuGuestTextureLayout {
    mip_offsets: Vec<u64>,
    mip_row_pitches: Vec<u32>,
    mip_rows: Vec<u32>,
    layer_stride: u64,
    total_size: u64,
}

fn compute_guest_texture_layout(
    format_u32: u32,
    width: u32,
    height: u32,
    mip_level_count: u32,
    array_layers: u32,
    row_pitch_bytes_mip0: u32,
) -> Result<AerogpuGuestTextureLayout> {
    if mip_level_count == 0 || array_layers == 0 {
        bail!("mip_level_count/array_layers must be >= 1");
    }
    let layout = aerogpu_texture_format_layout(format_u32)?;
    let mip_extent = |v: u32, level: u32| v.checked_shr(level).unwrap_or(0).max(1);

    let mut mip_offsets = Vec::with_capacity(mip_level_count as usize);
    let mut mip_row_pitches = Vec::with_capacity(mip_level_count as usize);
    let mut mip_rows = Vec::with_capacity(mip_level_count as usize);

    let mut layer_stride = 0u64;
    for level in 0..mip_level_count {
        let level_width = mip_extent(width, level);
        let level_height = mip_extent(height, level);
        let tight_bpr = layout.bytes_per_row_tight(level_width)?;
        let level_row_pitch = if level == 0 && row_pitch_bytes_mip0 != 0 {
            row_pitch_bytes_mip0
        } else {
            tight_bpr
        };
        if level_row_pitch < tight_bpr {
            bail!("texture row_pitch_bytes too small for mip level {level}");
        }

        let rows = layout.rows(level_height);
        let level_size = u64::from(level_row_pitch)
            .checked_mul(rows as u64)
            .ok_or_else(|| anyhow!("texture size overflow"))?;

        mip_offsets.push(layer_stride);
        mip_row_pitches.push(level_row_pitch);
        mip_rows.push(rows);
        layer_stride = layer_stride
            .checked_add(level_size)
            .ok_or_else(|| anyhow!("texture size overflow"))?;
    }

    let total_size = layer_stride
        .checked_mul(array_layers as u64)
        .ok_or_else(|| anyhow!("texture size overflow"))?;

    Ok(AerogpuGuestTextureLayout {
        mip_offsets,
        mip_row_pitches,
        mip_rows,
        layer_stride,
        total_size,
    })
}

fn aerogpu_format_supports_writeback_dst(format_u32: u32) -> bool {
    // WRITEBACK_DST currently only supports byte-identical uncompressed 32bpp (RGBA/BGRA) layouts.
    matches!(
        format_u32,
        AEROGPU_FORMAT_B8G8R8A8_UNORM
            | AEROGPU_FORMAT_B8G8R8X8_UNORM
            | AEROGPU_FORMAT_R8G8B8A8_UNORM
            | AEROGPU_FORMAT_R8G8B8X8_UNORM
            | AEROGPU_FORMAT_B8G8R8A8_UNORM_SRGB
            | AEROGPU_FORMAT_B8G8R8X8_UNORM_SRGB
            | AEROGPU_FORMAT_R8G8B8A8_UNORM_SRGB
            | AEROGPU_FORMAT_R8G8B8X8_UNORM_SRGB
    )
}

fn force_opaque_alpha_rgba8(pixels: &mut [u8]) {
    for alpha in pixels.iter_mut().skip(3).step_by(4) {
        *alpha = 0xFF;
    }
}

fn bytes_per_texel(format: wgpu::TextureFormat) -> Result<u32> {
    Ok(match format {
        wgpu::TextureFormat::Rgba8Unorm
        | wgpu::TextureFormat::Rgba8UnormSrgb
        | wgpu::TextureFormat::Bgra8Unorm
        | wgpu::TextureFormat::Bgra8UnormSrgb
        | wgpu::TextureFormat::Depth24PlusStencil8
        | wgpu::TextureFormat::Depth32Float => 4,
        other => bail!("unsupported bytes_per_texel format {other:?}"),
    })
}

fn bc_block_bytes(format: wgpu::TextureFormat) -> Option<u32> {
    match format {
        wgpu::TextureFormat::Bc1RgbaUnorm | wgpu::TextureFormat::Bc1RgbaUnormSrgb => Some(8),
        wgpu::TextureFormat::Bc2RgbaUnorm
        | wgpu::TextureFormat::Bc2RgbaUnormSrgb
        | wgpu::TextureFormat::Bc3RgbaUnorm
        | wgpu::TextureFormat::Bc3RgbaUnormSrgb
        | wgpu::TextureFormat::Bc7RgbaUnorm
        | wgpu::TextureFormat::Bc7RgbaUnormSrgb => Some(16),
        _ => None,
    }
}

/// Compute the tight `bytes_per_row` and row count for `wgpu::Queue::write_texture`.
///
/// For block-compressed formats this returns layout in *blocks* (4x4 for BC), matching WebGPU's
/// copy rules: `rows_per_image` counts block rows.
fn write_texture_layout(format: wgpu::TextureFormat, width: u32, height: u32) -> Result<(u32, u32)> {
    if let Some(block_bytes) = bc_block_bytes(format) {
        let blocks_w = width.div_ceil(4);
        let blocks_h = height.div_ceil(4);
        let bytes_per_row = blocks_w
            .checked_mul(block_bytes)
            .ok_or_else(|| anyhow!("write_texture: bytes_per_row overflow"))?;
        return Ok((bytes_per_row, blocks_h));
    }

    let bpt = bytes_per_texel(format)?;
    let bytes_per_row = width
        .checked_mul(bpt)
        .ok_or_else(|| anyhow!("write_texture: bytes_per_row overflow"))?;
    Ok((bytes_per_row, height))
}

fn write_texture_subresource_linear(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    desc: Texture2dDesc,
    mip_level: u32,
    array_layer: u32,
    src_bytes_per_row: u32,
    bytes: &[u8],
    force_opaque_alpha: bool,
) -> Result<()> {
    let (unpadded_bpr, layout_rows) =
        write_texture_layout(desc.format, desc.width, desc.height)?;
    if src_bytes_per_row < unpadded_bpr {
        bail!("write_texture: src_bytes_per_row too small");
    }
    let required = (src_bytes_per_row as usize).saturating_mul(layout_rows as usize);
    if bytes.len() < required {
        bail!(
            "write_texture: source too small: need {} bytes, got {}",
            required,
            bytes.len()
        );
    }

    // wgpu requires bytes_per_row alignment for multi-row writes. Repack when needed.
    let aligned = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    if layout_rows > 1 {
        // WebGPU requires `bytes_per_row` to be aligned. Also, when the source row pitch contains
        // extra padding, repack into the minimal aligned stride so we don't upload unnecessary
        // bytes.
        let padded_bpr = unpadded_bpr
            .checked_add(aligned - 1)
            .map(|v| v / aligned)
            .and_then(|v| v.checked_mul(aligned))
            .ok_or_else(|| anyhow!("write_texture: padded bytes_per_row overflow"))?;

        if src_bytes_per_row != padded_bpr || force_opaque_alpha {
            let repacked_len = (padded_bpr as usize)
                .checked_mul(layout_rows as usize)
                .ok_or_else(|| anyhow!("write_texture: repacked size overflow"))?;
            let mut repacked = vec![0u8; repacked_len];
            for row in 0..layout_rows as usize {
                let src_start = row
                    .checked_mul(src_bytes_per_row as usize)
                    .ok_or_else(|| anyhow!("write_texture: src row offset overflow"))?;
                let dst_start = row
                    .checked_mul(padded_bpr as usize)
                    .ok_or_else(|| anyhow!("write_texture: dst row offset overflow"))?;
                let row_bytes = &mut repacked[dst_start..dst_start + unpadded_bpr as usize];
                row_bytes
                    .copy_from_slice(&bytes[src_start..src_start + unpadded_bpr as usize]);
                if force_opaque_alpha {
                    force_opaque_alpha_rgba8(row_bytes);
                }
            }
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
                &repacked,
                wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bpr),
                    rows_per_image: Some(layout_rows),
                },
                wgpu::Extent3d {
                    width: desc.width,
                    height: desc.height,
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
                    bytes_per_row: Some(src_bytes_per_row),
                    rows_per_image: Some(layout_rows),
                },
                wgpu::Extent3d {
                    width: desc.width,
                    height: desc.height,
                    depth_or_array_layers: 1,
                },
            );
        }
    } else {
        // Single-row textures don't require row pitch alignment.
        if force_opaque_alpha {
            let src_stride_usize: usize = src_bytes_per_row
                .try_into()
                .map_err(|_| anyhow!("write_texture: src_bytes_per_row out of range"))?;
            let mut repacked = vec![0u8; src_stride_usize];
            repacked[..unpadded_bpr as usize]
                .copy_from_slice(&bytes[..unpadded_bpr as usize]);
            force_opaque_alpha_rgba8(&mut repacked[..unpadded_bpr as usize]);
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
                &repacked,
                wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(src_bytes_per_row),
                    rows_per_image: Some(layout_rows),
                },
                wgpu::Extent3d {
                    width: desc.width,
                    height: desc.height,
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
                    bytes_per_row: Some(src_bytes_per_row),
                    rows_per_image: Some(layout_rows),
                },
                wgpu::Extent3d {
                    width: desc.width,
                    height: desc.height,
                    depth_or_array_layers: 1,
                },
            );
        }
    }

    Ok(())
}

fn write_texture_linear(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    desc: Texture2dDesc,
    src_bytes_per_row: u32,
    bytes: &[u8],
    force_opaque_alpha: bool,
) -> Result<()> {
    write_texture_subresource_linear(
        queue,
        texture,
        desc,
        0,
        0,
        src_bytes_per_row,
        bytes,
        force_opaque_alpha,
    )
}

fn try_translate_sm4_signature_driven(
    dxbc: &DxbcFile<'_>,
    program: &Sm4Program,
    signatures: &crate::ShaderSignatures,
) -> Result<ShaderTranslation> {
    let module = program.decode().context("decode SM4/5 token stream")?;
    translate_sm4_module_to_wgsl(dxbc, &module, signatures)
        .context("signature-driven SM4/5 translation")
}

fn align4(len: usize) -> usize {
    (len + 3) & !3
}

fn align_copy_buffer_size(size: u64) -> Result<u64> {
    let mask = wgpu::COPY_BUFFER_ALIGNMENT - 1;
    size.checked_add(mask)
        .map(|v| v & !mask)
        .ok_or_else(|| anyhow!("buffer size overflows u64"))
}

fn read_u32_le(buf: &[u8], offset: usize) -> Result<u32> {
    let bytes = buf
        .get(offset..offset + 4)
        .ok_or_else(|| anyhow!("truncated u32"))?;
    Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
}

fn read_i32_le(buf: &[u8], offset: usize) -> Result<i32> {
    Ok(read_u32_le(buf, offset)? as i32)
}

fn read_u64_le(buf: &[u8], offset: usize) -> Result<u64> {
    let bytes = buf
        .get(offset..offset + 8)
        .ok_or_else(|| anyhow!("truncated u64"))?;
    Ok(u64::from_le_bytes(bytes.try_into().unwrap()))
}

fn read_packed_unaligned<T: Copy>(bytes: &[u8]) -> Result<T> {
    let size = std::mem::size_of::<T>();
    if bytes.len() < size {
        bail!(
            "truncated packet: expected {size} bytes, got {}",
            bytes.len()
        );
    }

    // SAFETY: Bounds checked above and `read_unaligned` avoids alignment requirements.
    Ok(unsafe { std::ptr::read_unaligned(bytes.as_ptr() as *const T) })
}

fn texture_format_has_depth(format: wgpu::TextureFormat) -> bool {
    matches!(
        format,
        wgpu::TextureFormat::Depth16Unorm
            | wgpu::TextureFormat::Depth24Plus
            | wgpu::TextureFormat::Depth24PlusStencil8
            | wgpu::TextureFormat::Depth32Float
            | wgpu::TextureFormat::Depth32FloatStencil8
    )
}

fn texture_format_has_stencil(format: wgpu::TextureFormat) -> bool {
    matches!(
        format,
        wgpu::TextureFormat::Depth24PlusStencil8 | wgpu::TextureFormat::Depth32FloatStencil8
    )
}
fn map_color_write_mask(mask: u8) -> wgpu::ColorWrites {
    let mut out = wgpu::ColorWrites::empty();
    if mask & 0x1 != 0 {
        out |= wgpu::ColorWrites::RED;
    }
    if mask & 0x2 != 0 {
        out |= wgpu::ColorWrites::GREEN;
    }
    if mask & 0x4 != 0 {
        out |= wgpu::ColorWrites::BLUE;
    }
    if mask & 0x8 != 0 {
        out |= wgpu::ColorWrites::ALPHA;
    }
    out
}

fn map_blend_factor(v: u32) -> Option<wgpu::BlendFactor> {
    Some(match v {
        0 => wgpu::BlendFactor::Zero,
        1 => wgpu::BlendFactor::One,
        2 => wgpu::BlendFactor::SrcAlpha,
        3 => wgpu::BlendFactor::OneMinusSrcAlpha,
        4 => wgpu::BlendFactor::DstAlpha,
        5 => wgpu::BlendFactor::OneMinusDstAlpha,
        6 => wgpu::BlendFactor::Constant,
        7 => wgpu::BlendFactor::OneMinusConstant,
        _ => return None,
    })
}

fn map_compare_func(v: u32) -> Option<wgpu::CompareFunction> {
    Some(match v {
        0 => wgpu::CompareFunction::Never,
        1 => wgpu::CompareFunction::Less,
        2 => wgpu::CompareFunction::Equal,
        3 => wgpu::CompareFunction::LessEqual,
        4 => wgpu::CompareFunction::Greater,
        5 => wgpu::CompareFunction::NotEqual,
        6 => wgpu::CompareFunction::GreaterEqual,
        7 => wgpu::CompareFunction::Always,
        _ => return None,
    })
}

fn map_blend_op(v: u32) -> Option<wgpu::BlendOperation> {
    Some(match v {
        0 => wgpu::BlendOperation::Add,
        1 => wgpu::BlendOperation::Subtract,
        2 => wgpu::BlendOperation::ReverseSubtract,
        3 => wgpu::BlendOperation::Min,
        4 => wgpu::BlendOperation::Max,
        _ => return None,
    })
}

fn anyhow_guest_mem(err: GuestMemoryError) -> anyhow::Error {
    anyhow!("{err}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use aero_protocol::aerogpu::aerogpu_cmd::AerogpuCmdOpcode;

    fn require_webgpu() -> bool {
        let Ok(raw) = std::env::var("AERO_REQUIRE_WEBGPU") else {
            return false;
        };
        let v = raw.trim();
        v == "1"
            || v.eq_ignore_ascii_case("true")
            || v.eq_ignore_ascii_case("yes")
            || v.eq_ignore_ascii_case("on")
    }

    fn skip_or_panic(test_name: &str, reason: &str) {
        if require_webgpu() {
            panic!("AERO_REQUIRE_WEBGPU is enabled but {test_name} cannot run: {reason}");
        }
        eprintln!("skipping {test_name}: {reason}");
    }

    #[test]
    fn set_shader_constants_f_marks_encoder_has_commands() {
        pollster::block_on(async {
            let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
                Ok(exec) => exec,
                Err(e) => {
                    skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                    return;
                }
            };

            assert!(!exec.encoder_has_commands);

            let mut encoder = exec
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("aerogpu_cmd test encoder_has_commands"),
                });

            let vec4_count = 1u32;
            let size_bytes = (24 + 16) as u32;
            let mut cmd_bytes = Vec::with_capacity(size_bytes as usize);
            cmd_bytes
                .extend_from_slice(&(AerogpuCmdOpcode::SetShaderConstantsF as u32).to_le_bytes());
            cmd_bytes.extend_from_slice(&size_bytes.to_le_bytes());
            cmd_bytes.extend_from_slice(&0u32.to_le_bytes()); // stage = vertex
            cmd_bytes.extend_from_slice(&0u32.to_le_bytes()); // start_register
            cmd_bytes.extend_from_slice(&vec4_count.to_le_bytes());
            cmd_bytes.extend_from_slice(&0u32.to_le_bytes()); // reserved0
            cmd_bytes.extend_from_slice(&[0xABu8; 16]); // vec4 data
            assert_eq!(cmd_bytes.len(), size_bytes as usize);

            exec.exec_set_shader_constants_f(&mut encoder, &cmd_bytes)
                .expect("SET_SHADER_CONSTANTS_F should succeed");

            assert!(exec.encoder_has_commands);
        });
    }

    #[test]
    fn aerogpu_format_is_x8_includes_srgb_variants() {
        assert!(aerogpu_format_is_x8(AEROGPU_FORMAT_B8G8R8X8_UNORM));
        assert!(aerogpu_format_is_x8(AEROGPU_FORMAT_R8G8B8X8_UNORM));
        assert!(aerogpu_format_is_x8(AEROGPU_FORMAT_B8G8R8X8_UNORM_SRGB));
        assert!(aerogpu_format_is_x8(AEROGPU_FORMAT_R8G8B8X8_UNORM_SRGB));

        assert!(!aerogpu_format_is_x8(AEROGPU_FORMAT_B8G8R8A8_UNORM_SRGB));
        assert!(!aerogpu_format_is_x8(AEROGPU_FORMAT_R8G8B8A8_UNORM_SRGB));
    }

    #[test]
    fn map_aerogpu_texture_format_bc_falls_back_when_disabled() {
        assert_eq!(
            map_aerogpu_texture_format(AEROGPU_FORMAT_BC1_RGBA_UNORM, false).unwrap(),
            wgpu::TextureFormat::Rgba8Unorm
        );
        assert_eq!(
            map_aerogpu_texture_format(AEROGPU_FORMAT_BC1_RGBA_UNORM_SRGB, false).unwrap(),
            wgpu::TextureFormat::Rgba8UnormSrgb
        );
    }

    #[test]
    fn map_aerogpu_texture_format_bc_uses_native_when_enabled() {
        assert_eq!(
            map_aerogpu_texture_format(AEROGPU_FORMAT_BC1_RGBA_UNORM, true).unwrap(),
            wgpu::TextureFormat::Bc1RgbaUnorm
        );
        assert_eq!(
            map_aerogpu_texture_format(AEROGPU_FORMAT_BC1_RGBA_UNORM_SRGB, true).unwrap(),
            wgpu::TextureFormat::Bc1RgbaUnormSrgb
        );
    }
}
