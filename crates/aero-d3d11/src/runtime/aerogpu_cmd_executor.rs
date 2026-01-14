use std::collections::{BTreeSet, HashMap, HashSet};
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
use aero_gpu::pipeline_key::{
    ColorTargetKey, ComputePipelineKey, PipelineLayoutKey, RenderPipelineKey, ShaderHash,
};
use aero_gpu::wgpu_bc_texture_dimensions_compatible;
use aero_gpu::GpuCapabilities;
use aero_gpu::{
    expand_b5g5r5a1_unorm_to_rgba8, expand_b5g6r5_unorm_to_rgba8, pack_rgba8_to_b5g5r5a1_unorm,
    pack_rgba8_to_b5g6r5_unorm,
};
use aero_protocol::aerogpu::aerogpu_cmd::{
    decode_cmd_bind_shaders_payload_le, decode_cmd_copy_buffer_le, decode_cmd_copy_texture2d_le,
    decode_cmd_create_input_layout_blob_le, decode_cmd_create_shader_dxbc_payload_le,
    decode_cmd_set_vertex_buffers_bindings_le, decode_cmd_upload_resource_payload_le,
    AerogpuCmdOpcode, AerogpuCmdStreamHeader, AerogpuCmdStreamIter, AerogpuShaderStage,
    AEROGPU_CLEAR_COLOR,
    AEROGPU_CLEAR_DEPTH, AEROGPU_CLEAR_STENCIL, AEROGPU_COPY_FLAG_WRITEBACK_DST,
    AEROGPU_RASTERIZER_FLAG_DEPTH_CLIP_DISABLE, AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER,
    AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL, AEROGPU_RESOURCE_USAGE_INDEX_BUFFER,
    AEROGPU_RESOURCE_USAGE_RENDER_TARGET, AEROGPU_RESOURCE_USAGE_SCANOUT,
    AEROGPU_RESOURCE_USAGE_STORAGE, AEROGPU_RESOURCE_USAGE_TEXTURE,
    AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::aerogpu_ring::{AerogpuAllocEntry, AEROGPU_ALLOC_FLAG_READONLY};
use anyhow::{anyhow, bail, Context, Result};

use crate::binding_model::{
    BINDING_BASE_SAMPLER, BINDING_BASE_TEXTURE, BINDING_BASE_UAV, D3D11_MAX_CONSTANT_BUFFER_SLOTS,
    MAX_SAMPLER_SLOTS, MAX_TEXTURE_SLOTS, MAX_UAV_SLOTS,
};
use crate::input_layout::{
    fnv1a_32, map_layout_to_shader_locations_compact, InputLayoutBinding, InputLayoutDesc,
    VertexBufferLayoutOwned, VsInputSignatureElement, MAX_INPUT_SLOTS,
};
use crate::{
    parse_signatures, translate_sm4_module_to_wgsl, DxbcFile, ShaderReflection, ShaderTranslation,
    Sm4Program,
};
use crate::sm4::opcode as sm4_opcode;

use super::bindings::{BindingState, BoundBuffer, BoundConstantBuffer, BoundSampler, ShaderStage};
use super::expansion_scratch::{ExpansionScratchAllocator, ExpansionScratchDescriptor};
use super::indirect_args::DrawIndexedIndirectArgs;
use super::index_pulling::{
    IndexPullingParams, INDEX_PULLING_BUFFER_BINDING, INDEX_PULLING_PARAMS_BINDING,
};
#[cfg(target_arch = "wasm32")]
use super::shader_cache::{
    PersistedBinding, PersistedShaderArtifact, PersistedShaderStage,
    PersistedVsInputSignatureElement, ShaderCache as PersistentShaderCache, ShaderCacheSource,
    ShaderCacheStats, ShaderTranslationFlags as PersistentShaderTranslationFlags,
};
use super::pipeline_layout_cache::PipelineLayoutCache;
use super::reflection_bindings;
use super::vertex_pulling::{
    VertexPullingDrawParams, VertexPullingLayout, VertexPullingSlot, VERTEX_PULLING_GROUP,
    VERTEX_PULLING_UNIFORM_BINDING, VERTEX_PULLING_VERTEX_BUFFER_BINDING_BASE,
};
use super::scratch_allocator::GpuScratchAllocator;
use super::tessellation::TessellationRuntime;

const DEFAULT_MAX_VERTEX_SLOTS: usize = MAX_INPUT_SLOTS as usize;
// D3D11 exposes 128 SRV slots per stage. Our shader translation keeps the D3D register index as the
// WGSL/WebGPU binding number (samplers live at an offset), so the executor must accept and track
// slots up to 127 even if only a smaller subset is used by a given shader.
const DEFAULT_MAX_TEXTURE_SLOTS: usize = MAX_TEXTURE_SLOTS as usize;
const DEFAULT_MAX_SAMPLER_SLOTS: usize = MAX_SAMPLER_SLOTS as usize;
// D3D11 exposes 8 UAV slots (`u0..u7`) to SM5 shaders.
const DEFAULT_MAX_UAV_SLOTS: usize = MAX_UAV_SLOTS as usize;
// D3D10/11 exposes 14 constant buffer slots (0..13) per shader stage.
const DEFAULT_MAX_CONSTANT_BUFFER_SLOTS: usize = D3D11_MAX_CONSTANT_BUFFER_SLOTS as usize;
const LEGACY_CONSTANTS_SIZE_BYTES: u64 = 4096 * 16;

// WebGPU pipelines must declare at least one color target when a fragment shader writes a color
// output. D3D11 commonly executes depth-only passes (no RTVs bound) while a PS is still present.
// Our current DXBC→WGSL translation always emits `@location(0)` output, so we bind an internal
// dummy color attachment for depth-only passes to satisfy validation.
const DEPTH_ONLY_DUMMY_COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

#[cfg(target_arch = "wasm32")]
fn compute_wgpu_caps_hash(device: &wgpu::Device, backend: wgpu::Backend) -> String {
    // This hash is included in the persistent shader cache key to avoid reusing translation output
    // across WebGPU capability changes. It does not need to match the JS-side
    // `computeWebGpuCapsHash`; it only needs to be stable for a given device/browser.
    const VERSION: &[u8] = b"aero-d3d11 wgpu caps hash v1";

    let mut hasher = blake3::Hasher::new();
    hasher.update(VERSION);
    hasher.update(format!("{backend:?}").as_bytes());
    hasher.update(&device.features().bits().to_le_bytes());
    // `wgpu::Limits` is a large struct without `Serialize`; use a debug representation for a
    // stable-ish byte stream. Any change here just forces retranslation, which is safe.
    hasher.update(format!("{:?}", device.limits()).as_bytes());
    hasher.finalize().to_hex().to_string()
}

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
const OPCODE_SET_SHADER_RESOURCE_BUFFERS: u32 = AerogpuCmdOpcode::SetShaderResourceBuffers as u32;
const OPCODE_SET_UNORDERED_ACCESS_BUFFERS: u32 = AerogpuCmdOpcode::SetUnorderedAccessBuffers as u32;

const OPCODE_CLEAR: u32 = AerogpuCmdOpcode::Clear as u32;
const OPCODE_DRAW: u32 = AerogpuCmdOpcode::Draw as u32;
const OPCODE_DRAW_INDEXED: u32 = AerogpuCmdOpcode::DrawIndexed as u32;
const OPCODE_DISPATCH: u32 = AerogpuCmdOpcode::Dispatch as u32;

const OPCODE_PRESENT: u32 = AerogpuCmdOpcode::Present as u32;
const OPCODE_PRESENT_EX: u32 = AerogpuCmdOpcode::PresentEx as u32;

const OPCODE_EXPORT_SHARED_SURFACE: u32 = AerogpuCmdOpcode::ExportSharedSurface as u32;
const OPCODE_IMPORT_SHARED_SURFACE: u32 = AerogpuCmdOpcode::ImportSharedSurface as u32;
const OPCODE_RELEASE_SHARED_SURFACE: u32 = AerogpuCmdOpcode::ReleaseSharedSurface as u32;

const OPCODE_FLUSH: u32 = AerogpuCmdOpcode::Flush as u32;

const DEFAULT_BIND_GROUP_CACHE_CAPACITY: usize = 4096;

// Placeholder geometry/tessellation emulation path:
// - Compute prepass expands one or more synthetic primitives into an "expanded vertex" buffer +
//   indirect args.
// - Render pass consumes those buffers via `draw_indirect`/`draw_indexed_indirect`.
//
// This prepass is scaffolding for GS/HS/DS compute-based emulation. In particular:
// - `@builtin(global_invocation_id).x` is treated as the GS `SV_PrimitiveID`
// - `@builtin(global_invocation_id).y` is treated as the GS `SV_GSInstanceID`
const GEOMETRY_PREPASS_EXPANDED_VERTEX_STRIDE_BYTES: u64 = 32;
// Use the indexed-indirect layout size since it is a strict superset of `DrawIndirectArgs`.
const GEOMETRY_PREPASS_INDIRECT_ARGS_SIZE_BYTES: u64 =
    core::mem::size_of::<DrawIndexedIndirectArgs>() as u64;
const GEOMETRY_PREPASS_COUNTER_SIZE_BYTES: u64 = 4; // 1x u32
// `vec4<f32>` color + `vec4<u32>` counts.
const GEOMETRY_PREPASS_PARAMS_SIZE_BYTES: u64 = 32;

const GEOMETRY_PREPASS_CS_WGSL: &str = r#"
struct ExpandedVertex {
    pos: vec4<f32>,
    o1: vec4<f32>,
};

struct Params {
    color: vec4<f32>,
    counts: vec4<u32>,
};

struct DepthParams {
    data: vec4<f32>,
};

@group(0) @binding(0) var<storage, read_write> out_vertices: array<ExpandedVertex>;
@group(0) @binding(1) var<storage, read_write> out_indices: array<u32>;
@group(0) @binding(2) var<storage, read_write> out_indirect: array<u32>;
@group(0) @binding(3) var<storage, read_write> out_counter: array<u32>;
@group(0) @binding(4) var<uniform> params: Params;
@group(0) @binding(5) var<uniform> depth_params: DepthParams;

@compute @workgroup_size(1)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    // Compute-based GS emulation system values.
    let primitive_id: u32 = id.x;
    let gs_instance_id: u32 = id.y;

    let primitive_count: u32 = params.counts.x;
    if (primitive_id >= primitive_count) {
        return;
    }

    let z = depth_params.data.x;

    // Default placeholder color:
    // - primitive 0: `params.color` (red in tests)
    // - primitive 1+: green, to make primitive_id-dependent output easy to validate
    var c = params.color;
    if (primitive_id != 0u) {
        c = vec4<f32>(0.0, 1.0, 0.0, 1.0);
    }

    if (primitive_count == 1u) {
        // Clockwise centered triangle (matches default `FrontFace::Cw` + back-face culling).
        // Keep it away from the corners so tests can assert untouched pixels.
        out_vertices[0].pos = vec4<f32>(-0.5, -0.5, z, 1.0);
        out_vertices[1].pos = vec4<f32>(0.0, 0.5, z, 1.0);
        out_vertices[2].pos = vec4<f32>(0.5, -0.5, z, 1.0);

        out_vertices[0].o1 = c;
        out_vertices[1].o1 = c;
        out_vertices[2].o1 = c;

        // Indices for indexed draws.
        out_indices[0] = 0u;
        out_indices[1] = 1u;
        out_indices[2] = 2u;
    } else {
        // Side-by-side triangles (used for primitive-id tests).
        let base: u32 = primitive_id * 3u;

        var x0: f32 = -2.0;
        var x1: f32 = -2.0;
        if (primitive_id == 0u) {
            x0 = -1.0;
            x1 = 0.0;
        } else if (primitive_id == 1u) {
            x0 = 0.0;
            x1 = 3.0;
        }

        out_vertices[base + 0u].pos = vec4<f32>(x0, -1.0, z, 1.0);
        out_vertices[base + 1u].pos = vec4<f32>(x0, 3.0, z, 1.0);
        out_vertices[base + 2u].pos = vec4<f32>(x1, -1.0, z, 1.0);

        out_vertices[base + 0u].o1 = c;
        out_vertices[base + 1u].o1 = c;
        out_vertices[base + 2u].o1 = c;

        out_indices[base + 0u] = base + 0u;
        out_indices[base + 1u] = base + 1u;
        out_indices[base + 2u] = base + 2u;
    }

    // Write 5 u32s so the same buffer works for both:
    // - draw_indirect:           vertex_count, instance_count, first_vertex, first_instance
    // - draw_indexed_indirect:   index_count, instance_count, first_index, base_vertex, first_instance
    //
    // Only the first GS instance writes indirect args (so future GS instancing does not race).
    if (primitive_id == 0u && gs_instance_id == 0u) {
        let count: u32 = primitive_count * 3u;
        // Placeholder counter (for eventual GS-style append/emit emulation).
        out_counter[0] = count;
        out_indirect[0] = count;
        out_indirect[1] = params.counts.y;
        out_indirect[2] = 0u;
        out_indirect[3] = 0u;
        out_indirect[4] = 0u;
    }
}
"#;

// Variant of `GEOMETRY_PREPASS_CS_WGSL` that additionally performs one IA vertex-buffer load via the
// vertex-pulling bind group (when present).
//
// This is a placeholder for the eventual VS-as-compute implementation: we keep the output triangle
// identical to the non-vertex-pulling prepass, but force at least one read from the IA buffers so
// the vertex pulling binding scheme is exercised end-to-end.
//
// NOTE: This WGSL is only valid when `runtime::vertex_pulling::VertexPullingLayout::wgsl_prelude()`
// is prepended to the shader source.
const GEOMETRY_PREPASS_CS_VERTEX_PULLING_WGSL: &str = r#"
struct ExpandedVertex {
    pos: vec4<f32>,
    o1: vec4<f32>,
};

struct Params {
    color: vec4<f32>,
};

@group(0) @binding(0) var<storage, read_write> out_vertices: array<ExpandedVertex>;
@group(0) @binding(1) var<storage, read_write> out_indices: array<u32>;
@group(0) @binding(2) var<storage, read_write> out_indirect: array<u32>;
@group(0) @binding(3) var<storage, read_write> out_counter: array<u32>;
@group(0) @binding(4) var<uniform> params: Params;

@compute @workgroup_size(1)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x != 0u) {
        return;
    }

    // Exercise the vertex pulling bind group by reading a single dword from the first
    // vertex buffer slot.
    //
    // This is intentionally minimal: the placeholder prepass still emits a fixed fullscreen-ish
    // triangle, but the load ensures that IA buffers can be bound as storage and accessed.
    let base: u32 = aero_vp_ia.slots[0].base_offset_bytes
        + aero_vp_ia.first_vertex * aero_vp_ia.slots[0].stride_bytes;
    let _word: u32 = aero_vp_load_u32(0u, base);

    let c = params.color;

    // Clockwise full-screen-ish triangle (matches default `FrontFace::Cw` + back-face culling).
    out_vertices[0].pos = vec4<f32>(-1.0, -1.0, 0.0, 1.0);
    out_vertices[1].pos = vec4<f32>(-1.0, 3.0, 0.0, 1.0);
    out_vertices[2].pos = vec4<f32>(3.0, -1.0, 0.0, 1.0);

    out_vertices[0].o1 = c;
    out_vertices[1].o1 = c;
    out_vertices[2].o1 = c;

    // Indices for indexed draws.
    out_indices[0] = 0u;
    out_indices[1] = 1u;
    out_indices[2] = 2u;

    // Placeholder counter (for eventual GS-style append/emit emulation).
    out_counter[0] = 3u;

    // Write 5 u32s so the same buffer works for both:
    // - draw_indirect:           vertex_count, instance_count, first_vertex, first_instance
    // - draw_indexed_indirect:   index_count, instance_count, first_index, base_vertex, first_instance
    out_indirect[0] = 3u;
    out_indirect[1] = 1u;
    out_indirect[2] = 0u;
    out_indirect[3] = 0u;
    out_indirect[4] = 0u;
}
"#;

const EXPANDED_DRAW_PASSTHROUGH_VS_WGSL: &str = r#"
struct VsIn {
    @location(0) v0: vec4<f32>,
    @location(1) v1: vec4<f32>,
};

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(1) o1: vec4<f32>,
};

@vertex
fn vs_main(input: VsIn) -> VsOut {
    var out: VsOut;
    out.pos = input.v0;
    out.o1 = input.v1;
    return out;
}
"#;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoundShaderHandles {
    pub vs: Option<u32>,
    pub ps: Option<u32>,
    pub gs: Option<u32>,
    pub hs: Option<u32>,
    pub ds: Option<u32>,
    pub cs: Option<u32>,
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

    /// Resolves a handle coming from an AeroGPU command stream.
    ///
    /// This differs from `resolve_handle()` by treating "reserved underlying IDs" as invalid:
    /// if an original handle has been destroyed while shared-surface aliases still exist, the
    /// underlying numeric ID is kept alive in `refcounts` to prevent handle reuse/collision, but
    /// the original handle value must not be used for subsequent commands.
    fn resolve_cmd_handle(&self, handle: u32, op: &str) -> Result<u32> {
        if handle == 0 {
            return Ok(0);
        }

        if self.handles.contains_key(&handle) {
            return Ok(self.resolve_handle(handle));
        }

        if self.refcounts.contains_key(&handle) {
            bail!(
                "{op}: resource handle {handle} was destroyed (underlying id kept alive by shared surface aliases)"
            );
        }

        Ok(handle)
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
        let underlying = self.handles.get(&resource_handle).copied().ok_or_else(|| {
            anyhow!("EXPORT_SHARED_SURFACE: unknown resource handle {resource_handle}")
        })?;

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
            bail!("IMPORT_SHARED_SURFACE: unknown share_token 0x{share_token:016X} (not exported)");
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
        } else if self.refcounts.contains_key(&out_handle) {
            // Underlying handles remain reserved as long as any aliases still reference them.
            // If an original handle was destroyed, it must not be reused as a new alias handle
            // until the underlying resource is fully released.
            bail!(
                "IMPORT_SHARED_SURFACE: out_resource_handle {out_handle} is still in use (underlying id kept alive by shared surface aliases)"
            );
        }

        self.handles.insert(out_handle, underlying);
        *self.refcounts.entry(underlying).or_insert(0) += 1;
        Ok(())
    }

    fn release_token(&mut self, share_token: u64) {
        if share_token == 0 {
            return;
        }
        // Idempotent: unknown tokens are a no-op (see `aerogpu_cmd.h` contract).
        //
        // Only retire tokens that were actually exported at some point (present in `by_token`),
        // or that are already retired.
        if self.by_token.remove(&share_token).is_some() {
            self.retired_tokens.insert(share_token);
        }
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
enum TextureWritebackTransform {
    /// Write staging bytes directly into guest memory.
    Direct { force_opaque_alpha: bool },
    /// Pack staging RGBA8 bytes into `B5G6R5Unorm` before writing to guest memory.
    B5G6R5,
    /// Pack staging RGBA8 bytes into `B5G5R5A1Unorm` before writing to guest memory.
    B5G5R5A1,
}

#[derive(Debug, Clone, Copy)]
struct TextureWritebackPlan {
    base_gpa: u64,
    row_pitch: u64,
    padded_bytes_per_row: u32,
    staging_unpadded_bytes_per_row: u32,
    guest_unpadded_bytes_per_row: u32,
    height: u32,
    transform: TextureWritebackTransform,
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
    // Test-only: expose the computed wgpu usage flags for assertions.
    #[cfg(test)]
    usage: wgpu::BufferUsages,
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
    /// Tracks whether the guest backing memory (if any) still matches the GPU texture contents.
    ///
    /// Guest-backed textures are uploaded from guest memory. GPU writes (draw/clear/copy) do not
    /// automatically write results back into guest memory, so after such writes the guest backing
    /// becomes stale. This is used by BC COPY_TEXTURE2D CPU fallbacks (notably on the wgpu GL
    /// backend) to avoid sourcing stale compressed blocks from guest memory.
    guest_backing_is_current: bool,
    /// CPU shadow for textures updated via `UPLOAD_RESOURCE`.
    ///
    /// The command stream expresses uploads as a linear byte range into the guest UMD's canonical
    /// packed `(array_layer, mip_level)` layout for the full mip+array chain (see
    /// `compute_guest_texture_layout`), but WebGPU uploads are 2D per-subresource. For partial
    /// updates we patch into this shadow buffer and then re-upload the affected subresource(s).
    ///
    /// The shadow is invalidated when the texture is written by GPU operations (draw/clear/copy).
    host_shadow: Option<Vec<u8>>,
    /// Per-subresource validity for `host_shadow`.
    ///
    /// Indexing matches `compute_guest_texture_layout`: `array_layer * mip_level_count + mip_level`.
    ///
    /// A subresource is considered valid if all bytes for that mip/layer are present in
    /// `host_shadow` and match the GPU contents. This is used to prevent partial `UPLOAD_RESOURCE`
    /// patches from overwriting bytes we don't have.
    host_shadow_valid: Vec<bool>,
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
    wgsl_source: String,
}

#[derive(Debug, Clone, Copy)]
struct GsShaderMetadata {
    /// Geometry-shader instance count (`dcl_gsinstancecount` / `[instance(n)]`).
    ///
    /// The WebGPU backend does not execute the GS token stream yet; we keep enough metadata to
    /// fail fast (instead of silently misrendering) when unsupported GS instancing is requested.
    instance_count: u32,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CmdPrimitiveTopology {
    PointList,
    LineList,
    LineStrip,
    TriangleList,
    TriangleStrip,
    /// D3D9 triangle fan. Not directly supported by WebGPU; the current executor treats it as a
    /// triangle list.
    TriangleFan,

    // D3D11 adjacency topologies (GS input).
    LineListAdj,
    LineStripAdj,
    TriangleListAdj,
    TriangleStripAdj,
    // D3D11 patchlists (HS/DS input).
    PatchList {
        control_points: u8,
    },
}

impl CmdPrimitiveTopology {
    fn from_u32(v: u32) -> Option<Self> {
        Some(match v {
            1 => Self::PointList,
            2 => Self::LineList,
            3 => Self::LineStrip,
            4 => Self::TriangleList,
            5 => Self::TriangleStrip,
            6 => Self::TriangleFan,
            10 => Self::LineListAdj,
            11 => Self::LineStripAdj,
            12 => Self::TriangleListAdj,
            13 => Self::TriangleStripAdj,
            33..=64 => Self::PatchList {
                control_points: (v - 32) as u8,
            },
            _ => return None,
        })
    }

    fn wgpu_topology_for_direct_draw(self) -> Option<wgpu::PrimitiveTopology> {
        Some(match self {
            Self::PointList => wgpu::PrimitiveTopology::PointList,
            Self::LineList => wgpu::PrimitiveTopology::LineList,
            Self::LineStrip => wgpu::PrimitiveTopology::LineStrip,
            Self::TriangleList => wgpu::PrimitiveTopology::TriangleList,
            Self::TriangleStrip => wgpu::PrimitiveTopology::TriangleStrip,
            // TriangleFan is not directly supported; fall back to TriangleList.
            Self::TriangleFan => wgpu::PrimitiveTopology::TriangleList,
            Self::LineListAdj
            | Self::LineStripAdj
            | Self::TriangleListAdj
            | Self::TriangleStripAdj
            | Self::PatchList { .. } => return None,
        })
    }

    fn validate_direct_draw(self) -> Result<wgpu::PrimitiveTopology> {
        if let Some(t) = self.wgpu_topology_for_direct_draw() {
            return Ok(t);
        }

        match self {
            Self::LineListAdj
            | Self::LineStripAdj
            | Self::TriangleListAdj
            | Self::TriangleStripAdj => {
                bail!("adjacency primitive topology requires geometry shader emulation")
            }
            Self::PatchList { .. } => bail!("patchlist topology requires tessellation emulation"),
            _ => unreachable!(),
        }
    }
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

#[derive(Debug)]
#[allow(dead_code)]
struct GsScratchBuffer {
    id: BufferId,
    buffer: Arc<wgpu::Buffer>,
    size: u64,
}

#[derive(Debug, Default)]
#[allow(dead_code)]
struct GsScratchPool {
    output: Option<GsScratchBuffer>,
    index: Option<GsScratchBuffer>,
    counter: Option<GsScratchBuffer>,
    indirect_args: Option<GsScratchBuffer>,
    output_allocations: u32,
    index_allocations: u32,
    counter_allocations: u32,
    indirect_allocations: u32,
}

#[allow(dead_code)]
impl GsScratchPool {
    fn clear(&mut self) {
        *self = Self::default();
    }

    fn ensure_output(
        &mut self,
        device: &wgpu::Device,
        next_id: &mut u64,
        required_size: u64,
    ) -> &GsScratchBuffer {
        Self::ensure_buffer(
            device,
            next_id,
            &mut self.output,
            &mut self.output_allocations,
            required_size,
            wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::VERTEX,
            "aerogpu_cmd gs scratch output vertex buffer",
        )
    }

    fn ensure_counter(
        &mut self,
        device: &wgpu::Device,
        next_id: &mut u64,
        required_size: u64,
    ) -> &GsScratchBuffer {
        Self::ensure_buffer(
            device,
            next_id,
            &mut self.counter,
            &mut self.counter_allocations,
            required_size,
            wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::STORAGE,
            "aerogpu_cmd gs scratch counter buffer",
        )
    }

    fn ensure_index(
        &mut self,
        device: &wgpu::Device,
        next_id: &mut u64,
        required_size: u64,
    ) -> &GsScratchBuffer {
        Self::ensure_buffer(
            device,
            next_id,
            &mut self.index,
            &mut self.index_allocations,
            required_size,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::INDEX,
            "aerogpu_cmd gs scratch index buffer",
        )
    }

    fn ensure_indirect_args(
        &mut self,
        device: &wgpu::Device,
        next_id: &mut u64,
        required_size: u64,
    ) -> &GsScratchBuffer {
        let required_size = required_size.max(20);
        Self::ensure_buffer(
            device,
            next_id,
            &mut self.indirect_args,
            &mut self.indirect_allocations,
            required_size,
            wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::INDIRECT,
            "aerogpu_cmd gs scratch indirect args buffer",
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn ensure_buffer<'a>(
        device: &wgpu::Device,
        next_id: &mut u64,
        slot: &'a mut Option<GsScratchBuffer>,
        allocation_counter: &mut u32,
        required_size: u64,
        usage: wgpu::BufferUsages,
        label: &'static str,
    ) -> &'a GsScratchBuffer {
        // WebGPU disallows zero-sized buffers; also ensure COPY_BUFFER_ALIGNMENT for any copy/reset.
        let required_size = required_size.max(wgpu::COPY_BUFFER_ALIGNMENT);
        let required_size = required_size.saturating_add(wgpu::COPY_BUFFER_ALIGNMENT - 1)
            & !(wgpu::COPY_BUFFER_ALIGNMENT - 1);

        let needs_new = slot
            .as_ref()
            .map(|existing| existing.size < required_size)
            .unwrap_or(true);

        if needs_new {
            let size = required_size
                .checked_next_power_of_two()
                .unwrap_or(required_size);
            let id = BufferId(*next_id);
            *next_id = next_id.wrapping_add(1);
            let buffer = Arc::new(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage,
                mapped_at_creation: false,
            }));
            *allocation_counter = allocation_counter.saturating_add(1);
            *slot = Some(GsScratchBuffer { id, buffer, size });
        }

        slot.as_ref().expect("buffer inserted above")
    }
}
#[derive(Debug, Default)]
struct AerogpuD3d11Resources {
    buffers: HashMap<u32, BufferResource>,
    textures: HashMap<u32, Texture2dResource>,
    samplers: HashMap<u32, aero_gpu::bindings::samplers::CachedSampler>,
    shaders: HashMap<u32, ShaderResource>,
    gs_shaders: HashMap<u32, GsShaderMetadata>,
    input_layouts: HashMap<u32, InputLayoutResource>,
}

#[derive(Debug)]
struct AerogpuD3d11State {
    /// Render target texture handles by D3D11 slot index.
    ///
    /// D3D11 allows "gaps" in the RTV array (e.g. `[NULL, RT1]`). WebGPU models this with
    /// `Option` entries in the `FragmentState.targets` / `RenderPassDescriptor.color_attachments`
    /// arrays.
    ///
    /// The length is `color_count` from `SET_RENDER_TARGETS` (up to 8).
    render_targets: Vec<Option<u32>>,
    depth_stencil: Option<u32>,
    viewport: Option<Viewport>,
    scissor: Option<Scissor>,

    vertex_buffers: Vec<Option<VertexBufferBinding>>,
    index_buffer: Option<IndexBufferBinding>,
    primitive_topology: CmdPrimitiveTopology,

    vs: Option<u32>,
    /// Geometry shader handle when GS emulation is enabled on the guest side.
    ///
    /// WebGPU has no geometry shader stage; Aero emulates GS by lowering it to a compute + indirect
    /// draw sequence. The stable AeroGPU command stream does not yet have a dedicated GS slot, so
    /// a non-zero value may be provided via reserved fields (see `exec_bind_shaders`).
    gs: Option<u32>,
    ps: Option<u32>,
    cs: Option<u32>,
    // NOTE: The stable AeroGPU command stream does not currently expose binding slots for these
    // stages, but future extensions/backends may emulate D3D11 tessellation stages using compute.
    hs: Option<u32>,
    ds: Option<u32>,
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
            primitive_topology: CmdPrimitiveTopology::TriangleList,
            vs: None,
            gs: None,
            ps: None,
            cs: None,
            hs: None,
            ds: None,
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
    caps: GpuCapabilities,
    device: wgpu::Device,
    queue: wgpu::Queue,
    backend: wgpu::Backend,

    resources: AerogpuD3d11Resources,
    state: AerogpuD3d11State,
    shared_surfaces: SharedSurfaceTable,

    bindings: BindingState,
    legacy_constants: HashMap<ShaderStage, wgpu::Buffer>,

    gpu_scratch: GpuScratchAllocator,
    cbuffer_scratch: HashMap<(ShaderStage, u32), ConstantBufferScratch>,
    gs_scratch: GsScratchPool,
    next_scratch_buffer_id: u64,
    expansion_scratch: ExpansionScratchAllocator,
    tessellation: TessellationRuntime,

    dummy_uniform: wgpu::Buffer,
    dummy_storage: wgpu::Buffer,
    dummy_texture_view: wgpu::TextureView,

    /// Cache of internal dummy color targets for depth-only render passes.
    ///
    /// Keyed by `(width, height, format)` so repeated depth-only draws (e.g. shadow passes) do not
    /// allocate a new host texture every time.
    depth_only_dummy_color_targets: HashMap<(u32, u32, wgpu::TextureFormat), wgpu::Texture>,

    sampler_cache: SamplerCache,
    default_sampler: aero_gpu::bindings::samplers::CachedSampler,

    bind_group_layout_cache: BindGroupLayoutCache,
    bind_group_cache: BindGroupCache<Arc<wgpu::BindGroup>>,
    pipeline_layout_cache: PipelineLayoutCache<Arc<wgpu::PipelineLayout>>,
    pipeline_cache: PipelineCache,

    /// WASM-only persistent shader translation cache (IndexedDB/OPFS).
    #[cfg(target_arch = "wasm32")]
    persistent_shader_cache: PersistentShaderCache,
    /// Translation flags used for persistent shader cache lookups (wasm32 only).
    ///
    /// Includes a stable per-device capabilities hash so cached artifacts are not reused when
    /// WebGPU limits/features differ.
    #[cfg(target_arch = "wasm32")]
    persistent_shader_cache_flags: PersistentShaderTranslationFlags,

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
    /// Construct an executor.
    ///
    /// This constructor does not have access to the originating `wgpu::Adapter`, so it
    /// conservatively assumes compute is unsupported. Callers that have an adapter should instead
    /// derive `supports_compute` from downlevel flags and use [`Self::new_with_supports_compute`]
    /// (or [`Self::new_with_supports`] if indirect execution support is also needed).
    pub fn new(device: wgpu::Device, queue: wgpu::Queue, backend: wgpu::Backend) -> Self {
        let mut caps = GpuCapabilities::from_device(&device);
        // Conservative default: without the adapter's downlevel caps, assume compute is not
        // available (e.g. wgpu's WebGL2 backend).
        caps.supports_compute = false;
        // We don't currently have the adapter's downlevel signal for indirect execution here.
        // Assume it is available except on wasm32, where `wgpu::Backend::Gl` corresponds to WebGL2
        // and indirect draws are unavailable.
        let supports_indirect = !(cfg!(target_arch = "wasm32") && backend == wgpu::Backend::Gl);
        Self::new_with_caps(device, queue, backend, caps, supports_indirect)
    }

    /// Construct an executor with an explicit compute capability override.
    ///
    /// `wgpu::Device` does not currently expose whether compute pipelines are supported, so callers
    /// that have a `wgpu::Adapter` should pass
    /// `adapter.get_downlevel_capabilities().flags.contains(wgpu::DownlevelFlags::COMPUTE_SHADERS)`
    /// here to ensure compute is deterministically enabled/disabled.
    ///
    /// This assumes indirect execution is available; callers that need to override indirect support
    /// should use [`Self::new_with_supports`] / [`Self::new_with_caps`].
    pub fn new_with_supports_compute(
        device: wgpu::Device,
        queue: wgpu::Queue,
        backend: wgpu::Backend,
        supports_compute: bool,
    ) -> Self {
        let mut caps = GpuCapabilities::from_device(&device);
        caps.supports_compute = supports_compute;
        Self::new_with_caps(device, queue, backend, caps, true)
    }

    /// Construct an executor with explicit downlevel capability overrides.
    ///
    /// This is used for downlevel/compatibility backends where compute and/or indirect execution
    /// may be unavailable.
    pub fn new_with_supports(
        device: wgpu::Device,
        queue: wgpu::Queue,
        backend: wgpu::Backend,
        supports_compute: bool,
        supports_indirect: bool,
    ) -> Self {
        let mut caps = GpuCapabilities::from_device(&device);
        caps.supports_compute = supports_compute;
        Self::new_with_caps(device, queue, backend, caps, supports_indirect)
    }

    /// Construct an executor with explicitly-provided GPU capabilities.
    pub fn new_with_caps(
        device: wgpu::Device,
        queue: wgpu::Queue,
        backend: wgpu::Backend,
        mut caps: GpuCapabilities,
        supports_indirect: bool,
    ) -> Self {
        caps.supports_indirect_execution = supports_indirect;
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
            usage: wgpu::BufferUsages::UNIFORM
                | wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: true,
        });
        {
            let mut mapped = dummy_uniform.slice(..).get_mapped_range_mut();
            mapped.fill(0);
        }
        dummy_uniform.unmap();

        let dummy_storage = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aerogpu_cmd dummy storage buffer"),
            size: 4096,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: true,
        });
        {
            let mut mapped = dummy_storage.slice(..).get_mapped_range_mut();
            mapped.fill(0);
        }
        dummy_storage.unmap();
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
            ShaderStage::Geometry,
            ShaderStage::Hull,
            ShaderStage::Domain,
            ShaderStage::Compute,
        ] {
            let buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("aerogpu_cmd legacy constants buffer"),
                size: LEGACY_CONSTANTS_SIZE_BYTES,
                usage: wgpu::BufferUsages::UNIFORM
                    | wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_DST,
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
            ShaderStage::Geometry,
            ShaderStage::Hull,
            ShaderStage::Domain,
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

        let gpu_scratch = GpuScratchAllocator::new(&device);
        #[cfg(target_arch = "wasm32")]
        let persistent_shader_cache_flags = PersistentShaderTranslationFlags::new(Some(
            compute_wgpu_caps_hash(&device, backend),
        ));
        #[cfg(target_arch = "wasm32")]
        let persistent_shader_cache = PersistentShaderCache::new();

        Self {
            caps,
            device,
            queue,
            backend,
            resources: AerogpuD3d11Resources::default(),
            state: AerogpuD3d11State::default(),
            shared_surfaces: SharedSurfaceTable::default(),
            bindings,
            legacy_constants,
            gpu_scratch,
            cbuffer_scratch: HashMap::new(),
            gs_scratch: GsScratchPool::default(),
            next_scratch_buffer_id: 1u64 << 32,
            expansion_scratch: ExpansionScratchAllocator::new(ExpansionScratchDescriptor::default()),
            tessellation: TessellationRuntime::default(),
            dummy_uniform,
            dummy_storage,
            dummy_texture_view,
            depth_only_dummy_color_targets: HashMap::new(),
            sampler_cache,
            default_sampler,
            bind_group_layout_cache: BindGroupLayoutCache::new(),
            bind_group_cache: BindGroupCache::new(DEFAULT_BIND_GROUP_CACHE_CAPACITY),
            pipeline_layout_cache: PipelineLayoutCache::new(),
            pipeline_cache,
            #[cfg(target_arch = "wasm32")]
            persistent_shader_cache,
            #[cfg(target_arch = "wasm32")]
            persistent_shader_cache_flags,
            encoder_used_buffers: HashSet::new(),
            encoder_used_textures: HashSet::new(),
            encoder_has_commands: false,
        }
    }

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

        let downlevel_flags = adapter.get_downlevel_capabilities().flags;
        let supports_indirect = downlevel_flags.contains(wgpu::DownlevelFlags::INDIRECT_EXECUTION);
        let backend = adapter.get_info().backend;
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

        let caps = GpuCapabilities::from_device(&device).with_downlevel_flags(downlevel_flags);
        Ok(Self::new_with_caps(
            device,
            queue,
            backend,
            caps,
            supports_indirect,
        ))
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    /// GPU/device capabilities detected at executor creation time.
    pub fn caps(&self) -> &GpuCapabilities {
        &self.caps
    }

    /// Whether the underlying adapter/device supports indirect execution.
    pub fn supports_indirect(&self) -> bool {
        self.caps.supports_indirect_execution
    }

    pub fn backend(&self) -> wgpu::Backend {
        self.backend
    }

    pub fn capabilities(&self) -> &GpuCapabilities {
        &self.caps
    }

    pub fn supports_compute(&self) -> bool {
        self.caps.supports_compute
    }

    #[doc(hidden)]
    pub fn bound_shader_handles(&self) -> BoundShaderHandles {
        BoundShaderHandles {
            vs: self.state.vs,
            ps: self.state.ps,
            gs: self.state.gs,
            hs: self.state.hs,
            ds: self.state.ds,
            cs: self.state.cs,
        }
    }

    #[doc(hidden)]
    pub fn shader_stage(&self, shader_id: u32) -> Option<ShaderStage> {
        self.resources.shaders.get(&shader_id).map(|s| s.stage)
    }

    #[doc(hidden)]
    pub fn binding_state(&self) -> &BindingState {
        &self.bindings
    }

    fn gs_hs_ds_emulation_required(&self) -> bool {
        if self.state.gs.is_some() || self.state.hs.is_some() || self.state.ds.is_some() {
            return true;
        }

        matches!(
            self.state.primitive_topology,
            CmdPrimitiveTopology::LineListAdj
                | CmdPrimitiveTopology::LineStripAdj
                | CmdPrimitiveTopology::TriangleListAdj
                | CmdPrimitiveTopology::TriangleStripAdj
                | CmdPrimitiveTopology::PatchList { .. }
        )
    }

    fn validate_gs_hs_ds_emulation_capabilities(&self) -> Result<()> {
        if !self.gs_hs_ds_emulation_required() {
            return Ok(());
        }

        let mut missing = Vec::new();
        if !self.caps.supports_compute {
            missing.push("wgpu::DownlevelFlags::COMPUTE_SHADERS");
        }
        if !self.caps.supports_indirect_execution {
            missing.push("wgpu::DownlevelFlags::INDIRECT_EXECUTION");
        }

        if !missing.is_empty() {
            bail!(
                "GS/HS/DS emulation requires compute shaders and indirect execution; backend {:?} missing {}",
                self.backend,
                missing.join(", ")
            );
        }

        Ok(())
    }

    #[allow(dead_code)]
    pub(crate) fn gpu_scratch_allocator_mut(&mut self) -> &mut GpuScratchAllocator {
        &mut self.gpu_scratch
    }

    pub fn reset(&mut self) {
        self.resources = AerogpuD3d11Resources::default();
        self.state = AerogpuD3d11State::default();
        self.shared_surfaces.clear();
        self.pipeline_cache.clear();
        self.cbuffer_scratch.clear();
        self.gpu_scratch.clear();
        self.gs_scratch.clear();
        self.expansion_scratch.reset();
        self.tessellation.reset();
        self.encoder_used_buffers.clear();
        self.encoder_used_textures.clear();
        self.next_scratch_buffer_id = 1u64 << 32;
        self.bindings = BindingState::default();
        for stage in [
            ShaderStage::Vertex,
            ShaderStage::Pixel,
            ShaderStage::Geometry,
            ShaderStage::Hull,
            ShaderStage::Domain,
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

    /// Snapshot of D3D11 DXBC->WGSL shader cache counters (wasm32 only).
    #[cfg(target_arch = "wasm32")]
    pub fn shader_cache_stats(&self) -> ShaderCacheStats {
        self.persistent_shader_cache.stats()
    }

    pub fn texture_size(&self, texture_id: u32) -> Result<(u32, u32)> {
        // Test/emulator helper: accept either an alias handle or the underlying ID. In particular,
        // when an original shared-surface handle is destroyed while aliases are still alive, the
        // underlying ID remains valid for internal host bookkeeping (e.g. PRESENT reports) even
        // though it is no longer a "live" guest handle.
        let texture_id = self.shared_surfaces.resolve_handle(texture_id);
        let texture = self
            .resources
            .textures
            .get(&texture_id)
            .ok_or_else(|| anyhow!("unknown texture {texture_id}"))?;
        Ok((texture.desc.width, texture.desc.height))
    }

    pub fn shader_entry_point(&self, shader_id: u32) -> Result<&'static str> {
        let shader = self
            .resources
            .shaders
            .get(&shader_id)
            .ok_or_else(|| anyhow!("unknown shader {shader_id}"))?;
        Ok(shader.entry_point)
    }

    pub async fn read_texture_rgba8(&self, texture_id: u32) -> Result<Vec<u8>> {
        self.read_texture_rgba8_subresource(texture_id, 0, 0).await
    }

    pub async fn read_texture_rgba8_subresource(
        &self,
        texture_id: u32,
        mip_level: u32,
        array_layer: u32,
    ) -> Result<Vec<u8>> {
        // Test/emulator helper: see `texture_size`.
        let texture_id = self.shared_surfaces.resolve_handle(texture_id);
        let texture = self
            .resources
            .textures
            .get(&texture_id)
            .ok_or_else(|| anyhow!("unknown texture {texture_id}"))?;

        if mip_level >= texture.desc.mip_level_count {
            bail!(
                "read_texture_rgba8_subresource: mip_level out of bounds (mip_level={mip_level}, mip_levels={})",
                texture.desc.mip_level_count
            );
        }
        if array_layer >= texture.desc.array_layers {
            bail!(
                "read_texture_rgba8_subresource: array_layer out of bounds (array_layer={array_layer}, array_layers={})",
                texture.desc.array_layers
            );
        }

        let mip_extent = |v: u32, level: u32| v.checked_shr(level).unwrap_or(0).max(1);
        let width = mip_extent(texture.desc.width, mip_level);
        let height = mip_extent(texture.desc.height, mip_level);

        // Some backends (notably GL) can behave strangely when attempting to copy compressed
        // texture formats directly to a buffer. For tests, we instead render BC textures into an
        // RGBA8 render target and then read that back.
        if bc_block_bytes(texture.desc.format).is_some() {
            let resolved_format = match texture.desc.format {
                wgpu::TextureFormat::Bc1RgbaUnormSrgb
                | wgpu::TextureFormat::Bc2RgbaUnormSrgb
                | wgpu::TextureFormat::Bc3RgbaUnormSrgb
                | wgpu::TextureFormat::Bc7RgbaUnormSrgb => wgpu::TextureFormat::Rgba8UnormSrgb,
                _ => wgpu::TextureFormat::Rgba8Unorm,
            };

            let resolved = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("aero-d3d11 aerogpu_cmd read_texture resolved"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: resolved_format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let resolved_view = resolved.create_view(&wgpu::TextureViewDescriptor::default());
            let src_view = texture.texture.create_view(&wgpu::TextureViewDescriptor {
                label: Some("aero-d3d11 aerogpu_cmd read_texture src_view"),
                format: None,
                dimension: Some(wgpu::TextureViewDimension::D2),
                aspect: wgpu::TextureAspect::All,
                base_mip_level: mip_level,
                mip_level_count: Some(1),
                base_array_layer: array_layer,
                array_layer_count: Some(1),
            });

            const SHADER: &str = r#"
                @group(0) @binding(0) var src: texture_2d<f32>;
                @group(0) @binding(1) var samp: sampler;

                struct VsOut {
                    @builtin(position) pos: vec4<f32>,
                    @location(0) uv: vec2<f32>,
                };

                @vertex
                fn vs(@builtin(vertex_index) vid: u32) -> VsOut {
                    // Full-screen triangle with UVs that cover the full [0,1] range.
                    var positions = array<vec2<f32>, 3>(
                        vec2<f32>(-1.0, -3.0),
                        vec2<f32>( 3.0,  1.0),
                        vec2<f32>(-1.0,  1.0),
                    );
                    var uvs = array<vec2<f32>, 3>(
                        vec2<f32>(0.0, 2.0),
                        vec2<f32>(2.0, 0.0),
                        vec2<f32>(0.0, 0.0),
                    );
                    var out: VsOut;
                    out.pos = vec4<f32>(positions[vid], 0.0, 1.0);
                    out.uv = uvs[vid];
                    return out;
                }

                @fragment
                fn fs(in: VsOut) -> @location(0) vec4<f32> {
                    // Use a nearest sampler to sample exact texels without having to reason about
                    // `@builtin(position)` origin conventions across backends.
                    return textureSampleLevel(src, samp, in.uv, 0.0);
                }
            "#;

            let shader = self
                .device
                .create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("aero-d3d11 aerogpu_cmd read_texture bc shader"),
                    source: wgpu::ShaderSource::Wgsl(SHADER.into()),
                });

            let bind_group_layout =
                self.device
                    .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                        label: Some("aero-d3d11 aerogpu_cmd read_texture bc bgl"),
                        entries: &[
                            wgpu::BindGroupLayoutEntry {
                                binding: 0,
                                visibility: wgpu::ShaderStages::FRAGMENT,
                                ty: wgpu::BindingType::Texture {
                                    multisampled: false,
                                    view_dimension: wgpu::TextureViewDimension::D2,
                                    sample_type: wgpu::TextureSampleType::Float {
                                        filterable: true,
                                    },
                                },
                                count: None,
                            },
                            wgpu::BindGroupLayoutEntry {
                                binding: 1,
                                visibility: wgpu::ShaderStages::FRAGMENT,
                                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                                count: None,
                            },
                        ],
                    });
            let pipeline_layout =
                self.device
                    .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                        label: Some("aero-d3d11 aerogpu_cmd read_texture bc pipeline layout"),
                        bind_group_layouts: &[&bind_group_layout],
                        push_constant_ranges: &[],
                    });
            let pipeline = self
                .device
                .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("aero-d3d11 aerogpu_cmd read_texture bc pipeline"),
                    layout: Some(&pipeline_layout),
                    vertex: wgpu::VertexState {
                        module: &shader,
                        entry_point: "vs",
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                        buffers: &[],
                    },
                    fragment: Some(wgpu::FragmentState {
                        module: &shader,
                        entry_point: "fs",
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                        targets: &[Some(wgpu::ColorTargetState {
                            format: resolved_format,
                            blend: None,
                            write_mask: wgpu::ColorWrites::ALL,
                        })],
                    }),
                    primitive: wgpu::PrimitiveState {
                        topology: wgpu::PrimitiveTopology::TriangleList,
                        strip_index_format: None,
                        front_face: wgpu::FrontFace::Ccw,
                        cull_mode: None,
                        polygon_mode: wgpu::PolygonMode::Fill,
                        unclipped_depth: false,
                        conservative: false,
                    },
                    depth_stencil: None,
                    multisample: wgpu::MultisampleState::default(),
                    multiview: None,
                });

            let sampler = self.device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("aero-d3d11 aerogpu_cmd read_texture bc sampler"),
                address_mode_u: wgpu::AddressMode::ClampToEdge,
                address_mode_v: wgpu::AddressMode::ClampToEdge,
                address_mode_w: wgpu::AddressMode::ClampToEdge,
                mag_filter: wgpu::FilterMode::Nearest,
                min_filter: wgpu::FilterMode::Nearest,
                mipmap_filter: wgpu::FilterMode::Nearest,
                ..Default::default()
            });

            let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("aero-d3d11 aerogpu_cmd read_texture bc bg"),
                layout: &bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&src_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&sampler),
                    },
                ],
            });

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

            let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("aero-d3d11 aerogpu_cmd read_texture bc staging"),
                size: buffer_size,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });

            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("aero-d3d11 aerogpu_cmd read_texture bc encoder"),
                });

            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("aero-d3d11 aerogpu_cmd read_texture bc pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &resolved_view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
                pass.set_pipeline(&pipeline);
                pass.set_bind_group(0, &bind_group, &[]);
                pass.draw(0..3, 0..1);
            }

            encoder.copy_texture_to_buffer(
                wgpu::ImageCopyTexture {
                    texture: &resolved,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::ImageCopyBuffer {
                    buffer: &staging,
                    layout: wgpu::ImageDataLayout {
                        offset: 0,
                        bytes_per_row: Some(padded_bytes_per_row),
                        rows_per_image: Some(height),
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
            let unpadded_bpr_usize: usize = unpadded_bytes_per_row
                .try_into()
                .map_err(|_| anyhow!("read_texture_rgba8: bytes_per_row out of range"))?;

            let out_len = (unpadded_bytes_per_row as u64)
                .checked_mul(height as u64)
                .ok_or_else(|| anyhow!("read_texture_rgba8: output size overflow"))?;
            let out_len_usize: usize = out_len
                .try_into()
                .map_err(|_| anyhow!("read_texture_rgba8: output size out of range"))?;

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

            drop(mapped);
            staging.unmap();
            return Ok(out);
        }

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
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded_bytes_per_row = unpadded_bytes_per_row
            .checked_add(align - 1)
            .map(|v| v / align)
            .and_then(|v| v.checked_mul(align))
            .ok_or_else(|| anyhow!("read_texture_rgba8: padded bytes_per_row overflow"))?;
        let buffer_size = (padded_bytes_per_row as u64)
            .checked_mul(height as u64)
            .ok_or_else(|| anyhow!("read_texture_rgba8: staging buffer size overflow"))?;

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
                mip_level,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: array_layer,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &staging,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bytes_per_row),
                    rows_per_image: Some(height),
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
        let unpadded_bpr_usize: usize = unpadded_bytes_per_row
            .try_into()
            .map_err(|_| anyhow!("read_texture_rgba8: bytes_per_row out of range"))?;

        let out_len = (unpadded_bytes_per_row as u64)
            .checked_mul(height as u64)
            .ok_or_else(|| anyhow!("read_texture_rgba8: output size overflow"))?;
        let out_len_usize: usize = out_len
            .try_into()
            .map_err(|_| anyhow!("read_texture_rgba8: output size out of range"))?;

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

        drop(mapped);
        staging.unmap();

        if needs_bgra_swizzle {
            for px in out.chunks_exact_mut(4) {
                px.swap(0, 2);
            }
        }

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
                    // Shader creation uses the persistent shader cache on wasm, which requires async IO.
                    // Reject synchronous execution to avoid silently bypassing persistence.
                    OPCODE_CREATE_SHADER_DXBC => {
                        bail!(
                            "CREATE_SHADER_DXBC requires async execution on wasm (call execute_cmd_stream_async); first CREATE_SHADER_DXBC at packet {packet_index}"
                        );
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
        #[cfg(target_arch = "wasm32")]
        let report = self
            .execute_cmd_stream_inner_async(stream_bytes, allocs, guest_mem, &mut pending_writebacks)
            .await?;

        #[cfg(not(target_arch = "wasm32"))]
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
        // Scratch allocations are per-command-stream; reset the bump cursor so new work can reuse
        // existing backing buffers.
        self.gpu_scratch.reset();
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
                        if self.gs_hs_ds_emulation_required() {
                            self.exec_draw_with_compute_prepass(
                                &mut encoder,
                                &mut stream,
                                &alloc_map,
                                guest_mem,
                                &mut report,
                            )?;
                        } else {
                            self.exec_render_pass_load(
                                &mut encoder,
                                &mut stream,
                                &alloc_map,
                                guest_mem,
                                &mut report,
                            )?;
                        }
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

    #[cfg(target_arch = "wasm32")]
    async fn execute_cmd_stream_inner_async(
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

        let mut cursor = AerogpuCmdStreamHeader::SIZE_BYTES;
        let result: Result<()> = async {
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

                if opcode == OPCODE_CREATE_SHADER_DXBC {
                    self.exec_create_shader_dxbc_persistent(cmd_bytes).await?;
                } else {
                    self.exec_non_draw_command(
                        &mut encoder,
                        opcode,
                        cmd_bytes,
                        &alloc_map,
                        guest_mem,
                        pending_writebacks,
                        &mut report,
                    )?;
                }

                report.commands = report.commands.saturating_add(1);
                cursor = cmd_end;
            }
            Ok(())
        }
        .await;

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
                    let staging_unpadded_bpr_usize: usize = plan
                        .staging_unpadded_bytes_per_row
                        .try_into()
                        .map_err(|_| anyhow!("COPY_TEXTURE2D: bytes_per_row out of range"))?;
                    let guest_unpadded_bpr_usize: usize = plan
                        .guest_unpadded_bytes_per_row
                        .try_into()
                        .map_err(|_| anyhow!("COPY_TEXTURE2D: bytes_per_row out of range"))?;
                    let mapped = slice.get_mapped_range();
                    let mut converted_row = Vec::<u8>::new();
                    for row in 0..plan.height as u64 {
                        let src_start = row as usize * padded_bpr_usize;
                        let src_end = src_start
                            .checked_add(staging_unpadded_bpr_usize)
                            .ok_or_else(|| {
                                anyhow!("COPY_TEXTURE2D: src row end overflows usize")
                            })?;
                        let row_bytes = mapped.get(src_start..src_end).ok_or_else(|| {
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

                        match plan.transform {
                            TextureWritebackTransform::Direct { force_opaque_alpha } => {
                                let row_bytes =
                                    row_bytes.get(..guest_unpadded_bpr_usize).ok_or_else(|| {
                                        anyhow!("COPY_TEXTURE2D: staging buffer too small for row")
                                    })?;
                                if force_opaque_alpha {
                                    converted_row.clear();
                                    converted_row.extend_from_slice(row_bytes);
                                    force_opaque_alpha_rgba8(&mut converted_row);
                                    guest_mem
                                        .write(dst_gpa, &converted_row)
                                        .map_err(anyhow_guest_mem)?;
                                } else {
                                    guest_mem
                                        .write(dst_gpa, row_bytes)
                                        .map_err(anyhow_guest_mem)?;
                                }
                            }
                            TextureWritebackTransform::B5G6R5 => {
                                if guest_unpadded_bpr_usize != row_bytes.len() / 2 {
                                    bail!(
                                        "COPY_TEXTURE2D: internal error: packed writeback row size mismatch"
                                    );
                                }
                                converted_row.resize(guest_unpadded_bpr_usize, 0);
                                pack_rgba8_to_b5g6r5_unorm(row_bytes, &mut converted_row);
                                guest_mem
                                    .write(dst_gpa, &converted_row)
                                    .map_err(anyhow_guest_mem)?;
                            }
                            TextureWritebackTransform::B5G5R5A1 => {
                                if guest_unpadded_bpr_usize != row_bytes.len() / 2 {
                                    bail!(
                                        "COPY_TEXTURE2D: internal error: packed writeback row size mismatch"
                                    );
                                }
                                converted_row.resize(guest_unpadded_bpr_usize, 0);
                                pack_rgba8_to_b5g5r5a1_unorm(row_bytes, &mut converted_row);
                                guest_mem
                                    .write(dst_gpa, &converted_row)
                                    .map_err(anyhow_guest_mem)?;
                            }
                        }
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
                    let staging_unpadded_bpr_usize: usize = plan
                        .staging_unpadded_bytes_per_row
                        .try_into()
                        .map_err(|_| anyhow!("COPY_TEXTURE2D: bytes_per_row out of range"))?;
                    let guest_unpadded_bpr_usize: usize = plan
                        .guest_unpadded_bytes_per_row
                        .try_into()
                        .map_err(|_| anyhow!("COPY_TEXTURE2D: bytes_per_row out of range"))?;
                    let mapped = slice.get_mapped_range();
                    let mut converted_row = Vec::<u8>::new();
                    for row in 0..plan.height as u64 {
                        let src_start = row as usize * padded_bpr_usize;
                        let src_end = src_start
                            .checked_add(staging_unpadded_bpr_usize)
                            .ok_or_else(|| {
                                anyhow!("COPY_TEXTURE2D: src row end overflows usize")
                            })?;
                        let row_bytes = mapped.get(src_start..src_end).ok_or_else(|| {
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

                        match plan.transform {
                            TextureWritebackTransform::Direct { force_opaque_alpha } => {
                                let row_bytes =
                                    row_bytes.get(..guest_unpadded_bpr_usize).ok_or_else(|| {
                                        anyhow!("COPY_TEXTURE2D: staging buffer too small for row")
                                    })?;
                                if force_opaque_alpha {
                                    converted_row.clear();
                                    converted_row.extend_from_slice(row_bytes);
                                    force_opaque_alpha_rgba8(&mut converted_row);
                                    guest_mem
                                        .write(dst_gpa, &converted_row)
                                        .map_err(anyhow_guest_mem)?;
                                } else {
                                    guest_mem
                                        .write(dst_gpa, row_bytes)
                                        .map_err(anyhow_guest_mem)?;
                                }
                            }
                            TextureWritebackTransform::B5G6R5 => {
                                if guest_unpadded_bpr_usize != row_bytes.len() / 2 {
                                    bail!(
                                        "COPY_TEXTURE2D: internal error: packed writeback row size mismatch"
                                    );
                                }
                                converted_row.resize(guest_unpadded_bpr_usize, 0);
                                pack_rgba8_to_b5g6r5_unorm(row_bytes, &mut converted_row);
                                guest_mem
                                    .write(dst_gpa, &converted_row)
                                    .map_err(anyhow_guest_mem)?;
                            }
                            TextureWritebackTransform::B5G5R5A1 => {
                                if guest_unpadded_bpr_usize != row_bytes.len() / 2 {
                                    bail!(
                                        "COPY_TEXTURE2D: internal error: packed writeback row size mismatch"
                                    );
                                }
                                converted_row.resize(guest_unpadded_bpr_usize, 0);
                                pack_rgba8_to_b5g5r5a1_unorm(row_bytes, &mut converted_row);
                                guest_mem
                                    .write(dst_gpa, &converted_row)
                                    .map_err(anyhow_guest_mem)?;
                            }
                        }
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
            OPCODE_DESTROY_RESOURCE => self.exec_destroy_resource(encoder, cmd_bytes),
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
            OPCODE_SET_SHADER_RESOURCE_BUFFERS => self.exec_set_shader_resource_buffers(cmd_bytes),
            OPCODE_SET_UNORDERED_ACCESS_BUFFERS => self.exec_set_unordered_access_buffers(cmd_bytes),
            OPCODE_CLEAR => self.exec_clear(encoder, cmd_bytes, allocs, guest_mem),
            OPCODE_DISPATCH => self.exec_dispatch(encoder, cmd_bytes, allocs, guest_mem),
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

    /// Execute a single draw that requires a compute prepass (GS/HS/DS emulation).
    ///
    /// This records:
    /// 1) a compute pass that writes expanded vertex/index buffers + indirect args, then
    /// 2) a render pass that consumes those buffers via `draw_indirect`/`draw_indexed_indirect`.
    #[allow(clippy::too_many_arguments)]
    fn exec_draw_with_compute_prepass<'a>(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        stream: &mut CmdStreamCtx<'a, '_>,
        allocs: &AllocTable,
        guest_mem: &mut dyn GuestMemory,
        report: &mut ExecuteReport,
    ) -> Result<()> {
        self.validate_gs_hs_ds_emulation_capabilities()?;

        let has_color_targets = self.state.render_targets.iter().any(|rt| rt.is_some());
        if !has_color_targets && self.state.depth_stencil.is_none() {
            bail!("aerogpu_cmd: draw without bound render target or depth-stencil");
        }
        let depth_only_pass = !has_color_targets && self.state.depth_stencil.is_some();

        self.validate_gs_hs_ds_emulation_capabilities()?;

        let Some(next) = stream.iter.peek() else {
            return Ok(());
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

        if opcode != OPCODE_DRAW && opcode != OPCODE_DRAW_INDEXED {
            bail!("exec_draw_with_compute_prepass called on non-draw opcode {opcode:#x}");
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

        let vertex_pulling_draw = match opcode {
            OPCODE_DRAW => {
                // struct aerogpu_cmd_draw (24 bytes)
                if cmd_bytes.len() < 24 {
                    bail!(
                        "DRAW: expected at least 24 bytes, got {}",
                        cmd_bytes.len()
                    );
                }
                VertexPullingDrawParams {
                    first_vertex: read_u32_le(cmd_bytes, 16)?,
                    first_instance: read_u32_le(cmd_bytes, 20)?,
                    base_vertex: 0,
                    first_index: 0,
                }
            }
            OPCODE_DRAW_INDEXED => {
                // struct aerogpu_cmd_draw_indexed (28 bytes)
                if cmd_bytes.len() < 28 {
                    bail!(
                        "DRAW_INDEXED: expected at least 28 bytes, got {}",
                        cmd_bytes.len()
                    );
                }
                let mut first_index = read_u32_le(cmd_bytes, 16)?;
                // Fold the IASetIndexBuffer byte offset into `first_index` so compute-based index
                // pulling can bind the full index buffer at offset 0 (storage buffer bindings
                // typically require 256-byte alignment, which D3D11 does not guarantee).
                if let Some(ib) = self.state.index_buffer {
                    let stride = match ib.format {
                        wgpu::IndexFormat::Uint16 => 2u64,
                        wgpu::IndexFormat::Uint32 => 4u64,
                    };
                    if (ib.offset_bytes % stride) != 0 {
                        bail!(
                            "index buffer offset {} is not aligned to index stride {}",
                            ib.offset_bytes,
                            stride
                        );
                    }
                    let offset_indices_u64 = ib.offset_bytes / stride;
                    let offset_indices: u32 = offset_indices_u64.try_into().map_err(|_| {
                        anyhow!(
                            "index buffer offset {} is too large for u32 index math",
                            ib.offset_bytes
                        )
                    })?;
                    first_index = first_index.checked_add(offset_indices).ok_or_else(|| {
                        anyhow!("DRAW_INDEXED first_index overflows after applying index buffer offset")
                    })?;
                }
                VertexPullingDrawParams {
                    first_vertex: 0,
                    first_instance: read_u32_le(cmd_bytes, 24)?,
                    base_vertex: read_i32_le(cmd_bytes, 20)?,
                    first_index,
                }
            }
            _ => unreachable!(),
        };
        let (element_count, instance_count) = match opcode {
            OPCODE_DRAW => (read_u32_le(cmd_bytes, 8)?, read_u32_le(cmd_bytes, 12)?),
            OPCODE_DRAW_INDEXED => (read_u32_le(cmd_bytes, 8)?, read_u32_le(cmd_bytes, 12)?),
            _ => unreachable!(),
        };
        let primitive_count: u32 = match self.state.primitive_topology {
            CmdPrimitiveTopology::PointList => element_count,
            CmdPrimitiveTopology::LineList | CmdPrimitiveTopology::LineListAdj => element_count / 2,
            CmdPrimitiveTopology::LineStrip | CmdPrimitiveTopology::LineStripAdj => {
                element_count.saturating_sub(1)
            }
            CmdPrimitiveTopology::TriangleList
            | CmdPrimitiveTopology::TriangleListAdj
            | CmdPrimitiveTopology::PatchList { .. } => element_count / 3,
            CmdPrimitiveTopology::TriangleStrip
            | CmdPrimitiveTopology::TriangleStripAdj
            | CmdPrimitiveTopology::TriangleFan => element_count.saturating_sub(2),
        };

        // Consume the draw packet now so errors include consistent cursor information.
        stream.iter.next().expect("peeked Some").map_err(|err| {
            anyhow!(
                "aerogpu_cmd: invalid cmd header @0x{:x}: {err:?}",
                *stream.cursor
            )
        })?;

        if opcode == OPCODE_DRAW_INDEXED && self.state.index_buffer.is_none() {
            bail!("DRAW_INDEXED without index buffer");
        }

        if primitive_count == 0 || instance_count == 0 {
            report.commands = report.commands.saturating_add(1);
            *stream.cursor = cmd_end;
            return Ok(());
        }

        if let Some(gs_handle) = self.state.gs {
            if let Some(meta) = self.resources.gs_shaders.get(&gs_handle) {
                if meta.instance_count > 1 {
                    bail!(
                        "GS emulation: geometry shader {gs_handle} declares gsinstancecount {} (GS instancing is not supported yet)",
                        meta.instance_count
                    );
                }
            }
        }

        // Upload any dirty render targets/depth-stencil attachments before starting the passes.
        let render_targets = self.state.render_targets.clone();
        let depth_stencil = self.state.depth_stencil;
        for &handle in render_targets.iter().flatten() {
            self.ensure_texture_uploaded(encoder, handle, allocs, guest_mem)?;
        }
        if let Some(handle) = depth_stencil {
            self.ensure_texture_uploaded(encoder, handle, allocs, guest_mem)?;
        }

        // Upload any dirty resources used by the current input assembler bindings. The
        // vertex-pulling prepass reads at least one dword (and the eventual GS/HS/DS emulation path
        // will use the full vertex pulling layout).
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

        // The upcoming render pass will write to bound targets. Invalidate any CPU shadow copies
        // so that later partial `UPLOAD_RESOURCE` operations don't accidentally overwrite
        // GPU-produced contents.
        for &handle in render_targets.iter().flatten() {
            if let Some(tex) = self.resources.textures.get_mut(&handle) {
                tex.host_shadow = None;
                tex.host_shadow_valid.clear();
                tex.guest_backing_is_current = false;
            }
        }
        if let Some(handle) = depth_stencil {
            if let Some(tex) = self.resources.textures.get_mut(&handle) {
                tex.host_shadow = None;
                tex.host_shadow_valid.clear();
                tex.guest_backing_is_current = false;
            }
        }

        // Require VS/PS bindings to match the normal draw path (the VS will be consumed by the
        // eventual emulation compute shader, even though the placeholder prepass does not).
        let vs_handle = self
            .state
            .vs
            .ok_or_else(|| anyhow!("render draw without bound VS"))?;
        let ps_handle = self
            .state
            .ps
            .ok_or_else(|| anyhow!("render draw without bound PS"))?;

        let ps = self
            .resources
            .shaders
            .get(&ps_handle)
            .ok_or_else(|| anyhow!("unknown PS shader {ps_handle}"))?
            .clone();
        if ps.stage != ShaderStage::Pixel {
            bail!("shader {ps_handle} is not a pixel shader");
        }

        // Build render-pipeline bindings using the pixel shader only (the expanded-vertex
        // passthrough VS has no resource bindings).
        let mut pipeline_bindings = reflection_bindings::build_pipeline_bindings_info(
            &self.device,
            &mut self.bind_group_layout_cache,
            [reflection_bindings::ShaderBindingSet::Guest(
                ps.reflection.bindings.as_slice(),
            )],
            reflection_bindings::BindGroupIndexValidation::GuestShaders,
        )?;
        let layout_key = std::mem::replace(
            &mut pipeline_bindings.layout_key,
            PipelineLayoutKey::empty(),
        );

        let pipeline_layout = {
            let device = &self.device;
            let cache = &mut self.pipeline_layout_cache;
            cache.get_or_create_with(&layout_key, || {
                let layout_refs: Vec<&wgpu::BindGroupLayout> = pipeline_bindings
                    .group_layouts
                    .iter()
                    .map(|l| l.layout.as_ref())
                    .collect();
                Arc::new(
                    device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                        label: Some("aerogpu_cmd expanded draw pipeline layout"),
                        bind_group_layouts: &layout_refs,
                        push_constant_ranges: &[],
                    }),
                )
            })
        };

        // Ensure any guest-backed resources referenced by the current binding state are uploaded
        // before entering the render pass.
        self.ensure_bound_resources_uploaded(encoder, &pipeline_bindings, allocs, guest_mem)?;

        // If sample_mask disables all samples, treat the draw as a no-op (mirrors the normal draw
        // path behavior).
        if (self.state.sample_mask & 1) == 0 {
            report.commands = report.commands.saturating_add(1);
            *stream.cursor = cmd_end;
            return Ok(());
        }

        // If the effective viewport/scissor is empty (e.g. entirely out of bounds), treat the draw
        // as a no-op. This mirrors D3D11 behavior while avoiding invalid WebGPU dynamic state (wgpu
        // does not allow zero-sized viewports/scissors).
        let rt_dims = self
            .state
            .render_targets
            .iter()
            .flatten()
            .next()
            .and_then(|rt| self.resources.textures.get(rt))
            .map(|tex| (tex.desc.width, tex.desc.height))
            .or_else(|| {
                self.state
                    .depth_stencil
                    .and_then(|ds| self.resources.textures.get(&ds))
                    .map(|tex| (tex.desc.width, tex.desc.height))
            });
        if let Some((rt_w, rt_h)) = rt_dims {
            let mut viewport_empty = false;
            if let Some(vp) = self.state.viewport {
                let valid = vp.x.is_finite()
                    && vp.y.is_finite()
                    && vp.width.is_finite()
                    && vp.height.is_finite()
                    && vp.min_depth.is_finite()
                    && vp.max_depth.is_finite()
                    && vp.width > 0.0
                    && vp.height > 0.0;
                if valid {
                    let max_w = rt_w as f32;
                    let max_h = rt_h as f32;
                    let left = vp.x.max(0.0);
                    let top = vp.y.max(0.0);
                    let right = (vp.x + vp.width).max(0.0).min(max_w);
                    let bottom = (vp.y + vp.height).max(0.0).min(max_h);
                    let width = (right - left).max(0.0);
                    let height = (bottom - top).max(0.0);
                    if width == 0.0 || height == 0.0 {
                        viewport_empty = true;
                    }
                }
            }

            let mut scissor_empty = false;
            if self.state.scissor_enable {
                if let Some(sc) = self.state.scissor {
                    let x = sc.x.min(rt_w);
                    let y = sc.y.min(rt_h);
                    let width = sc.width.min(rt_w.saturating_sub(x));
                    let height = sc.height.min(rt_h.saturating_sub(y));
                    if width == 0 || height == 0 {
                        scissor_empty = true;
                    }
                }
            }

            if viewport_empty || scissor_empty {
                report.commands = report.commands.saturating_add(1);
                *stream.cursor = cmd_end;
                return Ok(());
            }
        }

        // Prepare compute prepass output buffers.
        let uniform_align = (self.device.limits().min_uniform_buffer_offset_alignment as u64).max(1);
        let expanded_vertex_count = u64::from(primitive_count)
            .checked_mul(3)
            .ok_or_else(|| anyhow!("geometry prepass expanded vertex count overflow"))?;
        let expanded_index_count = expanded_vertex_count;
        let expanded_vertex_size = GEOMETRY_PREPASS_EXPANDED_VERTEX_STRIDE_BYTES
            .checked_mul(expanded_vertex_count)
            .ok_or_else(|| anyhow!("geometry prepass expanded vertex buffer size overflow"))?;
        let expanded_index_size = expanded_index_count
            .checked_mul(4)
            .ok_or_else(|| anyhow!("geometry prepass expanded index buffer size overflow"))?;

        let expanded_vertex_alloc = self
            .expansion_scratch
            .alloc_vertex_output(&self.device, expanded_vertex_size)
            .map_err(|e| anyhow!("geometry prepass: alloc expanded vertex buffer: {e}"))?;
        let expanded_index_alloc = self
            .expansion_scratch
            .alloc_index_output(&self.device, expanded_index_size)
            .map_err(|e| anyhow!("geometry prepass: alloc expanded index buffer: {e}"))?;
        let indirect_args_alloc = self
            .expansion_scratch
            .alloc_indirect_draw_indexed(&self.device)
            .map_err(|e| anyhow!("geometry prepass: alloc indirect args buffer: {e}"))?;
        let counter_alloc = self
            .expansion_scratch
            .alloc_counter_u32(&self.device)
            .map_err(|e| anyhow!("geometry prepass: alloc counter buffer: {e}"))?;

        // Default placeholder color: solid red.
        let mut params_bytes = [0u8; GEOMETRY_PREPASS_PARAMS_SIZE_BYTES as usize];
        params_bytes[0..4].copy_from_slice(&1.0f32.to_le_bytes());
        params_bytes[4..8].copy_from_slice(&0.0f32.to_le_bytes());
        params_bytes[8..12].copy_from_slice(&0.0f32.to_le_bytes());
        params_bytes[12..16].copy_from_slice(&1.0f32.to_le_bytes());
        // Counts (`vec4<u32>`) for compute-based GS emulation:
        // - x: primitive_count (dispatch.x)
        // - y: instance_count (for indirect draw args)
        params_bytes[16..20].copy_from_slice(&primitive_count.to_le_bytes());
        params_bytes[20..24].copy_from_slice(&instance_count.to_le_bytes());
        let params_alloc = self
            .expansion_scratch
            .alloc_metadata(&self.device, GEOMETRY_PREPASS_PARAMS_SIZE_BYTES, uniform_align)
            .map_err(|e| anyhow!("geometry prepass: alloc params buffer: {e}"))?;
        self.queue.write_buffer(
            params_alloc.buffer.as_ref(),
            params_alloc.offset,
            &params_bytes,
        );

        let depth_params_buffer = self
            .legacy_constants
            .get(&ShaderStage::Compute)
            .expect("legacy constants buffer exists for every stage");
        let depth_params_size = wgpu::BufferSize::new(16).expect("non-zero buffer size");

        // Optionally set up vertex/index pulling bindings for the compute prepass. This is needed
        // for the eventual VS-as-compute implementation.
        //
        // - Vertex pulling requires an input layout + vertex buffers, so we only enable it when an
        //   input layout is bound.
        // - Indexed draws can still bind index pulling even without an input layout (useful for
        //   future VS-as-compute shaders that use only `SV_VertexID`).
        let mut vertex_pulling_bgl: Option<aero_gpu::bindings::layout_cache::CachedBindGroupLayout> =
            None;
        let mut vertex_pulling_bg: Option<wgpu::BindGroup> = None;
        let mut vertex_pulling_cs_wgsl: Option<String> = None;
        if let Some(layout_handle) = self.state.input_layout {
            let vs = self
                .resources
                .shaders
                .get(&vs_handle)
                .ok_or_else(|| anyhow!("unknown VS shader {vs_handle}"))?
                .clone();
            if vs.stage != ShaderStage::Vertex {
                bail!("shader {vs_handle} is not a vertex shader");
            }
            let layout = self
                .resources
                .input_layouts
                .get(&layout_handle)
                .ok_or_else(|| anyhow!("unknown input layout {layout_handle}"))?;

            // Strides come from IASetVertexBuffers state, not from ILAY.
            let slot_strides: Vec<u32> = self
                .state
                .vertex_buffers
                .iter()
                .map(|vb| vb.as_ref().map(|b| b.stride_bytes).unwrap_or(0))
                .collect();
            let binding = InputLayoutBinding::new(&layout.layout, &slot_strides);
            let pulling =
                VertexPullingLayout::new(&binding, &vs.vs_input_signature).map_err(|e| {
                    anyhow!("failed to build vertex pulling layout for input layout {layout_handle}: {e}")
                })?;

            // Build per-slot uniform data + bind group.
            let mut slots: Vec<VertexPullingSlot> =
                Vec::with_capacity(pulling.pulling_slot_to_d3d_slot.len());
            let mut buffers: Vec<&wgpu::Buffer> =
                Vec::with_capacity(pulling.pulling_slot_to_d3d_slot.len());
            for &d3d_slot in &pulling.pulling_slot_to_d3d_slot {
                let vb = self
                    .state
                    .vertex_buffers
                    .get(d3d_slot as usize)
                    .and_then(|v| *v)
                    .ok_or_else(|| anyhow!("missing vertex buffer binding for slot {d3d_slot}"))?;
                let base_offset_bytes: u32 = vb.offset_bytes.try_into().map_err(|_| {
                    anyhow!("vertex buffer slot {d3d_slot} offset {} out of range", vb.offset_bytes)
                })?;

                let buf = self
                    .resources
                    .buffers
                    .get(&vb.buffer)
                    .ok_or_else(|| anyhow!("unknown vertex buffer {}", vb.buffer))?;
                slots.push(VertexPullingSlot {
                    base_offset_bytes,
                    stride_bytes: vb.stride_bytes,
                });
                buffers.push(&buf.buffer);
            }

            let uniform_bytes = pulling.pack_uniform_bytes(&slots, vertex_pulling_draw);
            let uniform_alloc = self
                .expansion_scratch
                .alloc_metadata(&self.device, uniform_bytes.len() as u64, uniform_align)
                .map_err(|e| anyhow!("geometry prepass: alloc vertex pulling uniform: {e}"))?;
            self.queue.write_buffer(
                uniform_alloc.buffer.as_ref(),
                uniform_alloc.offset,
                &uniform_bytes,
            );

            let mut vp_bgl_entries = pulling.bind_group_layout_entries();
            let mut vp_bg_entries: Vec<wgpu::BindGroupEntry<'_>> =
                Vec::with_capacity(buffers.len() + 3);
            for (slot, buf) in buffers.iter().enumerate() {
                vp_bg_entries.push(wgpu::BindGroupEntry {
                    binding: VERTEX_PULLING_VERTEX_BUFFER_BINDING_BASE + slot as u32,
                    resource: buf.as_entire_binding(),
                });
            }
            vp_bg_entries.push(wgpu::BindGroupEntry {
                binding: VERTEX_PULLING_UNIFORM_BINDING,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: uniform_alloc.buffer.as_ref(),
                    offset: uniform_alloc.offset,
                    size: wgpu::BufferSize::new(uniform_alloc.size),
                }),
            });

            let mut vp_cs_prelude = pulling.wgsl_prelude();
            let index_pulling_setup = if opcode == OPCODE_DRAW_INDEXED {
                let ib = self.state.index_buffer.expect("checked above");
                let ib_buf = self
                    .resources
                    .buffers
                    .get(&ib.buffer)
                    .ok_or_else(|| anyhow!("unknown index buffer {}", ib.buffer))?;

                let index_format = match ib.format {
                    wgpu::IndexFormat::Uint16 => super::index_pulling::INDEX_FORMAT_U16,
                    wgpu::IndexFormat::Uint32 => super::index_pulling::INDEX_FORMAT_U32,
                };
                let params = IndexPullingParams {
                    first_index: vertex_pulling_draw.first_index,
                    base_vertex: vertex_pulling_draw.base_vertex,
                    index_format,
                    _pad0: 0,
                };

                let params_bytes = params.to_le_bytes();
                let params_alloc = self
                    .expansion_scratch
                    .alloc_metadata(&self.device, params_bytes.len() as u64, uniform_align)
                    .map_err(|e| anyhow!("geometry prepass: alloc index pulling params: {e}"))?;
                self.queue.write_buffer(
                    params_alloc.buffer.as_ref(),
                    params_alloc.offset,
                    &params_bytes,
                );

                vp_cs_prelude.push_str(&super::index_pulling::wgsl_index_pulling_lib(
                    VERTEX_PULLING_GROUP,
                    INDEX_PULLING_PARAMS_BINDING,
                    INDEX_PULLING_BUFFER_BINDING,
                ));
                Some((params_alloc, &ib_buf.buffer))
            } else {
                None
            };

            if let Some((params_alloc, ib_buffer)) = index_pulling_setup.as_ref() {
                vp_bgl_entries.push(wgpu::BindGroupLayoutEntry {
                    binding: INDEX_PULLING_PARAMS_BINDING,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(16),
                    },
                    count: None,
                });
                vp_bgl_entries.push(wgpu::BindGroupLayoutEntry {
                    binding: INDEX_PULLING_BUFFER_BINDING,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                });

                vp_bg_entries.push(wgpu::BindGroupEntry {
                    binding: INDEX_PULLING_PARAMS_BINDING,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: params_alloc.buffer.as_ref(),
                        offset: params_alloc.offset,
                        size: wgpu::BufferSize::new(params_alloc.size),
                    }),
                });
                vp_bg_entries.push(wgpu::BindGroupEntry {
                    binding: INDEX_PULLING_BUFFER_BINDING,
                    resource: ib_buffer.as_entire_binding(),
                });
            }

            let vp_bgl = self
                .bind_group_layout_cache
                .get_or_create(&self.device, &vp_bgl_entries);
            let vp_bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("aerogpu_cmd vertex pulling bind group"),
                layout: vp_bgl.layout.as_ref(),
                entries: &vp_bg_entries,
            });

            let cs_body = if pulling.slot_count() > 0 {
                GEOMETRY_PREPASS_CS_VERTEX_PULLING_WGSL
            } else {
                GEOMETRY_PREPASS_CS_WGSL
            };
            vertex_pulling_cs_wgsl = Some(format!("{vp_cs_prelude}\n{cs_body}"));
            vertex_pulling_bgl = Some(vp_bgl);
            vertex_pulling_bg = Some(vp_bg);
        } else if opcode == OPCODE_DRAW_INDEXED {
            // Even without an input layout, indexed draws still need index pulling when executing
            // the IA/VS stages in compute. Bind the real index buffer + params so future VS-as-
            // compute implementations can resolve `SV_VertexID` correctly.
            let ib = self
                .state
                .index_buffer
                .expect("checked DRAW_INDEXED precondition above");
            let ib_buf = self
                .resources
                .buffers
                .get(&ib.buffer)
                .ok_or_else(|| anyhow!("unknown index buffer {}", ib.buffer))?;

            let index_format = match ib.format {
                wgpu::IndexFormat::Uint16 => super::index_pulling::INDEX_FORMAT_U16,
                wgpu::IndexFormat::Uint32 => super::index_pulling::INDEX_FORMAT_U32,
            };
            let params = IndexPullingParams {
                first_index: vertex_pulling_draw.first_index,
                base_vertex: vertex_pulling_draw.base_vertex,
                index_format,
                _pad0: 0,
            };
            let params_bytes = params.to_le_bytes();
            let params_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("aerogpu_cmd index pulling params"),
                size: params_bytes.len() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: true,
            });
            {
                let mut mapped = params_buffer.slice(..).get_mapped_range_mut();
                mapped.copy_from_slice(&params_bytes);
            }
            params_buffer.unmap();

            let ip_bgl_entries = [
                wgpu::BindGroupLayoutEntry {
                    binding: INDEX_PULLING_PARAMS_BINDING,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(16),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: INDEX_PULLING_BUFFER_BINDING,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ];
            let ip_bgl = self
                .bind_group_layout_cache
                .get_or_create(&self.device, &ip_bgl_entries);
            let ip_bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("aerogpu_cmd index pulling bind group"),
                layout: ip_bgl.layout.as_ref(),
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: INDEX_PULLING_PARAMS_BINDING,
                        resource: params_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: INDEX_PULLING_BUFFER_BINDING,
                        resource: ib_buf.buffer.as_entire_binding(),
                    },
                ],
            });

            let ip_wgsl = super::index_pulling::wgsl_index_pulling_lib(
                VERTEX_PULLING_GROUP,
                INDEX_PULLING_PARAMS_BINDING,
                INDEX_PULLING_BUFFER_BINDING,
            );
            vertex_pulling_cs_wgsl = Some(format!("{ip_wgsl}\n{GEOMETRY_PREPASS_CS_WGSL}"));
            vertex_pulling_bgl = Some(ip_bgl);
            vertex_pulling_bg = Some(ip_bg);
        }

        // Build compute prepass pipeline + bind group.
        let compute_bgl_entries = [
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: false },
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(expanded_vertex_size),
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: false },
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(expanded_index_size),
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: false },
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(
                        GEOMETRY_PREPASS_INDIRECT_ARGS_SIZE_BYTES,
                    ),
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: false },
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(GEOMETRY_PREPASS_COUNTER_SIZE_BYTES),
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 4,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(GEOMETRY_PREPASS_PARAMS_SIZE_BYTES),
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 5,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: Some(depth_params_size),
                },
                count: None,
            },
        ];

        let compute_bgl = self
            .bind_group_layout_cache
            .get_or_create(&self.device, &compute_bgl_entries);
        let empty_bgl = self
            .bind_group_layout_cache
            .get_or_create(&self.device, &[]);
        let (compute_layout_key, compute_pipeline_layout) = if let Some(vp_bgl) = &vertex_pulling_bgl
        {
            // Vertex pulling WGSL is written to `@group(VERTEX_PULLING_GROUP)`. This compute prepass
            // uses `@group(0)` for its output buffers, so we must insert empty bind-group layouts for
            // any intermediate groups so the pipeline layout has a compatible layout at
            // `VERTEX_PULLING_GROUP`.
            let mut hashes: Vec<u64> = Vec::with_capacity(VERTEX_PULLING_GROUP as usize + 1);
            let mut layouts: Vec<&wgpu::BindGroupLayout> =
                Vec::with_capacity(VERTEX_PULLING_GROUP as usize + 1);
            hashes.push(compute_bgl.hash);
            layouts.push(compute_bgl.layout.as_ref());
            for _ in 1..VERTEX_PULLING_GROUP {
                hashes.push(empty_bgl.hash);
                layouts.push(empty_bgl.layout.as_ref());
            }
            hashes.push(vp_bgl.hash);
            layouts.push(vp_bgl.layout.as_ref());

            let key = PipelineLayoutKey {
                bind_group_layout_hashes: hashes,
            };
            let layout = self.pipeline_layout_cache.get_or_create(
                &self.device,
                &key,
                &layouts,
                Some("aerogpu_cmd geometry prepass pipeline layout (vertex pulling)"),
            );
            (key, layout)
        } else {
            let key = PipelineLayoutKey {
                bind_group_layout_hashes: vec![compute_bgl.hash],
            };
            let layouts = [compute_bgl.layout.as_ref()];
            let layout = self.pipeline_layout_cache.get_or_create(
                &self.device,
                &key,
                &layouts,
                Some("aerogpu_cmd geometry prepass pipeline layout"),
            );
            (key, layout)
        };

        let compute_bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("aerogpu_cmd geometry prepass bind group"),
            layout: compute_bgl.layout.as_ref(),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: expanded_vertex_alloc.buffer.as_ref(),
                        offset: expanded_vertex_alloc.offset,
                        size: wgpu::BufferSize::new(expanded_vertex_size),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: expanded_index_alloc.buffer.as_ref(),
                        offset: expanded_index_alloc.offset,
                        size: wgpu::BufferSize::new(expanded_index_size),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: indirect_args_alloc.buffer.as_ref(),
                        offset: indirect_args_alloc.offset,
                        size: wgpu::BufferSize::new(GEOMETRY_PREPASS_INDIRECT_ARGS_SIZE_BYTES),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: counter_alloc.buffer.as_ref(),
                        offset: counter_alloc.offset,
                        size: wgpu::BufferSize::new(GEOMETRY_PREPASS_COUNTER_SIZE_BYTES),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: params_alloc.buffer.as_ref(),
                        offset: params_alloc.offset,
                        size: wgpu::BufferSize::new(GEOMETRY_PREPASS_PARAMS_SIZE_BYTES),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: depth_params_buffer,
                        offset: 0,
                        size: Some(depth_params_size),
                    }),
                },
            ],
        });

        let compute_pipeline_ptr = {
            let (cs_hash, _module) = self.pipeline_cache.get_or_create_shader_module(
                &self.device,
                aero_gpu::pipeline_key::ShaderStage::Compute,
                vertex_pulling_cs_wgsl
                    .as_deref()
                    .unwrap_or(GEOMETRY_PREPASS_CS_WGSL),
                Some("aerogpu_cmd geometry prepass CS"),
            );
            let key = ComputePipelineKey {
                shader: cs_hash,
                layout: compute_layout_key.clone(),
            };
            let pipeline = self
                .pipeline_cache
                .get_or_create_compute_pipeline(&self.device, key, move |device, cs| {
                    device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                        label: Some("aerogpu_cmd geometry prepass compute pipeline"),
                        layout: Some(compute_pipeline_layout.as_ref()),
                        module: cs,
                        entry_point: "cs_main",
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                    })
                })
                .map_err(|e| anyhow!("wgpu pipeline cache: {e:?}"))?;
            pipeline as *const wgpu::ComputePipeline
        };
        let compute_pipeline = unsafe { &*compute_pipeline_ptr };

        self.encoder_has_commands = true;
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("aerogpu_cmd geometry prepass compute pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(compute_pipeline);
            pass.set_bind_group(0, &compute_bind_group, &[]);
            if let Some(bg) = vertex_pulling_bg.as_ref() {
                pass.set_bind_group(VERTEX_PULLING_GROUP, bg, &[]);
            }
            pass.dispatch_workgroups(primitive_count, 1, 1);
        }

        // Build bind groups for the render pass (before starting the pass so we can freely mutate
        // executor caches).
        let mut render_bind_groups: Vec<Arc<wgpu::BindGroup>> =
            Vec::with_capacity(pipeline_bindings.group_layouts.len());
        for group_index in 0..pipeline_bindings.group_layouts.len() {
            if pipeline_bindings.group_bindings[group_index].is_empty() {
                let entries: [BindGroupCacheEntry<'_>; 0] = [];
                let bg = self.bind_group_cache.get_or_create(
                    &self.device,
                    &pipeline_bindings.group_layouts[group_index],
                    &entries,
                );
                render_bind_groups.push(bg);
            } else {
                let stage = group_index_to_stage(group_index as u32)?;
                let stage_bindings = self.bindings.stage_mut(stage);
                if stage_bindings.is_dirty() {
                    let provider = CmdExecutorBindGroupProvider {
                        resources: &self.resources,
                        legacy_constants: &self.legacy_constants,
                        cbuffer_scratch: &self.cbuffer_scratch,
                        dummy_uniform: &self.dummy_uniform,
                        dummy_storage: &self.dummy_storage,
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
                    stage_bindings.clear_dirty();
                    render_bind_groups.push(bg);
                } else {
                    // Stage not dirty; reuse cached bind group if possible by rebuilding with the
                    // current state. This is still cheap due to BindGroupCache.
                    let provider = CmdExecutorBindGroupProvider {
                        resources: &self.resources,
                        legacy_constants: &self.legacy_constants,
                        cbuffer_scratch: &self.cbuffer_scratch,
                        dummy_uniform: &self.dummy_uniform,
                        dummy_storage: &self.dummy_storage,
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
                    render_bind_groups.push(bg);
                }
            }
        }

        // `PipelineCache` returns a reference tied to the mutable borrow. Convert it to a raw
        // pointer so we can keep using executor state while the render pass is alive.
        let render_pipeline_ptr = {
            let (_key, pipeline) = get_or_create_render_pipeline_for_expanded_draw(
                &self.device,
                &mut self.pipeline_cache,
                pipeline_layout.as_ref(),
                &self.resources,
                &self.state,
                layout_key,
            )?;
            pipeline as *const wgpu::RenderPipeline
        };
        let render_pipeline = unsafe { &*render_pipeline_ptr };

        let rt_dims = self
            .state
            .render_targets
            .iter()
            .flatten()
            .next()
            .and_then(|rt| self.resources.textures.get(rt))
            .map(|tex| (tex.desc.width, tex.desc.height))
            .or_else(|| {
                self.state
                    .depth_stencil
                    .and_then(|ds| self.resources.textures.get(&ds))
                    .map(|tex| (tex.desc.width, tex.desc.height))
            });

        let ds_info: Option<(u32, u32, *const wgpu::TextureView, wgpu::TextureFormat)> =
            if depth_only_pass {
                let ds_id = self
                    .state
                    .depth_stencil
                    .expect("depth_only_pass implies depth_stencil.is_some()");
                let ds_tex = self
                    .resources
                    .textures
                    .get(&ds_id)
                    .ok_or_else(|| anyhow!("unknown depth stencil texture {ds_id}"))?;
                Some((
                    ds_tex.desc.width,
                    ds_tex.desc.height,
                    &ds_tex.view as *const wgpu::TextureView,
                    ds_tex.desc.format,
                ))
            } else {
                None
            };

        let dummy_color_view: Option<wgpu::TextureView> = ds_info
            .as_ref()
            .map(|(w, h, _, _)| self.depth_only_dummy_color_view(*w, *h));

        let (color_attachments, depth_stencil_attachment) = if depth_only_pass {
            let (_ds_w, _ds_h, ds_view_ptr, ds_format) =
                ds_info.expect("depth_only_pass implies ds_info.is_some()");
            let view = dummy_color_view.as_ref().expect("dummy view exists");
            let color_attachments = vec![Some(wgpu::RenderPassColorAttachment {
                view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Discard,
                },
            })];

            // SAFETY: textures are not created/destroyed while recording the draw, so the view
            // pointer remains valid for the lifetime of this render pass.
            let ds_view = unsafe { &*ds_view_ptr };
            let format = ds_format;
            let depth_stencil_attachment = Some(wgpu::RenderPassDepthStencilAttachment {
                view: ds_view,
                depth_ops: texture_format_has_depth(format).then_some(wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: texture_format_has_stencil(format).then_some(wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                }),
            });
            (color_attachments, depth_stencil_attachment)
        } else {
            build_render_pass_attachments(&self.resources, &self.state, wgpu::LoadOp::Load)?
        };

        self.encoder_has_commands = true;
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("aerogpu_cmd expanded draw render pass"),
                color_attachments: &color_attachments,
                depth_stencil_attachment,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            let mut skip_draw = false;

            if let Some(vp) = self.state.viewport {
                let valid = vp.x.is_finite()
                    && vp.y.is_finite()
                    && vp.width.is_finite()
                    && vp.height.is_finite()
                    && vp.min_depth.is_finite()
                    && vp.max_depth.is_finite()
                    && vp.width > 0.0
                    && vp.height > 0.0;
                if valid {
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
                        } else {
                            skip_draw = true;
                        }
                    } else {
                        let mut min_depth = vp.min_depth.clamp(0.0, 1.0);
                        let mut max_depth = vp.max_depth.clamp(0.0, 1.0);
                        if min_depth > max_depth {
                            std::mem::swap(&mut min_depth, &mut max_depth);
                        }
                        pass.set_viewport(vp.x, vp.y, vp.width, vp.height, min_depth, max_depth);
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
                        } else {
                            skip_draw = true;
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

            pass.set_pipeline(render_pipeline);
            let vb_end = expanded_vertex_alloc
                .offset
                .checked_add(expanded_vertex_alloc.size)
                .ok_or_else(|| anyhow!("geometry prepass expanded vertex slice overflows u64"))?;
            pass.set_vertex_buffer(
                0,
                expanded_vertex_alloc
                    .buffer
                    .slice(expanded_vertex_alloc.offset..vb_end),
            );

            for (group_index, bg) in render_bind_groups.iter().enumerate() {
                pass.set_bind_group(group_index as u32, bg.as_ref(), &[]);
            }

            if !skip_draw {
                if opcode == OPCODE_DRAW_INDEXED {
                    let ib_end = expanded_index_alloc
                        .offset
                        .checked_add(expanded_index_alloc.size)
                        .ok_or_else(|| anyhow!("geometry prepass expanded index slice overflows u64"))?;
                    pass.set_index_buffer(
                        expanded_index_alloc
                            .buffer
                            .slice(expanded_index_alloc.offset..ib_end),
                        wgpu::IndexFormat::Uint32,
                    );
                    pass.draw_indexed_indirect(
                        indirect_args_alloc.buffer.as_ref(),
                        indirect_args_alloc.offset,
                    );
                } else {
                    pass.draw_indirect(
                        indirect_args_alloc.buffer.as_ref(),
                        indirect_args_alloc.offset,
                    );
                }
            }
        }

        for &handle in render_targets.iter().flatten() {
            self.encoder_used_textures.insert(handle);
        }
        if let Some(handle) = depth_stencil {
            self.encoder_used_textures.insert(handle);
        }

        // Conservatively mark IA buffers and shader-bound resources as used to preserve
        // queue.write_* ordering heuristics.
        for vb in self.state.vertex_buffers.iter().flatten() {
            self.encoder_used_buffers.insert(vb.buffer);
        }
        if let Some(ib) = self.state.index_buffer {
            self.encoder_used_buffers.insert(ib.buffer);
        }
        for (group_index, group_bindings) in pipeline_bindings.group_bindings.iter().enumerate() {
            let stage = group_index_to_stage(group_index as u32)?;
            let stage_bindings = self.bindings.stage(stage);
            for binding in group_bindings {
                #[allow(unreachable_patterns)]
                match &binding.kind {
                    crate::BindingKind::Texture2D { slot } => {
                        if let Some(tex) = stage_bindings.texture(*slot) {
                            self.encoder_used_textures.insert(tex.texture);
                        }
                    }
                    crate::BindingKind::SrvBuffer { slot } => {
                        if let Some(buf) = stage_bindings.srv_buffer(*slot) {
                            self.encoder_used_buffers.insert(buf.buffer);
                        }
                    }
                    crate::BindingKind::UavBuffer { slot } => {
                        if let Some(buf) = stage_bindings.uav_buffer(*slot) {
                            self.encoder_used_buffers.insert(buf.buffer);
                        }
                        if let Some(tex) = stage_bindings.uav_texture(*slot) {
                            self.encoder_used_textures.insert(tex.texture);
                        }
                    }
                    crate::BindingKind::ConstantBuffer { slot, .. } => {
                        if let Some(cb) = stage_bindings.constant_buffer(*slot) {
                            self.encoder_used_buffers.insert(cb.buffer);
                        }
                    }
                    crate::BindingKind::Sampler { .. } => {}
                    _ => {
                        let binding_num = binding.binding;
                        if binding_num >= BINDING_BASE_UAV {
                            let slot = binding_num.saturating_sub(BINDING_BASE_UAV);
                            if let Some(buf) = stage_bindings.uav_buffer(slot) {
                                self.encoder_used_buffers.insert(buf.buffer);
                            }
                            if let Some(tex) = stage_bindings.uav_texture(slot) {
                                self.encoder_used_textures.insert(tex.texture);
                            }
                        } else if binding_num >= BINDING_BASE_TEXTURE
                            && binding_num < BINDING_BASE_SAMPLER
                        {
                            let slot = binding_num.saturating_sub(BINDING_BASE_TEXTURE);
                            if let Some(tex) = stage_bindings.texture(slot) {
                                self.encoder_used_textures.insert(tex.texture);
                            }
                            if let Some(buf) = stage_bindings.srv_buffer(slot) {
                                self.encoder_used_buffers.insert(buf.buffer);
                            }
                        } else if binding_num < BINDING_BASE_TEXTURE {
                            if let Some(cb) = stage_bindings.constant_buffer(binding_num) {
                                self.encoder_used_buffers.insert(cb.buffer);
                            }
                        }
                    }
                }
            }
        }

        report.commands = report.commands.saturating_add(1);
        *stream.cursor = cmd_end;
        Ok(())
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
        // If a guest binds a compute shader and then issues a draw with no graphics shaders
        // bound, treat it as an explicit (but unsupported) attempt to run compute work through
        // the graphics pipeline.
        //
        // Compute shaders must be executed via the DISPATCH opcode.
        if self.state.vs.is_none() && self.state.ps.is_none() && self.state.cs.is_some() {
            bail!("aerogpu_cmd: draw called with only a compute shader bound (use DISPATCH)");
        }

        let has_color_targets = self.state.render_targets.iter().any(|rt| rt.is_some());
        if !has_color_targets && self.state.depth_stencil.is_none() {
            bail!("aerogpu_cmd: draw without bound render target or depth-stencil");
        }

        self.validate_gs_hs_ds_emulation_capabilities()?;

        let depth_only_pass = !has_color_targets && self.state.depth_stencil.is_some();

        // Some D3D11 primitive topologies (patchlists + adjacency) cannot be expressed in WebGPU
        // render pipelines. Accept them in `SET_PRIMITIVE_TOPOLOGY` so the command stream can carry
        // the original D3D11 value, but reject attempts to draw through the non-emulated path.
        //
        // This is prerequisite plumbing for future HS/DS + GS emulation.
        let _topology = self.state.primitive_topology.validate_direct_draw()?;

        let render_targets = self.state.render_targets.clone();
        let depth_stencil = self.state.depth_stencil;
        for &handle in render_targets.iter().flatten() {
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
        for &handle in render_targets.iter().flatten() {
            if let Some(tex) = self.resources.textures.get_mut(&handle) {
                tex.host_shadow = None;
                tex.host_shadow_valid.clear();
                tex.guest_backing_is_current = false;
            }
        }
        if let Some(handle) = depth_stencil {
            if let Some(tex) = self.resources.textures.get_mut(&handle) {
                tex.host_shadow = None;
                tex.host_shadow_valid.clear();
                tex.guest_backing_is_current = false;
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
            [
                reflection_bindings::ShaderBindingSet::Guest(vs.reflection.bindings.as_slice()),
                reflection_bindings::ShaderBindingSet::Guest(ps.reflection.bindings.as_slice()),
            ],
            reflection_bindings::BindGroupIndexValidation::GuestShaders,
        )?;

        // `PipelineLayoutKey` is used both for pipeline-layout caching and as part of the pipeline
        // cache key. Avoid cloning the underlying Vec by moving it out of `pipeline_bindings`.
        let layout_key = std::mem::replace(
            &mut pipeline_bindings.layout_key,
            PipelineLayoutKey::empty(),
        );

        let pipeline_layout = {
            let device = &self.device;
            let cache = &mut self.pipeline_layout_cache;
            cache.get_or_create_with(&layout_key, || {
                let layout_refs: Vec<&wgpu::BindGroupLayout> = pipeline_bindings
                    .group_layouts
                    .iter()
                    .map(|l| l.layout.as_ref())
                    .collect();
                Arc::new(
                    device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                        label: Some("aerogpu_cmd pipeline layout"),
                        bind_group_layouts: &layout_refs,
                        push_constant_ranges: &[],
                    }),
                )
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
        //
        // Note: depth-only draws bind a dummy color target (see
        // `get_or_create_render_pipeline_for_state`). In that mode we intentionally ignore any
        // explicit NULL RTV slots carried in the D3D state; the render pass color attachment list
        // must match the pipeline target list (1 dummy target).
        let color_views: Vec<Option<wgpu::TextureView>> = if depth_only_pass {
            let ds_id = self
                .state
                .depth_stencil
                .expect("depth_only_pass implies depth_stencil.is_some()");
            let (ds_w, ds_h) = {
                let tex = self
                    .resources
                    .textures
                    .get(&ds_id)
                    .ok_or_else(|| anyhow!("unknown depth stencil texture {ds_id}"))?;
                (tex.desc.width, tex.desc.height)
            };
            vec![Some(self.depth_only_dummy_color_view(ds_w, ds_h))]
        } else {
            let mut out = Vec::with_capacity(self.state.render_targets.len());
            for &tex_id in &self.state.render_targets {
                let Some(tex_id) = tex_id else {
                    out.push(None);
                    continue;
                };
                let tex = self
                    .resources
                    .textures
                    .get(&tex_id)
                    .ok_or_else(|| anyhow!("unknown render target texture {tex_id}"))?;
                out.push(Some(
                    tex.texture
                        .create_view(&wgpu::TextureViewDescriptor::default()),
                ));
            }
            out
        };
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
        for view in color_views.iter() {
            let Some(view) = view.as_ref() else {
                color_attachments.push(None);
                continue;
            };
            let (load, store) = if depth_only_pass {
                (wgpu::LoadOp::Clear(wgpu::Color::BLACK), wgpu::StoreOp::Discard)
            } else {
                (wgpu::LoadOp::Load, wgpu::StoreOp::Store)
            };
            color_attachments.push(Some(wgpu::RenderPassColorAttachment {
                view,
                resolve_target: None,
                ops: wgpu::Operations { load, store },
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
            .iter()
            .flatten()
            .next()
            .and_then(|rt| self.resources.textures.get(rt))
            .map(|tex| (tex.desc.width, tex.desc.height))
            .or_else(|| {
                self.state
                    .depth_stencil
                    .and_then(|ds| self.resources.textures.get(&ds))
                    .map(|tex| (tex.desc.width, tex.desc.height))
            });

        // WebGPU dynamic state defaults to a full-target viewport/scissor at render-pass begin. The
        // AeroGPU protocol encodes "reset to default" using degenerate 0-sized state. However, a
        // *valid* viewport/scissor can still be entirely out of bounds; D3D11 would then draw
        // nothing. WebGPU does not allow zero-sized viewports/scissors, so we emulate this by
        // skipping draws while the effective region is empty.
        let mut viewport_empty = false;
        let mut scissor_empty = false;

        if let Some(vp) = self.state.viewport {
            let valid = vp.x.is_finite()
                && vp.y.is_finite()
                && vp.width.is_finite()
                && vp.height.is_finite()
                && vp.min_depth.is_finite()
                && vp.max_depth.is_finite()
                && vp.width > 0.0
                && vp.height > 0.0;

            if valid {
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
                    } else {
                        viewport_empty = true;
                    }
                } else {
                    let mut min_depth = vp.min_depth.clamp(0.0, 1.0);
                    let mut max_depth = vp.max_depth.clamp(0.0, 1.0);
                    if min_depth > max_depth {
                        std::mem::swap(&mut min_depth, &mut max_depth);
                    }
                    pass.set_viewport(vp.x, vp.y, vp.width, vp.height, min_depth, max_depth);
                }
            }
        }

        if let Some((rt_w, rt_h)) = rt_dims {
            if self.state.scissor_enable {
                if let Some(sc) = self.state.scissor {
                    let x = sc.x.min(rt_w);
                    let y = sc.y.min(rt_h);
                    let width = sc.width.min(rt_w.saturating_sub(x));
                    let height = sc.height.min(rt_h.saturating_sub(y));
                    if width > 0 && height > 0 {
                        pass.set_scissor_rect(x, y, width, height);
                    } else {
                        scissor_empty = true;
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
        // NOTE: SRV slots (`t#`) can refer to either a Texture2D SRV or a buffer SRV. These
        // `used_*` masks track the slot indices, not the underlying resource type.
        let mut used_textures_vs = vec![false; DEFAULT_MAX_TEXTURE_SLOTS];
        let mut used_textures_ps = vec![false; DEFAULT_MAX_TEXTURE_SLOTS];
        let mut used_textures_gs = vec![false; DEFAULT_MAX_TEXTURE_SLOTS];
        let mut used_textures_cs = vec![false; DEFAULT_MAX_TEXTURE_SLOTS];
        let mut used_cb_vs = vec![false; DEFAULT_MAX_CONSTANT_BUFFER_SLOTS];
        let mut used_cb_ps = vec![false; DEFAULT_MAX_CONSTANT_BUFFER_SLOTS];
        let mut used_cb_gs = vec![false; DEFAULT_MAX_CONSTANT_BUFFER_SLOTS];
        let mut used_cb_cs = vec![false; DEFAULT_MAX_CONSTANT_BUFFER_SLOTS];
        let mut used_uavs_vs = vec![false; DEFAULT_MAX_UAV_SLOTS];
        let mut used_uavs_ps = vec![false; DEFAULT_MAX_UAV_SLOTS];
        let mut used_uavs_cs = vec![false; DEFAULT_MAX_UAV_SLOTS];
        for (group_index, group_bindings) in pipeline_bindings.group_bindings.iter().enumerate() {
            let stage = group_index_to_stage(group_index as u32)?;
            for binding in group_bindings {
                #[allow(unreachable_patterns)]
                match &binding.kind {
                    crate::BindingKind::ConstantBuffer { slot, .. } => {
                        let slot_usize = *slot as usize;
                        let used = match stage {
                            ShaderStage::Vertex => used_cb_vs.get_mut(slot_usize),
                            ShaderStage::Pixel => used_cb_ps.get_mut(slot_usize),
                            ShaderStage::Compute => used_cb_cs.get_mut(slot_usize),
                            ShaderStage::Geometry | ShaderStage::Hull | ShaderStage::Domain => {
                                used_cb_gs.get_mut(slot_usize)
                            }
                        };
                        if let Some(entry) = used {
                            *entry = true;
                        }
                    }
                    crate::BindingKind::Texture2D { slot } | crate::BindingKind::SrvBuffer { slot } => {
                        let slot_usize = *slot as usize;
                        let used = match stage {
                            ShaderStage::Vertex => used_textures_vs.get_mut(slot_usize),
                            ShaderStage::Pixel => used_textures_ps.get_mut(slot_usize),
                            ShaderStage::Compute => used_textures_cs.get_mut(slot_usize),
                            ShaderStage::Geometry | ShaderStage::Hull | ShaderStage::Domain => {
                                used_textures_gs.get_mut(slot_usize)
                            }
                        };
                        if let Some(entry) = used {
                            *entry = true;
                        }
                    }
                    crate::BindingKind::UavBuffer { slot } => {
                        let slot_usize = *slot as usize;
                        let used = match stage {
                            ShaderStage::Vertex => used_uavs_vs.get_mut(slot_usize),
                            ShaderStage::Pixel => used_uavs_ps.get_mut(slot_usize),
                            ShaderStage::Compute => used_uavs_cs.get_mut(slot_usize),
                            ShaderStage::Geometry | ShaderStage::Hull | ShaderStage::Domain => None,
                        };
                        if let Some(entry) = used {
                            *entry = true;
                        }
                    }
                    crate::BindingKind::Sampler { .. } => {}
                    // Forward-compat: fall back to binding-number range classification for any new
                    // `BindingKind` variants (e.g. future UAV textures).
                    _ => {
                        let binding_num = binding.binding;
                        if binding_num >= BINDING_BASE_UAV {
                            let slot_usize = binding_num.saturating_sub(BINDING_BASE_UAV) as usize;
                            let used = match stage {
                                ShaderStage::Vertex => used_uavs_vs.get_mut(slot_usize),
                                ShaderStage::Pixel => used_uavs_ps.get_mut(slot_usize),
                                ShaderStage::Compute => used_uavs_cs.get_mut(slot_usize),
                                ShaderStage::Geometry | ShaderStage::Hull | ShaderStage::Domain => None,
                            };
                            if let Some(entry) = used {
                                *entry = true;
                            }
                        } else if binding_num >= BINDING_BASE_TEXTURE && binding_num < BINDING_BASE_SAMPLER {
                            let slot_usize =
                                binding_num.saturating_sub(BINDING_BASE_TEXTURE) as usize;
                            let used = match stage {
                                ShaderStage::Vertex => used_textures_vs.get_mut(slot_usize),
                                ShaderStage::Pixel => used_textures_ps.get_mut(slot_usize),
                                ShaderStage::Compute => used_textures_cs.get_mut(slot_usize),
                                ShaderStage::Geometry | ShaderStage::Hull | ShaderStage::Domain => None,
                            };
                            if let Some(entry) = used {
                                *entry = true;
                            }
                        } else if binding_num < BINDING_BASE_TEXTURE {
                            let slot_usize = binding_num as usize;
                            let used = match stage {
                                ShaderStage::Vertex => used_cb_vs.get_mut(slot_usize),
                                ShaderStage::Pixel => used_cb_ps.get_mut(slot_usize),
                                ShaderStage::Compute => used_cb_cs.get_mut(slot_usize),
                                ShaderStage::Geometry | ShaderStage::Hull | ShaderStage::Domain => None,
                            };
                            if let Some(entry) = used {
                                *entry = true;
                            }
                        }
                    }
                };
            }
        }

        // Tracks whether any previous draw in this render pass actually used the legacy constants
        // uniform buffer for a given stage. If it has, we cannot safely apply
        // `SET_SHADER_CONSTANTS_F` via `queue.write_buffer` without reordering it ahead of the
        // earlier draw commands.
        let mut legacy_constants_used = [false; 4];
        for &handle in render_targets.iter().flatten() {
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
                | OPCODE_SET_SHADER_RESOURCE_BUFFERS
                | OPCODE_SET_UNORDERED_ACCESS_BUFFERS
                | OPCODE_CLEAR
                | OPCODE_NOP
                | OPCODE_DEBUG_MARKER => {}
                _ => break, // leave the opcode for the outer loop
            }

            // Geometry/tessellation emulation requires a compute prepass, which cannot run inside
            // an active render pass. End the current pass and leave the draw for the outer loop.
            if (opcode == OPCODE_DRAW || opcode == OPCODE_DRAW_INDEXED)
                && self.gs_hs_ds_emulation_required()
            {
                break;
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
                // `struct aerogpu_cmd_bind_shaders` (24 bytes) with optional append-only extension
                // `{gs,hs,ds}`.
                let (cmd, ex) = match decode_cmd_bind_shaders_payload_le(cmd_bytes) {
                    Ok(v) => v,
                    Err(_) => break,
                };
                let next_vs = if cmd.vs == 0 { None } else { Some(cmd.vs) };
                let next_ps = if cmd.ps == 0 { None } else { Some(cmd.ps) };
                let (gs, hs, ds) = match ex {
                    Some(ex) => (ex.gs, ex.hs, ex.ds),
                    // Legacy format: treat the old `reserved0` field as `gs`.
                    None => (cmd.gs(), 0, 0),
                };
                let next_gs = if gs == 0 { None } else { Some(gs) };
                let next_hs = if hs == 0 { None } else { Some(hs) };
                let next_ds = if ds == 0 { None } else { Some(ds) };
                if next_vs != self.state.vs
                    || next_ps != self.state.ps
                    || next_gs != self.state.gs
                    || next_hs != self.state.hs
                    || next_ds != self.state.ds
                {
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
                for i in 0..color_count {
                    let tex_id = read_u32_le(cmd_bytes, 16 + i * 4)?;
                    let tex_id = if tex_id == 0 {
                        None
                    } else {
                        Some(
                            self.shared_surfaces
                                .resolve_cmd_handle(tex_id, "SET_RENDER_TARGETS")?,
                        )
                    };
                    colors.push(tex_id);
                }

                let depth_stencil = if depth_stencil == 0 {
                    None
                } else {
                    Some(
                        self.shared_surfaces
                            .resolve_cmd_handle(depth_stencil, "SET_RENDER_TARGETS")?,
                    )
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
                let size_bytes = read_u64_le(cmd_bytes, 24)?;
                if size_bytes != 0 {
                    let handle_raw = read_u32_le(cmd_bytes, 8)?;
                    let handle = self
                        .shared_surfaces
                        .resolve_cmd_handle(handle_raw, "UPLOAD_RESOURCE")?;
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
                let handle = self
                    .shared_surfaces
                    .resolve_handle(read_u32_le(cmd_bytes, 8)?);
                let mut needs_break = false;

                if render_targets
                    .iter()
                    .any(|rt| rt.is_some_and(|rt| rt == handle))
                    || depth_stencil.is_some_and(|ds| ds == handle)
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
                            (ShaderStage::Geometry, &used_cb_gs),
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
                                    .srv_buffer(slot as u32)
                                    .is_some_and(|srv| srv.buffer == handle)
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
                                    .srv_buffer(slot as u32)
                                    .is_some_and(|srv| srv.buffer == handle)
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

                // SRV bindings (`t#`) can point at either textures or buffers. Check the SRV slot
                // table regardless of the underlying resource type.
                if !needs_break
                    && (self.resources.textures.contains_key(&handle)
                        || self.resources.buffers.contains_key(&handle))
                {
                    for (stage, used_slots) in [
                        (ShaderStage::Vertex, &used_textures_vs),
                        (ShaderStage::Pixel, &used_textures_ps),
                        (ShaderStage::Geometry, &used_textures_gs),
                        (ShaderStage::Compute, &used_textures_cs),
                    ] {
                        let stage_bindings = self.bindings.stage(stage);
                        for (slot, used) in used_slots.iter().copied().enumerate() {
                            if !used {
                                continue;
                            }
                            let slot_u32 = slot as u32;
                            if stage_bindings
                                .texture(slot_u32)
                                .is_some_and(|tex| tex.texture == handle)
                                || stage_bindings
                                    .srv_buffer(slot_u32)
                                    .is_some_and(|buf| buf.buffer == handle)
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

                if !needs_break
                    && (self.resources.textures.contains_key(&handle)
                        || self.resources.buffers.contains_key(&handle))
                {
                    for (stage, used_slots) in [
                        (ShaderStage::Vertex, &used_uavs_vs),
                        (ShaderStage::Pixel, &used_uavs_ps),
                        (ShaderStage::Compute, &used_uavs_cs),
                    ] {
                        let stage_bindings = self.bindings.stage(stage);
                        for (slot, used) in used_slots.iter().copied().enumerate() {
                            if !used {
                                continue;
                            }
                            let slot_u32 = slot as u32;
                            if stage_bindings
                                .uav_buffer(slot_u32)
                                .is_some_and(|buf| buf.buffer == handle)
                                || stage_bindings
                                    .uav_texture(slot_u32)
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
                            (ShaderStage::Geometry, &used_cb_gs),
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

                    // Buffer SRVs share the `t#` slot space with textures. A buffer can therefore
                    // be used via `SET_TEXTURE` even though it lives in the buffer resource table.
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
                                let slot_u32 = slot as u32;
                                if stage_bindings
                                    .texture(slot_u32)
                                    .is_some_and(|tex| tex.texture == handle)
                                    || stage_bindings
                                        .srv_buffer(slot_u32)
                                        .is_some_and(|buf| buf.buffer == handle)
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

                    if !needs_break {
                        for (stage, used_slots) in [
                            (ShaderStage::Vertex, &used_uavs_vs),
                            (ShaderStage::Pixel, &used_uavs_ps),
                            (ShaderStage::Compute, &used_uavs_cs),
                        ] {
                            let stage_bindings = self.bindings.stage(stage);
                            for (slot, used) in used_slots.iter().copied().enumerate() {
                                if !used {
                                    continue;
                                }
                                let slot_u32 = slot as u32;
                                if stage_bindings
                                    .uav_buffer(slot_u32)
                                    .is_some_and(|buf| buf.buffer == handle)
                                    || stage_bindings
                                        .uav_texture(slot_u32)
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

                let texture_backing = self
                    .resources
                    .textures
                    .get(&handle)
                    .and_then(|tex| tex.backing);
                if !needs_break && texture_backing.is_some() {
                    if render_targets
                        .iter()
                        .any(|rt| rt.is_some_and(|rt| rt == handle))
                        || depth_stencil.is_some_and(|ds| ds == handle)
                    {
                        needs_break = true;
                    }
                    if !needs_break {
                        for (stage, used_slots) in [
                            (ShaderStage::Vertex, &used_textures_vs),
                            (ShaderStage::Pixel, &used_textures_ps),
                            (ShaderStage::Geometry, &used_textures_gs),
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

                    if !needs_break {
                        for (stage, used_slots) in [
                            (ShaderStage::Vertex, &used_uavs_vs),
                            (ShaderStage::Pixel, &used_uavs_ps),
                            (ShaderStage::Compute, &used_uavs_cs),
                        ] {
                            let stage_bindings = self.bindings.stage(stage);
                                for (slot, used) in used_slots.iter().copied().enumerate() {
                                    if !used {
                                        continue;
                                    }
                                    let slot_u32 = slot as u32;
                                    if stage_bindings
                                        .uav_buffer(slot_u32)
                                        .is_some_and(|buf| buf.buffer == handle)
                                        || stage_bindings
                                            .uav_texture(slot_u32)
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
                let stage_ex = read_u32_le(cmd_bytes, 20)?;
                let Some(stage) =
                    ShaderStage::from_aerogpu_u32_with_stage_ex(stage_raw, stage_ex)
                else {
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
                    let Some(next) = CmdPrimitiveTopology::from_u32(topology_u32) else {
                        break;
                    };
                    // `SET_PRIMITIVE_TOPOLOGY` is pipeline-affecting; it can only be applied inside
                    // an in-flight render pass if it is a no-op for the current pipeline.
                    //
                    // Compare against the WebGPU topology used by the "direct draw" path (e.g.
                    // TriangleFan falls back to TriangleList).
                    let Some(next_key) = next.wgpu_topology_for_direct_draw() else {
                        break;
                    };
                    let Some(cur_key) = self
                        .state
                        .primitive_topology
                        .wgpu_topology_for_direct_draw()
                    else {
                        break;
                    };
                    if next_key != cur_key {
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
                if cmd_bytes.len() >= 24 {
                    let stage_raw = read_u32_le(cmd_bytes, 8)?;
                    let slot = read_u32_le(cmd_bytes, 12)?;
                    let texture = read_u32_le(cmd_bytes, 16)?;
                    let stage_ex = read_u32_le(cmd_bytes, 20)?;
                    if texture != 0 {
                        let texture = self
                            .shared_surfaces
                            .resolve_cmd_handle(texture, "SET_TEXTURE")?;
                        let Some(stage) =
                            ShaderStage::from_aerogpu_u32_with_stage_ex(stage_raw, stage_ex)
                        else {
                            break;
                        };
                        let used_slots = match stage {
                            ShaderStage::Vertex => &used_textures_vs,
                            ShaderStage::Pixel => &used_textures_ps,
                            ShaderStage::Compute => &used_textures_cs,
                            ShaderStage::Geometry | ShaderStage::Hull | ShaderStage::Domain => {
                                &used_textures_gs
                            }
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
                            if let Some(buf) = self.resources.buffers.get(&texture) {
                                if buf.backing.is_some()
                                    && buf.dirty.is_some()
                                    && self.encoder_used_buffers.contains(&texture)
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
                    let buffer = self
                        .shared_surfaces
                        .resolve_cmd_handle(buffer, "SET_VERTEX_BUFFERS")?;

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
                        let buffer = self
                            .shared_surfaces
                            .resolve_cmd_handle(buffer, "SET_INDEX_BUFFER")?;
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
                    let stage_ex = read_u32_le(cmd_bytes, 20)?;
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
                        let Some(stage) =
                            ShaderStage::from_aerogpu_u32_with_stage_ex(stage_raw, stage_ex)
                        else {
                            break;
                        };
                        let used_slots = match stage {
                            ShaderStage::Vertex => &used_cb_vs,
                            ShaderStage::Pixel => &used_cb_ps,
                            ShaderStage::Compute => &used_cb_cs,
                            ShaderStage::Geometry | ShaderStage::Hull | ShaderStage::Domain => &used_cb_gs,
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
                            let buffer = self
                                .shared_surfaces
                                .resolve_cmd_handle(buffer, "SET_CONSTANT_BUFFERS")?;
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
                    if viewport_empty || scissor_empty {
                        // D3D11 allows viewports/scissors that clip the entire render target. wgpu
                        // does not allow zero-sized dynamic state, so we emulate this by treating
                        // the draw as a no-op while the effective region is empty.
                    } else if (self.state.sample_mask & 1) != 0 {
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
                                        dummy_storage: &self.dummy_storage,
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
                        if used_cb_gs.first().is_some_and(|v| *v)
                            && self
                                .bindings
                                .stage(ShaderStage::Geometry)
                                .constant_buffer(0)
                                .is_some_and(|cb| {
                                    cb.buffer == legacy_constants_buffer_id(ShaderStage::Geometry)
                                })
                        {
                            legacy_constants_used
                                [ShaderStage::Geometry.as_bind_group_index() as usize] = true;
                            self.encoder_used_buffers
                                .insert(legacy_constants_buffer_id(ShaderStage::Geometry));
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
                                #[allow(unreachable_patterns)]
                                match &binding.kind {
                                    crate::BindingKind::Texture2D { slot } => {
                                        if let Some(tex) = stage_bindings.texture(*slot) {
                                            self.encoder_used_textures.insert(tex.texture);
                                        }
                                    }
                                    crate::BindingKind::SrvBuffer { slot } => {
                                        if let Some(buf) = stage_bindings.srv_buffer(*slot) {
                                            self.encoder_used_buffers.insert(buf.buffer);
                                        }
                                    }
                                    crate::BindingKind::UavBuffer { slot } => {
                                        if let Some(buf) = stage_bindings.uav_buffer(*slot) {
                                            self.encoder_used_buffers.insert(buf.buffer);
                                        }
                                        if let Some(tex) = stage_bindings.uav_texture(*slot) {
                                            self.encoder_used_textures.insert(tex.texture);
                                        }
                                    }
                                    crate::BindingKind::ConstantBuffer { slot, .. } => {
                                        if let Some(cb) = stage_bindings.constant_buffer(*slot) {
                                            self.encoder_used_buffers.insert(cb.buffer);
                                        }
                                    }
                                    crate::BindingKind::Sampler { .. } => {}
                                    // Forward-compat: fall back to binding-number range inspection.
                                    _ => {
                                        let binding_num = binding.binding;
                                        if binding_num >= BINDING_BASE_UAV {
                                            let slot =
                                                binding_num.saturating_sub(BINDING_BASE_UAV);
                                            if let Some(buf) = stage_bindings.uav_buffer(slot) {
                                                self.encoder_used_buffers.insert(buf.buffer);
                                            }
                                            if let Some(tex) = stage_bindings.uav_texture(slot) {
                                                self.encoder_used_textures.insert(tex.texture);
                                            }
                                        } else if binding_num >= BINDING_BASE_TEXTURE
                                            && binding_num < BINDING_BASE_SAMPLER
                                        {
                                            let slot =
                                                binding_num.saturating_sub(BINDING_BASE_TEXTURE);
                                            if let Some(tex) = stage_bindings.texture(slot) {
                                                self.encoder_used_textures.insert(tex.texture);
                                            }
                                            if let Some(buf) = stage_bindings.srv_buffer(slot) {
                                                self.encoder_used_buffers.insert(buf.buffer);
                                            }
                                        } else if binding_num < BINDING_BASE_TEXTURE {
                                            if let Some(cb) =
                                                stage_bindings.constant_buffer(binding_num)
                                            {
                                                self.encoder_used_buffers.insert(cb.buffer);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                OPCODE_DRAW_INDEXED => {
                    if self.state.index_buffer.is_none() {
                        bail!("DRAW_INDEXED without index buffer");
                    }
                    if viewport_empty || scissor_empty {
                        // See draw path above.
                    } else if (self.state.sample_mask & 1) != 0 {
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
                                        dummy_storage: &self.dummy_storage,
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
                        if used_cb_gs.first().is_some_and(|v| *v)
                            && self
                                .bindings
                                .stage(ShaderStage::Geometry)
                                .constant_buffer(0)
                                .is_some_and(|cb| {
                                    cb.buffer == legacy_constants_buffer_id(ShaderStage::Geometry)
                                })
                        {
                            legacy_constants_used
                                [ShaderStage::Geometry.as_bind_group_index() as usize] = true;
                            self.encoder_used_buffers
                                .insert(legacy_constants_buffer_id(ShaderStage::Geometry));
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
                                #[allow(unreachable_patterns)]
                                match &binding.kind {
                                    crate::BindingKind::Texture2D { slot } => {
                                        if let Some(tex) = stage_bindings.texture(*slot) {
                                            self.encoder_used_textures.insert(tex.texture);
                                        }
                                    }
                                    crate::BindingKind::SrvBuffer { slot } => {
                                        if let Some(buf) = stage_bindings.srv_buffer(*slot) {
                                            self.encoder_used_buffers.insert(buf.buffer);
                                        }
                                    }
                                    crate::BindingKind::UavBuffer { slot } => {
                                        if let Some(buf) = stage_bindings.uav_buffer(*slot) {
                                            self.encoder_used_buffers.insert(buf.buffer);
                                        }
                                        if let Some(tex) = stage_bindings.uav_texture(*slot) {
                                            self.encoder_used_textures.insert(tex.texture);
                                        }
                                    }
                                    crate::BindingKind::ConstantBuffer { slot, .. } => {
                                        if let Some(cb) = stage_bindings.constant_buffer(*slot) {
                                            self.encoder_used_buffers.insert(cb.buffer);
                                        }
                                    }
                                    crate::BindingKind::Sampler { .. } => {}
                                    // Forward-compat: fall back to binding-number range inspection.
                                    _ => {
                                        let binding_num = binding.binding;
                                        if binding_num >= BINDING_BASE_UAV {
                                            let slot =
                                                binding_num.saturating_sub(BINDING_BASE_UAV);
                                            if let Some(buf) = stage_bindings.uav_buffer(slot) {
                                                self.encoder_used_buffers.insert(buf.buffer);
                                            }
                                            if let Some(tex) = stage_bindings.uav_texture(slot) {
                                                self.encoder_used_textures.insert(tex.texture);
                                            }
                                        } else if binding_num >= BINDING_BASE_TEXTURE
                                            && binding_num < BINDING_BASE_SAMPLER
                                        {
                                            let slot =
                                                binding_num.saturating_sub(BINDING_BASE_TEXTURE);
                                            if let Some(tex) = stage_bindings.texture(slot) {
                                                self.encoder_used_textures.insert(tex.texture);
                                            }
                                            if let Some(buf) = stage_bindings.srv_buffer(slot) {
                                                self.encoder_used_buffers.insert(buf.buffer);
                                            }
                                        } else if binding_num < BINDING_BASE_TEXTURE {
                                            if let Some(cb) =
                                                stage_bindings.constant_buffer(binding_num)
                                            {
                                                self.encoder_used_buffers.insert(cb.buffer);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                OPCODE_SET_VIEWPORT => {
                    self.exec_set_viewport(cmd_bytes)?;
                    // WebGPU requires that viewports stay within the render target bounds and have
                    // positive dimensions.
                    //
                    // The AeroGPU protocol uses a degenerate 0x0 viewport (or invalid NaN payload)
                    // to represent "reset to default", so we must explicitly restore the default
                    // viewport mid-render-pass.
                    //
                    // Separately, D3D11 allows a valid viewport that is entirely out of bounds,
                    // which should draw nothing. WebGPU does not accept an empty viewport, so we
                    // track this via `viewport_empty` and skip draws until a non-empty viewport is
                    // set.
                    if let Some((rt_w, rt_h)) = rt_dims {
                        let default_w = rt_w as f32;
                        let default_h = rt_h as f32;

                        let mut reset = true;
                        if let Some(vp) = self.state.viewport {
                            let valid = vp.x.is_finite()
                                && vp.y.is_finite()
                                && vp.width.is_finite()
                                && vp.height.is_finite()
                                && vp.min_depth.is_finite()
                                && vp.max_depth.is_finite();

                            if valid && vp.width > 0.0 && vp.height > 0.0 {
                                reset = false;

                                let max_w = default_w;
                                let max_h = default_h;

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
                                    viewport_empty = false;
                                } else {
                                    viewport_empty = true;
                                }
                            }
                        }

                        if reset && default_w.is_finite() && default_h.is_finite() {
                            // Reset to the default viewport (full render target).
                            viewport_empty = false;
                            pass.set_viewport(0.0, 0.0, default_w, default_h, 0.0, 1.0);
                        }
                    } else if let Some(vp) = self.state.viewport {
                        // No known render target dims (should be rare). Apply the raw viewport only
                        // when it is valid; otherwise treat it as a no-op.
                        if vp.x.is_finite()
                            && vp.y.is_finite()
                            && vp.width.is_finite()
                            && vp.height.is_finite()
                            && vp.min_depth.is_finite()
                            && vp.max_depth.is_finite()
                            && vp.width > 0.0
                            && vp.height > 0.0
                        {
                            let mut min_depth = vp.min_depth.clamp(0.0, 1.0);
                            let mut max_depth = vp.max_depth.clamp(0.0, 1.0);
                            if min_depth > max_depth {
                                std::mem::swap(&mut min_depth, &mut max_depth);
                            }
                            pass.set_viewport(vp.x, vp.y, vp.width, vp.height, min_depth, max_depth);
                            viewport_empty = false;
                        } else {
                            // Cannot apply; clear empty state to avoid spuriously skipping draws.
                            viewport_empty = false;
                        }
                    }
                }
                OPCODE_SET_SCISSOR => {
                    self.exec_set_scissor(cmd_bytes)?;
                    // Similar to viewports, scissor state persists within a render pass.
                    //
                    // The AeroGPU protocol uses a 0x0 rect to encode "scissor disabled", so we must
                    // explicitly restore the full-target scissor mid-pass. Conversely, a valid
                    // scissor rect may still be entirely out of bounds, which should draw nothing;
                    // we emulate that with `scissor_empty` + draw skipping.
                    if let Some((rt_w, rt_h)) = rt_dims {
                        if self.state.scissor_enable {
                            if let Some(sc) = self.state.scissor {
                                let x = sc.x.min(rt_w);
                                let y = sc.y.min(rt_h);
                                let width = sc.width.min(rt_w.saturating_sub(x));
                                let height = sc.height.min(rt_h.saturating_sub(y));
                                if width > 0 && height > 0 {
                                    pass.set_scissor_rect(x, y, width, height);
                                    scissor_empty = false;
                                } else {
                                    scissor_empty = true;
                                }
                            } else {
                                // Scissor disabled via protocol encoding (0x0 rect).
                                scissor_empty = false;
                                pass.set_scissor_rect(0, 0, rt_w, rt_h);
                            }
                        } else {
                            // Scissor test disabled via rasterizer state.
                            scissor_empty = false;
                            pass.set_scissor_rect(0, 0, rt_w, rt_h);
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
                    let stage_ex = read_u32_le(cmd_bytes, 20)?;
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

                    let stage = ShaderStage::from_aerogpu_u32_with_stage_ex(stage_raw, stage_ex)
                        .ok_or_else(|| {
                            anyhow!(
                                "SET_SHADER_CONSTANTS_F: unknown shader stage {stage_raw} (stage_ex={stage_ex})"
                            )
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
                                    scissor_empty = false;
                                } else {
                                    scissor_empty = true;
                                }
                            } else {
                                scissor_empty = false;
                                pass.set_scissor_rect(0, 0, rt_w, rt_h);
                            }
                        } else {
                            scissor_empty = false;
                            pass.set_scissor_rect(0, 0, rt_w, rt_h);
                        }
                    }
                }
                OPCODE_BIND_SHADERS => self.exec_bind_shaders(cmd_bytes)?,
                OPCODE_CREATE_BUFFER => self.exec_create_buffer(cmd_bytes, allocs)?,
                OPCODE_CREATE_TEXTURE2D => self.exec_create_texture2d(cmd_bytes, allocs)?,
                OPCODE_DESTROY_RESOURCE => {
                    unreachable!("DESTROY_RESOURCE cannot be processed inside an active render pass")
                }
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
                    if cmd.size_bytes != 0 {
                        let handle = self
                            .shared_surfaces
                            .resolve_cmd_handle(cmd.resource_handle, "UPLOAD_RESOURCE")?;
                        self.upload_resource_payload(
                            handle,
                            cmd.offset_bytes,
                            cmd.size_bytes,
                            data,
                        )?;
                    }
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
                    if cmd_bytes.len() >= 24 {
                        let stage_raw = read_u32_le(cmd_bytes, 8)?;
                        let slot = read_u32_le(cmd_bytes, 12)?;
                        let texture = read_u32_le(cmd_bytes, 16)?;
                        let stage_ex = read_u32_le(cmd_bytes, 20)?;
                        if texture != 0 {
                            let stage = ShaderStage::from_aerogpu_u32_with_stage_ex(stage_raw, stage_ex)
                                .ok_or_else(|| {
                                    anyhow!(
                                        "SET_TEXTURE: unknown shader stage {stage_raw} (stage_ex={stage_ex})"
                                    )
                                })?;
                            let used_slots = match stage {
                                ShaderStage::Vertex => &used_textures_vs,
                                ShaderStage::Pixel => &used_textures_ps,
                                ShaderStage::Compute => &used_textures_cs,
                                ShaderStage::Geometry | ShaderStage::Hull | ShaderStage::Domain => {
                                    &used_textures_gs
                                }
                            };
                            let slot_usize: usize = slot
                                .try_into()
                                .map_err(|_| anyhow!("SET_TEXTURE: slot out of range"))?;
                            if slot_usize < used_slots.len() && used_slots[slot_usize] {
                                if self
                                    .resources
                                    .buffers
                                    .get(&texture)
                                    .is_some_and(|buf| buf.backing.is_some() && buf.dirty.is_some())
                                {
                                    if !self.encoder_used_buffers.contains(&texture) {
                                        self.upload_buffer_from_guest_memory(
                                            texture, allocs, guest_mem,
                                        )?;
                                    }
                                } else {
                                    let needs_upload = self
                                        .resources
                                        .textures
                                        .get(&texture)
                                        .is_some_and(|tex| tex.dirty && tex.backing.is_some());
                                    if needs_upload && !self.encoder_used_textures.contains(&texture)
                                    {
                                        self.upload_texture_from_guest_memory(
                                            texture, allocs, guest_mem,
                                        )?;
                                    }
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
                        let stage_ex = read_u32_le(cmd_bytes, 20)?;
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
                            if let Some(stage) =
                                ShaderStage::from_aerogpu_u32_with_stage_ex(stage_raw, stage_ex)
                            {
                                let used_slots = match stage {
                                    ShaderStage::Vertex => &used_cb_vs,
                                    ShaderStage::Pixel => &used_cb_ps,
                                    ShaderStage::Compute => &used_cb_cs,
                                    ShaderStage::Geometry | ShaderStage::Hull | ShaderStage::Domain => &used_cb_gs,
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
                OPCODE_SET_SHADER_RESOURCE_BUFFERS => {
                    // Allow first-use uploads of allocation-backed SRV buffers inside a render pass
                    // by reordering the upload ahead of the pass submission. This is only safe
                    // when the buffer has not been referenced by any previously recorded GPU
                    // commands in the current command encoder.
                    if cmd_bytes.len() >= 24 {
                        let stage_raw = read_u32_le(cmd_bytes, 8)?;
                        let start_slot = read_u32_le(cmd_bytes, 12)?;
                        let buffer_count_u32 = read_u32_le(cmd_bytes, 16)?;
                        let stage_ex = read_u32_le(cmd_bytes, 20)?;
                        let buffer_count: usize = buffer_count_u32.try_into().map_err(|_| {
                            anyhow!("SET_SHADER_RESOURCE_BUFFERS: buffer_count out of range")
                        })?;
                        let expected =
                            24usize
                                .checked_add(buffer_count.checked_mul(16).ok_or_else(|| {
                                    anyhow!("SET_SHADER_RESOURCE_BUFFERS: size overflow")
                                })?)
                                .ok_or_else(|| {
                                    anyhow!("SET_SHADER_RESOURCE_BUFFERS: size overflow")
                                })?;
                        if cmd_bytes.len() >= expected {
                            let stage = ShaderStage::from_aerogpu_u32_with_stage_ex(
                                stage_raw, stage_ex,
                            )
                            .ok_or_else(|| {
                                anyhow!(
                                    "SET_SHADER_RESOURCE_BUFFERS: unknown shader stage {stage_raw} (stage_ex={stage_ex})"
                                )
                            })?;
                            let used_slots = match stage {
                                ShaderStage::Vertex => &used_textures_vs,
                                ShaderStage::Pixel => &used_textures_ps,
                                ShaderStage::Compute => &used_textures_cs,
                                ShaderStage::Geometry | ShaderStage::Hull | ShaderStage::Domain => {
                                    &used_textures_cs
                                }
                            };
                            for i in 0..buffer_count {
                                let slot = start_slot.checked_add(i as u32).ok_or_else(|| {
                                    anyhow!("SET_SHADER_RESOURCE_BUFFERS: slot overflow")
                                })?;
                                let slot_usize: usize = slot.try_into().map_err(|_| {
                                    anyhow!("SET_SHADER_RESOURCE_BUFFERS: slot out of range")
                                })?;
                                if slot_usize >= used_slots.len() || !used_slots[slot_usize] {
                                    continue;
                                }

                                let base = 24 + i * 16;
                                let buffer = read_u32_le(cmd_bytes, base)?;
                                if buffer == 0 {
                                    continue;
                                }
                                let needs_upload =
                                    self.resources.buffers.get(&buffer).is_some_and(|buf| {
                                        buf.backing.is_some() && buf.dirty.is_some()
                                    });
                                if needs_upload && !self.encoder_used_buffers.contains(&buffer) {
                                    self.upload_buffer_from_guest_memory(
                                        buffer, allocs, guest_mem,
                                    )?;
                                }
                            }
                        }
                    }
                    self.exec_set_shader_resource_buffers(cmd_bytes)?;
                }
                OPCODE_SET_UNORDERED_ACCESS_BUFFERS => {
                    // Allow first-use uploads of allocation-backed UAV buffers inside a render pass
                    // by reordering the upload ahead of the pass submission. This is only safe
                    // when the buffer has not been referenced by any previously recorded GPU
                    // commands in the current command encoder.
                    if cmd_bytes.len() >= 24 {
                        let stage_raw = read_u32_le(cmd_bytes, 8)?;
                        let start_slot = read_u32_le(cmd_bytes, 12)?;
                        let uav_count_u32 = read_u32_le(cmd_bytes, 16)?;
                        let stage_ex = read_u32_le(cmd_bytes, 20)?;
                        let uav_count: usize = uav_count_u32.try_into().map_err(|_| {
                            anyhow!("SET_UNORDERED_ACCESS_BUFFERS: uav_count out of range")
                        })?;
                        let expected =
                            24usize
                                .checked_add(uav_count.checked_mul(16).ok_or_else(|| {
                                    anyhow!("SET_UNORDERED_ACCESS_BUFFERS: size overflow")
                                })?)
                                .ok_or_else(|| {
                                    anyhow!("SET_UNORDERED_ACCESS_BUFFERS: size overflow")
                                })?;
                        if cmd_bytes.len() >= expected {
                            let stage = ShaderStage::from_aerogpu_u32_with_stage_ex(
                                stage_raw, stage_ex,
                            )
                            .ok_or_else(|| {
                                anyhow!(
                                    "SET_UNORDERED_ACCESS_BUFFERS: unknown shader stage {stage_raw} (stage_ex={stage_ex})"
                                )
                            })?;
                            let used_slots = match stage {
                                ShaderStage::Vertex => &used_uavs_vs,
                                ShaderStage::Pixel => &used_uavs_ps,
                                ShaderStage::Compute => &used_uavs_cs,
                                ShaderStage::Geometry | ShaderStage::Hull | ShaderStage::Domain => {
                                    &used_uavs_cs
                                }
                            };
                            for i in 0..uav_count {
                                let slot = start_slot.checked_add(i as u32).ok_or_else(|| {
                                    anyhow!("SET_UNORDERED_ACCESS_BUFFERS: slot overflow")
                                })?;
                                let slot_usize: usize = slot.try_into().map_err(|_| {
                                    anyhow!("SET_UNORDERED_ACCESS_BUFFERS: slot out of range")
                                })?;
                                if slot_usize >= used_slots.len() || !used_slots[slot_usize] {
                                    continue;
                                }

                                let base = 24 + i * 16;
                                let buffer = read_u32_le(cmd_bytes, base)?;
                                if buffer == 0 {
                                    continue;
                                }
                                let needs_upload =
                                    self.resources.buffers.get(&buffer).is_some_and(|buf| {
                                        buf.backing.is_some() && buf.dirty.is_some()
                                    });
                                if needs_upload && !self.encoder_used_buffers.contains(&buffer) {
                                    self.upload_buffer_from_guest_memory(
                                        buffer, allocs, guest_mem,
                                    )?;
                                }
                            }
                        }
                    }
                    self.exec_set_unordered_access_buffers(cmd_bytes)?;
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
            bail!("CREATE_BUFFER: buffer_handle {buffer_handle} is still in use");
        } else if self.shared_surfaces.refcounts.contains_key(&buffer_handle) {
            // Underlying handles remain reserved as long as any aliases still reference them.
            // If the original handle was destroyed, reject reusing it until the underlying resource
            // is fully released.
            bail!(
                "CREATE_BUFFER: buffer_handle {buffer_handle} is still in use (underlying id kept alive by shared surface aliases)"
            );
        }

        let usage = map_buffer_usage_flags(usage_flags, self.caps.supports_compute);
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
            #[cfg(test)]
            usage,
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
        // WebGPU validation requires `mip_level_count` to be within the possible chain length for
        // the given dimensions. Rejecting this up front avoids wgpu validation panics and prevents
        // pathological mip counts from causing extremely large loops in guest-layout validation.
        let max_dim = width.max(height);
        let max_mip_levels = 32u32.saturating_sub(max_dim.leading_zeros());
        if mip_levels > max_mip_levels {
            bail!(
                "CREATE_TEXTURE2D: mip_levels too large for dimensions (width={width}, height={height}, mip_levels={mip_levels}, max_mip_levels={max_mip_levels})"
            );
        }
        if let Some(&existing) = self.shared_surfaces.handles.get(&texture_handle) {
            if existing != texture_handle {
                bail!(
                    "CREATE_TEXTURE2D: texture_handle {texture_handle} is already an alias (underlying={existing})"
                );
            }
            bail!("CREATE_TEXTURE2D: texture_handle {texture_handle} is still in use");
        } else if self.shared_surfaces.refcounts.contains_key(&texture_handle) {
            // Underlying handles remain reserved as long as any aliases still reference them.
            // If the original handle was destroyed, reject reusing it until the underlying resource
            // is fully released.
            bail!(
                "CREATE_TEXTURE2D: texture_handle {texture_handle} is still in use (underlying id kept alive by shared surface aliases)"
            );
        }

        let format_layout = aerogpu_texture_format_layout(format_u32)?;
        let mut bc_enabled = self
            .device
            .features()
            .contains(wgpu::Features::TEXTURE_COMPRESSION_BC);
        if bc_enabled
            && format_layout.is_block_compressed()
            && !wgpu_bc_texture_dimensions_compatible(width, height, mip_levels)
        {
            // wgpu/WebGPU require block-compressed texture dimensions to be block-aligned (4x4 for
            // BC formats) for mip levels that are at least one full block. Fall back to an RGBA8
            // texture + CPU decompression rather than triggering a wgpu validation panic.
            bc_enabled = false;
        }
        let format = map_aerogpu_texture_format(format_u32, bc_enabled)?;
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
                guest_backing_is_current: false,
                host_shadow: None,
                host_shadow_valid: Vec::new(),
            },
        );
        self.shared_surfaces.register_handle(texture_handle);
        Ok(())
    }

    fn exec_destroy_resource(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        cmd_bytes: &[u8],
    ) -> Result<()> {
        // struct aerogpu_cmd_destroy_resource (16 bytes)
        if cmd_bytes.len() < 16 {
            bail!(
                "DESTROY_RESOURCE: expected at least 16 bytes, got {}",
                cmd_bytes.len()
            );
        }
        let handle = read_u32_le(cmd_bytes, 8)?;

        // wgpu requires resources referenced by an encoder to remain alive until the encoder is
        // finished/submitted. If the guest destroys a resource that was used earlier in the same
        // command stream submission, force a submission boundary so we don't drop wgpu handles
        // while they are still referenced by in-flight commands.
        let underlying = self.shared_surfaces.resolve_handle(handle);
        if self.encoder_has_commands
            && (self.encoder_used_textures.contains(&underlying)
                || self.encoder_used_buffers.contains(&underlying)
                || self.encoder_used_textures.contains(&handle)
                || self.encoder_used_buffers.contains(&handle))
        {
            self.submit_encoder(encoder, "aerogpu_cmd encoder before destroy_resource");
        }

        if let Some((underlying, last_ref)) = self.shared_surfaces.destroy_handle(handle) {
            if last_ref {
                self.resources.buffers.remove(&underlying);
                self.resources.textures.remove(&underlying);
                self.encoder_used_buffers.remove(&underlying);
                self.encoder_used_textures.remove(&underlying);

                // Clean up bindings in state.
                for rt in &mut self.state.render_targets {
                    if rt.is_some_and(|rt| rt == underlying) {
                        *rt = None;
                    }
                }
                while let Some(None) = self.state.render_targets.last() {
                    self.state.render_targets.pop();
                }
                if self.state.depth_stencil == Some(underlying) {
                    self.state.depth_stencil = None;
                }
                for slot in &mut self.state.vertex_buffers {
                    if slot.is_some_and(|b| b.buffer == underlying) {
                        *slot = None;
                    }
                }
                if self
                    .state
                    .index_buffer
                    .is_some_and(|b| b.buffer == underlying)
                {
                    self.state.index_buffer = None;
                }
                for stage in [
                    ShaderStage::Vertex,
                    ShaderStage::Pixel,
                    ShaderStage::Geometry,
                    ShaderStage::Hull,
                    ShaderStage::Domain,
                    ShaderStage::Compute,
                    ShaderStage::Geometry,
                ] {
                    let stage_bindings = self.bindings.stage_mut(stage);
                    stage_bindings.clear_texture_handle(underlying);
                    stage_bindings.clear_uav_texture_handle(underlying);
                    stage_bindings.clear_constant_buffer_handle(underlying);
                    stage_bindings.clear_srv_buffer_handle(underlying);
                    stage_bindings.clear_uav_buffer_handle(underlying);
                }
            }
        } else {
            if self.shared_surfaces.refcounts.contains_key(&handle) {
                // This handle is no longer a live handle (it was already destroyed), but it is
                // still reserved as an underlying shared-surface ID because aliases remain alive.
                //
                // Avoid removing the underlying resource (which is keyed by this numeric handle)
                // on duplicate destroys.
                return Ok(());
            }

            // Untracked handle; treat as a best-effort destroy (robustness).
            self.resources.buffers.remove(&handle);
            self.resources.textures.remove(&handle);
            self.encoder_used_buffers.remove(&handle);
            self.encoder_used_textures.remove(&handle);

            for rt in &mut self.state.render_targets {
                if rt.is_some_and(|rt| rt == handle) {
                    *rt = None;
                }
            }
            while let Some(None) = self.state.render_targets.last() {
                self.state.render_targets.pop();
            }
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
                ShaderStage::Geometry,
                ShaderStage::Hull,
                ShaderStage::Domain,
                ShaderStage::Compute,
                ShaderStage::Geometry,
            ] {
                let stage_bindings = self.bindings.stage_mut(stage);
                stage_bindings.clear_texture_handle(handle);
                stage_bindings.clear_uav_texture_handle(handle);
                stage_bindings.clear_constant_buffer_handle(handle);
                stage_bindings.clear_srv_buffer_handle(handle);
                stage_bindings.clear_uav_buffer_handle(handle);
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
        let handle = self
            .shared_surfaces
            .resolve_cmd_handle(handle, "RESOURCE_DIRTY_RANGE")?;

        if let Some(buf) = self.resources.buffers.get_mut(&handle) {
            // RESOURCE_DIRTY_RANGE is only meaningful for guest-backed resources. For host-owned
            // resources the guest should use UPLOAD_RESOURCE instead; ignore dirty-range signals so
            // a misbehaving guest cannot force unnecessary uploads or invalidate host-side state.
            if buf.backing.is_none() {
                return Ok(());
            }
            let end = offset.saturating_add(size).min(buf.size);
            let start = offset.min(end);
            buf.mark_dirty(start..end);
        } else if let Some(tex) = self.resources.textures.get_mut(&handle) {
            // Same as buffers: ignore dirty-range signals for host-owned textures. In particular,
            // host-owned textures may rely on `host_shadow` for partial UPLOAD_RESOURCE patches, so
            // clearing it here would cause subsequent partial uploads to fail.
            if tex.backing.is_none() {
                return Ok(());
            }
            tex.dirty = true;
            tex.host_shadow = None;
            tex.host_shadow_valid.clear();
            tex.guest_backing_is_current = false;
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
        let offset = cmd.offset_bytes;
        let size = cmd.size_bytes;

        if size == 0 {
            return Ok(());
        }

        let handle = self
            .shared_surfaces
            .resolve_cmd_handle(cmd.resource_handle, "UPLOAD_RESOURCE")?;

        // Preserve command stream ordering relative to any previously encoded GPU work.
        if self.resources.buffers.contains_key(&handle)
            || self.resources.textures.contains_key(&handle)
        {
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

        let Some((desc, format_u32, row_pitch_bytes)) = self
            .resources
            .textures
            .get(&handle)
            .map(|tex| (tex.desc, tex.format_u32, tex.row_pitch_bytes))
        else {
            return Ok(());
        };

        // Texture uploads are expressed as a linear byte range into the guest UMD's canonical
        // packed mip+array layout (see `CREATE_TEXTURE2D`/`compute_guest_texture_layout`).
        let format_layout = aerogpu_texture_format_layout(format_u32)
            .context("UPLOAD_RESOURCE: unknown texture format")?;
        let guest_layout = compute_guest_texture_layout(
            format_u32,
            desc.width,
            desc.height,
            desc.mip_level_count,
            desc.array_layers,
            row_pitch_bytes,
        )
        .context("UPLOAD_RESOURCE: compute guest layout")?;

        let end = offset
            .checked_add(size)
            .ok_or_else(|| anyhow!("UPLOAD_RESOURCE: upload range overflows u64"))?;
        if end > guest_layout.total_size {
            bail!("UPLOAD_RESOURCE: texture upload out of bounds");
        }

        let total_size_usize: usize = guest_layout
            .total_size
            .try_into()
            .map_err(|_| anyhow!("UPLOAD_RESOURCE: texture size out of range"))?;
        let offset_usize: usize = offset
            .try_into()
            .map_err(|_| anyhow!("UPLOAD_RESOURCE: offset out of range"))?;
        let end_usize: usize = end
            .try_into()
            .map_err(|_| anyhow!("UPLOAD_RESOURCE: end out of range"))?;
        let size_usize: usize = size
            .try_into()
            .map_err(|_| anyhow!("UPLOAD_RESOURCE: size_bytes out of range"))?;
        if data.len() != size_usize {
            bail!(
                "UPLOAD_RESOURCE: payload size mismatch (expected {size_usize} bytes, got {})",
                data.len()
            );
        }

        let force_opaque_alpha = aerogpu_format_is_x8(format_u32);
        let b5_format = aerogpu_b5_format(format_u32);
        let mip_extent = |v: u32, level: u32| v.checked_shr(level).unwrap_or(0).max(1);

        let tex = self
            .resources
            .textures
            .get_mut(&handle)
            .ok_or_else(|| anyhow!("UPLOAD_RESOURCE: unknown texture {handle}"))?;

        let subresource_count = desc
            .mip_level_count
            .checked_mul(desc.array_layers)
            .ok_or_else(|| anyhow!("UPLOAD_RESOURCE: subresource count overflows u32"))?;
        let subresource_count_usize: usize = subresource_count
            .try_into()
            .map_err(|_| anyhow!("UPLOAD_RESOURCE: subresource count out of range"))?;

        if tex.host_shadow_valid.len() != subresource_count_usize {
            tex.host_shadow_valid = vec![false; subresource_count_usize];
        }

        // If any intersected subresource is only partially covered, require a valid CPU shadow for
        // that subresource so we don't overwrite bytes we don't have.
        for array_layer in 0..desc.array_layers {
            let layer_offset = guest_layout
                .layer_stride
                .checked_mul(array_layer as u64)
                .ok_or_else(|| anyhow!("UPLOAD_RESOURCE: layer offset overflows u64"))?;
            for mip_level in 0..desc.mip_level_count {
                let mip_offset = *guest_layout
                    .mip_offsets
                    .get(mip_level as usize)
                    .ok_or_else(|| anyhow!("UPLOAD_RESOURCE: missing mip offset"))?;
                let row_pitch = *guest_layout
                    .mip_row_pitches
                    .get(mip_level as usize)
                    .ok_or_else(|| anyhow!("UPLOAD_RESOURCE: missing mip row pitch"))?;
                let rows_u32 = *guest_layout
                    .mip_rows
                    .get(mip_level as usize)
                    .ok_or_else(|| anyhow!("UPLOAD_RESOURCE: missing mip row count"))?;

                let sub_offset = layer_offset
                    .checked_add(mip_offset)
                    .ok_or_else(|| anyhow!("UPLOAD_RESOURCE: subresource offset overflows u64"))?;
                let sub_size = u64::from(row_pitch)
                    .checked_mul(u64::from(rows_u32))
                    .ok_or_else(|| anyhow!("UPLOAD_RESOURCE: subresource size overflows u64"))?;
                let sub_end = sub_offset
                    .checked_add(sub_size)
                    .ok_or_else(|| anyhow!("UPLOAD_RESOURCE: subresource end overflows u64"))?;

                if sub_offset >= end || sub_end <= offset {
                    continue;
                }

                let fully_covered = offset <= sub_offset && end >= sub_end;
                if fully_covered {
                    continue;
                }

                let idx: usize = (array_layer as usize)
                    .checked_mul(desc.mip_level_count as usize)
                    .and_then(|v| v.checked_add(mip_level as usize))
                    .ok_or_else(|| anyhow!("UPLOAD_RESOURCE: subresource index overflow"))?;

                let has_valid_shadow =
                    tex.host_shadow.is_some() && tex.host_shadow_valid.get(idx).copied() == Some(true);
                if !has_valid_shadow {
                    bail!("UPLOAD_RESOURCE: partial texture uploads require a prior full upload");
                }
            }
        }

        if tex
            .host_shadow
            .as_ref()
            .is_some_and(|shadow| shadow.len() != total_size_usize)
        {
            bail!("UPLOAD_RESOURCE: internal shadow size mismatch");
        }

        // Patch payload bytes into the CPU shadow buffer and then upload the affected subresource(s)
        // via WebGPU's 2D copy APIs.
        if tex.host_shadow.is_none() {
            tex.host_shadow = Some(vec![0u8; total_size_usize]);
        }
        let shadow = tex
            .host_shadow
            .as_mut()
            .ok_or_else(|| anyhow!("UPLOAD_RESOURCE: internal error: missing shadow"))?;
        shadow
            .get_mut(offset_usize..end_usize)
            .ok_or_else(|| anyhow!("UPLOAD_RESOURCE: internal shadow slice out of range"))?
            .copy_from_slice(data);

        for array_layer in 0..desc.array_layers {
            let layer_offset = guest_layout
                .layer_stride
                .checked_mul(array_layer as u64)
                .ok_or_else(|| anyhow!("UPLOAD_RESOURCE: layer offset overflows u64"))?;
            for mip_level in 0..desc.mip_level_count {
                let mip_offset = *guest_layout
                    .mip_offsets
                    .get(mip_level as usize)
                    .ok_or_else(|| anyhow!("UPLOAD_RESOURCE: missing mip offset"))?;
                let row_pitch = *guest_layout
                    .mip_row_pitches
                    .get(mip_level as usize)
                    .ok_or_else(|| anyhow!("UPLOAD_RESOURCE: missing mip row pitch"))?;
                let rows_u32 = *guest_layout
                    .mip_rows
                    .get(mip_level as usize)
                    .ok_or_else(|| anyhow!("UPLOAD_RESOURCE: missing mip row count"))?;

                let sub_offset = layer_offset
                    .checked_add(mip_offset)
                    .ok_or_else(|| anyhow!("UPLOAD_RESOURCE: subresource offset overflows u64"))?;
                let sub_size = u64::from(row_pitch)
                    .checked_mul(u64::from(rows_u32))
                    .ok_or_else(|| anyhow!("UPLOAD_RESOURCE: subresource size overflows u64"))?;
                let sub_end = sub_offset
                    .checked_add(sub_size)
                    .ok_or_else(|| anyhow!("UPLOAD_RESOURCE: subresource end overflows u64"))?;

                // Skip subresources that do not intersect the uploaded byte range.
                if sub_offset >= end || sub_end <= offset {
                    continue;
                }

                let idx: usize = (array_layer as usize)
                    .checked_mul(desc.mip_level_count as usize)
                    .and_then(|v| v.checked_add(mip_level as usize))
                    .ok_or_else(|| anyhow!("UPLOAD_RESOURCE: subresource index overflow"))?;

                let sub_start_usize: usize = sub_offset
                    .try_into()
                    .map_err(|_| anyhow!("UPLOAD_RESOURCE: subresource offset out of range"))?;
                let sub_end_usize: usize = sub_end
                    .try_into()
                    .map_err(|_| anyhow!("UPLOAD_RESOURCE: subresource end out of range"))?;
                let sub_bytes = shadow.get(sub_start_usize..sub_end_usize).ok_or_else(|| {
                    anyhow!("UPLOAD_RESOURCE: internal shadow slice out of range")
                })?;

                let subresource = Texture2dSubresourceDesc {
                    desc: tex.desc,
                    mip_level,
                    array_layer,
                };

                if format_layout.is_block_compressed() {
                    // When BC support is disabled, textures are backed by RGBA8 and BC blocks are
                    // CPU-decompressed for upload.
                    let mip_w = mip_extent(desc.width, mip_level);
                    let mip_h = mip_extent(desc.height, mip_level);

                    if bc_block_bytes(tex.desc.format).is_some() {
                        // Upload BC blocks directly.
                        write_texture_subresource_linear(
                            &self.queue,
                            &tex.texture,
                            subresource,
                            row_pitch,
                            sub_bytes,
                            false,
                        )?;
                    } else {
                        // Fall back to RGBA8 + CPU decompression.
                        let tight_bpr = format_layout
                            .bytes_per_row_tight(mip_w)
                            .context("UPLOAD_RESOURCE: compute BC tight bytes_per_row")?;
                        if row_pitch < tight_bpr {
                            bail!("UPLOAD_RESOURCE: BC bytes_per_row too small");
                        }

                        let rows_usize: usize = rows_u32
                            .try_into()
                            .map_err(|_| anyhow!("UPLOAD_RESOURCE: BC rows out of range"))?;
                        let src_bpr_usize: usize = row_pitch.try_into().map_err(|_| {
                            anyhow!("UPLOAD_RESOURCE: BC bytes_per_row out of range")
                        })?;
                        let tight_bpr_usize: usize = tight_bpr.try_into().map_err(|_| {
                            anyhow!("UPLOAD_RESOURCE: BC bytes_per_row out of range")
                        })?;

                        let mut tight = vec![
                            0u8;
                            tight_bpr_usize.checked_mul(rows_usize).ok_or_else(
                                || anyhow!("UPLOAD_RESOURCE: BC data size overflow")
                            )?
                        ];
                        for row in 0..rows_usize {
                            let src_start = row.checked_mul(src_bpr_usize).ok_or_else(|| {
                                anyhow!("UPLOAD_RESOURCE: BC src row offset overflow")
                            })?;
                            let dst_start = row.checked_mul(tight_bpr_usize).ok_or_else(|| {
                                anyhow!("UPLOAD_RESOURCE: BC dst row offset overflow")
                            })?;
                            tight[dst_start..dst_start + tight_bpr_usize].copy_from_slice(
                                sub_bytes
                                    .get(src_start..src_start + tight_bpr_usize)
                                    .ok_or_else(|| {
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
                        let expected_len = mip_w.div_ceil(4) as usize
                            * mip_h.div_ceil(4) as usize
                            * (block_bytes as usize);
                        if tight.len() != expected_len {
                            bail!(
                                "UPLOAD_RESOURCE: BC data length mismatch: expected {expected_len} bytes, got {}",
                                tight.len()
                            );
                        }

                        let rgba = match bc {
                            AerogpuBcFormat::Bc1 => {
                                aero_gpu::decompress_bc1_rgba8(mip_w, mip_h, &tight)
                            }
                            AerogpuBcFormat::Bc2 => {
                                aero_gpu::decompress_bc2_rgba8(mip_w, mip_h, &tight)
                            }
                            AerogpuBcFormat::Bc3 => {
                                aero_gpu::decompress_bc3_rgba8(mip_w, mip_h, &tight)
                            }
                            AerogpuBcFormat::Bc7 => {
                                aero_gpu::decompress_bc7_rgba8(mip_w, mip_h, &tight)
                            }
                        };

                        let rgba_bpr = mip_w.checked_mul(4).ok_or_else(|| {
                            anyhow!("UPLOAD_RESOURCE: decompressed bytes_per_row overflow")
                        })?;
                        write_texture_subresource_linear(
                            &self.queue,
                            &tex.texture,
                            subresource,
                            rgba_bpr,
                            &rgba,
                            false,
                        )?;
                    }
                } else if let Some(b5_format) = b5_format {
                    let mip_w = mip_extent(desc.width, mip_level);
                    let mip_h = mip_extent(desc.height, mip_level);
                    let rgba = expand_b5_texture_to_rgba8(
                        b5_format,
                        mip_w,
                        mip_h,
                        row_pitch,
                        sub_bytes,
                    )?;
                    let rgba_bpr = mip_w.checked_mul(4).ok_or_else(|| {
                        anyhow!("UPLOAD_RESOURCE: B5 expanded bytes_per_row overflow")
                    })?;
                    write_texture_subresource_linear(
                        &self.queue,
                        &tex.texture,
                        subresource,
                        rgba_bpr,
                        &rgba,
                        false,
                    )?;
                } else {
                    write_texture_subresource_linear(
                        &self.queue,
                        &tex.texture,
                        subresource,
                        row_pitch,
                        sub_bytes,
                        force_opaque_alpha,
                    )?;
                }

                tex.host_shadow_valid[idx] = true;
            }
        }

        tex.dirty = false;
        // `UPLOAD_RESOURCE` updates do not update guest memory backing.
        tex.guest_backing_is_current = false;
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
        let dst_buffer_raw = cmd.dst_buffer;
        let src_buffer_raw = cmd.src_buffer;
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

        let dst_buffer = self
            .shared_surfaces
            .resolve_cmd_handle(dst_buffer_raw, "COPY_BUFFER")?;
        let src_buffer = self
            .shared_surfaces
            .resolve_cmd_handle(src_buffer_raw, "COPY_BUFFER")?;

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
        let dst_texture_raw = cmd.dst_texture;
        let src_texture_raw = cmd.src_texture;
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

        let dst_texture = self
            .shared_surfaces
            .resolve_cmd_handle(dst_texture_raw, "COPY_TEXTURE2D")?;
        let src_texture = self
            .shared_surfaces
            .resolve_cmd_handle(src_texture_raw, "COPY_TEXTURE2D")?;

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
            src_backing,
            src_row_pitch_bytes,
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
                    src.backing,
                    src.row_pitch_bytes,
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
            bail!("COPY_TEXTURE2D: WRITEBACK_DST is not supported for dst format {dst_format_u32}");
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

        // Block-compressed textures (BC1/2/3/7) require 4x4 block-aligned copies, except when the
        // region reaches the mip edge (partial blocks are only representable at the edge).
        //
        // IMPORTANT: This validation must be based on the *guest-requested* Aerogpu format, not the
        // mapped host `wgpu::TextureFormat`. When texture compression features are disabled (e.g.
        // wgpu GL backend or `AERO_DISABLE_WGPU_TEXTURE_COMPRESSION=1`), BC textures are
        // represented as RGBA8 host textures, but guest BC copy semantics still apply.
        let guest_format_layout = aerogpu_texture_format_layout(src_format_u32)?;
        let guest_is_bc = guest_format_layout.is_block_compressed();
        if guest_is_bc {
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

        // WebGPU additionally requires BC copy extents to be block-aligned, even for smaller-than-a-
        // block mips. (E.g. a 2x2 mip still uses a 4x4 physical extent.) Keep guest-facing
        // semantics and round up for the host copy.
        let host_is_bc = bc_block_bytes(src_desc.format).is_some();
        let (wgpu_copy_width, wgpu_copy_height) = if host_is_bc {
            (align_to(width, 4)?, align_to(height, 4)?)
        } else {
            (width, height)
        };

        // If the destination is guest-backed and dirty, we need to preserve guest-memory contents
        // that are *not* overwritten by the copy:
        // - within the destination subresource (partial rectangle updates), and
        // - in other mips/array layers (the dirty flag is coarse and we do not track per-subresource
        //   dirtiness).
        //
        // Since COPY_TEXTURE2D clears the destination's dirty marker (to prevent overwriting the
        // copy with stale guest bytes), we must upload from guest memory first unless the copy
        // completely overwrites the entire texture (single-subresource full copy).
        //
        // Note: the texture dirty flag is coarse (we don't track per-mip/per-layer dirtiness), so
        // uploading here refreshes all subresources from guest memory.
        let dst_has_other_subresources =
            dst_desc.mip_level_count != 1 || dst_desc.array_layers != 1;
        let copy_overwrites_entire_subresource =
            dst_x == 0 && dst_y == 0 && width == dst_w && height == dst_h;
        let needs_dst_upload = dst_backing.is_some()
            && dst_dirty
            && (dst_has_other_subresources || !copy_overwrites_entire_subresource);

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

            let host_bytes_per_pixel_u32 = bytes_per_texel(dst_desc.format)?;
            let host_row_bytes_u32 = width
                .checked_mul(host_bytes_per_pixel_u32)
                .ok_or_else(|| anyhow!("COPY_TEXTURE2D: host bytes_per_row overflow"))?;

            // Guest layout may differ from host `wgpu::TextureFormat` for formats that require
            // CPU-side conversions (e.g. B5G6R5/B5G5R5A1 represented as RGBA8 on the host).
            let guest_bytes_per_pixel_u32 = match guest_format_layout {
                AerogpuTextureFormatLayout::Uncompressed { bytes_per_texel } => bytes_per_texel,
                AerogpuTextureFormatLayout::BlockCompressed { .. } => {
                    bail!("COPY_TEXTURE2D: WRITEBACK_DST does not support block-compressed formats")
                }
            };

            let dst_x_bytes = (dst_x as u64)
                .checked_mul(guest_bytes_per_pixel_u32 as u64)
                .ok_or_else(|| anyhow!("COPY_TEXTURE2D: dst_x byte offset overflow"))?;

            let guest_row_bytes_u32 = width
                .checked_mul(guest_bytes_per_pixel_u32)
                .ok_or_else(|| anyhow!("COPY_TEXTURE2D: bytes_per_row overflow"))?;
            let guest_row_bytes = guest_row_bytes_u32 as u64;
            if dst_x_bytes
                .checked_add(guest_row_bytes)
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
                .checked_add(guest_row_bytes)
                .ok_or_else(|| anyhow!("COPY_TEXTURE2D: dst backing end overflow"))?;

            let validate_size = end_offset
                .checked_sub(start_offset)
                .ok_or_else(|| anyhow!("COPY_TEXTURE2D: dst backing size underflow"))?;

            let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
            let padded_bpr = host_row_bytes_u32
                .checked_add(align - 1)
                .map(|v| v / align)
                .and_then(|v| v.checked_mul(align))
                .ok_or_else(|| anyhow!("COPY_TEXTURE2D: padded bytes_per_row overflow"))?;

            let base_gpa = allocs
                .validate_write_range(dst_backing.alloc_id, start_offset, validate_size)
                .context("COPY_TEXTURE2D: WRITEBACK_DST alloc table validation failed")?;

            let transform = if let Some(b5) = aerogpu_b5_format(dst_format_u32) {
                match b5 {
                    AerogpuB5Format::B5G6R5 => TextureWritebackTransform::B5G6R5,
                    AerogpuB5Format::B5G5R5A1 => TextureWritebackTransform::B5G5R5A1,
                }
            } else {
                TextureWritebackTransform::Direct {
                    force_opaque_alpha: aerogpu_format_is_x8(dst_format_u32),
                }
            };

            if matches!(transform, TextureWritebackTransform::Direct { .. })
                && guest_row_bytes_u32 != host_row_bytes_u32
            {
                bail!("COPY_TEXTURE2D: internal error: direct writeback row size mismatch");
            }

            Some((
                TextureWritebackPlan {
                    base_gpa,
                    row_pitch,
                    padded_bytes_per_row: padded_bpr,
                    staging_unpadded_bytes_per_row: host_row_bytes_u32,
                    guest_unpadded_bytes_per_row: guest_row_bytes_u32,
                    height,
                    transform,
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

        let is_bc = host_is_bc;
        let bc_copy_requires_cpu_fallback = is_bc && matches!(self.backend, wgpu::Backend::Gl);

        if bc_copy_requires_cpu_fallback {
            // wgpu's GL backend has historically been unreliable when executing
            // `CommandEncoder::copy_texture_to_texture` for BC-compressed textures.
            //
            // For correctness and determinism we route BC copies through CPU-visible bytes and then
            // re-upload with `Queue::write_texture`. This is only viable when the BC block stream is
            // available on the CPU:
            // - guest-backed sources: authoritative bytes live in guest memory
            // - `UPLOAD_RESOURCE` sources: authoritative bytes live in `host_shadow`
            //
            // If neither is available (GPU-only BC source), we fail fast instead of attempting a
            // backend-dependent compressed copy.

            let block_bytes =
                bc_block_bytes(src_desc.format).expect("bc_copy_requires_cpu_fallback implies BC");

            let src_block_x = src_x / 4;
            let src_block_y = src_y / 4;
            let blocks_w = width.div_ceil(4);
            let blocks_h = height.div_ceil(4);
            let copy_width_texels = blocks_w
                .checked_mul(4)
                .ok_or_else(|| anyhow!("COPY_TEXTURE2D: BC copy width overflows u32"))?;

            let row_bytes = blocks_w
                .checked_mul(block_bytes)
                .ok_or_else(|| anyhow!("COPY_TEXTURE2D: BC row byte size overflow"))?;
            let row_bytes_usize: usize = row_bytes
                .try_into()
                .map_err(|_| anyhow!("COPY_TEXTURE2D: BC row byte size out of range"))?;
            let blocks_h_usize: usize = blocks_h
                .try_into()
                .map_err(|_| anyhow!("COPY_TEXTURE2D: BC block row count out of range"))?;

            // Gather BC blocks for the copy region into a tight per-block-row buffer. We write one
            // block row at a time to avoid wgpu's 256-byte `bytes_per_row` alignment requirement for
            // multi-row writes.
            let region_len = row_bytes_usize
                .checked_mul(blocks_h_usize)
                .ok_or_else(|| anyhow!("COPY_TEXTURE2D: BC region buffer size overflow"))?;
            let mut region_blocks = vec![0u8; region_len];

            // Helper: read BC blocks from a CPU shadow buffer (mip0/layer0 guest-linear layout).
            let mut fill_region_from_shadow = |src_shadow: &[u8]| -> Result<()> {
                // `host_shadow` stores mip0/layer0 in guest linear layout.
                let format_layout = aerogpu_texture_format_layout(src_format_u32)
                    .context("COPY_TEXTURE2D: compute BC shadow format layout")?;
                let src_bytes_per_row = if src_row_pitch_bytes != 0 {
                    src_row_pitch_bytes
                } else {
                    format_layout
                        .bytes_per_row_tight(src_desc.width)
                        .context("COPY_TEXTURE2D: compute BC shadow bytes_per_row")?
                };

                let src_bpr_usize: usize = src_bytes_per_row
                    .try_into()
                    .map_err(|_| anyhow!("COPY_TEXTURE2D: BC shadow bytes_per_row out of range"))?;
                let src_x_bytes: usize =
                    (src_block_x as usize)
                        .checked_mul(block_bytes as usize)
                        .ok_or_else(|| anyhow!("COPY_TEXTURE2D: BC src_x byte offset overflow"))?;
                if src_x_bytes
                    .checked_add(row_bytes_usize)
                    .ok_or_else(|| anyhow!("COPY_TEXTURE2D: BC src row range overflow"))?
                    > src_bpr_usize
                {
                    bail!("COPY_TEXTURE2D: BC shadow row pitch too small for copy region");
                }

                for block_row in 0..blocks_h {
                    let dst_start = (block_row as usize)
                        .checked_mul(row_bytes_usize)
                        .ok_or_else(|| anyhow!("COPY_TEXTURE2D: BC dst offset overflow"))?;
                    let dst_end = dst_start
                        .checked_add(row_bytes_usize)
                        .ok_or_else(|| anyhow!("COPY_TEXTURE2D: BC dst end overflow"))?;
                    let dst_slice = region_blocks
                        .get_mut(dst_start..dst_end)
                        .ok_or_else(|| anyhow!("COPY_TEXTURE2D: BC region buffer too small"))?;

                    let src_row_index: usize = src_block_y
                        .checked_add(block_row)
                        .ok_or_else(|| anyhow!("COPY_TEXTURE2D: BC src row index overflow"))?
                        .try_into()
                        .map_err(|_| anyhow!("COPY_TEXTURE2D: BC src row index out of range"))?;
                    let src_start = src_row_index
                        .checked_mul(src_bpr_usize)
                        .and_then(|v| v.checked_add(src_x_bytes))
                        .ok_or_else(|| anyhow!("COPY_TEXTURE2D: BC shadow src offset overflow"))?;
                    let src_end = src_start
                        .checked_add(row_bytes_usize)
                        .ok_or_else(|| anyhow!("COPY_TEXTURE2D: BC shadow src end overflow"))?;
                    dst_slice.copy_from_slice(src_shadow.get(src_start..src_end).ok_or_else(
                        || anyhow!("COPY_TEXTURE2D: BC shadow too small for copy region"),
                    )?);
                }
                Ok(())
            };

            if let Some(src_backing) = src_backing {
                // Source is guest-backed. Prefer guest memory when it still matches GPU contents,
                // otherwise fall back to `host_shadow` if available.
                let src_backing_is_current = self
                    .resources
                    .textures
                    .get(&src_texture)
                    .map(|t| t.guest_backing_is_current)
                    .unwrap_or(false);

                if src_backing_is_current {
                    let guest_layout = compute_guest_texture_layout(
                        src_format_u32,
                        src_desc.width,
                        src_desc.height,
                        src_desc.mip_level_count,
                        src_desc.array_layers,
                        src_row_pitch_bytes,
                    )
                    .context("COPY_TEXTURE2D: compute guest layout for BC fallback")?;

                    allocs.validate_range(
                        src_backing.alloc_id,
                        src_backing.offset_bytes,
                        guest_layout.total_size,
                    )?;
                    let base_gpa = allocs
                        .gpa(src_backing.alloc_id)?
                        .checked_add(src_backing.offset_bytes)
                        .ok_or_else(|| anyhow!("COPY_TEXTURE2D: src backing GPA overflow"))?;

                    let layer_offset = guest_layout
                        .layer_stride
                        .checked_mul(src_array_layer as u64)
                        .ok_or_else(|| anyhow!("COPY_TEXTURE2D: src layer offset overflow"))?;
                    let mip_offset = *guest_layout
                        .mip_offsets
                        .get(src_mip_level as usize)
                        .ok_or_else(|| anyhow!("COPY_TEXTURE2D: missing src mip offset"))?;
                    let src_row_pitch =
                        *guest_layout
                            .mip_row_pitches
                            .get(src_mip_level as usize)
                            .ok_or_else(|| anyhow!("COPY_TEXTURE2D: missing src mip row pitch"))?;

                    let src_row_pitch_usize: usize = src_row_pitch
                        .try_into()
                        .map_err(|_| anyhow!("COPY_TEXTURE2D: BC src row pitch out of range"))?;
                    let src_x_bytes: usize = (src_block_x as usize)
                        .checked_mul(block_bytes as usize)
                        .ok_or_else(|| anyhow!("COPY_TEXTURE2D: BC src_x byte offset overflow"))?;
                    if src_x_bytes
                        .checked_add(row_bytes_usize)
                        .ok_or_else(|| anyhow!("COPY_TEXTURE2D: BC src row range overflow"))?
                        > src_row_pitch_usize
                    {
                        bail!("COPY_TEXTURE2D: src row pitch too small for BC copy region");
                    }

                    for block_row in 0..blocks_h {
                        let dst_start = (block_row as usize)
                            .checked_mul(row_bytes_usize)
                            .ok_or_else(|| anyhow!("COPY_TEXTURE2D: BC dst offset overflow"))?;
                        let dst_end = dst_start
                            .checked_add(row_bytes_usize)
                            .ok_or_else(|| anyhow!("COPY_TEXTURE2D: BC dst end overflow"))?;
                        let dst_slice = region_blocks
                            .get_mut(dst_start..dst_end)
                            .ok_or_else(|| anyhow!("COPY_TEXTURE2D: BC region buffer too small"))?;

                        let src_row_index =
                            u64::from(src_block_y.checked_add(block_row).ok_or_else(|| {
                                anyhow!("COPY_TEXTURE2D: BC src row index overflow")
                            })?);
                        let src_row_offset = src_row_index
                            .checked_mul(src_row_pitch as u64)
                            .ok_or_else(|| anyhow!("COPY_TEXTURE2D: BC src row offset overflow"))?;
                        let src_addr = base_gpa
                            .checked_add(layer_offset)
                            .and_then(|v| v.checked_add(mip_offset))
                            .and_then(|v| v.checked_add(src_row_offset))
                            .and_then(|v| v.checked_add(src_x_bytes as u64))
                            .ok_or_else(|| anyhow!("COPY_TEXTURE2D: BC src address overflow"))?;

                        guest_mem
                            .read(src_addr, dst_slice)
                            .map_err(anyhow_guest_mem)?;
                    }
                } else {
                    // Guest backing is stale (texture was modified by GPU ops). Require a CPU shadow
                    // copy that tracks GPU contents.
                    if src_mip_level != 0 || src_array_layer != 0 {
                        bail!(
                            "COPY_TEXTURE2D: BC copy on GL requires a CPU shadow for src_mip_level=0/src_array_layer=0 when guest backing is stale (got mip={} layer={}). Consider setting AERO_DISABLE_WGPU_TEXTURE_COMPRESSION=1 (CPU decompression fallback) or using a non-GL backend.",
                            src_mip_level,
                            src_array_layer
                        );
                    }
                    let src_shadow = self
                        .resources
                        .textures
                        .get(&src_texture)
                        .and_then(|t| {
                            let idx = (src_array_layer as usize)
                                .checked_mul(t.desc.mip_level_count as usize)?
                                .checked_add(src_mip_level as usize)?;
                            if t.host_shadow_valid.get(idx).copied().unwrap_or(false) {
                                t.host_shadow.as_deref()
                            } else {
                                None
                            }
                        })
                        .ok_or_else(|| {
                            anyhow!(
                                "COPY_TEXTURE2D: BC copy on GL cannot source blocks from guest memory because src_texture={src_texture} has been modified by GPU operations and its guest backing is stale, and no CPU shadow is available. Consider setting AERO_DISABLE_WGPU_TEXTURE_COMPRESSION=1 (CPU decompression fallback) or using a non-GL backend."
                            )
                        })?;
                    fill_region_from_shadow(src_shadow)?;
                }
            } else {
                // Source is not guest-backed; require a CPU shadow from `UPLOAD_RESOURCE` or other
                // maintained shadow paths.
                if src_mip_level != 0 || src_array_layer != 0 {
                    bail!(
                        "COPY_TEXTURE2D: BC copy on GL requires an UPLOAD_RESOURCE shadow for src_mip_level=0/src_array_layer=0 (got mip={} layer={}). Consider setting AERO_DISABLE_WGPU_TEXTURE_COMPRESSION=1 (CPU decompression fallback) or using a non-GL backend.",
                        src_mip_level,
                        src_array_layer
                    );
                }

                let src_shadow = self
                    .resources
                    .textures
                    .get(&src_texture)
                    .and_then(|t| {
                        let idx = (src_array_layer as usize)
                            .checked_mul(t.desc.mip_level_count as usize)?
                            .checked_add(src_mip_level as usize)?;
                        if t.host_shadow_valid.get(idx).copied().unwrap_or(false) {
                            t.host_shadow.as_deref()
                        } else {
                            None
                        }
                    })
                    .ok_or_else(|| {
                        anyhow!(
                            "COPY_TEXTURE2D: BC copy on GL requires a guest-backed source or a prior full UPLOAD_RESOURCE to establish a CPU shadow (src_texture={src_texture}). If this is a GPU-only BC texture, consider setting AERO_DISABLE_WGPU_TEXTURE_COMPRESSION=1 (CPU decompression fallback) or using a non-GL backend."
                        )
                    })?;
                fill_region_from_shadow(src_shadow)?;
            }

            // We're about to call `queue.write_texture`, which is ordered relative to `queue.submit`.
            // Flush any previously recorded commands so the write cannot reorder ahead of them.
            self.submit_encoder_if_has_commands(
                encoder,
                "aerogpu_cmd encoder before BC COPY_TEXTURE2D CPU fallback",
            );

            {
                let dst =
                    self.resources.textures.get(&dst_texture).ok_or_else(|| {
                        anyhow!("COPY_TEXTURE2D: unknown dst texture {dst_texture}")
                    })?;

                // WebGPU requires BC uploads to use the physical (block-rounded) extents. This is
                // observable for small mips (e.g. a 2x2 mip still occupies a full 4x4 BC block).
                let chunk_height_texels = 4;

                for block_row in 0..blocks_h {
                    let origin_y_texels = dst_y
                        .checked_add(
                            block_row
                                .checked_mul(4)
                                .ok_or_else(|| anyhow!("COPY_TEXTURE2D: dst origin.y overflow"))?,
                        )
                        .ok_or_else(|| anyhow!("COPY_TEXTURE2D: dst origin.y overflow"))?;

                    let src_start = (block_row as usize)
                        .checked_mul(row_bytes_usize)
                        .ok_or_else(|| anyhow!("COPY_TEXTURE2D: BC src row offset overflow"))?;
                    let src_end = src_start
                        .checked_add(row_bytes_usize)
                        .ok_or_else(|| anyhow!("COPY_TEXTURE2D: BC src row end overflow"))?;
                    let row_bytes_slice = region_blocks
                        .get(src_start..src_end)
                        .ok_or_else(|| anyhow!("COPY_TEXTURE2D: BC region buffer too small"))?;

                    self.queue.write_texture(
                        wgpu::ImageCopyTexture {
                            texture: &dst.texture,
                            mip_level: dst_mip_level,
                            origin: wgpu::Origin3d {
                                x: dst_x,
                                y: origin_y_texels,
                                z: dst_array_layer,
                            },
                            aspect: wgpu::TextureAspect::All,
                        },
                        row_bytes_slice,
                        wgpu::ImageDataLayout {
                            offset: 0,
                            bytes_per_row: Some(row_bytes),
                            rows_per_image: Some(1),
                        },
                        wgpu::Extent3d {
                            width: copy_width_texels,
                            height: chunk_height_texels,
                            depth_or_array_layers: 1,
                        },
                    );
                }
            }

            // The destination GPU texture content has changed; discard any pending "dirty" marker
            // that would otherwise cause us to overwrite the copy with stale guest-memory contents.
            if let Some(dst_res) = self.resources.textures.get_mut(&dst_texture) {
                dst_res.dirty = false;
                let dst_guest_backing_was_current = dst_res.guest_backing_is_current;
                // GPU copies do not update guest memory backing.
                dst_res.guest_backing_is_current = false;

                let dst_sub_idx: usize = (dst_array_layer as usize)
                    .checked_mul(dst_desc.mip_level_count as usize)
                    .and_then(|v| v.checked_add(dst_mip_level as usize))
                    .ok_or_else(|| anyhow!("COPY_TEXTURE2D: dst subresource index overflow"))?;
                let dst_shadow_was_valid = dst_res
                    .host_shadow_valid
                    .get(dst_sub_idx)
                    .copied()
                    .unwrap_or(false);
                if let Some(v) = dst_res.host_shadow_valid.get_mut(dst_sub_idx) {
                    *v = false;
                }

                // Maintain/update a CPU shadow so that later BC copies on GL can source blocks
                // without relying on the GPU compressed copy path.
                //
                // This is currently only maintained for mip0/layer0 because the BC shadow read path
                // assumes that layout.
                if dst_mip_level == 0 && dst_array_layer == 0 {
                    let format_layout = aerogpu_texture_format_layout(dst_format_u32).context(
                        "COPY_TEXTURE2D: compute dst format layout for BC shadow update",
                    )?;
                    let dst_bytes_per_row = if dst_row_pitch_bytes != 0 {
                        dst_row_pitch_bytes
                    } else {
                        format_layout
                            .bytes_per_row_tight(dst_desc.width)
                            .context("COPY_TEXTURE2D: compute dst bytes_per_row")?
                    };
                    let dst_rows_u32 = format_layout.rows(dst_desc.height);
                    let dst_bpr_usize: usize = dst_bytes_per_row
                        .try_into()
                        .map_err(|_| anyhow!("COPY_TEXTURE2D: dst bytes_per_row out of range"))?;
                    let dst_rows_usize: usize = dst_rows_u32
                        .try_into()
                        .map_err(|_| anyhow!("COPY_TEXTURE2D: dst rows out of range"))?;
                    let dst_shadow_len =
                        dst_bpr_usize.checked_mul(dst_rows_usize).ok_or_else(|| {
                            anyhow!("COPY_TEXTURE2D: dst shadow size overflows usize")
                        })?;

                    let full_copy = dst_x == 0
                        && dst_y == 0
                        && width == dst_desc.width
                        && height == dst_desc.height;

                    let dst_guest_layout = compute_guest_texture_layout(
                        dst_format_u32,
                        dst_desc.width,
                        dst_desc.height,
                        dst_desc.mip_level_count,
                        dst_desc.array_layers,
                        dst_row_pitch_bytes,
                    )
                    .context("COPY_TEXTURE2D: compute guest layout for BC shadow update")?;
                    let total_shadow_len_usize: usize = dst_guest_layout
                        .total_size
                        .try_into()
                        .map_err(|_| anyhow!("COPY_TEXTURE2D: dst shadow size out of range"))?;
                    let subresource_count_usize: usize = (dst_desc.mip_level_count as usize)
                        .checked_mul(dst_desc.array_layers as usize)
                        .ok_or_else(|| anyhow!("COPY_TEXTURE2D: dst subresource count overflow"))?;
                    if dst_res.host_shadow_valid.len() != subresource_count_usize {
                        dst_res.host_shadow_valid = vec![false; subresource_count_usize];
                    }

                    let layer_offset = dst_guest_layout
                        .layer_stride
                        .checked_mul(dst_array_layer as u64)
                        .ok_or_else(|| anyhow!("COPY_TEXTURE2D: dst layer offset overflow"))?;
                    let mip_offset = *dst_guest_layout
                        .mip_offsets
                        .get(dst_mip_level as usize)
                        .ok_or_else(|| anyhow!("COPY_TEXTURE2D: missing dst mip offset"))?;
                    let dst_shadow_base = layer_offset
                        .checked_add(mip_offset)
                        .ok_or_else(|| anyhow!("COPY_TEXTURE2D: dst shadow base overflow"))?;
                    let dst_shadow_base_usize: usize = dst_shadow_base
                        .try_into()
                        .map_err(|_| anyhow!("COPY_TEXTURE2D: dst shadow base out of range"))?;
                    let dst_shadow_end = dst_shadow_base_usize
                        .checked_add(dst_shadow_len)
                        .ok_or_else(|| anyhow!("COPY_TEXTURE2D: dst shadow range overflow"))?;
                    if dst_shadow_end > total_shadow_len_usize {
                        bail!("COPY_TEXTURE2D: dst shadow range out of bounds");
                    }

                    if dst_res
                        .host_shadow
                        .as_ref()
                        .is_some_and(|shadow| shadow.len() != total_shadow_len_usize)
                    {
                        dst_res.host_shadow = None;
                        dst_res.host_shadow_valid.clear();
                    }

                    let mut seeded_shadow = false;
                    if dst_res.host_shadow.is_none() {
                        if full_copy {
                            dst_res.host_shadow = Some(vec![0u8; total_shadow_len_usize]);
                            dst_res.host_shadow_valid = vec![false; subresource_count_usize];
                        } else if dst_res.backing.is_some() && dst_guest_backing_was_current {
                            // Seed a shadow copy from guest memory (which we believe still matches
                            // GPU contents) so we can patch in the copied blocks.
                            let backing = dst_res.backing.expect("checked backing.is_some above");
                            let shadow_len_u64: u64 = dst_shadow_len.try_into().map_err(|_| {
                                anyhow!("COPY_TEXTURE2D: dst shadow size out of range")
                            })?;
                            let backing_offset = backing
                                .offset_bytes
                                .checked_add(dst_shadow_base)
                                .ok_or_else(|| anyhow!("COPY_TEXTURE2D: dst backing offset overflow"))?;
                            allocs.validate_range(backing.alloc_id, backing_offset, shadow_len_u64)?;
                            let base_gpa = allocs
                                .gpa(backing.alloc_id)?
                                .checked_add(backing_offset)
                                .ok_or_else(|| anyhow!("COPY_TEXTURE2D: dst backing GPA overflow"))?;

                            let mut shadow = vec![0u8; total_shadow_len_usize];
                            for row in 0..dst_rows_u32 {
                                let row_offset = (row as u64)
                                    .checked_mul(dst_bytes_per_row as u64)
                                    .ok_or_else(|| {
                                        anyhow!("COPY_TEXTURE2D: dst shadow row offset overflow")
                                    })?;
                                let src_addr =
                                    base_gpa.checked_add(row_offset).ok_or_else(|| {
                                        anyhow!("COPY_TEXTURE2D: dst shadow src address overflow")
                                    })?;
                                let start = dst_shadow_base_usize
                                    .checked_add(
                                        (row as usize).checked_mul(dst_bpr_usize).ok_or_else(|| {
                                            anyhow!("COPY_TEXTURE2D: dst shadow dst offset overflow")
                                        })?,
                                    )
                                    .ok_or_else(|| anyhow!("COPY_TEXTURE2D: dst shadow dst offset overflow"))?;
                                let end = start.checked_add(dst_bpr_usize).ok_or_else(|| {
                                    anyhow!("COPY_TEXTURE2D: dst shadow dst end overflow")
                                })?;
                                guest_mem
                                    .read(
                                        src_addr,
                                        shadow.get_mut(start..end).ok_or_else(|| {
                                            anyhow!("COPY_TEXTURE2D: dst shadow buffer too small")
                                        })?,
                                    )
                                    .map_err(anyhow_guest_mem)?;
                            }

                            dst_res.host_shadow = Some(shadow);
                            dst_res.host_shadow_valid = vec![false; subresource_count_usize];
                            seeded_shadow = true;
                        }
                    }

                    if let Some(shadow) = dst_res.host_shadow.as_mut() {
                        if shadow.len() != total_shadow_len_usize {
                            dst_res.host_shadow = None;
                            dst_res.host_shadow_valid.clear();
                        } else {
                            let dst_block_x = dst_x / 4;
                            let dst_block_y = dst_y / 4;
                            let dst_x_bytes: usize = (dst_block_x as usize)
                                .checked_mul(block_bytes as usize)
                                .ok_or_else(|| anyhow!("COPY_TEXTURE2D: dst_x byte offset overflow"))?;
                            if dst_x_bytes.checked_add(row_bytes_usize).ok_or_else(|| {
                                anyhow!("COPY_TEXTURE2D: dst shadow row range overflow")
                            })? > dst_bpr_usize
                            {
                                dst_res.host_shadow = None;
                                dst_res.host_shadow_valid.clear();
                            } else {
                                for block_row in 0..blocks_h {
                                    let src_start = (block_row as usize)
                                        .checked_mul(row_bytes_usize)
                                        .ok_or_else(|| {
                                            anyhow!("COPY_TEXTURE2D: BC shadow src row offset overflow")
                                        })?;
                                    let src_end = src_start
                                        .checked_add(row_bytes_usize)
                                        .ok_or_else(|| {
                                            anyhow!("COPY_TEXTURE2D: BC shadow src row end overflow")
                                        })?;

                                    let dst_row_index: usize = dst_block_y
                                        .checked_add(block_row)
                                        .ok_or_else(|| {
                                            anyhow!("COPY_TEXTURE2D: BC shadow dst row index overflow")
                                        })?
                                        .try_into()
                                        .map_err(|_| {
                                            anyhow!("COPY_TEXTURE2D: BC shadow dst row index out of range")
                                        })?;
                                    let dst_start = dst_shadow_base_usize
                                        .checked_add(
                                            dst_row_index
                                                .checked_mul(dst_bpr_usize)
                                                .and_then(|v| v.checked_add(dst_x_bytes))
                                                .ok_or_else(|| {
                                                    anyhow!(
                                                        "COPY_TEXTURE2D: BC shadow dst row offset overflow"
                                                    )
                                                })?,
                                        )
                                        .ok_or_else(|| anyhow!("COPY_TEXTURE2D: BC shadow dst row offset overflow"))?;
                                    let dst_end = dst_start
                                        .checked_add(row_bytes_usize)
                                        .ok_or_else(|| anyhow!("COPY_TEXTURE2D: BC shadow dst row end overflow"))?;

                                    shadow
                                        .get_mut(dst_start..dst_end)
                                        .ok_or_else(|| anyhow!("COPY_TEXTURE2D: BC shadow dst buffer too small"))?
                                        .copy_from_slice(
                                            region_blocks.get(src_start..src_end).ok_or_else(
                                                || anyhow!("COPY_TEXTURE2D: BC region buffer too small"),
                                            )?,
                                        );
                                }

                                let can_mark_valid = full_copy || dst_shadow_was_valid || seeded_shadow;
                                if can_mark_valid {
                                    if let Some(v) = dst_res.host_shadow_valid.get_mut(dst_sub_idx) {
                                        *v = true;
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // If the destination is guest-backed we intentionally leave its backing store stale;
            // later implicit uploads must not overwrite GPU-produced contents.
            return Ok(());
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
                    width: wgpu_copy_width,
                    height: wgpu_copy_height,
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
                        width: wgpu_copy_width,
                        height: wgpu_copy_height,
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
            // The GPU copy modifies the destination subresource; invalidate any CPU shadow validity
            // so later partial `UPLOAD_RESOURCE` patches don't accidentally overwrite bytes we
            // don't have.
            if !dst.host_shadow_valid.is_empty() {
                let idx = (dst_array_layer as usize)
                    .checked_mul(dst.desc.mip_level_count as usize)
                    .and_then(|v| v.checked_add(dst_mip_level as usize));
                match idx.and_then(|idx| dst.host_shadow_valid.get_mut(idx)) {
                    Some(v) => *v = false,
                    None => dst.host_shadow_valid.clear(),
                }
            }
            // GPU copies do not update guest memory backing.
            dst.guest_backing_is_current = false;
        }

        Ok(())
    }

    fn exec_create_shader_dxbc(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        let (cmd, dxbc_bytes) = decode_cmd_create_shader_dxbc_payload_le(cmd_bytes)
            .map_err(|e| anyhow!("CREATE_SHADER_DXBC: invalid payload: {e:?}"))?;
        let shader_handle = cmd.shader_handle;
        let stage_raw = cmd.stage;
        let stage_ex = cmd.reserved0;
        let dxbc = DxbcFile::parse(dxbc_bytes).context("DXBC parse failed")?;
        let program = Sm4Program::parse_from_dxbc(&dxbc).context("DXBC decode failed")?;

        // SM5 geometry shaders can emit to multiple output streams via `emit_stream` / `cut_stream`.
        // Aero's initial GS bring-up only targets stream 0, so reject any shaders that use a
        // non-zero stream index with a clear diagnostic.
        //
        // Validate before stage dispatch so the policy is enforced even for GS/HS/DS shaders that
        // are currently accepted-but-ignored by the WebGPU backend.
        validate_sm5_gs_streams(&program)?;
        let parsed_stage = match program.stage {
            crate::ShaderStage::Vertex => ShaderStage::Vertex,
            crate::ShaderStage::Pixel => ShaderStage::Pixel,
            crate::ShaderStage::Compute => ShaderStage::Compute,
            crate::ShaderStage::Geometry => ShaderStage::Geometry,
            crate::ShaderStage::Hull => ShaderStage::Hull,
            crate::ShaderStage::Domain => ShaderStage::Domain,
            other => bail!("CREATE_SHADER_DXBC: unsupported DXBC shader stage {other:?}"),
        };

        let stage =
            ShaderStage::from_aerogpu_u32_with_stage_ex(stage_raw, stage_ex).ok_or_else(|| {
                anyhow!("CREATE_SHADER_DXBC: unknown shader stage {stage_raw} (stage_ex={stage_ex})")
            })?;
        if parsed_stage != stage {
            bail!("CREATE_SHADER_DXBC: stage mismatch (cmd={stage:?}, dxbc={parsed_stage:?})");
        }

        if stage == ShaderStage::Geometry {
            let instance_count = sm5_gs_instance_count(&program).unwrap_or(1).max(1);
            self.resources
                .gs_shaders
                .insert(shader_handle, GsShaderMetadata { instance_count });
        }

        let dxbc_hash_fnv1a64 = fnv1a64(dxbc_bytes);

        let signatures = parse_signatures(&dxbc).context("parse DXBC signatures")?;

        // Future-proofing for SM5 geometry-shader stream semantics:
        // - The DXBC signature entries include a `stream` field (used by GS multi-stream output /
        //   stream-out).
        // - Our rasterization path currently only supports stream 0. If a shader declares outputs
        //   on non-zero streams, treating them as stream 0 would silently misrender.
        //
        // Fail fast with a clear error so we never rasterize with the wrong stream mapping.
        if matches!(stage, ShaderStage::Vertex | ShaderStage::Pixel) {
            if let Some(osgn) = signatures.osgn.as_ref() {
                for p in &osgn.parameters {
                    if p.stream != 0 {
                        bail!(
                            "CREATE_SHADER_DXBC: output signature parameter {}{} (r{}) is declared on stream {} (only stream 0 is supported)",
                            p.semantic_name,
                            p.semantic_index,
                            p.register,
                            p.stream
                        );
                    }
                }
            }
        }

        // Compute-stage DXBC frequently omits signature chunks entirely. The signature-driven
        // translator can still handle compute shaders, so only require ISGN/OSGN for VS/PS.
        let signature_driven =
            stage == ShaderStage::Compute || (signatures.isgn.is_some() && signatures.osgn.is_some());

        let entry_point = match stage {
            ShaderStage::Vertex => "vs_main",
            ShaderStage::Pixel => "fs_main",
            ShaderStage::Compute => "cs_main",
            // Geometry/tessellation stages are emulated via compute.
            ShaderStage::Geometry => "gs_main",
            ShaderStage::Hull => "hs_main",
            ShaderStage::Domain => "ds_main",
        };

        let (wgsl, reflection) = match stage {
            ShaderStage::Geometry | ShaderStage::Hull | ShaderStage::Domain => {
                // Placeholder geometry/tessellation path: we currently accept and store these DXBC
                // shaders (via the `stage_ex` ABI extension) but cannot translate them yet.
                //
                // Compile a minimal compute shader so the pipeline cache can create a shader module
                // and future code can bind the shader by handle.
                (
                    format!("@compute @workgroup_size(1)\nfn {entry_point}() {{}}\n"),
                    ShaderReflection::default(),
                )
            }
            ShaderStage::Vertex | ShaderStage::Pixel | ShaderStage::Compute => {
                if signature_driven {
                    let translated =
                        try_translate_sm4_signature_driven(&dxbc, &program, &signatures)?;
                    (translated.wgsl, translated.reflection)
                } else {
                    (
                        crate::wgsl_bootstrap::translate_sm4_to_wgsl_bootstrap(&program)
                            .context("DXBC->WGSL translation failed")?
                            .wgsl,
                        ShaderReflection::default(),
                    )
                }
            }
        };

        let (hash, _module) = self.pipeline_cache.get_or_create_shader_module(
            &self.device,
            map_pipeline_cache_stage(stage),
            &wgsl,
            Some("aerogpu_cmd shader"),
        );

        let vs_input_signature = if stage == ShaderStage::Vertex {
            if signature_driven {
                let module =
                    crate::sm4::decode_program(&program).context("decode SM4/5 token stream")?;
                extract_vs_input_signature_unique_locations(&signatures, &module)
                    .context("extract VS input signature")?
            } else {
                extract_vs_input_signature(&signatures).context("extract VS input signature")?
            }
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

        self.resources.shaders.insert(shader_handle, shader);
        Ok(())
    }

    #[cfg(target_arch = "wasm32")]
    async fn exec_create_shader_dxbc_persistent(&mut self, cmd_bytes: &[u8]) -> Result<()> {
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

        let flags = self.persistent_shader_cache_flags.clone();

        // At most one invalidation+retranslate retry for corruption defense.
        let mut invalidated_once = false;

        loop {
            let (artifact, source) = self
                .persistent_shader_cache
                .get_or_translate_with_source(dxbc_bytes, flags.clone(), || async {
                    // Translation path (only on miss).
                    let dxbc = DxbcFile::parse(dxbc_bytes)
                        .context("DXBC parse failed")
                        .map_err(|e| e.to_string())?;
                    let program = Sm4Program::parse_from_dxbc(&dxbc)
                        .context("DXBC decode failed")
                        .map_err(|e| e.to_string())?;

                    // Geometry/hull/domain stages are not represented in the AeroGPU command stream
                    // (WebGPU does not expose them), but Win7 D3D11 applications may still create
                    // these shaders. Persist an explicit "ignored" result so we can skip re-parsing
                    // them on subsequent runs.
                    let parsed_stage = match program.stage {
                        crate::ShaderStage::Vertex => Some(ShaderStage::Vertex),
                        crate::ShaderStage::Pixel => Some(ShaderStage::Pixel),
                        crate::ShaderStage::Compute => Some(ShaderStage::Compute),
                        crate::ShaderStage::Geometry
                        | crate::ShaderStage::Hull
                        | crate::ShaderStage::Domain => None,
                        other => {
                            return Err(format!(
                                "CREATE_SHADER_DXBC: unsupported DXBC shader stage {other:?}"
                            ));
                        }
                    };

                    if let Some(parsed_stage) = parsed_stage {
                        if parsed_stage != stage {
                            return Err(format!(
                                "CREATE_SHADER_DXBC: stage mismatch (cmd={stage:?}, dxbc={parsed_stage:?})"
                            ));
                        }
                    }

                    if parsed_stage.is_none() {
                        return Ok(PersistedShaderArtifact {
                            wgsl: String::new(),
                            stage: PersistedShaderStage::Ignored,
                            bindings: Vec::new(),
                            vs_input_signature: Vec::new(),
                        });
                    }

                    let signatures = parse_signatures(&dxbc)
                        .context("parse DXBC signatures")
                        .map_err(|e| e.to_string())?;
                    let signature_driven = signatures.isgn.is_some() && signatures.osgn.is_some();

                    let (wgsl, reflection) = if signature_driven {
                        let translated =
                            try_translate_sm4_signature_driven(&dxbc, &program, &signatures)
                                .map_err(|e| e.to_string())?;
                        (translated.wgsl, translated.reflection)
                    } else {
                        (
                            crate::wgsl_bootstrap::translate_sm4_to_wgsl_bootstrap(&program)
                                .map_err(|e| e.to_string())?
                                .wgsl,
                            ShaderReflection::default(),
                        )
                    };

                    let bindings: Vec<PersistedBinding> = reflection
                        .bindings
                        .iter()
                        .map(PersistedBinding::from_binding)
                        .collect();

                    let vs_input_signature = if stage == ShaderStage::Vertex {
                        if signature_driven {
                            let module = program
                                .decode()
                                .context("decode SM4/5 token stream")
                                .map_err(|e| e.to_string())?;
                            extract_vs_input_signature_unique_locations(&signatures, &module)
                                .context("extract VS input signature")
                                .map_err(|e| e.to_string())?
                        } else {
                            extract_vs_input_signature(&signatures)
                                .context("extract VS input signature")
                                .map_err(|e| e.to_string())?
                        }
                    } else {
                        Vec::new()
                    };
                    let vs_input_signature: Vec<PersistedVsInputSignatureElement> =
                        vs_input_signature
                            .iter()
                            .map(PersistedVsInputSignatureElement::from_element)
                            .collect();

                    Ok(PersistedShaderArtifact {
                        wgsl,
                        stage: PersistedShaderStage::from_stage(stage),
                        bindings,
                        vs_input_signature,
                    })
                })
                .await
                .map_err(|err| anyhow!(err.as_string().unwrap_or_else(|| format!("{err:?}"))))?;

            let Some(artifact_stage) = artifact.stage.to_stage() else {
                // Ignored shader stage (GS/HS/DS): accept create but do not track a shader resource.
                return Ok(());
            };

            if artifact_stage != stage {
                if !invalidated_once {
                    invalidated_once = true;
                    let _ = self
                        .persistent_shader_cache
                        .invalidate(dxbc_bytes, flags.clone())
                        .await;
                    continue;
                }
                // Fall back to the non-persistent path to avoid looping forever.
                return self.exec_create_shader_dxbc(cmd_bytes);
            }

            // Optional: validate cached WGSL on persistent hit to guard against corruption/staleness.
            if source == ShaderCacheSource::Persistent {
                self.device.push_error_scope(wgpu::ErrorFilter::Validation);
            }

            let PersistedShaderArtifact {
                wgsl,
                bindings,
                vs_input_signature,
                ..
            } = artifact;

            let reflection = ShaderReflection {
                inputs: Vec::new(),
                outputs: Vec::new(),
                bindings: bindings.iter().map(PersistedBinding::to_binding).collect(),
            };
            let vs_input_signature: Vec<VsInputSignatureElement> = vs_input_signature
                .into_iter()
                .map(PersistedVsInputSignatureElement::to_element)
                .collect();

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

            if source == ShaderCacheSource::Persistent {
                self.poll();
                let err = self.device.pop_error_scope().await;
                if let Some(err) = err {
                    if !invalidated_once {
                        invalidated_once = true;
                        let _ = self
                            .persistent_shader_cache
                            .invalidate(dxbc_bytes, flags.clone())
                            .await;
                        continue;
                    }
                    return Err(anyhow!(
                        "cached WGSL failed wgpu validation for shader {shader_handle}: {err:?}"
                    ));
                }
            }

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
            self.resources.shaders.insert(shader_handle, shader);
            return Ok(());
        }
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
        self.resources.gs_shaders.remove(&shader_handle);
        Ok(())
    }

    fn exec_bind_shaders(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        let (cmd, ex) = decode_cmd_bind_shaders_payload_le(cmd_bytes).map_err(|err| {
            anyhow!(
                "BIND_SHADERS: failed to decode packet (size_bytes={}, err={err:?})",
                cmd_bytes.len()
            )
        })?;

        let vs = cmd.vs;
        let ps = cmd.ps;
        let cs = cmd.cs;
        let (gs, hs, ds) = match ex {
            Some(ex) => (ex.gs, ex.hs, ex.ds),
            // Legacy format: treat the old `reserved0` field as `gs` so existing streams/tests
            // can force the compute-prepass path without appending new fields.
            None => (cmd.gs(), 0, 0),
        };

        self.state.vs = if vs == 0 { None } else { Some(vs) };
        self.state.gs = if gs == 0 { None } else { Some(gs) };
        self.state.hs = if hs == 0 { None } else { Some(hs) };
        self.state.ds = if ds == 0 { None } else { Some(ds) };
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
        let stage_ex = read_u32_le(cmd_bytes, 20)?;
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

        let stage =
            ShaderStage::from_aerogpu_u32_with_stage_ex(stage_raw, stage_ex).ok_or_else(|| {
                anyhow!("SET_SHADER_CONSTANTS_F: unknown shader stage {stage_raw} (stage_ex={stage_ex})")
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
        let stage_ex = read_u32_le(cmd_bytes, 20)?;

        if slot as usize >= DEFAULT_MAX_TEXTURE_SLOTS {
            bail!(
                "SET_TEXTURE: slot out of supported range (slot={slot} max_slot={})",
                DEFAULT_MAX_TEXTURE_SLOTS - 1
            );
        }

        let stage =
            ShaderStage::from_aerogpu_u32_with_stage_ex(stage_raw, stage_ex).ok_or_else(|| {
                anyhow!("SET_TEXTURE: unknown shader stage {stage_raw} (stage_ex={stage_ex})")
            })?;
        let texture = if texture == 0 {
            None
        } else {
            Some(
                self.shared_surfaces
                    .resolve_cmd_handle(texture, "SET_TEXTURE")?,
            )
        };
        // A `t#` register can be either a texture SRV or a buffer SRV. Route the binding to the
        // appropriate slot table based on the resource handle type when known.
        let stage_bindings = self.bindings.stage_mut(stage);
        match texture {
            None => stage_bindings.set_texture(slot, None),
            Some(handle) => {
                if self.resources.buffers.contains_key(&handle) {
                    stage_bindings.set_srv_buffer(
                        slot,
                        Some(BoundBuffer {
                            buffer: handle,
                            offset: 0,
                            size: None,
                        }),
                    );
                } else {
                    stage_bindings.set_texture(slot, Some(handle));
                }
            }
        }
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
            ShaderStage::Geometry,
            ShaderStage::Hull,
            ShaderStage::Domain,
            ShaderStage::Compute,
            ShaderStage::Geometry,
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
        let stage_ex = read_u32_le(cmd_bytes, 20)?;

        let stage =
            ShaderStage::from_aerogpu_u32_with_stage_ex(stage_raw, stage_ex).ok_or_else(|| {
                anyhow!("SET_SAMPLERS: unknown shader stage {stage_raw} (stage_ex={stage_ex})")
            })?;
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
        let stage_ex = read_u32_le(cmd_bytes, 20)?;

        let stage =
            ShaderStage::from_aerogpu_u32_with_stage_ex(stage_raw, stage_ex).ok_or_else(|| {
                anyhow!(
                    "SET_CONSTANT_BUFFERS: unknown shader stage {stage_raw} (stage_ex={stage_ex})"
                )
            })?;
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
                let buffer = self
                    .shared_surfaces
                    .resolve_cmd_handle(buffer_raw, "SET_CONSTANT_BUFFERS")?;
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

    fn exec_set_shader_resource_buffers(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        // struct aerogpu_cmd_set_shader_resource_buffers (24 bytes) + bindings.
        if cmd_bytes.len() < 24 {
            bail!(
                "SET_SHADER_RESOURCE_BUFFERS: expected at least 24 bytes, got {}",
                cmd_bytes.len()
            );
        }
        let stage_raw = read_u32_le(cmd_bytes, 8)?;
        let start_slot_u32 = read_u32_le(cmd_bytes, 12)?;
        let buffer_count_u32 = read_u32_le(cmd_bytes, 16)?;
        let stage_ex = read_u32_le(cmd_bytes, 20)?;

        let stage = ShaderStage::from_aerogpu_u32_with_stage_ex(stage_raw, stage_ex).ok_or_else(
            || {
                anyhow!(
                    "SET_SHADER_RESOURCE_BUFFERS: unknown shader stage {stage_raw} (stage_ex={stage_ex})"
                )
            },
        )?;
        let start_slot: u32 = start_slot_u32;
        let buffer_count: usize = buffer_count_u32
            .try_into()
            .map_err(|_| anyhow!("SET_SHADER_RESOURCE_BUFFERS: buffer_count out of range"))?;

        let end_slot = start_slot
            .checked_add(buffer_count_u32)
            .ok_or_else(|| anyhow!("SET_SHADER_RESOURCE_BUFFERS: slot range overflow"))?;
        if end_slot as usize > DEFAULT_MAX_TEXTURE_SLOTS {
            bail!(
                "SET_SHADER_RESOURCE_BUFFERS: slot range out of supported range (range={start_slot}..{end_slot} max_slot={})",
                DEFAULT_MAX_TEXTURE_SLOTS - 1
            );
        }

        let expected = 24 + buffer_count * 16;
        // Forward-compat: allow this packet to grow by appending new fields after `bindings[]`.
        if cmd_bytes.len() < expected {
            bail!(
                "SET_SHADER_RESOURCE_BUFFERS: expected at least {expected} bytes, got {}",
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
                let buffer = self
                    .shared_surfaces
                    .resolve_cmd_handle(buffer_raw, "SET_SHADER_RESOURCE_BUFFERS")?;
                Some(BoundBuffer {
                    buffer,
                    offset: offset_bytes as u64,
                    size: (size_bytes != 0).then_some(size_bytes as u64),
                })
            };
            let slot = start_slot
                .checked_add(i as u32)
                .ok_or_else(|| anyhow!("SET_SHADER_RESOURCE_BUFFERS: slot overflow"))?;
            self.bindings.stage_mut(stage).set_srv_buffer(slot, bound);
        }

        Ok(())
    }

    fn exec_set_unordered_access_buffers(&mut self, cmd_bytes: &[u8]) -> Result<()> {
        // struct aerogpu_cmd_set_unordered_access_buffers (24 bytes) + bindings.
        if cmd_bytes.len() < 24 {
            bail!(
                "SET_UNORDERED_ACCESS_BUFFERS: expected at least 24 bytes, got {}",
                cmd_bytes.len()
            );
        }
        let stage_raw = read_u32_le(cmd_bytes, 8)?;
        let start_slot_u32 = read_u32_le(cmd_bytes, 12)?;
        let uav_count_u32 = read_u32_le(cmd_bytes, 16)?;
        let stage_ex = read_u32_le(cmd_bytes, 20)?;

        let stage = ShaderStage::from_aerogpu_u32_with_stage_ex(stage_raw, stage_ex).ok_or_else(
            || {
                anyhow!(
                    "SET_UNORDERED_ACCESS_BUFFERS: unknown shader stage {stage_raw} (stage_ex={stage_ex})"
                )
            },
        )?;
        if stage != ShaderStage::Compute {
            bail!(
                "SET_UNORDERED_ACCESS_BUFFERS: only compute stage is supported right now (stage={stage:?})"
            );
        }
        let start_slot: u32 = start_slot_u32;
        let uav_count: usize = uav_count_u32
            .try_into()
            .map_err(|_| anyhow!("SET_UNORDERED_ACCESS_BUFFERS: uav_count out of range"))?;

        let end_slot = start_slot
            .checked_add(uav_count_u32)
            .ok_or_else(|| anyhow!("SET_UNORDERED_ACCESS_BUFFERS: slot range overflow"))?;
        if end_slot as usize > DEFAULT_MAX_UAV_SLOTS {
            bail!(
                "SET_UNORDERED_ACCESS_BUFFERS: slot range out of supported range (range={start_slot}..{end_slot} max_slot={})",
                DEFAULT_MAX_UAV_SLOTS - 1
            );
        }

        let expected = 24 + uav_count * 16;
        // Forward-compat: allow this packet to grow by appending new fields after `bindings[]`.
        if cmd_bytes.len() < expected {
            bail!(
                "SET_UNORDERED_ACCESS_BUFFERS: expected at least {expected} bytes, got {}",
                cmd_bytes.len(),
            );
        }

        for i in 0..uav_count {
            let base = 24 + i * 16;
            let buffer_raw = read_u32_le(cmd_bytes, base)?;
            let offset_bytes = read_u32_le(cmd_bytes, base + 4)?;
            let size_bytes = read_u32_le(cmd_bytes, base + 8)?;
            // initial_count @ +12 currently ignored.

            let bound = if buffer_raw == 0 {
                None
            } else {
                let buffer = self
                    .shared_surfaces
                    .resolve_cmd_handle(buffer_raw, "SET_UNORDERED_ACCESS_BUFFERS")?;
                Some(BoundBuffer {
                    buffer,
                    offset: offset_bytes as u64,
                    size: (size_bytes != 0).then_some(size_bytes as u64),
                })
            };
            let slot = start_slot
                .checked_add(i as u32)
                .ok_or_else(|| anyhow!("SET_UNORDERED_ACCESS_BUFFERS: slot overflow"))?;
            self.bindings.stage_mut(stage).set_uav_buffer(slot, bound);
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
        for i in 0..color_count {
            let tex_id = read_u32_le(cmd_bytes, 16 + i * 4)?;
            let tex_id = if tex_id == 0 {
                None
            } else {
                Some(
                    self.shared_surfaces
                        .resolve_cmd_handle(tex_id, "SET_RENDER_TARGETS")?,
                )
            };
            colors.push(tex_id);
        }
        // Preserve gaps, but trim trailing `None` entries so a caller that always supplies 8 RTV
        // slots (all NULL) behaves like `color_count=0`.
        //
        // This avoids forcing render passes to carry around long `None` tails, and ensures our
        // internal depth-only dummy attachment doesn't exceed `max_color_attachments` on strict
        // WebGPU implementations.
        while let Some(None) = colors.last() {
            colors.pop();
        }
        self.state.render_targets = colors;
        self.state.depth_stencil = if depth_stencil == 0 {
            None
        } else {
            Some(
                self.shared_surfaces
                    .resolve_cmd_handle(depth_stencil, "SET_RENDER_TARGETS")?,
            )
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
        // Unlike the 0x0 "scissor disabled" encoding (w<=0||h<=0), a scissor rect that becomes
        // empty after clamping to non-negative coordinates should be treated as an *empty scissor*
        // (no pixels pass), not as "disabled". We preserve it as a 0-sized rect here and let the
        // render-pass path skip draws when the effective scissor is empty.
        self.state.scissor = Some(Scissor {
            x: left as u32,
            y: top as u32,
            width: (right - left).max(0) as u32,
            height: (bottom - top).max(0) as u32,
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
                self.shared_surfaces
                    .resolve_cmd_handle(buffer_raw, "SET_VERTEX_BUFFERS")?
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
        let buffer = self
            .shared_surfaces
            .resolve_cmd_handle(buffer, "SET_INDEX_BUFFER")?;

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
        let Some(topology) = CmdPrimitiveTopology::from_u32(topology_u32) else {
            bail!("SET_PRIMITIVE_TOPOLOGY: unknown topology {topology_u32}");
        };
        self.state.primitive_topology = topology;
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
        if self.state.render_targets.iter().all(|rt| rt.is_none())
            && self.state.depth_stencil.is_none()
        {
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
        for &handle in render_targets.iter().flatten() {
            self.ensure_texture_uploaded(encoder, handle, allocs, guest_mem)?;
        }
        if let Some(handle) = depth_stencil {
            self.ensure_texture_uploaded(encoder, handle, allocs, guest_mem)?;
        }

        for &handle in render_targets.iter().flatten() {
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
            for &handle in render_targets.iter().flatten() {
                if let Some(tex) = self.resources.textures.get_mut(&handle) {
                    tex.host_shadow = None;
                    tex.host_shadow_valid.clear();
                    tex.guest_backing_is_current = false;
                }
            }
        }
        if (flags & (AEROGPU_CLEAR_DEPTH | AEROGPU_CLEAR_STENCIL)) != 0 {
            if let Some(handle) = depth_stencil {
                if let Some(tex) = self.resources.textures.get_mut(&handle) {
                    tex.host_shadow = None;
                    tex.host_shadow_valid.clear();
                    tex.guest_backing_is_current = false;
                }
            }
        }

        self.encoder_has_commands = true;
        let (mut color_attachments, mut depth_stencil_attachment) =
            build_render_pass_attachments(&self.resources, &self.state, wgpu::LoadOp::Load)?;

        if flags & AEROGPU_CLEAR_COLOR != 0 {
            for (idx, att) in color_attachments.iter_mut().enumerate() {
                if let Some(att) = att.as_mut() {
                    let Some(rt_handle) = render_targets.get(idx).copied().flatten() else {
                        bail!("CLEAR: render target slot {idx} is unbound");
                    };
                    let rt_tex = self.resources.textures.get(&rt_handle).ok_or_else(|| {
                        anyhow!("CLEAR: unknown render target texture {rt_handle}")
                    })?;
                    let a = if aerogpu_format_is_x8(rt_tex.format_u32) {
                        1.0
                    } else {
                        color[3]
                    };
                    att.ops.load = wgpu::LoadOp::Clear(wgpu::Color {
                        r: color[0] as f64,
                        g: color[1] as f64,
                        b: color[2] as f64,
                        a: a as f64,
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

    fn exec_dispatch(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        cmd_bytes: &[u8],
        allocs: &AllocTable,
        guest_mem: &mut dyn GuestMemory,
    ) -> Result<()> {
        // struct aerogpu_cmd_dispatch (24 bytes)
        if cmd_bytes.len() < 24 {
            bail!(
                "DISPATCH: expected at least 24 bytes, got {}",
                cmd_bytes.len()
            );
        }
        let group_count_x = read_u32_le(cmd_bytes, 8)?;
        let group_count_y = read_u32_le(cmd_bytes, 12)?;
        let group_count_z = read_u32_le(cmd_bytes, 16)?;

        // D3D11 treats any zero group count as a no-op dispatch.
        if group_count_x == 0 || group_count_y == 0 || group_count_z == 0 {
            return Ok(());
        }

        let cs_handle = self
            .state
            .cs
            .ok_or_else(|| anyhow!("DISPATCH: no compute shader bound"))?;
        // Clone shader metadata out of `self.resources` to avoid holding immutable borrows across
        // pipeline/bind-group construction.
        let cs = self
            .resources
            .shaders
            .get(&cs_handle)
            .ok_or_else(|| anyhow!("DISPATCH: unknown CS shader {cs_handle}"))?
            .clone();
        if cs.stage != ShaderStage::Compute {
            bail!("DISPATCH: shader {cs_handle} is not a compute shader");
        }

        let mut pipeline_bindings = reflection_bindings::build_pipeline_bindings_info(
            &self.device,
            &mut self.bind_group_layout_cache,
            [reflection_bindings::ShaderBindingSet::Guest(
                cs.reflection.bindings.as_slice(),
            )],
            reflection_bindings::BindGroupIndexValidation::GuestShaders,
        )?;

        // `PipelineLayoutKey` is used both for pipeline-layout caching and as part of the pipeline
        // cache key. Avoid cloning the underlying Vec by moving it out of `pipeline_bindings`.
        let layout_key = std::mem::replace(
            &mut pipeline_bindings.layout_key,
            PipelineLayoutKey::empty(),
        );

        let pipeline_layout = {
            let device = &self.device;
            let cache = &mut self.pipeline_layout_cache;
            cache.get_or_create_with(&layout_key, || {
                let layout_refs: Vec<&wgpu::BindGroupLayout> = pipeline_bindings
                    .group_layouts
                    .iter()
                    .map(|l| l.layout.as_ref())
                    .collect();
                Arc::new(
                    device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                        label: Some("aerogpu_cmd compute pipeline layout"),
                        bind_group_layouts: &layout_refs,
                        push_constant_ranges: &[],
                    }),
                )
            })
        };

        // Ensure any guest-backed resources referenced by the current binding state are uploaded
        // before entering the compute pass.
        self.ensure_bound_resources_uploaded(encoder, &pipeline_bindings, allocs, guest_mem)?;

        // `PipelineCache` returns a reference tied to the mutable borrow. Convert it to a raw
        // pointer so we can continue mutating unrelated executor state while the compute pass is
        // alive.
        let compute_pipeline_ptr = {
            let entry_point = cs.entry_point;
            let pipeline_layout = pipeline_layout.clone();
            let key = ComputePipelineKey {
                shader: cs.wgsl_hash,
                layout: layout_key.clone(),
            };
            let pipeline = self
                .pipeline_cache
                .get_or_create_compute_pipeline(&self.device, key, move |device, cs| {
                    device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                        label: Some("aerogpu_cmd compute pipeline"),
                        layout: Some(pipeline_layout.as_ref()),
                        module: cs,
                        entry_point,
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                    })
                })
                .map_err(|e| anyhow!("wgpu pipeline cache: {e:?}"))?;
            pipeline as *const wgpu::ComputePipeline
        };
        let compute_pipeline = unsafe { &*compute_pipeline_ptr };

        // Build bind groups (before starting the pass so we can freely mutate executor caches).
        let mut bind_groups: Vec<Arc<wgpu::BindGroup>> =
            Vec::with_capacity(pipeline_bindings.group_layouts.len());
        for group_index in 0..pipeline_bindings.group_layouts.len() {
            if pipeline_bindings.group_bindings[group_index].is_empty() {
                let entries: [BindGroupCacheEntry<'_>; 0] = [];
                let bg = self.bind_group_cache.get_or_create(
                    &self.device,
                    &pipeline_bindings.group_layouts[group_index],
                    &entries,
                );
                bind_groups.push(bg);
            } else {
                let stage = group_index_to_stage(group_index as u32)?;
                let stage_bindings = self.bindings.stage_mut(stage);
                let was_dirty = stage_bindings.is_dirty();
                let provider = CmdExecutorBindGroupProvider {
                    resources: &self.resources,
                    legacy_constants: &self.legacy_constants,
                    cbuffer_scratch: &self.cbuffer_scratch,
                    dummy_uniform: &self.dummy_uniform,
                    dummy_storage: &self.dummy_storage,
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
                if was_dirty {
                    stage_bindings.clear_dirty();
                }
                bind_groups.push(bg);
            }
        }

        self.encoder_has_commands = true;
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("aerogpu_cmd compute pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(compute_pipeline);
            for (group_index, bg) in bind_groups.iter().enumerate() {
                pass.set_bind_group(group_index as u32, bg.as_ref(), &[]);
            }
            pass.dispatch_workgroups(group_count_x, group_count_y, group_count_z);
        }

        // Conservatively mark bound resources as used to preserve queue.write_* ordering heuristics.
        for (group_index, group_bindings) in pipeline_bindings.group_bindings.iter().enumerate() {
            let stage = group_index_to_stage(group_index as u32)?;
            let stage_bindings = self.bindings.stage(stage);
            for binding in group_bindings {
                #[allow(unreachable_patterns)]
                match &binding.kind {
                    crate::BindingKind::Texture2D { slot } => {
                        if let Some(tex) = stage_bindings.texture(*slot) {
                            self.encoder_used_textures.insert(tex.texture);
                        }
                    }
                    crate::BindingKind::SrvBuffer { slot } => {
                        if let Some(buf) = stage_bindings.srv_buffer(*slot) {
                            self.encoder_used_buffers.insert(buf.buffer);
                        }
                    }
                    crate::BindingKind::ConstantBuffer { slot, .. } => {
                        if let Some(cb) = stage_bindings.constant_buffer(*slot) {
                            self.encoder_used_buffers.insert(cb.buffer);
                        }
                    }
                    crate::BindingKind::UavBuffer { slot } => {
                        if let Some(buf) = stage_bindings.uav_buffer(*slot) {
                            self.encoder_used_buffers.insert(buf.buffer);
                        }
                        if let Some(tex) = stage_bindings.uav_texture(*slot) {
                            self.encoder_used_textures.insert(tex.texture);
                        }
                    }
                    crate::BindingKind::Sampler { .. } => {}
                    // Forward-compat: fall back to binding-number range inspection for any new
                    // `BindingKind` variants (e.g. future UAV textures).
                    _ => {
                        let binding_num = binding.binding;
                        if binding_num >= BINDING_BASE_UAV {
                            let slot = binding_num.saturating_sub(BINDING_BASE_UAV);
                            if let Some(buf) = stage_bindings.uav_buffer(slot) {
                                self.encoder_used_buffers.insert(buf.buffer);
                            }
                            if let Some(tex) = stage_bindings.uav_texture(slot) {
                                self.encoder_used_textures.insert(tex.texture);
                            }
                        } else if binding_num >= BINDING_BASE_TEXTURE && binding_num < BINDING_BASE_SAMPLER
                        {
                            let slot = binding_num.saturating_sub(BINDING_BASE_TEXTURE);
                            if let Some(tex) = stage_bindings.texture(slot) {
                                self.encoder_used_textures.insert(tex.texture);
                            }
                            if let Some(buf) = stage_bindings.srv_buffer(slot) {
                                self.encoder_used_buffers.insert(buf.buffer);
                            }
                        } else if binding_num < BINDING_BASE_TEXTURE {
                            if let Some(cb) = stage_bindings.constant_buffer(binding_num) {
                                self.encoder_used_buffers.insert(cb.buffer);
                            }
                        }
                    }
                }
            }
        }

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
            .iter()
            .flatten()
            .next()
            .copied()
            .map(|h| self.shared_surfaces.resolve_handle(h));
        report.presents.push(PresentEvent {
            scanout_id,
            flags,
            d3d9_present_flags: None,
            presented_render_target,
        });
        self.submit_encoder(encoder, "aerogpu_cmd encoder after present");
        self.expansion_scratch.begin_frame();
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
            .iter()
            .flatten()
            .next()
            .copied()
            .map(|h| self.shared_surfaces.resolve_handle(h));
        report.presents.push(PresentEvent {
            scanout_id,
            flags,
            d3d9_present_flags: Some(d3d9_present_flags),
            presented_render_target,
        });
        self.submit_encoder(encoder, "aerogpu_cmd encoder after present_ex");
        self.expansion_scratch.begin_frame();
        Ok(())
    }

    fn exec_flush(&mut self, encoder: &mut wgpu::CommandEncoder) -> Result<()> {
        self.submit_encoder(encoder, "aerogpu_cmd encoder after flush");
        self.expansion_scratch.begin_frame();
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
                #[allow(unreachable_patterns)]
                match &binding.kind {
                    crate::BindingKind::ConstantBuffer { slot, .. } => {
                        if let Some(cb) = self.bindings.stage(stage).constant_buffer(*slot) {
                            if cb.buffer != legacy_constants_buffer_id(stage) {
                                self.ensure_buffer_uploaded(encoder, cb.buffer, allocs, guest_mem)?;
                            }
                        }
                    }
                    crate::BindingKind::Texture2D { slot } => {
                        if let Some(tex) = self.bindings.stage(stage).texture(*slot) {
                            self.ensure_texture_uploaded(encoder, tex.texture, allocs, guest_mem)?;
                        }
                    }
                    crate::BindingKind::SrvBuffer { slot } => {
                        if let Some(buf) = self.bindings.stage(stage).srv_buffer(*slot) {
                            self.ensure_buffer_uploaded(encoder, buf.buffer, allocs, guest_mem)?;
                        }
                    }
                    crate::BindingKind::UavBuffer { slot } => {
                        if let Some(buf) = self.bindings.stage(stage).uav_buffer(*slot) {
                            self.ensure_buffer_uploaded(encoder, buf.buffer, allocs, guest_mem)?;
                        }
                        if let Some(tex) = self.bindings.stage(stage).uav_texture(*slot) {
                            self.ensure_texture_uploaded(encoder, tex.texture, allocs, guest_mem)?;
                        }
                    }
                    crate::BindingKind::Sampler { .. } => {}
                    // Forward-compat: for new variants (e.g. future UAV textures), fall back to
                    // binding-number range inspection and upload whatever is currently bound at the
                    // corresponding slots.
                    _ => {
                        let binding_num = binding.binding;
                        if binding_num >= BINDING_BASE_UAV {
                            let slot = binding_num.saturating_sub(BINDING_BASE_UAV);
                            if let Some(buf) = self.bindings.stage(stage).uav_buffer(slot) {
                                self.ensure_buffer_uploaded(encoder, buf.buffer, allocs, guest_mem)?;
                            }
                            if let Some(tex) = self.bindings.stage(stage).uav_texture(slot) {
                                self.ensure_texture_uploaded(encoder, tex.texture, allocs, guest_mem)?;
                            }
                        } else if binding_num >= BINDING_BASE_TEXTURE && binding_num < BINDING_BASE_SAMPLER {
                            let slot = binding_num.saturating_sub(BINDING_BASE_TEXTURE);
                            if let Some(tex) = self.bindings.stage(stage).texture(slot) {
                                self.ensure_texture_uploaded(encoder, tex.texture, allocs, guest_mem)?;
                            }
                            if let Some(buf) = self.bindings.stage(stage).srv_buffer(slot) {
                                self.ensure_buffer_uploaded(encoder, buf.buffer, allocs, guest_mem)?;
                            }
                        } else if binding_num < BINDING_BASE_TEXTURE {
                            if let Some(cb) = self.bindings.stage(stage).constant_buffer(binding_num) {
                                if cb.buffer != legacy_constants_buffer_id(stage) {
                                    self.ensure_buffer_uploaded(encoder, cb.buffer, allocs, guest_mem)?;
                                }
                            }
                        }
                    }
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
                let crate::BindingKind::ConstantBuffer { slot, reg_count } = &binding.kind else {
                    continue;
                };
                let Some(cb) = self.bindings.stage(stage).constant_buffer(*slot) else {
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
                let required_min = (*reg_count as u64)
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

                let scratch = self.get_or_create_constant_buffer_scratch(stage, *slot, size);
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
        let (desc, format_u32, row_pitch_bytes, backing) =
            match self.resources.textures.get(&texture_handle) {
                Some(tex) if tex.dirty => {
                    (tex.desc, tex.format_u32, tex.row_pitch_bytes, tex.backing)
                }
                _ => return Ok(()),
            };

        let Some(backing) = backing else {
            if let Some(tex) = self.resources.textures.get_mut(&texture_handle) {
                tex.dirty = false;
            }
            return Ok(());
        };

        let force_opaque_alpha = aerogpu_format_is_x8(format_u32);
        let b5_format = aerogpu_b5_format(format_u32);

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
                            let start = row.checked_mul(src_row_pitch).ok_or_else(|| {
                                anyhow!("texture upload row offset overflows usize")
                            })?;
                            let end = start
                                .checked_add(unpadded_bpr_usize)
                                .ok_or_else(|| anyhow!("texture upload row end overflows usize"))?;
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
        fn upload_b5_subresource(
            queue: &wgpu::Queue,
            texture: &wgpu::Texture,
            mip_level: u32,
            array_layer: u32,
            width: u32,
            height: u32,
            src_bytes_per_row: u32,
            b5_format: AerogpuB5Format,
            gpa: u64,
            guest_mem: &mut dyn GuestMemory,
        ) -> Result<()> {
            // Guest layout is 16bpp packed, host texture is RGBA8.
            let src_unpadded_bpr = width
                .checked_mul(2)
                .ok_or_else(|| anyhow!("texture upload: B5 bytes_per_row overflow"))?;
            if src_bytes_per_row < src_unpadded_bpr {
                bail!("texture upload: B5 bytes_per_row too small");
            }

            let dst_unpadded_bpr = width
                .checked_mul(4)
                .ok_or_else(|| anyhow!("texture upload: B5 expanded bytes_per_row overflow"))?;
            let aligned = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
            let padded_bpr = if height > 1 {
                dst_unpadded_bpr
                    .checked_add(aligned - 1)
                    .map(|v| v / aligned)
                    .and_then(|v| v.checked_mul(aligned))
                    .ok_or_else(|| anyhow!("texture upload: B5 padded bytes_per_row overflow"))?
            } else {
                dst_unpadded_bpr
            };

            let height_usize: usize = height
                .try_into()
                .map_err(|_| anyhow!("texture upload: B5 height out of range"))?;

            let padded_bpr_usize: usize = padded_bpr
                .try_into()
                .map_err(|_| anyhow!("texture upload: B5 padded bytes_per_row out of range"))?;
            let src_unpadded_bpr_usize: usize = src_unpadded_bpr
                .try_into()
                .map_err(|_| anyhow!("texture upload: B5 bytes_per_row out of range"))?;
            let dst_unpadded_bpr_usize: usize = dst_unpadded_bpr
                .try_into()
                .map_err(|_| anyhow!("texture upload: B5 bytes_per_row out of range"))?;

            let rows_per_chunk = (CHUNK_BYTES / padded_bpr_usize).max(1);
            let mut src_row_buf = vec![0u8; src_unpadded_bpr_usize];
            let mut expanded_rows = vec![0u8; padded_bpr_usize * rows_per_chunk];

            for y0 in (0..height_usize).step_by(rows_per_chunk) {
                let rows = (height_usize - y0).min(rows_per_chunk);
                let needed = padded_bpr_usize
                    .checked_mul(rows)
                    .ok_or_else(|| anyhow!("texture upload: B5 chunk overflows usize"))?;
                let chunk = &mut expanded_rows[..needed];
                chunk.fill(0);

                for row in 0..rows {
                    let row_index = y0
                        .checked_add(row)
                        .ok_or_else(|| anyhow!("texture upload: B5 row index overflows usize"))?;
                    let row_offset = u64::try_from(row_index)
                        .ok()
                        .and_then(|v| v.checked_mul(u64::from(src_bytes_per_row)))
                        .ok_or_else(|| anyhow!("texture upload: B5 row offset overflows u64"))?;
                    let src_addr = gpa
                        .checked_add(row_offset)
                        .ok_or_else(|| anyhow!("texture upload: B5 address overflows u64"))?;
                    guest_mem
                        .read(src_addr, &mut src_row_buf)
                        .map_err(anyhow_guest_mem)?;

                    let dst_start = row * padded_bpr_usize;
                    let dst_row = chunk
                        .get_mut(dst_start..dst_start + dst_unpadded_bpr_usize)
                        .ok_or_else(|| anyhow!("texture upload: B5 staging buffer too small"))?;
                    match b5_format {
                        AerogpuB5Format::B5G6R5 => {
                            expand_b5g6r5_unorm_to_rgba8(&src_row_buf, dst_row);
                        }
                        AerogpuB5Format::B5G5R5A1 => {
                            expand_b5g5r5a1_unorm_to_rgba8(&src_row_buf, dst_row);
                        }
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
                    chunk,
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
            let physical_width_texels = blocks_w
                .checked_mul(4)
                .ok_or_else(|| anyhow!("texture upload: BC width overflows u32"))?;
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

            let padded_bpr = if blocks_h > 1 {
                Some(
                    unpadded_bpr
                        .checked_add(aligned - 1)
                        .map(|v| v / aligned)
                        .and_then(|v| v.checked_mul(aligned))
                        .ok_or_else(|| {
                            anyhow!("texture upload: BC padded bytes_per_row overflow")
                        })?,
                )
            } else {
                None
            };

            // If the tight BC stride isn't aligned for multi-row writes, avoid the 256-byte
            // alignment requirement by uploading one block row at a time (each upload has a single
            // block row, so it does not require `bytes_per_row` alignment).
            if let Some(padded_bpr) = padded_bpr {
                if unpadded_bpr != padded_bpr {
                    let row_len_usize: usize = unpadded_bpr
                        .try_into()
                        .map_err(|_| anyhow!("texture upload: BC bytes_per_row out of range"))?;
                    let mut row_buf = vec![0u8; row_len_usize];
                    for block_row in 0..blocks_h_usize {
                        let row_offset = u64::try_from(block_row)
                            .ok()
                            .and_then(|v| v.checked_mul(u64::from(bytes_per_row)))
                            .ok_or_else(|| {
                                anyhow!("texture upload: BC row offset overflows u64")
                            })?;
                        let src_addr = gpa
                            .checked_add(row_offset)
                            .ok_or_else(|| anyhow!("texture upload: BC address overflows u64"))?;
                        guest_mem
                            .read(src_addr, &mut row_buf)
                            .map_err(anyhow_guest_mem)?;

                        let origin_y_texels = (block_row as u32)
                            .checked_mul(4)
                            .ok_or_else(|| anyhow!("texture upload: BC origin.y overflow"))?;
                        let remaining_height =
                            height.checked_sub(origin_y_texels).ok_or_else(|| {
                                anyhow!("texture upload: BC origin exceeds mip height")
                            })?;
                        let chunk_height_texels = remaining_height.min(4);

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
                            &row_buf,
                            wgpu::ImageDataLayout {
                                offset: 0,
                                bytes_per_row: Some(unpadded_bpr),
                                rows_per_image: Some(1),
                            },
                            wgpu::Extent3d {
                                width,
                                height: chunk_height_texels,
                                depth_or_array_layers: 1,
                            },
                        );
                    }

                    return Ok(());
                }
            }

            let repack_padded_bpr = padded_bpr.and_then(|padded_bpr| {
                (blocks_h > 1 && bytes_per_row != padded_bpr).then_some(padded_bpr)
            });

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
                        let row_index = y0.checked_add(row).ok_or_else(|| {
                            anyhow!("texture upload: BC row index overflows usize")
                        })?;
                        let row_offset = u64::try_from(row_index)
                            .ok()
                            .and_then(|v| v.checked_mul(u64::from(bytes_per_row)))
                            .ok_or_else(|| {
                                anyhow!("texture upload: BC row offset overflows u64")
                            })?;
                        let src_addr = gpa
                            .checked_add(row_offset)
                            .ok_or_else(|| anyhow!("texture upload: BC address overflows u64"))?;
                        guest_mem
                            .read(src_addr, &mut row_buf)
                            .map_err(anyhow_guest_mem)?;
                        let dst_start = row * padded_bpr_usize;
                        repacked[dst_start..dst_start + row_buf.len()].copy_from_slice(&row_buf);
                    }

                    let origin_y_texels = (y0 as u32)
                        .checked_mul(4)
                        .ok_or_else(|| anyhow!("texture upload: BC origin.y overflow"))?;
                    let chunk_height_texels = (rows as u32)
                        .checked_mul(4)
                        .ok_or_else(|| anyhow!("texture upload: BC extent height overflow"))?;

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
                            width: physical_width_texels,
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

                    let origin_y_texels = (y0 as u32)
                        .checked_mul(4)
                        .ok_or_else(|| anyhow!("texture upload: BC origin.y overflow"))?;
                    let chunk_height_texels = (rows as u32)
                        .checked_mul(4)
                        .ok_or_else(|| anyhow!("texture upload: BC extent height overflow"))?;

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
                            width: physical_width_texels,
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
                        let tight_bpr_usize: usize = tight_bpr.try_into().map_err(|_| {
                            anyhow!("texture upload: BC bytes_per_row out of range")
                        })?;
                        let rows_usize: usize = level_rows_u32
                            .try_into()
                            .map_err(|_| anyhow!("texture upload: BC rows out of range"))?;
                        let src_bpr_usize: usize =
                            level_row_pitch_u32.try_into().map_err(|_| {
                                anyhow!("texture upload: BC bytes_per_row out of range")
                            })?;
                        let tight_len =
                            tight_bpr_usize.checked_mul(rows_usize).ok_or_else(|| {
                                anyhow!("texture upload: BC data size overflows usize")
                            })?;

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
                                    .ok_or_else(|| {
                                        anyhow!("texture upload: BC row offset overflow")
                                    })?;
                                let addr = gpa.checked_add(row_offset).ok_or_else(|| {
                                    anyhow!("texture upload: BC row address overflow")
                                })?;
                                guest_mem
                                    .read(addr, &mut row_buf)
                                    .map_err(anyhow_guest_mem)?;
                                let dst_start =
                                    row.checked_mul(tight_bpr_usize).ok_or_else(|| {
                                        anyhow!("texture upload: BC dst offset overflow")
                                    })?;
                                tight[dst_start..dst_start + tight_bpr_usize]
                                    .copy_from_slice(&row_buf[..tight_bpr_usize]);
                            }
                            tight
                        };

                        // Guard against panic in the decompressor (it asserts on length).
                        let expected_len = level_width.div_ceil(4) as usize
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
                            Texture2dSubresourceDesc {
                                desc,
                                mip_level: level,
                                array_layer: layer,
                            },
                            level_width.checked_mul(4).ok_or_else(|| {
                                anyhow!("texture upload: decompressed bytes_per_row overflow")
                            })?,
                            &rgba,
                            false,
                        )?;
                    }
                } else {
                    if let Some(b5_format) = b5_format {
                        upload_b5_subresource(
                            queue,
                            &tex.texture,
                            level,
                            layer,
                            level_width,
                            level_height,
                            level_row_pitch_u32,
                            b5_format,
                            gpa,
                            guest_mem,
                        )?;
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
        }

        // Guest-backed uploads overwrite the GPU texture content; discard any stale UPLOAD_RESOURCE
        // shadow copy so later partial uploads don't accidentally clobber the updated contents.
        tex.host_shadow = None;
        tex.host_shadow_valid.clear();
        tex.dirty = false;
        tex.guest_backing_is_current = true;
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

    fn depth_only_dummy_color_view(&mut self, width: u32, height: u32) -> wgpu::TextureView {
        let key = (width, height, DEPTH_ONLY_DUMMY_COLOR_FORMAT);
        if !self.depth_only_dummy_color_targets.contains_key(&key) {
            let tex = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("aerogpu_cmd depth-only dummy color target"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: DEPTH_ONLY_DUMMY_COLOR_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            self.depth_only_dummy_color_targets.insert(key, tex);
        }

        self.depth_only_dummy_color_targets
            .get(&key)
            .expect("dummy color target inserted above")
            .create_view(&wgpu::TextureViewDescriptor::default())
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
        let Some(tex_id) = tex_id else {
            color_attachments.push(None);
            continue;
        };
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

#[allow(dead_code)]
fn wgsl_gs_passthrough_vertex_shader(varying_locations: &BTreeSet<u32>) -> Result<String> {
    // Pick a vertex attribute location for the clip-space position that does not collide with the
    // varyings expected by the pixel shader.
    let mut pos_location: u32 = 0;
    while varying_locations.contains(&pos_location) {
        pos_location = pos_location
            .checked_add(1)
            .ok_or_else(|| anyhow!("GS passthrough VS: ran out of @location values"))?;
    }

    let mut out = String::new();

    out.push_str("struct VsIn {\n");
    out.push_str(&format!(
        "    @location({pos_location}) pos: vec4<f32>,\n"
    ));
    for &loc in varying_locations {
        out.push_str(&format!("    @location({loc}) v{loc}: vec4<f32>,\n"));
    }
    out.push_str("};\n\n");

    out.push_str("struct VsOut {\n");
    out.push_str("    @builtin(position) pos: vec4<f32>,\n");
    for &loc in varying_locations {
        out.push_str(&format!("    @location({loc}) o{loc}: vec4<f32>,\n"));
    }
    out.push_str("};\n\n");

    out.push_str("@vertex\n");
    out.push_str("fn vs_main(input: VsIn) -> VsOut {\n");
    out.push_str("    var out: VsOut;\n");
    out.push_str("    out.pos = input.pos;\n");
    for &loc in varying_locations {
        out.push_str(&format!("    out.o{loc} = input.v{loc};\n"));
    }
    out.push_str("    return out;\n");
    out.push_str("}\n");

    Ok(out)
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
    dummy_storage: &'a wgpu::Buffer,
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

    fn srv_buffer(&self, slot: u32) -> Option<reflection_bindings::BufferBinding<'_>> {
        let bound = self.stage_state.srv_buffer(slot)?;
        let buf = self.resources.buffers.get(&bound.buffer)?;
        Some(reflection_bindings::BufferBinding {
            id: BufferId(bound.buffer as u64),
            buffer: &buf.buffer,
            offset: bound.offset,
            size: bound.size,
            total_size: buf.size,
        })
    }

    fn sampler(&self, slot: u32) -> Option<&aero_gpu::bindings::samplers::CachedSampler> {
        let bound = self.stage_state.sampler(slot)?;
        self.resources.samplers.get(&bound.sampler)
    }
    fn uav_buffer(&self, slot: u32) -> Option<reflection_bindings::BufferBinding<'_>> {
        let bound = self.stage_state.uav_buffer(slot)?;
        let buf = self.resources.buffers.get(&bound.buffer)?;
        Some(reflection_bindings::BufferBinding {
            id: BufferId(bound.buffer as u64),
            buffer: &buf.buffer,
            offset: bound.offset,
            size: bound.size,
            total_size: buf.size,
        })
    }

    fn dummy_uniform(&self) -> &wgpu::Buffer {
        self.dummy_uniform
    }

    fn dummy_storage(&self) -> &wgpu::Buffer {
        self.dummy_storage
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
    let (vertex_shader, ps_wgsl_hash, vs_dxbc_hash_fnv1a64, vs_entry_point, vs_input_signature, fs_entry_point) = {
        let vs = resources
            .shaders
            .get(&vs_handle)
            .ok_or_else(|| anyhow!("unknown VS shader {vs_handle}"))?;
        if vs.stage != ShaderStage::Vertex {
            bail!("shader {vs_handle} is not a vertex shader");
        }

        let ps = resources
            .shaders
            .get(&ps_handle)
            .ok_or_else(|| anyhow!("unknown PS shader {ps_handle}"))?;
        if ps.stage != ShaderStage::Pixel {
            bail!("shader {ps_handle} is not a pixel shader");
        }

        // WebGPU requires the vertex output interface to exactly match the fragment input
        // interface. D3D shaders often export extra varyings (so a single VS can be reused with
        // multiple PS variants), and pixel shaders may declare inputs they never read.
        //
        // To preserve D3D behavior, we trim the stage interface at pipeline-creation time:
        // - Drop unused PS inputs when the bound VS does not output them.
        // - Drop unused VS outputs that the PS does not declare.
        let ps_declared_inputs = super::wgsl_link::locations_in_struct(&ps.wgsl_source, "PsIn")?;
        let vs_outputs = super::wgsl_link::locations_in_struct(&vs.wgsl_source, "VsOut")?;
        let mut ps_link_locations = ps_declared_inputs.clone();

        let ps_missing_locations: BTreeSet<u32> = ps_declared_inputs
            .difference(&vs_outputs)
            .copied()
            .collect();
        if !ps_missing_locations.is_empty() {
            let ps_used_locations =
                super::wgsl_link::referenced_ps_input_locations(&ps.wgsl_source);
            let used_missing: Vec<u32> = ps_missing_locations
                .intersection(&ps_used_locations)
                .copied()
                .collect();
            if let Some(&loc) = used_missing.first() {
                bail!("pixel shader reads @location({loc}), but VS does not output it");
            }
            ps_link_locations = ps_declared_inputs
                .intersection(&vs_outputs)
                .copied()
                .collect();
        }

        let mut linked_ps_wgsl = std::borrow::Cow::Borrowed(ps.wgsl_source.as_str());
        if ps_link_locations != ps_declared_inputs {
            linked_ps_wgsl =
                std::borrow::Cow::Owned(super::wgsl_link::trim_ps_inputs_to_locations(
                    linked_ps_wgsl.as_ref(),
                    &ps_link_locations,
                ));
        }

        // Trim fragment shader outputs to the currently-bound render target slots.
        //
        // Note: `state.render_targets` may contain gaps (None) when the D3D app binds e.g.
        // RTV0 + RTV2 with RTV1 unbound. WebGPU requires that an output location has a matching
        // `ColorTargetState` entry (Some), so we only keep locations that are actually bound.
        let keep_output_locations: BTreeSet<u32> = state
            .render_targets
            .iter()
            .enumerate()
            .filter_map(|(idx, rt)| rt.is_some().then_some(idx as u32))
            .collect();
        let declared_outputs =
            super::wgsl_link::declared_ps_output_locations(linked_ps_wgsl.as_ref())?;
        let missing_outputs: BTreeSet<u32> = declared_outputs
            .difference(&keep_output_locations)
            .copied()
            .collect();
        if !missing_outputs.is_empty() {
            linked_ps_wgsl =
                std::borrow::Cow::Owned(super::wgsl_link::trim_ps_outputs_to_locations(
                    linked_ps_wgsl.as_ref(),
                    &keep_output_locations,
                ));
        }

        let fragment_shader = if linked_ps_wgsl.as_ref() == ps.wgsl_source.as_str() {
            ps.wgsl_hash
        } else {
            let (hash, _module) = pipeline_cache.get_or_create_shader_module(
                device,
                map_pipeline_cache_stage(ShaderStage::Pixel),
                linked_ps_wgsl.as_ref(),
                Some("aerogpu_cmd linked pixel shader"),
            );
            hash
        };

        let needs_trim = vs_outputs != ps_link_locations;

        let selected_vs_hash = if state.depth_clip_enabled {
            vs.wgsl_hash
        } else {
            vs.depth_clamp_wgsl_hash.unwrap_or(vs.wgsl_hash)
        };

        let vertex_shader = if !needs_trim {
            selected_vs_hash
        } else {
            let base_vs_wgsl = if state.depth_clip_enabled {
                std::borrow::Cow::Borrowed(vs.wgsl_source.as_str())
            } else {
                std::borrow::Cow::Owned(wgsl_depth_clamp_variant(&vs.wgsl_source))
            };
            let trimmed_vs_wgsl = super::wgsl_link::trim_vs_outputs_to_locations(
                base_vs_wgsl.as_ref(),
                &ps_link_locations,
            );
            let (hash, _module) = pipeline_cache.get_or_create_shader_module(
                device,
                map_pipeline_cache_stage(ShaderStage::Vertex),
                &trimmed_vs_wgsl,
                Some("aerogpu_cmd linked vertex shader"),
            );
            hash
        };

        (
            vertex_shader,
            fragment_shader,
            vs.dxbc_hash_fnv1a64,
            vs.entry_point,
            vs.vs_input_signature.clone(),
            ps.entry_point,
        )
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

    let depth_only_pass =
        state.render_targets.iter().all(|rt| rt.is_none()) && state.depth_stencil.is_some();

    let mut color_targets = if depth_only_pass {
        Vec::with_capacity(1)
    } else {
        Vec::with_capacity(state.render_targets.len())
    };
    let mut color_target_states = if depth_only_pass {
        Vec::with_capacity(1)
    } else {
        Vec::with_capacity(state.render_targets.len())
    };

    if depth_only_pass {
        // Depth-only render pass: bind a dummy color target so the fragment shader's `@location(0)`
        // output is accepted by wgpu/WebGPU validation.
        //
        // Use a fixed format and a disabled write mask so the color result is discarded.
        let ct = wgpu::ColorTargetState {
            format: DEPTH_ONLY_DUMMY_COLOR_FORMAT,
            blend: None,
            write_mask: wgpu::ColorWrites::empty(),
        };
        color_targets.push(Some(ColorTargetKey {
            format: ct.format,
            blend: None,
            write_mask: ct.write_mask,
        }));
        color_target_states.push(Some(ct));
    } else {
        for &rt in &state.render_targets {
            let Some(rt) = rt else {
                color_targets.push(None);
                color_target_states.push(None);
                continue;
            };
            let tex = resources
                .textures
                .get(&rt)
                .ok_or_else(|| anyhow!("unknown render target texture {rt}"))?;
            let mut write_mask = state.color_write_mask;
            if aerogpu_format_is_x8(tex.format_u32) {
                // X8 formats treat alpha as always opaque; ignore alpha writes so shaders can't
                // accidentally stomp the stored alpha channel (wgpu uses RGBA/BGRA textures for X8
                // formats).
                write_mask &= !wgpu::ColorWrites::ALPHA;
            }
            let ct = wgpu::ColorTargetState {
                format: tex.desc.format,
                blend: state.blend,
                write_mask,
            };
            color_targets.push(Some(ColorTargetKey {
                format: ct.format,
                blend: ct.blend.map(Into::into),
                write_mask: ct.write_mask,
            }));
            color_target_states.push(Some(ct));
        }
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

    // The compute-prepass path expands input primitives into a "normal" vertex/index stream.
    // Adjacency and patch-list topologies are not directly representable in WebGPU render
    // pipelines, so we map them onto the closest non-emulated topology for the expanded draw.
    //
    // Note: The placeholder compute shader always outputs a triangle; full GS/HS/DS correctness
    // will replace this mapping as the emulation pipeline matures.
    let topology = match state.primitive_topology {
        CmdPrimitiveTopology::PointList => wgpu::PrimitiveTopology::PointList,
        CmdPrimitiveTopology::LineList | CmdPrimitiveTopology::LineListAdj => {
            wgpu::PrimitiveTopology::LineList
        }
        CmdPrimitiveTopology::LineStrip | CmdPrimitiveTopology::LineStripAdj => {
            wgpu::PrimitiveTopology::LineStrip
        }
        CmdPrimitiveTopology::TriangleList
        | CmdPrimitiveTopology::TriangleFan
        | CmdPrimitiveTopology::TriangleListAdj
        | CmdPrimitiveTopology::PatchList { .. } => wgpu::PrimitiveTopology::TriangleList,
        CmdPrimitiveTopology::TriangleStrip | CmdPrimitiveTopology::TriangleStripAdj => {
            wgpu::PrimitiveTopology::TriangleStrip
        }
    };
    let strip_index_format = match topology {
        wgpu::PrimitiveTopology::LineStrip | wgpu::PrimitiveTopology::TriangleStrip => {
            state.index_buffer.map(|ib| ib.format)
        }
        _ => None,
    };
    let key = RenderPipelineKey {
        vertex_shader,
        fragment_shader: ps_wgsl_hash,
        color_targets,
        depth_stencil: depth_stencil_key,
        primitive_topology: topology,
        strip_index_format,
        cull_mode: state.cull_mode,
        front_face: state.front_face,
        vertex_buffers: vertex_buffer_keys,
        sample_count: 1,
        layout: layout_key,
    };

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
                    strip_index_format,
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

fn get_or_create_render_pipeline_for_expanded_draw<'a>(
    device: &wgpu::Device,
    pipeline_cache: &'a mut PipelineCache,
    pipeline_layout: &wgpu::PipelineLayout,
    resources: &AerogpuD3d11Resources,
    state: &AerogpuD3d11State,
    layout_key: PipelineLayoutKey,
) -> Result<(RenderPipelineKey, &'a wgpu::RenderPipeline)> {
    let ps_handle = state
        .ps
        .ok_or_else(|| anyhow!("render draw without bound PS"))?;
    let ps = resources
        .shaders
        .get(&ps_handle)
        .ok_or_else(|| anyhow!("unknown PS shader {ps_handle}"))?;
    if ps.stage != ShaderStage::Pixel {
        bail!("shader {ps_handle} is not a pixel shader");
    }

    // Compile (and cache) the expanded-vertex passthrough VS.
    let (vs_base_hash, _module) = pipeline_cache.get_or_create_shader_module(
        device,
        map_pipeline_cache_stage(ShaderStage::Vertex),
        EXPANDED_DRAW_PASSTHROUGH_VS_WGSL,
        Some("aerogpu_cmd expanded passthrough VS"),
    );

    let vs_depth_clamp_hash = if !state.depth_clip_enabled {
        let clamped = wgsl_depth_clamp_variant(EXPANDED_DRAW_PASSTHROUGH_VS_WGSL);
        let (hash, _module) = pipeline_cache.get_or_create_shader_module(
            device,
            map_pipeline_cache_stage(ShaderStage::Vertex),
            &clamped,
            Some("aerogpu_cmd expanded passthrough VS (depth clamp)"),
        );
        Some(hash)
    } else {
        None
    };

    // Link VS/PS interfaces by trimming unused varyings (mirrors the normal pipeline path).
    let ps_declared_inputs = super::wgsl_link::locations_in_struct(&ps.wgsl_source, "PsIn")?;
    let vs_outputs =
        super::wgsl_link::locations_in_struct(EXPANDED_DRAW_PASSTHROUGH_VS_WGSL, "VsOut")?;

    let mut ps_link_locations = ps_declared_inputs.clone();
    let mut linked_ps_wgsl = std::borrow::Cow::Borrowed(ps.wgsl_source.as_str());

    let ps_missing_locations: BTreeSet<u32> = ps_declared_inputs
        .difference(&vs_outputs)
        .copied()
        .collect();
    if !ps_missing_locations.is_empty() {
        let ps_used_locations = super::wgsl_link::referenced_ps_input_locations(&ps.wgsl_source);
        let used_missing: Vec<u32> = ps_missing_locations
            .intersection(&ps_used_locations)
            .copied()
            .collect();
        if let Some(&loc) = used_missing.first() {
            bail!("pixel shader reads @location({loc}), but expanded VS does not output it");
        }
        ps_link_locations = ps_declared_inputs
            .intersection(&vs_outputs)
            .copied()
            .collect();
        if ps_link_locations != ps_declared_inputs {
            linked_ps_wgsl = std::borrow::Cow::Owned(super::wgsl_link::trim_ps_inputs_to_locations(
                linked_ps_wgsl.as_ref(),
                &ps_link_locations,
            ));
        }
    }

    // Trim fragment shader outputs to the currently-bound render target slots (skipping gaps).
    // This mirrors the main pipeline path: D3D discards writes to unbound RTVs, but WebGPU
    // requires outputs to have a matching `ColorTargetState`.
    let keep_output_locations: BTreeSet<u32> = state
        .render_targets
        .iter()
        .enumerate()
        .filter_map(|(idx, rt)| rt.is_some().then_some(idx as u32))
        .collect();
    let declared_outputs =
        super::wgsl_link::declared_ps_output_locations(linked_ps_wgsl.as_ref())?;
    let missing_outputs: BTreeSet<u32> = declared_outputs
        .difference(&keep_output_locations)
        .copied()
        .collect();
    if !missing_outputs.is_empty() {
        linked_ps_wgsl = std::borrow::Cow::Owned(super::wgsl_link::trim_ps_outputs_to_locations(
            linked_ps_wgsl.as_ref(),
            &keep_output_locations,
        ));
    }

    let fragment_shader = if linked_ps_wgsl.as_ref() == ps.wgsl_source.as_str() {
        ps.wgsl_hash
    } else {
        let (hash, _module) = pipeline_cache.get_or_create_shader_module(
            device,
            map_pipeline_cache_stage(ShaderStage::Pixel),
            linked_ps_wgsl.as_ref(),
            Some("aerogpu_cmd expanded linked pixel shader"),
        );
        hash
    };

    let needs_trim = vs_outputs != ps_link_locations;
    let selected_vs_hash = if state.depth_clip_enabled {
        vs_base_hash
    } else {
        vs_depth_clamp_hash.unwrap_or(vs_base_hash)
    };
    let vertex_shader = if !needs_trim {
        selected_vs_hash
    } else {
        let base_vs_wgsl = if state.depth_clip_enabled {
            std::borrow::Cow::Borrowed(EXPANDED_DRAW_PASSTHROUGH_VS_WGSL)
        } else {
            std::borrow::Cow::Owned(wgsl_depth_clamp_variant(EXPANDED_DRAW_PASSTHROUGH_VS_WGSL))
        };
        let trimmed_vs_wgsl = super::wgsl_link::trim_vs_outputs_to_locations(
            base_vs_wgsl.as_ref(),
            &ps_link_locations,
        );
        let (hash, _module) = pipeline_cache.get_or_create_shader_module(
            device,
            map_pipeline_cache_stage(ShaderStage::Vertex),
            &trimmed_vs_wgsl,
            Some("aerogpu_cmd expanded linked vertex shader"),
        );
        hash
    };

    // Expanded vertex buffer layout: (pos: vec4) + (o1: vec4).
    let vertex_buffer = VertexBufferLayoutOwned {
        array_stride: GEOMETRY_PREPASS_EXPANDED_VERTEX_STRIDE_BYTES,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: vec![
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x4,
                offset: 0,
                shader_location: 0,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x4,
                offset: 16,
                shader_location: 1,
            },
        ],
    };
    let vb_layout = vertex_buffer.as_wgpu();
    let vb_key: aero_gpu::pipeline_key::VertexBufferLayoutKey = (&vb_layout).into();

    let depth_only_pass =
        state.render_targets.iter().all(|rt| rt.is_none()) && state.depth_stencil.is_some();

    let mut color_targets = if depth_only_pass {
        Vec::with_capacity(1)
    } else {
        Vec::with_capacity(state.render_targets.len())
    };
    let mut color_target_states = if depth_only_pass {
        Vec::with_capacity(1)
    } else {
        Vec::with_capacity(state.render_targets.len())
    };

    if depth_only_pass {
        let ct = wgpu::ColorTargetState {
            format: DEPTH_ONLY_DUMMY_COLOR_FORMAT,
            blend: None,
            write_mask: wgpu::ColorWrites::empty(),
        };
        color_targets.push(Some(ColorTargetKey {
            format: ct.format,
            blend: None,
            write_mask: ct.write_mask,
        }));
        color_target_states.push(Some(ct));
    } else {
        for &rt in &state.render_targets {
            let Some(rt) = rt else {
                color_targets.push(None);
                color_target_states.push(None);
                continue;
            };
            let tex = resources
                .textures
                .get(&rt)
                .ok_or_else(|| anyhow!("unknown render target texture {rt}"))?;
            let mut write_mask = state.color_write_mask;
            if aerogpu_format_is_x8(tex.format_u32) {
                write_mask &= !wgpu::ColorWrites::ALPHA;
            }
            let ct = wgpu::ColorTargetState {
                format: tex.desc.format,
                blend: state.blend,
                write_mask,
            };
            color_targets.push(Some(ColorTargetKey {
                format: ct.format,
                blend: ct.blend.map(Into::into),
                write_mask: ct.write_mask,
            }));
            color_target_states.push(Some(ct));
        }
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

    // The compute prepass emits expanded triangle-list geometry regardless of the original D3D11
    // primitive topology (patchlists + adjacency are not directly expressible in WebGPU).
    let topology = wgpu::PrimitiveTopology::TriangleList;
    let key = RenderPipelineKey {
        vertex_shader,
        fragment_shader,
        color_targets,
        depth_stencil: depth_stencil_key,
        primitive_topology: topology,
        strip_index_format: None,
        cull_mode: state.cull_mode,
        front_face: state.front_face,
        vertex_buffers: vec![vb_key],
        sample_count: 1,
        layout: layout_key,
    };

    let cull_mode = state.cull_mode;
    let front_face = state.front_face;
    let depth_stencil_state_for_pipeline = depth_stencil_state.clone();

    let pipeline = pipeline_cache
        .get_or_create_render_pipeline(device, key.clone(), move |device, vs, fs| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("aerogpu_cmd expanded draw render pipeline"),
                layout: Some(pipeline_layout),
                vertex: wgpu::VertexState {
                    module: vs,
                    entry_point: "vs_main",
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[vb_layout],
                },
                fragment: Some(wgpu::FragmentState {
                    module: fs,
                    entry_point: ps.entry_point,
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

    Ok((key, pipeline))
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
            ShaderStage::Geometry => 3,
            ShaderStage::Hull => 4,
            ShaderStage::Domain => 5,
        }
}

fn group_index_to_stage(group: u32) -> Result<ShaderStage> {
    match group {
        0 => Ok(ShaderStage::Vertex),
        1 => Ok(ShaderStage::Pixel),
        2 => Ok(ShaderStage::Compute),
        3 => Ok(ShaderStage::Geometry),
        other => bail!("unsupported bind group index {other}"),
    }
}

#[allow(dead_code)]
fn d3d_topology_requires_gs_hs_ds_emulation(topology: u32) -> bool {
    // D3D11 topology values:
    // - 10..13: *_ADJ adjacency topologies (geometry shader input)
    // - 33..64: N_CONTROL_POINT_PATCHLIST (tessellation)
    matches!(topology, 10 | 11 | 12 | 13) || (33..=64).contains(&topology)
}

fn map_pipeline_cache_stage(stage: ShaderStage) -> aero_gpu::pipeline_key::ShaderStage {
    match stage {
        ShaderStage::Vertex => aero_gpu::pipeline_key::ShaderStage::Vertex,
        ShaderStage::Pixel => aero_gpu::pipeline_key::ShaderStage::Fragment,
        // Geometry/tessellation stages are lowered/emulated via compute.
        ShaderStage::Compute | ShaderStage::Geometry | ShaderStage::Hull | ShaderStage::Domain => {
            aero_gpu::pipeline_key::ShaderStage::Compute
        }
    }
}

fn extract_vs_input_signature_unique_locations(
    signatures: &crate::ShaderSignatures,
    module: &crate::Sm4Module,
) -> Result<Vec<VsInputSignatureElement>> {
    const D3D_NAME_VERTEX_ID: u32 = 6;
    const D3D_NAME_INSTANCE_ID: u32 = 8;

    let Some(isgn) = signatures.isgn.as_ref() else {
        return Ok(Vec::new());
    };

    let mut sivs = HashMap::<u32, u32>::new();
    for decl in &module.decls {
        if let crate::Sm4Decl::InputSiv { reg, sys_value, .. } = decl {
            sivs.insert(*reg, *sys_value);
        }
    }

    let mut out = Vec::new();
    let mut next_location = 0u32;
    for p in &isgn.parameters {
        let sys_value = sivs
            .get(&p.register)
            .copied()
            .or_else(|| (p.system_value_type != 0).then_some(p.system_value_type));

        let is_builtin = matches!(sys_value, Some(D3D_NAME_VERTEX_ID | D3D_NAME_INSTANCE_ID))
            || p.semantic_name.eq_ignore_ascii_case("SV_VertexID")
            || p.semantic_name.eq_ignore_ascii_case("SV_InstanceID");
        if is_builtin {
            continue;
        }

        out.push(VsInputSignatureElement {
            semantic_name_hash: fnv1a_32(p.semantic_name.to_ascii_uppercase().as_bytes()),
            semantic_index: p.semantic_index,
            input_register: p.register,
            mask: p.mask,
            shader_location: next_location,
        });
        next_location += 1;
    }

    Ok(out)
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
            mask: p.mask,
            shader_location: p.register,
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
            mask: 0xF,
            shader_location: reg,
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

fn map_buffer_usage_flags(flags: u32, supports_compute: bool) -> wgpu::BufferUsages {
    let mut usage = wgpu::BufferUsages::COPY_DST;
    let mut needs_storage = flags & AEROGPU_RESOURCE_USAGE_STORAGE != 0;
    if flags & AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER != 0 {
        usage |= wgpu::BufferUsages::VERTEX;
        needs_storage = true;
    }
    if flags & AEROGPU_RESOURCE_USAGE_INDEX_BUFFER != 0 {
        usage |= wgpu::BufferUsages::INDEX;
        needs_storage = true;
    }
    // Compute-based GS emulation (vertex pulling + expansion) needs to read IA buffers via storage
    // bindings. Gate this on backend support; downlevel/WebGL2 backends do not support compute and
    // storage buffers.
    if supports_compute && needs_storage {
        usage |= wgpu::BufferUsages::STORAGE;
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
    // NOTE: `AEROGPU_RESOURCE_USAGE_STORAGE` is currently ignored for textures because SM5 storage
    // textures (UAVs) are not yet implemented in the command-stream executor.
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
const AEROGPU_FORMAT_B5G6R5_UNORM: u32 = AerogpuFormat::B5G6R5Unorm as u32;
const AEROGPU_FORMAT_B5G5R5A1_UNORM: u32 = AerogpuFormat::B5G5R5A1Unorm as u32;
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
        // wgpu/WebGPU does not expose 16-bit packed B5 formats; represent them as RGBA8 and
        // expand/pack on CPU upload/writeback paths.
        AEROGPU_FORMAT_B5G6R5_UNORM | AEROGPU_FORMAT_B5G5R5A1_UNORM => {
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
enum AerogpuB5Format {
    B5G6R5,
    B5G5R5A1,
}

fn aerogpu_b5_format(format_u32: u32) -> Option<AerogpuB5Format> {
    match format_u32 {
        AEROGPU_FORMAT_B5G6R5_UNORM => Some(AerogpuB5Format::B5G6R5),
        AEROGPU_FORMAT_B5G5R5A1_UNORM => Some(AerogpuB5Format::B5G5R5A1),
        _ => None,
    }
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
    Uncompressed {
        bytes_per_texel: u32,
    },
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
        AEROGPU_FORMAT_B5G6R5_UNORM | AEROGPU_FORMAT_B5G5R5A1_UNORM => {
            AerogpuTextureFormatLayout::Uncompressed { bytes_per_texel: 2 }
        }

        AEROGPU_FORMAT_B8G8R8A8_UNORM
        | AEROGPU_FORMAT_B8G8R8X8_UNORM
        | AEROGPU_FORMAT_R8G8B8A8_UNORM
        | AEROGPU_FORMAT_R8G8B8X8_UNORM
        | AEROGPU_FORMAT_B8G8R8A8_UNORM_SRGB
        | AEROGPU_FORMAT_B8G8R8X8_UNORM_SRGB
        | AEROGPU_FORMAT_R8G8B8A8_UNORM_SRGB
        | AEROGPU_FORMAT_R8G8B8X8_UNORM_SRGB
        | AEROGPU_FORMAT_D24_UNORM_S8_UINT
        | AEROGPU_FORMAT_D32_FLOAT => {
            AerogpuTextureFormatLayout::Uncompressed { bytes_per_texel: 4 }
        }

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
    // WRITEBACK_DST supports formats we can materialize in a CPU-visible staging buffer and write
    // back into guest memory.
    //
    // For 32bpp formats the staging bytes are written back directly (optionally forcing alpha to
    // opaque for X8 formats). For 16bpp packed B5 formats we read back RGBA8 and pack to the guest
    // format.
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
            | AEROGPU_FORMAT_B5G6R5_UNORM
            | AEROGPU_FORMAT_B5G5R5A1_UNORM
    )
}

fn force_opaque_alpha_rgba8(pixels: &mut [u8]) {
    for alpha in pixels.iter_mut().skip(3).step_by(4) {
        *alpha = 0xFF;
    }
}

fn expand_b5_texture_to_rgba8(
    b5_format: AerogpuB5Format,
    width: u32,
    height: u32,
    src_bytes_per_row: u32,
    src: &[u8],
) -> Result<Vec<u8>> {
    let src_unpadded_bpr = width
        .checked_mul(2)
        .ok_or_else(|| anyhow!("B5 expand: bytes_per_row overflow"))?;
    if src_bytes_per_row < src_unpadded_bpr {
        bail!("B5 expand: bytes_per_row too small");
    }

    let dst_unpadded_bpr = width
        .checked_mul(4)
        .ok_or_else(|| anyhow!("B5 expand: expanded bytes_per_row overflow"))?;

    let src_bpr_usize: usize = src_bytes_per_row
        .try_into()
        .map_err(|_| anyhow!("B5 expand: bytes_per_row out of range"))?;
    let src_unpadded_bpr_usize: usize = src_unpadded_bpr
        .try_into()
        .map_err(|_| anyhow!("B5 expand: bytes_per_row out of range"))?;
    let dst_bpr_usize: usize = dst_unpadded_bpr
        .try_into()
        .map_err(|_| anyhow!("B5 expand: bytes_per_row out of range"))?;
    let height_usize: usize = height
        .try_into()
        .map_err(|_| anyhow!("B5 expand: height out of range"))?;

    let required_src_len = src_bpr_usize
        .checked_mul(height_usize)
        .ok_or_else(|| anyhow!("B5 expand: src size overflow"))?;
    if src.len() < required_src_len {
        bail!(
            "B5 expand: source too small: need {required_src_len} bytes, got {}",
            src.len()
        );
    }

    let out_len = dst_bpr_usize
        .checked_mul(height_usize)
        .ok_or_else(|| anyhow!("B5 expand: dst size overflow"))?;
    let mut out = vec![0u8; out_len];

    for row in 0..height_usize {
        let src_start = row
            .checked_mul(src_bpr_usize)
            .ok_or_else(|| anyhow!("B5 expand: src row offset overflow"))?;
        let src_end = src_start
            .checked_add(src_unpadded_bpr_usize)
            .ok_or_else(|| anyhow!("B5 expand: src row end overflow"))?;
        let dst_start = row
            .checked_mul(dst_bpr_usize)
            .ok_or_else(|| anyhow!("B5 expand: dst row offset overflow"))?;
        let dst_end = dst_start
            .checked_add(dst_bpr_usize)
            .ok_or_else(|| anyhow!("B5 expand: dst row end overflow"))?;

        let src_row = src.get(src_start..src_end).ok_or_else(|| {
            anyhow!("B5 expand: source too small for row {row} (start={src_start} end={src_end})")
        })?;
        let dst_row = out.get_mut(dst_start..dst_end).ok_or_else(|| {
            anyhow!(
                "B5 expand: dst buffer too small for row {row} (start={dst_start} end={dst_end})"
            )
        })?;

        match b5_format {
            AerogpuB5Format::B5G6R5 => expand_b5g6r5_unorm_to_rgba8(src_row, dst_row),
            AerogpuB5Format::B5G5R5A1 => expand_b5g5r5a1_unorm_to_rgba8(src_row, dst_row),
        }
    }

    Ok(out)
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
fn write_texture_layout(
    format: wgpu::TextureFormat,
    width: u32,
    height: u32,
) -> Result<(u32, u32)> {
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

#[derive(Debug, Clone, Copy)]
struct Texture2dSubresourceDesc {
    desc: Texture2dDesc,
    mip_level: u32,
    array_layer: u32,
}
fn write_texture_subresource_linear(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    subresource: Texture2dSubresourceDesc,
    src_bytes_per_row: u32,
    bytes: &[u8],
    force_opaque_alpha: bool,
) -> Result<()> {
    let desc = &subresource.desc;
    let mip_extent = |v: u32, level: u32| v.checked_shr(level).unwrap_or(0).max(1);
    let mip_w = mip_extent(desc.width, subresource.mip_level);
    let mip_h = mip_extent(desc.height, subresource.mip_level);

    let (unpadded_bpr, layout_rows) = write_texture_layout(desc.format, mip_w, mip_h)?;
    let (extent_width, extent_height) = if bc_block_bytes(desc.format).is_some() {
        // WebGPU requires BC uploads to use the physical (block-rounded) size, even when the mip
        // itself is smaller than one block (e.g. 2x2 still copies as 4x4).
        (align_to(mip_w, 4)?, align_to(mip_h, 4)?)
    } else {
        (mip_w, mip_h)
    };
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
                row_bytes.copy_from_slice(&bytes[src_start..src_start + unpadded_bpr as usize]);
                if force_opaque_alpha {
                    force_opaque_alpha_rgba8(row_bytes);
                }
            }
            queue.write_texture(
                wgpu::ImageCopyTexture {
                    texture,
                    mip_level: subresource.mip_level,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: subresource.array_layer,
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
                    width: extent_width,
                    height: extent_height,
                    depth_or_array_layers: 1,
                },
            );
        } else {
            queue.write_texture(
                wgpu::ImageCopyTexture {
                    texture,
                    mip_level: subresource.mip_level,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: subresource.array_layer,
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
                    width: extent_width,
                    height: extent_height,
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
            repacked[..unpadded_bpr as usize].copy_from_slice(&bytes[..unpadded_bpr as usize]);
            force_opaque_alpha_rgba8(&mut repacked[..unpadded_bpr as usize]);
            queue.write_texture(
                wgpu::ImageCopyTexture {
                    texture,
                    mip_level: subresource.mip_level,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: subresource.array_layer,
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
                    width: extent_width,
                    height: extent_height,
                    depth_or_array_layers: 1,
                },
            );
        } else {
            queue.write_texture(
                wgpu::ImageCopyTexture {
                    texture,
                    mip_level: subresource.mip_level,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: subresource.array_layer,
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
                    width: extent_width,
                    height: extent_height,
                    depth_or_array_layers: 1,
                },
            );
        }
    }

    Ok(())
}

#[allow(dead_code)]
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
        Texture2dSubresourceDesc {
            desc,
            mip_level: 0,
            array_layer: 0,
        },
        src_bytes_per_row,
        bytes,
        force_opaque_alpha,
    )
}

fn validate_sm5_gs_streams(program: &Sm4Program) -> Result<()> {
    // DXBC encodes SM4/SM5 shaders as a stream of DWORD tokens. We only need to recognize the
    // `emit_stream` / `cut_stream` instruction forms, which carry the stream index as an
    // immediate32 operand.
    //
    // Keep this scan token-level (rather than using the full decoder) so we can detect unsupported
    // multi-stream usage even when other parts of the shader are not yet decodable/translateable.
    let declared_len = program.tokens.get(1).copied().unwrap_or(0) as usize;
    if declared_len < 2 || declared_len > program.tokens.len() {
        // Malformed token stream; the real decoder will report a better error later.
        return Ok(());
    }

    let toks = &program.tokens[..declared_len];
    let mut i = 2usize;
    while i < toks.len() {
        let opcode_token = toks[i];
        let opcode = opcode_token & sm4_opcode::OPCODE_MASK;
        let len = ((opcode_token >> sm4_opcode::OPCODE_LEN_SHIFT) & sm4_opcode::OPCODE_LEN_MASK)
            as usize;
        if len == 0 || i + len > toks.len() {
            // Malformed instruction; let downstream decode/translation surface the issue.
            return Ok(());
        }

        let stream_opcode_name = if opcode == sm4_opcode::OPCODE_EMIT_STREAM {
            Some("emit_stream")
        } else if opcode == sm4_opcode::OPCODE_CUT_STREAM {
            Some("cut_stream")
        } else {
            None
        };

        if let Some(op_name) = stream_opcode_name {
            // `emit_stream` / `cut_stream` take exactly one operand: an immediate32 scalar
            // (replicated lanes) indicating the stream index.
            //
            // Skip any extended opcode tokens to find the operand token.
            let inst_end = i + len;
            let mut operand_pos = i + 1;
            let mut extended = (opcode_token & sm4_opcode::OPCODE_EXTENDED_BIT) != 0;
            while extended {
                if operand_pos >= inst_end {
                    return Ok(());
                }
                let Some(ext) = toks.get(operand_pos).copied() else {
                    return Ok(());
                };
                operand_pos += 1;
                extended = (ext & sm4_opcode::OPCODE_EXTENDED_BIT) != 0;
            }

            if operand_pos >= inst_end {
                return Ok(());
            }
            let Some(operand_token) = toks.get(operand_pos).copied() else {
                return Ok(());
            };
            operand_pos += 1;

            let ty = (operand_token >> sm4_opcode::OPERAND_TYPE_SHIFT) & sm4_opcode::OPERAND_TYPE_MASK;
            if ty != sm4_opcode::OPERAND_TYPE_IMMEDIATE32 {
                // Malformed stream operand; the decoder will surface a better error later.
                return Ok(());
            }

            // Skip extended operand tokens (modifiers).
            let mut operand_ext = (operand_token & sm4_opcode::OPERAND_EXTENDED_BIT) != 0;
            while operand_ext {
                if operand_pos >= inst_end {
                    return Ok(());
                }
                let Some(ext) = toks.get(operand_pos).copied() else {
                    return Ok(());
                };
                operand_pos += 1;
                operand_ext = (ext & sm4_opcode::OPERAND_EXTENDED_BIT) != 0;
            }

            // Immediate operands should have no indices, but if they do, bail out and let the real
            // decoder handle it.
            let index_dim = (operand_token >> sm4_opcode::OPERAND_INDEX_DIMENSION_SHIFT)
                & sm4_opcode::OPERAND_INDEX_DIMENSION_MASK;
            if index_dim != sm4_opcode::OPERAND_INDEX_DIMENSION_0D {
                return Ok(());
            }

            let num_components = operand_token & sm4_opcode::OPERAND_NUM_COMPONENTS_MASK;
            let stream = match num_components {
                // Scalar immediate (1 DWORD payload).
                1 => {
                    if operand_pos >= inst_end {
                        return Ok(());
                    }
                    toks[operand_pos]
                }
                // 4-component immediate (4 DWORD payload); `decode_stream_index` uses lane 0.
                2 => {
                    if operand_pos + 3 >= inst_end {
                        return Ok(());
                    }
                    toks[operand_pos]
                }
                _ => return Ok(()),
            };
            if stream != 0 {
                bail!(
                    "CREATE_SHADER_DXBC: unsupported {op_name} stream index {stream} (only stream 0 is supported)"
                );
            }
        }

        i += len;
    }

    Ok(())
}

fn sm5_gs_instance_count(program: &Sm4Program) -> Option<u32> {
    // SM5 geometry shader instancing (`dcl_gsinstancecount` / `[instance(n)]`) is encoded as a
    // declaration opcode with a single immediate payload DWORD.
    //
    // Keep this token-level (rather than using the full decoder) so we can recover the instance
    // count even if other parts of the GS token stream are not yet decodeable/translateable.
    let declared_len = program.tokens.get(1).copied()? as usize;
    if declared_len < 2 || declared_len > program.tokens.len() {
        return None;
    }

    let toks = &program.tokens[..declared_len];
    let mut i = 2usize;
    while i < toks.len() {
        let opcode_token = toks[i];
        let opcode = opcode_token & sm4_opcode::OPCODE_MASK;
        let len =
            ((opcode_token >> sm4_opcode::OPCODE_LEN_SHIFT) & sm4_opcode::OPCODE_LEN_MASK) as usize;
        if len == 0 || i + len > toks.len() {
            return None;
        }

        if opcode == sm4_opcode::OPCODE_DCL_GS_INSTANCE_COUNT {
            let inst_end = i + len;
            let mut pos = i + 1;
            let mut extended = (opcode_token & sm4_opcode::OPCODE_EXTENDED_BIT) != 0;
            while extended {
                if pos >= inst_end {
                    return None;
                }
                let ext = toks[pos];
                pos += 1;
                extended = (ext & sm4_opcode::OPCODE_EXTENDED_BIT) != 0;
            }
            if pos >= inst_end {
                return None;
            }
            return toks.get(pos).copied();
        }

        i += len;
    }

    None
}

fn try_translate_sm4_signature_driven(
    dxbc: &DxbcFile<'_>,
    program: &Sm4Program,
    signatures: &crate::ShaderSignatures,
) -> Result<ShaderTranslation> {
    let module = crate::sm4::decode_program(program).context("decode SM4/5 token stream")?;
    translate_sm4_module_to_wgsl(dxbc, &module, signatures)
        .context("signature-driven SM4/5 translation")
}

fn align4(len: usize) -> usize {
    (len + 3) & !3
}

fn align_to(value: u32, alignment: u32) -> Result<u32> {
    if alignment == 0 || !alignment.is_power_of_two() {
        bail!("align_to: alignment must be a non-zero power of two (got {alignment})");
    }
    let mask = alignment - 1;
    value
        .checked_add(mask)
        .map(|v| v & !mask)
        .ok_or_else(|| anyhow!("align_to: value overflows u32"))
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
    use aero_gpu::guest_memory::VecGuestMemory;
    use aero_gpu::pipeline_key::{ComputePipelineKey, PipelineLayoutKey};
    use aero_gpu::GpuError;
    use aero_protocol::aerogpu::aerogpu_cmd::{
        AerogpuCmdOpcode, AerogpuPrimitiveTopology, AerogpuShaderStage,
    };
    use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;
    use std::sync::Arc;

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
    fn depth_clamp_variant_clamps_gs_passthrough_position() {
        // GS emulation renders from a host-generated vertex buffer using an internal passthrough
        // VS. Depth clipping must be emulated *after* GS, so verify that the VS rewrite inserts
        // the clip-space depth clamp next to the position assignment.
        let clamped = wgsl_depth_clamp_variant(EXPANDED_DRAW_PASSTHROUGH_VS_WGSL);

        let assign_idx = clamped
            .find("out.pos =")
            .expect("passthrough VS should assign out.pos");
        let clamp_idx = clamped
            .find("out.pos.z = clamp(out.pos.z, 0.0, out.pos.w);")
            .expect("depth clamp variant should inject z clamp");
        assert!(
            clamp_idx > assign_idx,
            "depth clamp must appear after the out.pos assignment"
        );
    }

    #[test]
    fn patchlist_topology_is_accepted_and_draw_runs_through_emulation() {
        pollster::block_on(async {
            let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
                Ok(exec) => exec,
                Err(e) => {
                    skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                    return;
                }
            };

            if !exec.caps.supports_compute || !exec.caps.supports_indirect_execution {
                skip_or_panic(
                    module_path!(),
                    "backend lacks compute/indirect execution required for GS/HS/DS emulation",
                );
                return;
            }

            const VS_PASSTHROUGH: &[u8] =
                include_bytes!("../../tests/fixtures/vs_passthrough.dxbc");

            fn build_dxbc(chunks: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
                let chunk_count: u32 = chunks
                    .len()
                    .try_into()
                    .expect("chunk count should fit in u32");

                let header_size = 4 + 16 + 4 + 4 + 4 + 4 * chunks.len();
                let mut offsets = Vec::with_capacity(chunks.len());
                let mut cursor = header_size;
                for (_fourcc, data) in chunks {
                    offsets.push(cursor);
                    cursor += 8 + align4(data.len());
                }

                let total_size: u32 = cursor.try_into().expect("dxbc size should fit in u32");

                let mut bytes = Vec::with_capacity(cursor);
                bytes.extend_from_slice(b"DXBC");
                bytes.extend_from_slice(&[0u8; 16]); // checksum (ignored)
                bytes.extend_from_slice(&1u32.to_le_bytes()); // "one"
                bytes.extend_from_slice(&total_size.to_le_bytes());
                bytes.extend_from_slice(&chunk_count.to_le_bytes());
                for offset in offsets {
                    bytes.extend_from_slice(&(offset as u32).to_le_bytes());
                }

                for (fourcc, data) in chunks {
                    bytes.extend_from_slice(fourcc);
                    bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
                    bytes.extend_from_slice(data);
                    bytes.resize(bytes.len() + (align4(data.len()) - data.len()), 0);
                }
                bytes
            }

            #[derive(Clone, Copy)]
            struct SigParam {
                semantic_name: &'static str,
                semantic_index: u32,
                register: u32,
                mask: u8,
            }

            fn build_signature_chunk(params: &[SigParam]) -> Vec<u8> {
                // Mirrors `aero_d3d11::signature::parse_signature_chunk` expectations.
                let mut out = Vec::new();
                out.extend_from_slice(&(params.len() as u32).to_le_bytes()); // param_count
                out.extend_from_slice(&8u32.to_le_bytes()); // param_offset

                let entry_size = 24usize;
                let table_start = out.len();
                out.resize(table_start + params.len() * entry_size, 0);

                for (i, p) in params.iter().enumerate() {
                    let semantic_name_offset = out.len() as u32;
                    out.extend_from_slice(p.semantic_name.as_bytes());
                    out.push(0);
                    while out.len() % 4 != 0 {
                        out.push(0);
                    }

                    let base = table_start + i * entry_size;
                    out[base..base + 4].copy_from_slice(&semantic_name_offset.to_le_bytes());
                    out[base + 4..base + 8].copy_from_slice(&p.semantic_index.to_le_bytes());
                    out[base + 8..base + 12].copy_from_slice(&0u32.to_le_bytes()); // system_value_type
                    out[base + 12..base + 16].copy_from_slice(&0u32.to_le_bytes()); // component_type
                    out[base + 16..base + 20].copy_from_slice(&p.register.to_le_bytes());
                    out[base + 20] = p.mask;
                    out[base + 21] = p.mask; // read_write_mask
                    out[base + 22] = 0; // stream
                    out[base + 23] = 0; // min_precision
                }

                out
            }

            fn tokens_to_bytes(tokens: &[u32]) -> Vec<u8> {
                let mut out = Vec::with_capacity(tokens.len() * 4);
                for &t in tokens {
                    out.extend_from_slice(&t.to_le_bytes());
                }
                out
            }

            fn build_ps_solid_green_dxbc() -> Vec<u8> {
                // ps_4_0: mov o0, l(0,1,0,1); ret
                let isgn = build_signature_chunk(&[]);
                let osgn = build_signature_chunk(&[SigParam {
                    semantic_name: "SV_Target",
                    semantic_index: 0,
                    register: 0,
                    mask: 0x0f,
                }]);

                let version_token = 0x40u32; // ps_4_0
                let mov_token = 0x01u32 | (8u32 << 11);
                let ret_token = 0x3eu32 | (1u32 << 11);

                let dst_o0 = 0x0010_f022u32;
                let imm_vec4 = 0x0000_f042u32;

                let zero = 0.0f32.to_bits();
                let one = 1.0f32.to_bits();

                let mut tokens = vec![
                    version_token,
                    0, // length patched below
                    mov_token,
                    dst_o0,
                    0, // o0 index
                    imm_vec4,
                    zero,
                    one,
                    zero,
                    one,
                    ret_token,
                ];
                tokens[1] = tokens.len() as u32;

                let shdr = tokens_to_bytes(&tokens);
                build_dxbc(&[(*b"ISGN", isgn), (*b"OSGN", osgn), (*b"SHDR", shdr)])
            }

            // Draw via the patchlist topology (tessellation input). Real HS/DS emulation isn't
            // implemented yet, but the executor should route patchlist draws through the compute
            // prepass plumbing and emit placeholder geometry.
            const RT: u32 = 1;
            const VS: u32 = 2;
            const PS: u32 = 3;

            let w = 64u32;
            let h = 64u32;
            let mut writer = AerogpuCmdWriter::new();
            writer.create_texture2d(
                RT,
                AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
                AerogpuFormat::R8G8B8A8Unorm as u32,
                w,
                h,
                1,
                1,
                0,
                0,
                0,
            );
            writer.set_render_targets(&[RT], 0);
            writer.set_viewport(0.0, 0.0, w as f32, h as f32, 0.0, 1.0);

            writer.create_shader_dxbc(VS, AerogpuShaderStage::Vertex, VS_PASSTHROUGH);
            writer.create_shader_dxbc(PS, AerogpuShaderStage::Pixel, &build_ps_solid_green_dxbc());
            writer.bind_shaders(VS, PS, 0);

            writer.set_primitive_topology(AerogpuPrimitiveTopology::PatchList3);
            writer.clear(AEROGPU_CLEAR_COLOR, [1.0, 0.0, 0.0, 1.0], 1.0, 0);
            writer.draw(3, 1, 0, 0);
            let stream = writer.finish();

            let mut guest_mem = VecGuestMemory::new(0);
            exec.execute_cmd_stream(&stream, None, &mut guest_mem)
                .expect("execute_cmd_stream should succeed");
            exec.poll_wait();

            let pixels = exec
                .read_texture_rgba8(RT)
                .await
                .expect("readback should succeed");
            assert_eq!(pixels.len(), (w * h * 4) as usize);

            let px = |x: u32, y: u32| -> [u8; 4] {
                let idx = ((y * w + x) * 4) as usize;
                pixels[idx..idx + 4].try_into().unwrap()
            };

            // Placeholder triangle is centered and does not cover the top-left corner.
            assert_eq!(px(0, 0), [255, 0, 0, 255]);
            // The center pixel should be covered by the triangle and shaded green by the PS.
            assert_eq!(px(w / 2, h / 2), [0, 255, 0, 255]);

            assert_eq!(
                exec.state.primitive_topology,
                CmdPrimitiveTopology::PatchList { control_points: 3 }
            );
        });
    }

    #[test]
    fn pipeline_cache_compute_support_is_derived_from_downlevel_flags() {
        pollster::block_on(async {
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
            };
            let Some(adapter) = adapter else {
                skip_or_panic(
                    module_path!(),
                    "wgpu unavailable (no suitable adapter found)",
                );
                return;
            };

            let expected_supports_compute = adapter
                .get_downlevel_capabilities()
                .flags
                .contains(wgpu::DownlevelFlags::COMPUTE_SHADERS);
            let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
                Ok(exec) => exec,
                Err(e) => {
                    skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                    return;
                }
            };

            // `GpuCapabilities::from_device` cannot determine compute support from a `wgpu::Device`
            // alone (notably WebGL2 has no compute stage), so it defaults `supports_compute=false`.
            // `new_for_tests` must override this using adapter downlevel flags so that
            // `PipelineCache` enables/disables compute pipelines deterministically across backends.
            let shader_hash = 0x1234_u128;
            let key = ComputePipelineKey {
                shader: shader_hash,
                layout: PipelineLayoutKey::empty(),
            };
            let err = exec
                .pipeline_cache
                .get_or_create_compute_pipeline(&exec.device, key, |_device, _cs| unreachable!())
                .unwrap_err();

            match (expected_supports_compute, err) {
                (false, GpuError::Unsupported("compute")) => {}
                (
                    true,
                    GpuError::MissingShaderModule {
                        stage: aero_gpu::pipeline_key::ShaderStage::Compute,
                        hash,
                    },
                ) if hash == shader_hash => {}
                (expected, err) => panic!(
                    "expected supports_compute={expected} based on DownlevelFlags, got error {err:?}"
                ),
            }
        });
    }

    #[test]
    fn adjacency_topology_is_accepted_and_draw_runs_through_emulation() {
        pollster::block_on(async {
            let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
                Ok(exec) => exec,
                Err(e) => {
                    skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                    return;
                }
            };

            if !exec.caps.supports_compute || !exec.caps.supports_indirect_execution {
                skip_or_panic(
                    module_path!(),
                    "backend lacks compute/indirect execution required for GS/HS/DS emulation",
                );
                return;
            }

            // Build a minimal stream that draws with a D3D11 adjacency topology. This is not
            // directly representable in WebGPU pipelines, so it must route through the emulation
            // compute-prepass + indirect draw path.
            const RT: u32 = 1;
            const VS: u32 = 2;
            const PS: u32 = 3;

            const DXBC_VS_PASSTHROUGH: &[u8] =
                include_bytes!("../../tests/fixtures/vs_passthrough.dxbc");
            const DXBC_PS_PASSTHROUGH: &[u8] =
                include_bytes!("../../tests/fixtures/ps_passthrough.dxbc");

            let mut writer = AerogpuCmdWriter::new();
            writer.create_texture2d(
                RT,
                AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
                AerogpuFormat::R8G8B8A8Unorm as u32,
                1,
                1,
                1,
                1,
                0,
                0,
                0,
            );
            writer.set_render_targets(&[RT], 0);
            writer.set_viewport(0.0, 0.0, 1.0, 1.0, 0.0, 1.0);
            writer.clear(AEROGPU_CLEAR_COLOR, [0.0, 0.0, 0.0, 1.0], 1.0, 0);
            writer.create_shader_dxbc(VS, AerogpuShaderStage::Vertex, DXBC_VS_PASSTHROUGH);
            writer.create_shader_dxbc(PS, AerogpuShaderStage::Pixel, DXBC_PS_PASSTHROUGH);
            writer.bind_shaders(VS, PS, 0);
            writer.set_primitive_topology(AerogpuPrimitiveTopology::TriangleListAdj);
            writer.draw(6, 1, 0, 0);
            let stream = writer.finish();

            let mut guest_mem = VecGuestMemory::new(0);
            exec.execute_cmd_stream(&stream, None, &mut guest_mem)
                .expect("adjacency draw should succeed through emulation path");
            exec.poll_wait();

            let pixels = exec
                .read_texture_rgba8(RT)
                .await
                .expect("readback should succeed");
            assert_eq!(&pixels[0..4], &[255, 0, 0, 255]);

            assert_eq!(exec.state.primitive_topology, CmdPrimitiveTopology::TriangleListAdj);
        });
    }

    #[test]
    fn expanded_draw_pipeline_trims_unbound_pixel_shader_outputs() {
        // Regression test: the expanded-draw (GS prepass) path must apply the same MRT output
        // trimming as the normal pipeline builder.
        //
        // wgpu/WebGPU requires that every `@location(N)` output has a matching `ColorTargetState`.
        // D3D discards writes to unbound targets.
        pollster::block_on(async {
            let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
                Ok(exec) => exec,
                Err(e) => {
                    skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                    return;
                }
            };

            // Minimal render target so the expanded pipeline has only one color target (slot 0).
            const RT: u32 = 1;
            let tex = exec.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("expanded_draw mrt trim RT"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
            exec.resources.textures.insert(
                RT,
                Texture2dResource {
                    texture: tex,
                    view,
                    desc: Texture2dDesc {
                        width: 1,
                        height: 1,
                        mip_level_count: 1,
                        array_layers: 1,
                        format: wgpu::TextureFormat::Rgba8Unorm,
                    },
                    format_u32: aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat::R8G8B8A8Unorm
                        as u32,
                    backing: None,
                     row_pitch_bytes: 0,
                     dirty: false,
                     guest_backing_is_current: true,
                     host_shadow: None,
                     host_shadow_valid: Vec::new(),
                 },
             );

            // Pixel shader writes to @location(0) and @location(2), but only RT0 is bound.
            const PS: u32 = 2;
            let fs_wgsl = r#"
                struct PsOut {
                    @location(0) o0: vec4<f32>,
                    @location(2) o2: vec4<f32>,
                };

                @fragment
                fn fs_main() -> PsOut {
                    var out: PsOut;
                    out.o0 = vec4<f32>(1.0, 0.0, 0.0, 1.0);
                    out.o2 = vec4<f32>(0.0, 1.0, 0.0, 1.0);
                    return out;
                }
            "#;

            let (ps_hash, _module) = exec.pipeline_cache.get_or_create_shader_module(
                &exec.device,
                map_pipeline_cache_stage(ShaderStage::Pixel),
                fs_wgsl,
                Some("expanded_draw mrt trim PS"),
            );
            exec.resources.shaders.insert(
                PS,
                ShaderResource {
                    stage: ShaderStage::Pixel,
                    wgsl_hash: ps_hash,
                    depth_clamp_wgsl_hash: None,
                    dxbc_hash_fnv1a64: 0,
                    entry_point: "fs_main",
                    vs_input_signature: Vec::new(),
                    reflection: ShaderReflection::default(),
                    wgsl_source: fs_wgsl.to_owned(),
                },
            );

            exec.state.ps = Some(PS);
            exec.state.render_targets = vec![Some(RT)];
            exec.state.depth_stencil = None;

            let layout_key = PipelineLayoutKey::empty();
            let pipeline_layout = exec
                .device
                .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("expanded_draw mrt trim pipeline layout"),
                    bind_group_layouts: &[],
                    push_constant_ranges: &[],
                });

            exec.device.push_error_scope(wgpu::ErrorFilter::Validation);
            let res = super::get_or_create_render_pipeline_for_expanded_draw(
                &exec.device,
                &mut exec.pipeline_cache,
                &pipeline_layout,
                &exec.resources,
                &exec.state,
                layout_key,
            );
            exec.device.poll(wgpu::Maintain::Wait);
            let err = exec.device.pop_error_scope().await;

            assert!(
                err.is_none(),
                "unexpected wgpu validation error while creating expanded draw pipeline: {err:?}"
            );
            res.expect("expanded draw pipeline creation should succeed with trimmed PS outputs");
        });
    }

    #[test]
    fn pipeline_trims_pixel_shader_outputs_for_unbound_gap_targets() {
        // Regression test: apps may bind RTV0 and RTV2 while leaving RTV1 unbound (gap).
        //
        // D3D discards writes to unbound targets, but WebGPU requires that every
        // `@location(N)` output has a matching `ColorTargetState` (Some) at index N.
        pollster::block_on(async {
            let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
                Ok(exec) => exec,
                Err(e) => {
                    skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                    return;
                }
            };

            // RT0 + RT2 bound, RT1 is an explicit gap.
            const RT0: u32 = 1;
            const RT2: u32 = 2;
            for (handle, label) in [(RT0, "rt0"), (RT2, "rt2")] {
                let tex = exec.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some(&format!("gap mrt trim {label}")),
                    size: wgpu::Extent3d {
                        width: 1,
                        height: 1,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                    view_formats: &[],
                });
                let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
                exec.resources.textures.insert(
                    handle,
                    Texture2dResource {
                        texture: tex,
                        view,
                        desc: Texture2dDesc {
                            width: 1,
                            height: 1,
                            mip_level_count: 1,
                            array_layers: 1,
                            format: wgpu::TextureFormat::Rgba8Unorm,
                        },
                        format_u32: aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat::R8G8B8A8Unorm
                            as u32,
                        backing: None,
                        row_pitch_bytes: 0,
                         dirty: false,
                         guest_backing_is_current: true,
                         host_shadow: None,
                         host_shadow_valid: Vec::new(),
                     },
                 );
             }

            // Minimal input layout so pipeline creation succeeds. This VS uses only builtins, so we
            // don't need any actual vertex attributes.
            const ILAY: u32 = 3;
            exec.resources.input_layouts.insert(
                ILAY,
                InputLayoutResource {
                    layout: InputLayoutDesc {
                        header: crate::input_layout::InputLayoutBlobHeader {
                            magic: crate::input_layout::AEROGPU_INPUT_LAYOUT_BLOB_MAGIC,
                            version: crate::input_layout::AEROGPU_INPUT_LAYOUT_BLOB_VERSION,
                            element_count: 0,
                        },
                        elements: Vec::new(),
                    },
                    used_slots: Vec::new(),
                    mapping_cache: HashMap::new(),
                },
            );

            const VS: u32 = 10;
            const PS: u32 = 11;

            let vs_wgsl = r#"
                struct VsOut {
                    @builtin(position) pos: vec4<f32>,
                };

                @vertex
                fn vs_main(@builtin(vertex_index) index: u32) -> VsOut {
                    _ = index;
                    var out: VsOut;
                    out.pos = vec4<f32>(0.0, 0.0, 0.0, 1.0);
                    return out;
                }
            "#;

            // Pixel shader writes to @location(1), but RT1 is unbound (gap). The runtime must trim
            // this output for WebGPU pipeline creation to succeed.
            let fs_wgsl = r#"
                struct PsOut {
                    @location(0) o0: vec4<f32>,
                    @location(1) o1: vec4<f32>,
                    @location(2) o2: vec4<f32>,
                };

                @fragment
                fn fs_main() -> PsOut {
                    var out: PsOut;
                    out.o0 = vec4<f32>(1.0, 0.0, 0.0, 1.0);
                    out.o1 = vec4<f32>(0.0, 0.0, 1.0, 1.0);
                    out.o2 = vec4<f32>(0.0, 1.0, 0.0, 1.0);
                    return out;
                }
            "#;

            let (vs_hash, _vs_module) = exec.pipeline_cache.get_or_create_shader_module(
                &exec.device,
                map_pipeline_cache_stage(ShaderStage::Vertex),
                vs_wgsl,
                Some("gap mrt trim VS"),
            );
            exec.resources.shaders.insert(
                VS,
                ShaderResource {
                    stage: ShaderStage::Vertex,
                    wgsl_hash: vs_hash,
                    depth_clamp_wgsl_hash: None,
                    dxbc_hash_fnv1a64: 0,
                    entry_point: "vs_main",
                    vs_input_signature: Vec::new(),
                    reflection: ShaderReflection::default(),
                    wgsl_source: vs_wgsl.to_owned(),
                },
            );

            let (ps_hash, _ps_module) = exec.pipeline_cache.get_or_create_shader_module(
                &exec.device,
                map_pipeline_cache_stage(ShaderStage::Pixel),
                fs_wgsl,
                Some("gap mrt trim PS"),
            );
            exec.resources.shaders.insert(
                PS,
                ShaderResource {
                    stage: ShaderStage::Pixel,
                    wgsl_hash: ps_hash,
                    depth_clamp_wgsl_hash: None,
                    dxbc_hash_fnv1a64: 0,
                    entry_point: "fs_main",
                    vs_input_signature: Vec::new(),
                    reflection: ShaderReflection::default(),
                    wgsl_source: fs_wgsl.to_owned(),
                },
            );

            exec.state.vs = Some(VS);
            exec.state.ps = Some(PS);
            exec.state.input_layout = Some(ILAY);
            exec.state.render_targets = vec![Some(RT0), None, Some(RT2)];
            exec.state.depth_stencil = None;

            let layout_key = PipelineLayoutKey::empty();
            let pipeline_layout = exec
                .device
                .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("gap mrt trim pipeline layout"),
                    bind_group_layouts: &[],
                    push_constant_ranges: &[],
                });

            exec.device.push_error_scope(wgpu::ErrorFilter::Validation);
            let res = super::get_or_create_render_pipeline_for_state(
                &exec.device,
                &mut exec.pipeline_cache,
                &pipeline_layout,
                &mut exec.resources,
                &exec.state,
                layout_key,
            );
            exec.device.poll(wgpu::Maintain::Wait);
            let err = exec.device.pop_error_scope().await;

            assert!(
                err.is_none(),
                "unexpected wgpu validation error while creating MRT-gap pipeline: {err:?}"
            );
            let (key, _pipeline, _mapping) =
                res.expect("pipeline creation should succeed with trimmed PS outputs for gap RTs");
            assert_ne!(
                key.fragment_shader, ps_hash,
                "expected the fragment shader hash to change after trimming @location(1)"
            );
        });
    }

    #[test]
    fn depth_only_pipeline_trims_pixel_shader_outputs() {
        // Regression test: depth-only passes may still use a pixel shader that declares color
        // outputs (common for reusable D3D shaders). D3D discards those writes when no RTVs are
        // bound; WebGPU requires the pipeline + shader interfaces to be compatible.
        //
        // Ensure pipeline creation succeeds even when trimming removes *all* color outputs.
        pollster::block_on(async {
            let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
                Ok(exec) => exec,
                Err(e) => {
                    skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                    return;
                }
            };

            // Depth stencil.
            const DS: u32 = 100;
            let ds_tex = exec.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("depth_only_pipeline_trims_pixel_shader_outputs DS"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Depth32Float,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            let ds_view = ds_tex.create_view(&wgpu::TextureViewDescriptor::default());
            exec.resources.textures.insert(
                DS,
                Texture2dResource {
                    texture: ds_tex,
                    view: ds_view,
                    desc: Texture2dDesc {
                        width: 1,
                        height: 1,
                        mip_level_count: 1,
                        array_layers: 1,
                        format: wgpu::TextureFormat::Depth32Float,
                    },
                    format_u32: aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat::D32Float as u32,
                    backing: None,
                    row_pitch_bytes: 0,
                    dirty: false,
                    guest_backing_is_current: true,
                    host_shadow: None,
                    host_shadow_valid: Vec::new(),
                },
            );

            // Minimal input layout so pipeline creation succeeds. This VS uses only builtins, so we
            // don't need any actual vertex attributes.
            const ILAY: u32 = 101;
            exec.resources.input_layouts.insert(
                ILAY,
                InputLayoutResource {
                    layout: InputLayoutDesc {
                        header: crate::input_layout::InputLayoutBlobHeader {
                            magic: crate::input_layout::AEROGPU_INPUT_LAYOUT_BLOB_MAGIC,
                            version: crate::input_layout::AEROGPU_INPUT_LAYOUT_BLOB_VERSION,
                            element_count: 0,
                        },
                        elements: Vec::new(),
                    },
                    used_slots: Vec::new(),
                    mapping_cache: HashMap::new(),
                },
            );

            const VS: u32 = 102;
            const PS: u32 = 103;

            // This VS uses only builtins; it doesn't need any vertex attributes or varyings for
            // this test.
            let vs_wgsl = r#"
                struct VsOut {
                    @builtin(position) pos: vec4<f32>,
                };

                @vertex
                fn vs_main(@builtin(vertex_index) index: u32) -> VsOut {
                    _ = index;
                    var out: VsOut;
                    out.pos = vec4<f32>(0.0, 0.0, 0.0, 1.0);
                    return out;
                }
            "#;

            // Pixel shader declares a color output even though no RTVs are bound (depth-only pass).
            let fs_wgsl = r#"
                struct PsOut {
                    @location(0) o0: vec4<f32>,
                };

                @fragment
                fn fs_main() -> PsOut {
                    var out: PsOut;
                    out.o0 = vec4<f32>(1.0, 0.0, 0.0, 1.0);
                    return out;
                }
            "#;

            let (vs_hash, _vs_module) = exec.pipeline_cache.get_or_create_shader_module(
                &exec.device,
                map_pipeline_cache_stage(ShaderStage::Vertex),
                vs_wgsl,
                Some("depth only mrt trim VS"),
            );
            exec.resources.shaders.insert(
                VS,
                ShaderResource {
                    stage: ShaderStage::Vertex,
                    wgsl_hash: vs_hash,
                    depth_clamp_wgsl_hash: None,
                    dxbc_hash_fnv1a64: 0,
                    entry_point: "vs_main",
                    vs_input_signature: Vec::new(),
                    reflection: ShaderReflection::default(),
                    wgsl_source: vs_wgsl.to_owned(),
                },
            );

            let (ps_hash, _ps_module) = exec.pipeline_cache.get_or_create_shader_module(
                &exec.device,
                map_pipeline_cache_stage(ShaderStage::Pixel),
                fs_wgsl,
                Some("depth only mrt trim PS"),
            );
            exec.resources.shaders.insert(
                PS,
                ShaderResource {
                    stage: ShaderStage::Pixel,
                    wgsl_hash: ps_hash,
                    depth_clamp_wgsl_hash: None,
                    dxbc_hash_fnv1a64: 0,
                    entry_point: "fs_main",
                    vs_input_signature: Vec::new(),
                    reflection: ShaderReflection::default(),
                    wgsl_source: fs_wgsl.to_owned(),
                },
            );

            exec.state.vs = Some(VS);
            exec.state.ps = Some(PS);
            exec.state.input_layout = Some(ILAY);
            exec.state.render_targets.clear();
            exec.state.depth_stencil = Some(DS);

            let layout_key = PipelineLayoutKey::empty();
            let pipeline_layout = exec
                .device
                .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("depth only mrt trim pipeline layout"),
                    bind_group_layouts: &[],
                    push_constant_ranges: &[],
                });

            exec.device.push_error_scope(wgpu::ErrorFilter::Validation);
            let res = super::get_or_create_render_pipeline_for_state(
                &exec.device,
                &mut exec.pipeline_cache,
                &pipeline_layout,
                &mut exec.resources,
                &exec.state,
                layout_key,
            );
            exec.device.poll(wgpu::Maintain::Wait);
            let err = exec.device.pop_error_scope().await;

            assert!(
                err.is_none(),
                "unexpected wgpu validation error while creating depth-only pipeline: {err:?}"
            );
            let (key, _pipeline, _mapping) =
                res.expect("depth-only pipeline creation should succeed with trimmed PS outputs");

            // Ensure trimming produced a distinct PS hash (we should drop the @location(0) output).
            assert_ne!(
                key.fragment_shader, ps_hash,
                "expected depth-only pipeline to use a trimmed PS variant"
            );
        });
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
    fn set_shader_constants_f_geometry_stage_updates_legacy_constants_tracking() {
        pollster::block_on(async {
            let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
                Ok(exec) => exec,
                Err(e) => {
                    skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                    return;
                }
            };
            assert!(
                exec.legacy_constants.contains_key(&ShaderStage::Geometry),
                "executor should allocate a legacy constants buffer for geometry stage"
            );

            let expected_cb0 = BoundConstantBuffer {
                buffer: legacy_constants_buffer_id(ShaderStage::Geometry),
                offset: 0,
                size: None,
            };
            assert_eq!(
                exec.bindings
                    .stage(ShaderStage::Geometry)
                    .constant_buffer(0),
                Some(expected_cb0),
                "geometry stage should default CB0 to legacy constants buffer"
            );
            assert!(
                !exec.bindings.stage(ShaderStage::Geometry).is_dirty(),
                "geometry stage bindings should not be dirty after initialization"
            );

            let mut encoder = exec
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("aerogpu_cmd test geometry shader constants"),
                });

            let vec4_count = 1u32;
            let size_bytes = (24 + 16) as u32;
            let mut cmd_bytes = Vec::with_capacity(size_bytes as usize);
            cmd_bytes
                .extend_from_slice(&(AerogpuCmdOpcode::SetShaderConstantsF as u32).to_le_bytes());
            cmd_bytes.extend_from_slice(&size_bytes.to_le_bytes());
            cmd_bytes.extend_from_slice(&2u32.to_le_bytes()); // stage = compute (extended via stage_ex)
            cmd_bytes.extend_from_slice(&0u32.to_le_bytes()); // start_register
            cmd_bytes.extend_from_slice(&vec4_count.to_le_bytes());
            cmd_bytes.extend_from_slice(&2u32.to_le_bytes()); // reserved0/stage_ex = geometry
            cmd_bytes.extend_from_slice(&[0xCDu8; 16]); // vec4 data
            assert_eq!(cmd_bytes.len(), size_bytes as usize);

            exec.exec_set_shader_constants_f(&mut encoder, &cmd_bytes)
                .expect("SET_SHADER_CONSTANTS_F (geometry) should succeed");

            assert!(
                exec.encoder_used_buffers
                    .contains(&legacy_constants_buffer_id(ShaderStage::Geometry)),
                "SET_SHADER_CONSTANTS_F should mark geometry legacy constants as used by the encoder"
            );
        });
    }

    #[test]
    fn ia_buffers_are_bindable_as_storage_for_vertex_pulling() {
        pollster::block_on(async {
            let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
                Ok(exec) => exec,
                Err(e) => {
                    skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                    return;
                }
            };
            if !exec.supports_compute() {
                skip_or_panic(module_path!(), "compute unsupported");
                return;
            }

            const VB: u32 = 1;
            const IB: u32 = 2;

            // Create buffers via the actual AeroGPU command stream path, so the test exercises the
            // same usage-flag mapping that guest D3D11 would hit.
            let mut writer = AerogpuCmdWriter::new();
            writer.create_buffer(VB, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER, 16, 0, 0);
            writer.create_buffer(IB, AEROGPU_RESOURCE_USAGE_INDEX_BUFFER, 16, 0, 0);
            let stream = writer.finish();

            let mut guest_mem = VecGuestMemory::new(0);
            exec.execute_cmd_stream(&stream, None, &mut guest_mem)
                .expect("execute_cmd_stream should succeed");

            // Vertex/index pulling compute prepasses require storage buffers. Some downlevel
            // backends (e.g. WebGL2) do not support compute/storage buffers; in that case the
            // executor must not request STORAGE usage, and this test should not attempt to bind the
            // buffers as storage.
            if !exec.caps.supports_compute {
                for (label, handle) in [("vertex", VB), ("index", IB)] {
                    let buf = exec
                        .resources
                        .buffers
                        .get(&handle)
                        .unwrap_or_else(|| panic!("{label} buffer should exist"));
                    assert!(
                        !buf.usage.contains(wgpu::BufferUsages::STORAGE),
                        "{label} buffer must not request STORAGE usage when compute is unsupported"
                    );
                }
                return;
            }

            let bgl = exec
                .device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("aerogpu_cmd ia buffer storage bind test bgl"),
                    entries: &[wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: wgpu::BufferSize::new(4),
                        },
                        count: None,
                    }],
                });

            for (label, handle) in [("vertex", VB), ("index", IB)] {
                let buffer = &exec
                    .resources
                    .buffers
                    .get(&handle)
                    .unwrap_or_else(|| panic!("{label} buffer should exist"))
                    .buffer;

                exec.device.push_error_scope(wgpu::ErrorFilter::Validation);
                exec.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("aerogpu_cmd ia buffer storage bind test bg"),
                    layout: &bgl,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: buffer.as_entire_binding(),
                    }],
                });
                #[cfg(not(target_arch = "wasm32"))]
                exec.device.poll(wgpu::Maintain::Wait);
                let err = exec.device.pop_error_scope().await;
                assert!(
                    err.is_none(),
                    "{label} buffer must be bindable as STORAGE for vertex pulling, got: {err:?}"
                );
            }
        });
    }

    #[test]
    fn compute_pipelines_can_be_deterministically_disabled_without_wgpu_calls() {
        pollster::block_on(async {
            let exec = match AerogpuD3d11Executor::new_for_tests().await {
                Ok(exec) => exec,
                Err(e) => {
                    skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                    return;
                }
            };

            // Simulate a backend without compute by forcing `supports_compute=false` at executor
            // construction time. This should cause `PipelineCache::get_or_create_compute_pipeline`
            // to return `GpuError::Unsupported(\"compute\")` without invoking the pipeline builder
            // (and therefore without calling into wgpu compute APIs).
            let AerogpuD3d11Executor {
                device,
                queue,
                backend,
                ..
            } = exec;
            let mut exec =
                AerogpuD3d11Executor::new_with_supports_compute(device, queue, backend, false);

            // Register a compute shader module so that if the compute-capability check ever stops
            // short-circuiting, the pipeline builder would be invoked (and the test would panic).
            const CS_WGSL: &str = r#"
                @compute @workgroup_size(1)
                fn cs_main() {
                }
            "#;
            let (cs_hash, _module) = exec.pipeline_cache.get_or_create_shader_module(
                &exec.device,
                aero_gpu::pipeline_key::ShaderStage::Compute,
                CS_WGSL,
                Some("aerogpu_cmd test CS"),
            );

            let key = ComputePipelineKey {
                shader: cs_hash,
                layout: PipelineLayoutKey::empty(),
            };
            let err = exec
                .pipeline_cache
                .get_or_create_compute_pipeline(&exec.device, key, |_device, _cs| {
                    panic!(
                        "compute pipeline builder invoked despite supports_compute=false; this would call wgpu compute APIs on unsupported backends"
                    )
                })
                .unwrap_err();
            assert_eq!(err, aero_gpu::GpuError::Unsupported("compute"));
        });
    }

    #[test]
    fn map_buffer_usage_flags_gates_storage_on_compute_support() {
        let vb = map_buffer_usage_flags(AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER, true);
        assert!(vb.contains(wgpu::BufferUsages::VERTEX));
        assert!(vb.contains(wgpu::BufferUsages::STORAGE));

        let ib = map_buffer_usage_flags(AEROGPU_RESOURCE_USAGE_INDEX_BUFFER, true);
        assert!(ib.contains(wgpu::BufferUsages::INDEX));
        assert!(ib.contains(wgpu::BufferUsages::STORAGE));

        let vb_no_compute = map_buffer_usage_flags(AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER, false);
        assert!(!vb_no_compute.contains(wgpu::BufferUsages::STORAGE));

        let ib_no_compute = map_buffer_usage_flags(AEROGPU_RESOURCE_USAGE_INDEX_BUFFER, false);
        assert!(!ib_no_compute.contains(wgpu::BufferUsages::STORAGE));

        let storage = map_buffer_usage_flags(AEROGPU_RESOURCE_USAGE_STORAGE, true);
        assert!(storage.contains(wgpu::BufferUsages::STORAGE));

        let storage_no_compute = map_buffer_usage_flags(AEROGPU_RESOURCE_USAGE_STORAGE, false);
        assert!(!storage_no_compute.contains(wgpu::BufferUsages::STORAGE));
    }

    #[test]
    fn storage_usage_flag_makes_buffer_bindable_as_storage() {
        pollster::block_on(async {
            let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
                Ok(exec) => exec,
                Err(e) => {
                    skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                    return;
                }
            };

            const BUF: u32 = 1;
            let allocs = AllocTable::new(None).unwrap();

            let mut cmd_bytes = Vec::new();
            cmd_bytes.extend_from_slice(&(AerogpuCmdOpcode::CreateBuffer as u32).to_le_bytes());
            cmd_bytes.extend_from_slice(&40u32.to_le_bytes()); // size_bytes
            cmd_bytes.extend_from_slice(&BUF.to_le_bytes());
            cmd_bytes.extend_from_slice(&AEROGPU_RESOURCE_USAGE_STORAGE.to_le_bytes());
            cmd_bytes.extend_from_slice(&16u64.to_le_bytes()); // size_bytes
            cmd_bytes.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
            cmd_bytes.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
            cmd_bytes.extend_from_slice(&0u64.to_le_bytes()); // reserved0
            assert_eq!(cmd_bytes.len(), 40);

            exec.exec_create_buffer(&cmd_bytes, &allocs)
                .expect("CREATE_BUFFER should succeed");

            let supports_compute = exec.caps.supports_compute;
            let buf = exec.resources.buffers.get(&BUF).expect("buffer exists");

            // The command-stream executor only enables STORAGE usage on compute-capable backends.
            if supports_compute {
                assert!(
                    buf.usage.contains(wgpu::BufferUsages::STORAGE),
                    "AEROGPU_RESOURCE_USAGE_STORAGE must enable STORAGE usage when compute is supported"
                );

                let bgl = exec
                    .device
                    .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                        label: Some("aerogpu_cmd storage flag bind test bgl"),
                        entries: &[wgpu::BindGroupLayoutEntry {
                            binding: 0,
                            visibility: wgpu::ShaderStages::COMPUTE,
                            ty: wgpu::BindingType::Buffer {
                                ty: wgpu::BufferBindingType::Storage { read_only: true },
                                has_dynamic_offset: false,
                                min_binding_size: wgpu::BufferSize::new(4),
                            },
                            count: None,
                        }],
                    });

                exec.device.push_error_scope(wgpu::ErrorFilter::Validation);
                exec.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("aerogpu_cmd storage flag bind test bg"),
                    layout: &bgl,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: buf.buffer.as_entire_binding(),
                    }],
                });
                #[cfg(not(target_arch = "wasm32"))]
                exec.device.poll(wgpu::Maintain::Wait);
                let err = exec.device.pop_error_scope().await;
                assert!(
                    err.is_none(),
                    "buffer created with AEROGPU_RESOURCE_USAGE_STORAGE must be bindable as STORAGE, got: {err:?}"
                );
            } else {
                assert!(
                    !buf.usage.contains(wgpu::BufferUsages::STORAGE),
                    "AEROGPU_RESOURCE_USAGE_STORAGE must be ignored when compute/storage buffers are unsupported"
                );
            }
        });
    }

    #[test]
    fn gs_hs_ds_emulation_requires_compute() {
        pollster::block_on(async {
            let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
                Ok(exec) => exec,
                Err(e) => {
                    skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                    return;
                }
            };

            // Force the compute capability off, regardless of what the host adapter supports.
            exec.caps.supports_compute = false;

            // Make the state look like it needs GS emulation. The stream itself does not contain a
            // GS bind command yet; this is a direct state injection for unit testing.
            exec.state.render_targets.push(Some(1));
            exec.state.gs = Some(123);

            let mut writer = AerogpuCmdWriter::new();
            writer.draw(3, 1, 0, 0);
            let stream = writer.finish();

            let mut guest_mem = VecGuestMemory::new(0);
            let err = exec
                .execute_cmd_stream(&stream, None, &mut guest_mem)
                .expect_err(
                    "draw should fail without compute support when GS/HS/DS emulation is required",
                );
            let msg = err.to_string();
            assert!(
                msg.contains(
                    "GS/HS/DS emulation requires compute shaders and indirect execution; backend"
                ),
                "unexpected error: {err:#}"
            );
            assert!(
                msg.contains(&format!("{:?}", exec.backend())),
                "unexpected error: {err:#}"
            );
            assert!(msg.contains("COMPUTE_SHADERS"), "unexpected error: {err:#}");
            if !exec.caps.supports_indirect_execution {
                assert!(msg.contains("INDIRECT_EXECUTION"), "unexpected error: {err:#}");
            }
        });
    }

    #[test]
    fn set_srv_uav_buffer_packets_update_binding_state() {
        pollster::block_on(async {
            let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
                Ok(exec) => exec,
                Err(e) => {
                    skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                    return;
                }
            };

            const SRV_BUF: u32 = 1;
            const UAV_BUF: u32 = 2;
            let allocs = AllocTable::new(None).unwrap();

            for &handle in &[SRV_BUF, UAV_BUF] {
                let mut cmd_bytes = Vec::new();
                cmd_bytes.extend_from_slice(&(AerogpuCmdOpcode::CreateBuffer as u32).to_le_bytes());
                cmd_bytes.extend_from_slice(&40u32.to_le_bytes()); // size_bytes
                cmd_bytes.extend_from_slice(&handle.to_le_bytes());
                cmd_bytes.extend_from_slice(&AEROGPU_RESOURCE_USAGE_STORAGE.to_le_bytes());
                cmd_bytes.extend_from_slice(&16u64.to_le_bytes()); // size_bytes
                cmd_bytes.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
                cmd_bytes.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
                cmd_bytes.extend_from_slice(&0u64.to_le_bytes()); // reserved0
                exec.exec_create_buffer(&cmd_bytes, &allocs)
                    .expect("CREATE_BUFFER should succeed");
            }

            // SET_SHADER_RESOURCE_BUFFERS (1 binding @slot0 in compute stage)
            let mut srv_cmd = Vec::new();
            srv_cmd.extend_from_slice(
                &(AerogpuCmdOpcode::SetShaderResourceBuffers as u32).to_le_bytes(),
            );
            srv_cmd.extend_from_slice(&(24u32 + 16u32).to_le_bytes());
            // Legacy AeroGPU stage encoding: 2 = COMPUTE.
            srv_cmd.extend_from_slice(&2u32.to_le_bytes());
            srv_cmd.extend_from_slice(&0u32.to_le_bytes()); // start_slot
            srv_cmd.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
            srv_cmd.extend_from_slice(&0u32.to_le_bytes()); // stage_ex / reserved0
            srv_cmd.extend_from_slice(&SRV_BUF.to_le_bytes());
            srv_cmd.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
            srv_cmd.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (0 = full)
            srv_cmd.extend_from_slice(&0u32.to_le_bytes()); // reserved0
            exec.exec_set_shader_resource_buffers(&srv_cmd)
                .expect("SET_SHADER_RESOURCE_BUFFERS should succeed");

            assert_eq!(
                exec.bindings.stage(ShaderStage::Compute).srv_buffer(0),
                Some(BoundBuffer {
                    buffer: SRV_BUF,
                    offset: 0,
                    size: None,
                })
            );

            // SET_UNORDERED_ACCESS_BUFFERS (1 binding @slot0 in compute stage)
            let mut uav_cmd = Vec::new();
            uav_cmd.extend_from_slice(
                &(AerogpuCmdOpcode::SetUnorderedAccessBuffers as u32).to_le_bytes(),
            );
            uav_cmd.extend_from_slice(&(24u32 + 16u32).to_le_bytes());
            // Legacy AeroGPU stage encoding: 2 = COMPUTE.
            uav_cmd.extend_from_slice(&2u32.to_le_bytes());
            uav_cmd.extend_from_slice(&0u32.to_le_bytes()); // start_slot
            uav_cmd.extend_from_slice(&1u32.to_le_bytes()); // uav_count
            uav_cmd.extend_from_slice(&0u32.to_le_bytes()); // stage_ex / reserved0
            uav_cmd.extend_from_slice(&UAV_BUF.to_le_bytes());
            uav_cmd.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
            uav_cmd.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (0 = full)
            uav_cmd.extend_from_slice(&0u32.to_le_bytes()); // initial_count (ignored)
            exec.exec_set_unordered_access_buffers(&uav_cmd)
                .expect("SET_UNORDERED_ACCESS_BUFFERS should succeed");

            assert_eq!(
                exec.bindings.stage(ShaderStage::Compute).uav_buffer(0),
                Some(BoundBuffer {
                    buffer: UAV_BUF,
                    offset: 0,
                    size: None,
                })
            );
        });
    }

    #[test]
    fn set_shader_resource_buffers_clears_any_texture_bound_to_same_t_slot() {
        pollster::block_on(async {
            let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
                Ok(exec) => exec,
                Err(e) => {
                    skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                    return;
                }
            };

            // Seed a texture binding at t0, then bind an SRV buffer at the same slot.
            exec.bindings
                .stage_mut(ShaderStage::Vertex)
                .set_texture(0, Some(0x1000));
            assert!(
                exec.bindings
                    .stage(ShaderStage::Vertex)
                    .texture(0)
                    .is_some(),
                "texture should be bound before SET_SHADER_RESOURCE_BUFFERS"
            );

            let mut cmd = Vec::new();
            cmd.extend_from_slice(
                &(AerogpuCmdOpcode::SetShaderResourceBuffers as u32).to_le_bytes(),
            );
            cmd.extend_from_slice(&(24u32 + 16u32).to_le_bytes());
            cmd.extend_from_slice(&0u32.to_le_bytes()); // stage = vertex
            cmd.extend_from_slice(&0u32.to_le_bytes()); // start_slot
            cmd.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
            cmd.extend_from_slice(&0u32.to_le_bytes()); // stage_ex / reserved0
            cmd.extend_from_slice(&0x2000u32.to_le_bytes()); // buffer
            cmd.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
            cmd.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (0 = full)
            cmd.extend_from_slice(&0u32.to_le_bytes()); // reserved0

            exec.exec_set_shader_resource_buffers(&cmd)
                .expect("SET_SHADER_RESOURCE_BUFFERS should succeed");

            let stage = exec.bindings.stage(ShaderStage::Vertex);
            assert!(
                stage.texture(0).is_none(),
                "binding an SRV buffer must unbind any texture in the same t-slot"
            );
            assert_eq!(
                stage.srv_buffer(0),
                Some(BoundBuffer {
                    buffer: 0x2000,
                    offset: 0,
                    size: None
                })
            );
        });
    }

    #[test]
    fn set_unordered_access_buffers_rejects_non_compute_stage() {
        pollster::block_on(async {
            let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
                Ok(exec) => exec,
                Err(e) => {
                    skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                    return;
                }
            };

            // SET_UNORDERED_ACCESS_BUFFERS must reject non-compute stages for now.
            let mut cmd = Vec::new();
            cmd.extend_from_slice(
                &(AerogpuCmdOpcode::SetUnorderedAccessBuffers as u32).to_le_bytes(),
            );
            cmd.extend_from_slice(&(24u32 + 16u32).to_le_bytes());
            cmd.extend_from_slice(&0u32.to_le_bytes()); // stage = vertex (invalid)
            cmd.extend_from_slice(&0u32.to_le_bytes()); // start_slot
            cmd.extend_from_slice(&1u32.to_le_bytes()); // uav_count
            cmd.extend_from_slice(&0u32.to_le_bytes()); // stage_ex / reserved0
            cmd.extend_from_slice(&0x3000u32.to_le_bytes()); // buffer
            cmd.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
            cmd.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (0 = full)
            cmd.extend_from_slice(&0u32.to_le_bytes()); // initial_count (ignored)

            let err = exec
                .exec_set_unordered_access_buffers(&cmd)
                .expect_err("SET_UNORDERED_ACCESS_BUFFERS should reject non-compute stage");
            assert!(
                err.to_string().contains("compute"),
                "unexpected error: {err:#}"
            );
        });
    }

    #[test]
    fn dispatch_smoke_encodes_compute_pass() {
        pollster::block_on(async {
            let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
                Ok(exec) => exec,
                Err(e) => {
                    skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                    return;
                }
            };

            // Inject a trivial compute shader directly (avoids needing to hand-author DXBC).
            const CS: u32 = 1;
            let wgsl = r#"
@compute @workgroup_size(1)
fn cs_main() {
}
"#;
            let (hash, _module) = exec.pipeline_cache.get_or_create_shader_module(
                &exec.device,
                map_pipeline_cache_stage(ShaderStage::Compute),
                wgsl,
                Some("aerogpu_cmd dispatch smoke cs"),
            );
            exec.resources.shaders.insert(
                CS,
                ShaderResource {
                    stage: ShaderStage::Compute,
                    wgsl_hash: hash,
                    depth_clamp_wgsl_hash: None,
                    dxbc_hash_fnv1a64: 0,
                    entry_point: "cs_main",
                    vs_input_signature: Vec::new(),
                    reflection: ShaderReflection::default(),
                    wgsl_source: wgsl.to_string(),
                },
            );
            exec.state.cs = Some(CS);

            let allocs = AllocTable::new(None).unwrap();
            let mut guest_mem = VecGuestMemory::new(0);
            let mut encoder = exec
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("aerogpu_cmd dispatch smoke encoder"),
                });

            let mut cmd_bytes = Vec::new();
            cmd_bytes.extend_from_slice(&(AerogpuCmdOpcode::Dispatch as u32).to_le_bytes());
            cmd_bytes.extend_from_slice(&24u32.to_le_bytes());
            cmd_bytes.extend_from_slice(&1u32.to_le_bytes());
            cmd_bytes.extend_from_slice(&1u32.to_le_bytes());
            cmd_bytes.extend_from_slice(&1u32.to_le_bytes());
            cmd_bytes.extend_from_slice(&0u32.to_le_bytes());

            exec.exec_dispatch(&mut encoder, &cmd_bytes, &allocs, &mut guest_mem)
                .expect("DISPATCH should succeed");

            exec.queue.submit([encoder.finish()]);
            exec.poll();
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
    fn gs_expansion_scratch_reuses_backing_buffer_and_grows() {
        pollster::block_on(async {
            let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
                Ok(exec) => exec,
                Err(e) => {
                    skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                    return;
                }
            };

            // Keep the scratch buffer small so the test is fast and can force growth without
            // allocating a multi-megabyte backing buffer.
            let mut desc = ExpansionScratchDescriptor::default();
            desc.frames_in_flight = 1;
            desc.per_frame_size = 256;
            exec.expansion_scratch = ExpansionScratchAllocator::new(desc);

            let a = exec
                .expansion_scratch
                .alloc_vertex_output(&exec.device, 16)
                .expect("alloc_vertex_output");
            let ptr_a = Arc::as_ptr(&a.buffer) as usize;
            let cap_a = exec
                .expansion_scratch
                .per_frame_capacity()
                .expect("scratch init");

            // Subsequent allocations should reuse the same backing buffer.
            let b = exec
                .expansion_scratch
                .alloc_index_output(&exec.device, 4)
                .expect("alloc_index_output");
            let ptr_b = Arc::as_ptr(&b.buffer) as usize;
            assert_eq!(
                ptr_a, ptr_b,
                "expansion scratch allocations should share a backing buffer"
            );

            // Force growth by requesting more than the current per-frame capacity.
            let c = exec
                .expansion_scratch
                .alloc_metadata(&exec.device, cap_a.saturating_add(1), 16)
                .expect("alloc_metadata grow");
            let ptr_c = Arc::as_ptr(&c.buffer) as usize;
            let cap_c = exec
                .expansion_scratch
                .per_frame_capacity()
                .expect("scratch init");
            assert!(
                cap_c > cap_a,
                "scratch capacity must grow to satisfy large request (cap_a={cap_a} cap_c={cap_c})"
            );
            assert_ne!(
                ptr_a, ptr_c,
                "growth should reallocate the backing buffer"
            );
        });
    }

    #[test]
    fn gs_prepass_reuses_scratch_buffers_across_draws() {
        pollster::block_on(async {
            let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
                Ok(exec) => exec,
                Err(e) => {
                    skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                    return;
                }
            };

            if !exec.caps.supports_compute || !exec.caps.supports_indirect_execution {
                skip_or_panic(
                    module_path!(),
                    "backend lacks compute/indirect execution required for GS/HS/DS emulation",
                );
                return;
            }

            // Keep the per-frame scratch capacity small (but deterministic) so that the test can
            // assert that repeated GS prepass draws do not force the allocator to grow/reallocate.
            let mut desc = ExpansionScratchDescriptor::default();
            desc.frames_in_flight = 1;
            let storage_alignment =
                (exec.device.limits().min_storage_buffer_offset_alignment as u64).max(1);
            desc.per_frame_size = storage_alignment.saturating_mul(32).max(64 * 1024);
            exec.expansion_scratch = ExpansionScratchAllocator::new(desc);
            let _ = exec
                .expansion_scratch
                .alloc_metadata(&exec.device, 4, 4)
                .expect("scratch init");
            let cap_before = exec
                .expansion_scratch
                .per_frame_capacity()
                .expect("scratch init");
            exec.expansion_scratch.begin_frame();

            // Minimal stream that triggers `exec_draw_with_compute_prepass` by binding a non-zero
            // geometry shader handle. The executor only needs VS/PS to exist; GS is currently just
            // the prepass trigger.
            const RT: u32 = 1;
            const VS: u32 = 2;
            const PS: u32 = 3;
            const GS: u32 = 4;

            const DXBC_VS_PASSTHROUGH: &[u8] =
                include_bytes!("../../tests/fixtures/vs_passthrough.dxbc");
            const DXBC_PS_PASSTHROUGH: &[u8] =
                include_bytes!("../../tests/fixtures/ps_passthrough.dxbc");

            let mut writer = AerogpuCmdWriter::new();
            writer.create_texture2d(
                RT,
                AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
                AEROGPU_FORMAT_R8G8B8A8_UNORM,
                4,
                4,
                1,
                1,
                0,
                0,
                0,
            );
            writer.create_shader_dxbc(VS, AerogpuShaderStage::Vertex, DXBC_VS_PASSTHROUGH);
            writer.create_shader_dxbc(PS, AerogpuShaderStage::Pixel, DXBC_PS_PASSTHROUGH);
            writer.set_render_targets(&[RT], 0);
            writer.set_viewport(0.0, 0.0, 4.0, 4.0, 0.0, 1.0);
            writer.bind_shaders_with_gs(VS, GS, PS, 0);

            // Two draws in the same command stream: scratch buffers should be allocated once and
            // then reused.
            writer.draw(3, 1, 0, 0);
            writer.draw(3, 1, 0, 0);

            let stream = writer.finish();
            let mut guest_mem = VecGuestMemory::new(0);
            exec.execute_cmd_stream(&stream, None, &mut guest_mem)
                .expect("execute_cmd_stream should succeed on backends that support emulation");

            let cap_after = exec
                .expansion_scratch
                .per_frame_capacity()
                .expect("scratch init");
            assert_eq!(
                cap_after, cap_before,
                "expansion scratch backing buffer should be reused across GS prepass draws"
            );
        });
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

    #[test]
    fn map_aerogpu_texture_format_supports_b5_formats() {
        assert_eq!(
            map_aerogpu_texture_format(AEROGPU_FORMAT_B5G6R5_UNORM, false).unwrap(),
            wgpu::TextureFormat::Rgba8Unorm
        );
        assert_eq!(
            map_aerogpu_texture_format(AEROGPU_FORMAT_B5G5R5A1_UNORM, false).unwrap(),
            wgpu::TextureFormat::Rgba8Unorm
        );
    }

    #[test]
    fn expansion_scratch_advances_on_present_and_flush() {
        pollster::block_on(async {
            let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
                Ok(exec) => exec,
                Err(e) => {
                    skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                    return;
                }
            };
            let a0 = exec
                .expansion_scratch
                .alloc_vertex_output(&exec.device, 16)
                .expect("initial scratch allocation should succeed");
            let per_frame = exec
                .expansion_scratch
                .per_frame_capacity()
                .expect("allocator must be initialized after first alloc");
            assert_eq!(a0.offset, 0);

            let mut encoder = exec
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("aerogpu_cmd expansion scratch present test"),
                });

            // PRESENT advances the scratch frame segment.
            let size_bytes = 16u32;
            let mut cmd_bytes = Vec::with_capacity(size_bytes as usize);
            cmd_bytes.extend_from_slice(&(AerogpuCmdOpcode::Present as u32).to_le_bytes());
            cmd_bytes.extend_from_slice(&size_bytes.to_le_bytes());
            cmd_bytes.extend_from_slice(&0u32.to_le_bytes()); // scanout_id
            cmd_bytes.extend_from_slice(&0u32.to_le_bytes()); // flags
            assert_eq!(cmd_bytes.len(), size_bytes as usize);
            let mut report = ExecuteReport::default();
            exec.exec_present(&mut encoder, &cmd_bytes, &mut report)
                .expect("PRESENT should succeed");

            let a1 = exec
                .expansion_scratch
                .alloc_vertex_output(&exec.device, 16)
                .expect("post-present scratch allocation should succeed");
            assert!(Arc::ptr_eq(&a0.buffer, &a1.buffer));
            assert_eq!(
                a1.offset, per_frame,
                "PRESENT must advance to the next frame segment"
            );

            // FLUSH advances again.
            exec.exec_flush(&mut encoder).expect("FLUSH should succeed");
            let a2 = exec
                .expansion_scratch
                .alloc_vertex_output(&exec.device, 16)
                .expect("post-flush scratch allocation should succeed");
            assert!(Arc::ptr_eq(&a0.buffer, &a2.buffer));
            assert_eq!(
                a2.offset,
                per_frame * 2,
                "FLUSH must advance to the next frame segment"
            );
        });
    }

    #[test]
    fn create_vertex_buffer_includes_storage_when_compute_supported() {
        pollster::block_on(async {
            let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
                Ok(exec) => exec,
                Err(e) => {
                    skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                    return;
                }
            };
            let allocs = AllocTable::new(None).expect("AllocTable::new");

            const VB_HANDLE: u32 = 1;
            let size_bytes: u64 = 64;

            // Build a CREATE_BUFFER packet for a vertex buffer with no guest backing.
            let mut create = Vec::new();
            create.extend_from_slice(&(AerogpuCmdOpcode::CreateBuffer as u32).to_le_bytes());
            create.extend_from_slice(&40u32.to_le_bytes());
            create.extend_from_slice(&VB_HANDLE.to_le_bytes());
            create.extend_from_slice(&AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER.to_le_bytes());
            create.extend_from_slice(&size_bytes.to_le_bytes());
            create.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
            create.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
            create.extend_from_slice(&0u64.to_le_bytes()); // reserved0
            assert_eq!(create.len(), 40);

            // Ensure buffer creation doesn't trigger a wgpu validation error (e.g. requesting
            // STORAGE usage on a backend that doesn't support storage buffers).
            exec.device
                .push_error_scope(wgpu::ErrorFilter::Validation);
            exec.exec_create_buffer(&create, &allocs)
                .expect("CREATE_BUFFER should succeed");
            exec.poll_wait();
            let err = exec.device.pop_error_scope().await;
            assert!(
                err.is_none(),
                "CREATE_BUFFER should not produce a wgpu validation error (got {err:?})"
            );

            let buf = exec
                .resources
                .buffers
                .get(&VB_HANDLE)
                .expect("buffer should exist");
            assert!(
                buf.usage.contains(wgpu::BufferUsages::VERTEX),
                "vertex buffer must include VERTEX usage"
            );

            if exec.caps.supports_compute {
                assert!(
                    buf.usage.contains(wgpu::BufferUsages::STORAGE),
                    "compute-capable backends must include STORAGE usage so vertex buffers can be read by compute-based GS emulation"
                );
            } else {
                assert!(
                    !buf.usage.contains(wgpu::BufferUsages::STORAGE),
                    "backends without compute/storage buffer support must not request STORAGE usage"
                );
            }
        });
    }

    async fn render_sample_texture_to_rgba8(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        src_view: &wgpu::TextureView,
        width: u32,
        height: u32,
    ) -> Result<Vec<u8>> {
        let resolved = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("aerogpu_cmd b5 sample resolved"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let resolved_view = resolved.create_view(&wgpu::TextureViewDescriptor::default());

        const SHADER: &str = r#"
            @group(0) @binding(0) var src: texture_2d<f32>;
            @group(0) @binding(1) var samp: sampler;

            struct VsOut {
                @builtin(position) pos: vec4<f32>,
                @location(0) uv: vec2<f32>,
            };

            @vertex
            fn vs(@builtin(vertex_index) vid: u32) -> VsOut {
                // Full-screen triangle with UVs that cover the full [0,1] range.
                var positions = array<vec2<f32>, 3>(
                    vec2<f32>(-1.0, -3.0),
                    vec2<f32>( 3.0,  1.0),
                    vec2<f32>(-1.0,  1.0),
                );
                var uvs = array<vec2<f32>, 3>(
                    vec2<f32>(0.0, 2.0),
                    vec2<f32>(2.0, 0.0),
                    vec2<f32>(0.0, 0.0),
                );
                var out: VsOut;
                out.pos = vec4<f32>(positions[vid], 0.0, 1.0);
                out.uv = uvs[vid];
                return out;
            }

            @fragment
            fn fs(in: VsOut) -> @location(0) vec4<f32> {
                return textureSampleLevel(src, samp, in.uv, 0.0);
            }
        "#;

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("aerogpu_cmd b5 sample shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("aerogpu_cmd b5 sample bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        multisampled: false,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("aerogpu_cmd b5 sample pipeline layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("aerogpu_cmd b5 sample pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs",
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs",
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("aerogpu_cmd b5 sample sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("aerogpu_cmd b5 sample bg"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(src_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        let bytes_per_pixel = 4u32;
        let unpadded_bytes_per_row = width
            .checked_mul(bytes_per_pixel)
            .ok_or_else(|| anyhow!("b5 sample: bytes_per_row overflow"))?;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded_bytes_per_row = unpadded_bytes_per_row
            .checked_add(align - 1)
            .map(|v| v / align)
            .and_then(|v| v.checked_mul(align))
            .ok_or_else(|| anyhow!("b5 sample: padded bytes_per_row overflow"))?;
        let buffer_size = (padded_bytes_per_row as u64)
            .checked_mul(height as u64)
            .ok_or_else(|| anyhow!("b5 sample: staging buffer size overflow"))?;

        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aerogpu_cmd b5 sample staging"),
            size: buffer_size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("aerogpu_cmd b5 sample encoder"),
        });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("aerogpu_cmd b5 sample pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &resolved_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }

        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture: &resolved,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &staging,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bytes_per_row),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );

        queue.submit([encoder.finish()]);

        let slice = staging.slice(..);
        let (sender, receiver) = futures_intrusive::channel::shared::oneshot_channel();
        slice.map_async(wgpu::MapMode::Read, move |v| {
            sender.send(v).ok();
        });

        #[cfg(not(target_arch = "wasm32"))]
        device.poll(wgpu::Maintain::Wait);

        #[cfg(target_arch = "wasm32")]
        device.poll(wgpu::Maintain::Poll);

        receiver
            .receive()
            .await
            .ok_or_else(|| anyhow!("wgpu: map_async dropped"))?
            .context("wgpu: map_async failed")?;

        let mapped = slice.get_mapped_range();
        let padded_bpr_usize: usize = padded_bytes_per_row
            .try_into()
            .map_err(|_| anyhow!("b5 sample: padded bytes_per_row out of range"))?;
        let unpadded_bpr_usize: usize = unpadded_bytes_per_row
            .try_into()
            .map_err(|_| anyhow!("b5 sample: bytes_per_row out of range"))?;
        let out_len = (unpadded_bytes_per_row as u64)
            .checked_mul(height as u64)
            .ok_or_else(|| anyhow!("b5 sample: output size overflow"))?;
        let out_len_usize: usize =
            out_len.try_into().map_err(|_| anyhow!("b5 sample: output size out of range"))?;
        let mut out = Vec::with_capacity(out_len_usize);
        for row in 0..height as usize {
            let start = row
                .checked_mul(padded_bpr_usize)
                .ok_or_else(|| anyhow!("b5 sample: row offset overflow"))?;
            let end = start
                .checked_add(unpadded_bpr_usize)
                .ok_or_else(|| anyhow!("b5 sample: row end overflow"))?;
            out.extend_from_slice(
                mapped
                    .get(start..end)
                    .ok_or_else(|| anyhow!("b5 sample: staging buffer too small"))?,
            );
        }
        drop(mapped);
        staging.unmap();
        Ok(out)
    }

    #[test]
    fn upload_b5_textures_and_sample_in_shader() {
        pollster::block_on(async {
            let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
                Ok(exec) => exec,
                Err(e) => {
                    skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                    return;
                }
            };

            let allocs = AllocTable::new(None).unwrap();

            // Helper to create a 2x2 host-owned texture of the given format.
            fn create_texture_cmd(handle: u32, format_u32: u32, width: u32, height: u32) -> Vec<u8> {
                let mut create = Vec::new();
                create.extend_from_slice(&(AerogpuCmdOpcode::CreateTexture2d as u32).to_le_bytes());
                create.extend_from_slice(&56u32.to_le_bytes());
                create.extend_from_slice(&handle.to_le_bytes());
                create.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
                create.extend_from_slice(&format_u32.to_le_bytes());
                create.extend_from_slice(&width.to_le_bytes());
                create.extend_from_slice(&height.to_le_bytes());
                create.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
                create.extend_from_slice(&1u32.to_le_bytes()); // array_layers
                create.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
                create.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
                create.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
                create.extend_from_slice(&0u64.to_le_bytes()); // reserved0
                assert_eq!(create.len(), 56);
                create
            }

            let width = 2u32;
            let height = 2u32;

            // B5G6R5: [red, green; blue, white]
            const TEX_565: u32 = 1;
            let create = create_texture_cmd(TEX_565, AEROGPU_FORMAT_B5G6R5_UNORM, width, height);
            exec.exec_create_texture2d(&create, &allocs)
                .expect("CREATE_TEXTURE2D(B5G6R5) should succeed");

            let b5g6r5_pixels: [u16; 4] = [
                0xF800, // red
                0x07E0, // green
                0x001F, // blue
                0xFFFF, // white
            ];
            let mut b5g6r5_bytes = Vec::new();
            for v in b5g6r5_pixels {
                b5g6r5_bytes.extend_from_slice(&v.to_le_bytes());
            }
            exec.upload_resource_payload(TEX_565, 0, b5g6r5_bytes.len() as u64, &b5g6r5_bytes)
                .expect("UPLOAD_RESOURCE(B5G6R5) should succeed");

            let src_view = &exec
                .resources
                .textures
                .get(&TEX_565)
                .expect("texture exists")
                .view;
            let sampled =
                render_sample_texture_to_rgba8(&exec.device, &exec.queue, src_view, width, height)
                    .await
                    .expect("sample render should succeed");
            let expected_565: Vec<u8> = vec![
                // row0
                255, 0, 0, 255, // red
                0, 255, 0, 255, // green
                // row1
                0, 0, 255, 255, // blue
                255, 255, 255, 255, // white
            ];
            assert_eq!(sampled, expected_565, "B5G6R5 sample output mismatch");

            // B5G5R5A1: [red(a=1), green(a=1); blue(a=0), white(a=1)]
            const TEX_5551: u32 = 2;
            let create =
                create_texture_cmd(TEX_5551, AEROGPU_FORMAT_B5G5R5A1_UNORM, width, height);
            exec.exec_create_texture2d(&create, &allocs)
                .expect("CREATE_TEXTURE2D(B5G5R5A1) should succeed");

            let b5g5r5a1_pixels: [u16; 4] = [
                0xFC00, // red, a=1
                0x83E0, // green, a=1
                0x001F, // blue, a=0
                0xFFFF, // white, a=1
            ];
            let mut b5g5r5a1_bytes = Vec::new();
            for v in b5g5r5a1_pixels {
                b5g5r5a1_bytes.extend_from_slice(&v.to_le_bytes());
            }
            exec.upload_resource_payload(
                TEX_5551,
                0,
                b5g5r5a1_bytes.len() as u64,
                &b5g5r5a1_bytes,
            )
            .expect("UPLOAD_RESOURCE(B5G5R5A1) should succeed");

            let src_view = &exec
                .resources
                .textures
                .get(&TEX_5551)
                .expect("texture exists")
                .view;
            let sampled =
                render_sample_texture_to_rgba8(&exec.device, &exec.queue, src_view, width, height)
                    .await
                    .expect("sample render should succeed");
            let expected_5551: Vec<u8> = vec![
                // row0
                255, 0, 0, 255, // red, a=1
                0, 255, 0, 255, // green, a=1
                // row1
                0, 0, 255, 0, // blue, a=0
                255, 255, 255, 255, // white, a=1
            ];
            assert_eq!(sampled, expected_5551, "B5G5R5A1 sample output mismatch");
        });
    }

    #[test]
    fn guest_backed_texture_invalidates_upload_resource_shadow_on_guest_updates() {
        pollster::block_on(async {
            let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
                Ok(exec) => exec,
                Err(e) => {
                    skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                    return;
                }
            };

            const TEX: u32 = 1;
            const ALLOC_ID: u32 = 1;
            const ALLOC_GPA: u64 = 0x100;

            let width = 2u32;
            let height = 2u32;
            let row_pitch_bytes = width * 4;
            let tex_len = (row_pitch_bytes * height) as usize;

            let allocs = [AerogpuAllocEntry {
                alloc_id: ALLOC_ID,
                flags: 0,
                gpa: ALLOC_GPA,
                size_bytes: tex_len as u64,
                reserved0: 0,
            }];
            let allocs = AllocTable::new(Some(&allocs)).unwrap();

            let mut guest_mem = VecGuestMemory::new(0x1000);

            // Create an allocation-backed texture.
            let mut create = Vec::new();
            create.extend_from_slice(&(AerogpuCmdOpcode::CreateTexture2d as u32).to_le_bytes());
            create.extend_from_slice(&56u32.to_le_bytes());
            create.extend_from_slice(&TEX.to_le_bytes());
            create.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
            create.extend_from_slice(&AEROGPU_FORMAT_R8G8B8A8_UNORM.to_le_bytes());
            create.extend_from_slice(&width.to_le_bytes());
            create.extend_from_slice(&height.to_le_bytes());
            create.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
            create.extend_from_slice(&1u32.to_le_bytes()); // array_layers
            create.extend_from_slice(&row_pitch_bytes.to_le_bytes()); // row_pitch_bytes
            create.extend_from_slice(&ALLOC_ID.to_le_bytes()); // backing_alloc_id
            create.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
            create.extend_from_slice(&0u64.to_le_bytes()); // reserved0
            assert_eq!(create.len(), 56);
            exec.exec_create_texture2d(&create, &allocs)
                .expect("CREATE_TEXTURE2D should succeed");

            // Upload via UPLOAD_RESOURCE to populate `host_shadow`.
            let full_data: Vec<u8> = (0u8..(tex_len as u8)).collect();
            exec.upload_resource_payload(TEX, 0, full_data.len() as u64, &full_data)
                .expect("full UPLOAD_RESOURCE should succeed");
            assert!(
                exec.resources
                    .textures
                    .get(&TEX)
                    .is_some_and(|tex| tex.host_shadow.is_some()),
                "UPLOAD_RESOURCE should populate host_shadow"
            );
            assert!(
                exec.resources
                    .textures
                    .get(&TEX)
                    .is_some_and(|tex| !tex.guest_backing_is_current),
                "UPLOAD_RESOURCE should mark guest backing as stale"
            );

            // Simulate guest memory update without invalidating `host_shadow`.
            // This matches the pre-fix failure mode (dirty=true + stale host_shadow).
            let guest_data = vec![0xEFu8; tex_len];
            guest_mem.write(ALLOC_GPA, &guest_data).unwrap();
            {
                let tex = exec.resources.textures.get_mut(&TEX).unwrap();
                tex.dirty = true;
                assert!(tex.host_shadow.is_some());
            }
            exec.upload_texture_from_guest_memory(TEX, &allocs, &mut guest_mem)
                .expect("guest upload should succeed");
            assert!(
                exec.resources
                    .textures
                    .get(&TEX)
                    .is_some_and(|tex| tex.host_shadow.is_none()),
                "guest-backed upload must invalidate stale host_shadow"
            );
            assert!(
                exec.resources
                    .textures
                    .get(&TEX)
                    .is_some_and(|tex| tex.guest_backing_is_current),
                "upload_texture_from_guest_memory must mark guest backing as current"
            );

            // RESOURCE_DIRTY_RANGE should mark the guest backing stale again (GPU is now out of sync
            // with guest memory).
            let mut dirty = Vec::new();
            dirty.extend_from_slice(&(AerogpuCmdOpcode::ResourceDirtyRange as u32).to_le_bytes());
            dirty.extend_from_slice(&32u32.to_le_bytes());
            dirty.extend_from_slice(&TEX.to_le_bytes());
            dirty.extend_from_slice(&0u32.to_le_bytes()); // reserved0
            dirty.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
            dirty.extend_from_slice(&(tex_len as u64).to_le_bytes()); // size_bytes
            assert_eq!(dirty.len(), 32);
            exec.exec_resource_dirty_range(&dirty)
                .expect("RESOURCE_DIRTY_RANGE should succeed");
            assert!(
                exec.resources
                    .textures
                    .get(&TEX)
                    .is_some_and(|tex| !tex.guest_backing_is_current),
                "RESOURCE_DIRTY_RANGE must mark guest backing as stale"
            );

            // Recreate `host_shadow` via another UPLOAD_RESOURCE and ensure RESOURCE_DIRTY_RANGE
            // invalidates it as well.
            exec.upload_resource_payload(TEX, 0, full_data.len() as u64, &full_data)
                .expect("full UPLOAD_RESOURCE should succeed");
            assert!(
                exec.resources
                    .textures
                    .get(&TEX)
                    .is_some_and(|tex| tex.host_shadow.is_some()),
                "UPLOAD_RESOURCE should repopulate host_shadow"
            );

            exec.exec_resource_dirty_range(&dirty)
                .expect("RESOURCE_DIRTY_RANGE should succeed");
            assert!(
                exec.resources
                    .textures
                    .get(&TEX)
                    .is_some_and(|tex| tex.host_shadow.is_none()),
                "RESOURCE_DIRTY_RANGE must invalidate stale host_shadow"
            );
        });
    }

    #[test]
    fn resource_dirty_range_is_ignored_for_host_owned_textures() {
        pollster::block_on(async {
            let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
                Ok(exec) => exec,
                Err(e) => {
                    skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                    return;
                }
            };

            const TEX: u32 = 1;
            let width = 2u32;
            let height = 2u32;
            let tex_len = (width * height * 4) as usize;

            let allocs = AllocTable::new(None).unwrap();

            let mut create = Vec::new();
            create.extend_from_slice(&(AerogpuCmdOpcode::CreateTexture2d as u32).to_le_bytes());
            create.extend_from_slice(&56u32.to_le_bytes());
            create.extend_from_slice(&TEX.to_le_bytes());
            create.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
            create.extend_from_slice(&AEROGPU_FORMAT_R8G8B8A8_UNORM.to_le_bytes());
            create.extend_from_slice(&width.to_le_bytes());
            create.extend_from_slice(&height.to_le_bytes());
            create.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
            create.extend_from_slice(&1u32.to_le_bytes()); // array_layers
            create.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
            create.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id (host owned)
            create.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
            create.extend_from_slice(&0u64.to_le_bytes()); // reserved0
            assert_eq!(create.len(), 56);
            exec.exec_create_texture2d(&create, &allocs)
                .expect("CREATE_TEXTURE2D should succeed");

            let full_data: Vec<u8> = (0u8..(tex_len as u8)).collect();
            exec.upload_resource_payload(TEX, 0, full_data.len() as u64, &full_data)
                .expect("full UPLOAD_RESOURCE should succeed");
            assert!(
                exec.resources
                    .textures
                    .get(&TEX)
                    .is_some_and(|tex| tex.host_shadow.is_some()),
                "UPLOAD_RESOURCE should populate host_shadow"
            );

            // Even though the protocol specifies RESOURCE_DIRTY_RANGE is only meaningful for
            // guest-backed resources, treat it as a no-op for host-owned textures so a misbehaving
            // guest cannot invalidate the CPU shadow required for partial UPLOAD_RESOURCE patches.
            let mut dirty = Vec::new();
            dirty.extend_from_slice(&(AerogpuCmdOpcode::ResourceDirtyRange as u32).to_le_bytes());
            dirty.extend_from_slice(&32u32.to_le_bytes());
            dirty.extend_from_slice(&TEX.to_le_bytes());
            dirty.extend_from_slice(&0u32.to_le_bytes()); // reserved0
            dirty.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
            dirty.extend_from_slice(&(tex_len as u64).to_le_bytes()); // size_bytes
            assert_eq!(dirty.len(), 32);
            exec.exec_resource_dirty_range(&dirty)
                .expect("RESOURCE_DIRTY_RANGE should succeed");
            assert!(
                exec.resources
                    .textures
                    .get(&TEX)
                    .is_some_and(|tex| !tex.dirty && tex.host_shadow.is_some()),
                "RESOURCE_DIRTY_RANGE must not affect host-owned textures"
            );

            // Verify that partial uploads still work after the dirty-range no-op.
            exec.upload_resource_payload(TEX, 1, 1, &[0xAA])
                .expect("partial UPLOAD_RESOURCE should succeed");
        });
    }

    #[test]
    fn copy_texture2d_uploads_dirty_guest_backed_dst_for_nonzero_mips() {
        pollster::block_on(async {
            let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
                Ok(exec) => exec,
                Err(e) => {
                    skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                    return;
                }
            };

            const SRC_TEX: u32 = 1;
            const DST_TEX: u32 = 2;
            const SRC_ALLOC_ID: u32 = 1;
            const DST_ALLOC_ID: u32 = 2;
            const SRC_GPA: u64 = 0x1000;
            const DST_GPA: u64 = 0x2000;

            let width = 4u32;
            let height = 4u32;
            let mip_levels = 2u32;
            let array_layers = 1u32;
            let row_pitch_bytes = width * 4;

            let layout = compute_guest_texture_layout(
                AEROGPU_FORMAT_R8G8B8A8_UNORM,
                width,
                height,
                mip_levels,
                array_layers,
                row_pitch_bytes,
            )
            .expect("compute guest layout");

            let allocs = [
                AerogpuAllocEntry {
                    alloc_id: SRC_ALLOC_ID,
                    flags: 0,
                    gpa: SRC_GPA,
                    size_bytes: layout.total_size,
                    reserved0: 0,
                },
                AerogpuAllocEntry {
                    alloc_id: DST_ALLOC_ID,
                    flags: 0,
                    gpa: DST_GPA,
                    size_bytes: layout.total_size,
                    reserved0: 0,
                },
            ];
            let allocs = AllocTable::new(Some(&allocs)).unwrap();

            let mut guest_mem = VecGuestMemory::new(0x10_000);

            // Populate mip1 for each allocation with a distinct pattern so we can verify that a
            // partial copy into a dirty destination preserves the untouched pixels by uploading the
            // destination from guest memory first.
            let mip1_offset = layout.mip_offsets[1] as usize;
            let mip1_size = (layout.mip_row_pitches[1] as usize) * (layout.mip_rows[1] as usize);

            let mut src_bytes = vec![0u8; layout.total_size as usize];
            src_bytes[mip1_offset..mip1_offset + mip1_size].fill(0xAA);
            guest_mem.write(SRC_GPA, &src_bytes).unwrap();

            let mut dst_bytes = vec![0u8; layout.total_size as usize];
            dst_bytes[mip1_offset..mip1_offset + mip1_size].fill(0x11);
            guest_mem.write(DST_GPA, &dst_bytes).unwrap();

            // Create SRC and DST as guest-backed RGBA8 textures with 2 mips.
            for (handle, alloc_id) in [(SRC_TEX, SRC_ALLOC_ID), (DST_TEX, DST_ALLOC_ID)] {
                let mut create = Vec::new();
                create.extend_from_slice(&(AerogpuCmdOpcode::CreateTexture2d as u32).to_le_bytes());
                create.extend_from_slice(&56u32.to_le_bytes());
                create.extend_from_slice(&handle.to_le_bytes());
                create.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
                create.extend_from_slice(&AEROGPU_FORMAT_R8G8B8A8_UNORM.to_le_bytes());
                create.extend_from_slice(&width.to_le_bytes());
                create.extend_from_slice(&height.to_le_bytes());
                create.extend_from_slice(&mip_levels.to_le_bytes());
                create.extend_from_slice(&array_layers.to_le_bytes());
                create.extend_from_slice(&row_pitch_bytes.to_le_bytes());
                create.extend_from_slice(&alloc_id.to_le_bytes()); // backing_alloc_id
                create.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
                create.extend_from_slice(&0u64.to_le_bytes()); // reserved0
                assert_eq!(create.len(), 56);
                exec.exec_create_texture2d(&create, &allocs)
                    .expect("CREATE_TEXTURE2D should succeed");
            }

            // Copy a 1x1 region on mip1. This is a partial update of the destination subresource.
            let mut copy = Vec::new();
            copy.extend_from_slice(&(AerogpuCmdOpcode::CopyTexture2d as u32).to_le_bytes());
            copy.extend_from_slice(&64u32.to_le_bytes());
            copy.extend_from_slice(&DST_TEX.to_le_bytes());
            copy.extend_from_slice(&SRC_TEX.to_le_bytes());
            copy.extend_from_slice(&1u32.to_le_bytes()); // dst_mip_level
            copy.extend_from_slice(&0u32.to_le_bytes()); // dst_array_layer
            copy.extend_from_slice(&1u32.to_le_bytes()); // src_mip_level
            copy.extend_from_slice(&0u32.to_le_bytes()); // src_array_layer
            copy.extend_from_slice(&0u32.to_le_bytes()); // dst_x
            copy.extend_from_slice(&0u32.to_le_bytes()); // dst_y
            copy.extend_from_slice(&0u32.to_le_bytes()); // src_x
            copy.extend_from_slice(&0u32.to_le_bytes()); // src_y
            copy.extend_from_slice(&1u32.to_le_bytes()); // width
            copy.extend_from_slice(&1u32.to_le_bytes()); // height
            copy.extend_from_slice(&0u32.to_le_bytes()); // flags
            copy.extend_from_slice(&0u32.to_le_bytes()); // reserved0
            assert_eq!(copy.len(), 64);

            let mut encoder = exec
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("aerogpu_cmd test copy mip1 encoder"),
                });
            let mut pending_writebacks = Vec::new();
            exec.exec_copy_texture2d(
                &mut encoder,
                &copy,
                &allocs,
                &mut guest_mem,
                &mut pending_writebacks,
            )
            .expect("COPY_TEXTURE2D should succeed");
            assert!(
                pending_writebacks.is_empty(),
                "test does not request WRITEBACK_DST"
            );
            exec.submit_encoder(&mut encoder, "aerogpu_cmd test copy mip1 submit");
            exec.poll_wait();

            // Read back mip1 (2x2) from the destination texture and verify:
            // - pixel (0,0) came from the source copy (0xAA)
            // - all other pixels remain the destination's original guest-memory pattern (0x11)
            let mip_level = 1u32;
            let mip_w = width >> mip_level;
            let mip_h = height >> mip_level;
            assert_eq!((mip_w, mip_h), (2, 2));

            let dst_tex = exec.resources.textures.get(&DST_TEX).unwrap();

            let unpadded_bpr = mip_w * 4;
            let padded_bpr = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
            let staging_size = (padded_bpr as u64) * (mip_h as u64);
            let staging = exec.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("aerogpu_cmd test copy mip1 staging"),
                size: staging_size,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });

            let mut read_encoder =
                exec.device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("aerogpu_cmd test copy mip1 read encoder"),
                    });
            read_encoder.copy_texture_to_buffer(
                wgpu::ImageCopyTexture {
                    texture: &dst_tex.texture,
                    mip_level,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::ImageCopyBuffer {
                    buffer: &staging,
                    layout: wgpu::ImageDataLayout {
                        offset: 0,
                        bytes_per_row: Some(padded_bpr),
                        rows_per_image: Some(mip_h),
                    },
                },
                wgpu::Extent3d {
                    width: mip_w,
                    height: mip_h,
                    depth_or_array_layers: 1,
                },
            );

            exec.queue.submit([read_encoder.finish()]);

            let slice = staging.slice(..);
            let (sender, receiver) = futures_intrusive::channel::shared::oneshot_channel();
            slice.map_async(wgpu::MapMode::Read, move |res| {
                sender.send(res).unwrap();
            });
            exec.poll_wait();
            receiver
                .receive()
                .await
                .expect("map_async callback should run")
                .expect("map_async should succeed");

            let mapped = slice.get_mapped_range();
            let padded_bpr_usize = padded_bpr as usize;
            let unpadded_bpr_usize = unpadded_bpr as usize;
            let mut out = Vec::new();
            for row in 0..mip_h as usize {
                let start = row * padded_bpr_usize;
                out.extend_from_slice(&mapped[start..start + unpadded_bpr_usize]);
            }
            drop(mapped);
            staging.unmap();

            let mut expected = vec![0x11u8; unpadded_bpr_usize * mip_h as usize];
            // Overwrite pixel (0,0) (first 4 bytes) with src pattern.
            expected[0..4].fill(0xAA);
            assert_eq!(
                out, expected,
                "COPY_TEXTURE2D into a dirty guest-backed destination must preserve untouched pixels by uploading from guest memory first"
            );
        });
    }

    #[test]
    fn copy_texture2d_uploads_dirty_guest_backed_dst_when_other_subresources_exist() {
        pollster::block_on(async {
            let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
                Ok(exec) => exec,
                Err(e) => {
                    skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                    return;
                }
            };

            const SRC_TEX: u32 = 1;
            const DST_TEX: u32 = 2;
            const SRC_ALLOC_ID: u32 = 1;
            const DST_ALLOC_ID: u32 = 2;
            const SRC_GPA: u64 = 0x1000;
            const DST_GPA: u64 = 0x2000;

            let width = 4u32;
            let height = 4u32;
            let mip_levels = 2u32;
            let array_layers = 1u32;
            let row_pitch_bytes = width * 4;

            let layout = compute_guest_texture_layout(
                AEROGPU_FORMAT_R8G8B8A8_UNORM,
                width,
                height,
                mip_levels,
                array_layers,
                row_pitch_bytes,
            )
            .expect("compute guest layout");

            let allocs = [
                AerogpuAllocEntry {
                    alloc_id: SRC_ALLOC_ID,
                    flags: 0,
                    gpa: SRC_GPA,
                    size_bytes: layout.total_size,
                    reserved0: 0,
                },
                AerogpuAllocEntry {
                    alloc_id: DST_ALLOC_ID,
                    flags: 0,
                    gpa: DST_GPA,
                    size_bytes: layout.total_size,
                    reserved0: 0,
                },
            ];
            let allocs = AllocTable::new(Some(&allocs)).unwrap();

            let mut guest_mem = VecGuestMemory::new(0x10_000);

            let mip0_offset = layout.mip_offsets[0] as usize;
            let mip0_size = (layout.mip_row_pitches[0] as usize) * (layout.mip_rows[0] as usize);
            let mip1_offset = layout.mip_offsets[1] as usize;
            let mip1_size = (layout.mip_row_pitches[1] as usize) * (layout.mip_rows[1] as usize);

            // Source: mip1 pattern 0xAA.
            let mut src_bytes = vec![0u8; layout.total_size as usize];
            src_bytes[mip1_offset..mip1_offset + mip1_size].fill(0xAA);
            guest_mem.write(SRC_GPA, &src_bytes).unwrap();

            // Destination: mip0 pattern 0x11, mip1 pattern 0x22.
            let mut dst_bytes = vec![0u8; layout.total_size as usize];
            dst_bytes[mip0_offset..mip0_offset + mip0_size].fill(0x11);
            dst_bytes[mip1_offset..mip1_offset + mip1_size].fill(0x22);
            guest_mem.write(DST_GPA, &dst_bytes).unwrap();

            // Create SRC and DST as guest-backed RGBA8 textures with 2 mips.
            for (handle, alloc_id) in [(SRC_TEX, SRC_ALLOC_ID), (DST_TEX, DST_ALLOC_ID)] {
                let mut create = Vec::new();
                create.extend_from_slice(&(AerogpuCmdOpcode::CreateTexture2d as u32).to_le_bytes());
                create.extend_from_slice(&56u32.to_le_bytes());
                create.extend_from_slice(&handle.to_le_bytes());
                create.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
                create.extend_from_slice(&AEROGPU_FORMAT_R8G8B8A8_UNORM.to_le_bytes());
                create.extend_from_slice(&width.to_le_bytes());
                create.extend_from_slice(&height.to_le_bytes());
                create.extend_from_slice(&mip_levels.to_le_bytes());
                create.extend_from_slice(&array_layers.to_le_bytes());
                create.extend_from_slice(&row_pitch_bytes.to_le_bytes());
                create.extend_from_slice(&alloc_id.to_le_bytes()); // backing_alloc_id
                create.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
                create.extend_from_slice(&0u64.to_le_bytes()); // reserved0
                assert_eq!(create.len(), 56);
                exec.exec_create_texture2d(&create, &allocs)
                    .expect("CREATE_TEXTURE2D should succeed");
            }

            // Copy the full mip1 subresource (2x2) from SRC to DST.
            let mut copy = Vec::new();
            copy.extend_from_slice(&(AerogpuCmdOpcode::CopyTexture2d as u32).to_le_bytes());
            copy.extend_from_slice(&64u32.to_le_bytes());
            copy.extend_from_slice(&DST_TEX.to_le_bytes());
            copy.extend_from_slice(&SRC_TEX.to_le_bytes());
            copy.extend_from_slice(&1u32.to_le_bytes()); // dst_mip_level
            copy.extend_from_slice(&0u32.to_le_bytes()); // dst_array_layer
            copy.extend_from_slice(&1u32.to_le_bytes()); // src_mip_level
            copy.extend_from_slice(&0u32.to_le_bytes()); // src_array_layer
            copy.extend_from_slice(&0u32.to_le_bytes()); // dst_x
            copy.extend_from_slice(&0u32.to_le_bytes()); // dst_y
            copy.extend_from_slice(&0u32.to_le_bytes()); // src_x
            copy.extend_from_slice(&0u32.to_le_bytes()); // src_y
            copy.extend_from_slice(&2u32.to_le_bytes()); // width
            copy.extend_from_slice(&2u32.to_le_bytes()); // height
            copy.extend_from_slice(&0u32.to_le_bytes()); // flags
            copy.extend_from_slice(&0u32.to_le_bytes()); // reserved0
            assert_eq!(copy.len(), 64);

            let mut encoder = exec
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("aerogpu_cmd test copy full mip1 encoder"),
                });
            let mut pending_writebacks = Vec::new();
            exec.exec_copy_texture2d(
                &mut encoder,
                &copy,
                &allocs,
                &mut guest_mem,
                &mut pending_writebacks,
            )
            .expect("COPY_TEXTURE2D should succeed");
            assert!(
                pending_writebacks.is_empty(),
                "test does not request WRITEBACK_DST"
            );
            exec.submit_encoder(&mut encoder, "aerogpu_cmd test copy full mip1 submit");
            exec.poll_wait();

            // Ensure the untouched mip0 subresource is still populated from guest memory.
            let bytes = exec
                .read_texture_rgba8(DST_TEX)
                .await
                .expect("read_texture_rgba8");
            assert_eq!(
                bytes,
                vec![0x11u8; (width * height * 4) as usize],
                "COPY_TEXTURE2D into a dirty guest-backed texture must upload guest memory first when other subresources exist"
            );
        });
    }

    #[test]
    fn copy_texture2d_into_mip1_preserves_upload_resource_shadow() {
        pollster::block_on(async {
            let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
                Ok(exec) => exec,
                Err(e) => {
                    skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                    return;
                }
            };

            const SRC_TEX: u32 = 1;
            const DST_TEX: u32 = 2;

            let width = 4u32;
            let height = 4u32;
            let mip_levels = 2u32;

            let allocs = AllocTable::new(None).unwrap();

            // Create SRC and DST as host-owned RGBA8 textures with 2 mips.
            for handle in [SRC_TEX, DST_TEX] {
                let mut create = Vec::new();
                create.extend_from_slice(&(AerogpuCmdOpcode::CreateTexture2d as u32).to_le_bytes());
                create.extend_from_slice(&56u32.to_le_bytes());
                create.extend_from_slice(&handle.to_le_bytes());
                create.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
                create.extend_from_slice(&AEROGPU_FORMAT_R8G8B8A8_UNORM.to_le_bytes());
                create.extend_from_slice(&width.to_le_bytes());
                create.extend_from_slice(&height.to_le_bytes());
                create.extend_from_slice(&mip_levels.to_le_bytes()); // mip_levels
                create.extend_from_slice(&1u32.to_le_bytes()); // array_layers
                create.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
                create.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id (host owned)
                create.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
                create.extend_from_slice(&0u64.to_le_bytes()); // reserved0
                assert_eq!(create.len(), 56);
                exec.exec_create_texture2d(&create, &allocs)
                    .expect("CREATE_TEXTURE2D should succeed");
            }

            // Full UPLOAD_RESOURCE to establish host shadows.
            let src_len = (width * height * 4) as usize;
            let src_data = vec![0x33u8; src_len];
            exec.upload_resource_payload(SRC_TEX, 0, src_data.len() as u64, &src_data)
                .expect("UPLOAD_RESOURCE(src) should succeed");

            let dst_data = vec![0x00u8; src_len];
            exec.upload_resource_payload(DST_TEX, 0, dst_data.len() as u64, &dst_data)
                .expect("UPLOAD_RESOURCE(dst) should succeed");
            assert!(
                exec.resources
                    .textures
                    .get(&DST_TEX)
                    .is_some_and(|tex| tex.host_shadow.is_some()),
                "UPLOAD_RESOURCE should populate host_shadow"
            );

            // Copy into mip1 (2x2) of dst. This should not invalidate the mip0/layer0 shadow.
            let mut copy = Vec::new();
            copy.extend_from_slice(&(AerogpuCmdOpcode::CopyTexture2d as u32).to_le_bytes());
            copy.extend_from_slice(&64u32.to_le_bytes());
            copy.extend_from_slice(&DST_TEX.to_le_bytes());
            copy.extend_from_slice(&SRC_TEX.to_le_bytes());
            copy.extend_from_slice(&1u32.to_le_bytes()); // dst_mip_level
            copy.extend_from_slice(&0u32.to_le_bytes()); // dst_array_layer
            copy.extend_from_slice(&0u32.to_le_bytes()); // src_mip_level
            copy.extend_from_slice(&0u32.to_le_bytes()); // src_array_layer
            copy.extend_from_slice(&0u32.to_le_bytes()); // dst_x
            copy.extend_from_slice(&0u32.to_le_bytes()); // dst_y
            copy.extend_from_slice(&0u32.to_le_bytes()); // src_x
            copy.extend_from_slice(&0u32.to_le_bytes()); // src_y
            copy.extend_from_slice(&2u32.to_le_bytes()); // width
            copy.extend_from_slice(&2u32.to_le_bytes()); // height
            copy.extend_from_slice(&0u32.to_le_bytes()); // flags
            copy.extend_from_slice(&0u32.to_le_bytes()); // reserved0
            assert_eq!(copy.len(), 64);

            let mut encoder = exec
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("aerogpu_cmd test copy mip1 preserves shadow encoder"),
                });
            let mut guest_mem = VecGuestMemory::new(0);
            let mut pending_writebacks = Vec::new();
            exec.exec_copy_texture2d(
                &mut encoder,
                &copy,
                &allocs,
                &mut guest_mem,
                &mut pending_writebacks,
            )
            .expect("COPY_TEXTURE2D should succeed");
            assert!(pending_writebacks.is_empty());
            exec.submit_encoder(&mut encoder, "aerogpu_cmd test copy mip1 submit");
            exec.poll_wait();

            assert!(
                exec.resources
                    .textures
                    .get(&DST_TEX)
                    .is_some_and(|tex| tex.host_shadow.is_some()),
                "COPY_TEXTURE2D into mip1 must not invalidate mip0 host_shadow"
            );

            // Verify that partial UPLOAD_RESOURCE patches still work (they rely on host_shadow).
            exec.upload_resource_payload(DST_TEX, 0, 1, &[0xAA])
                .expect("partial UPLOAD_RESOURCE should succeed");
            let pixels = exec
                .read_texture_rgba8(DST_TEX)
                .await
                .expect("readback should succeed");
            assert_eq!(pixels[0], 0xAA);
        });
    }
}
