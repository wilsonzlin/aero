//! Shared binding model between SM4/SM5→WGSL translation and the AeroGPU D3D11
//! command-stream executor.
//!
//! The executor uses stage-scoped bind groups for user (D3D) shader resources:
//! - `@group(0)` = vertex shader stage resources
//! - `@group(1)` = pixel shader stage resources
//! - `@group(2)` = compute shader stage resources
//!
//! WebGPU guarantees `maxBindGroups >= 4`, so AeroGPU uses [`BIND_GROUP_INTERNAL_EMULATION`]
//! (currently `@group(3)`) as a reserved internal/emulation group for both:
//! - D3D11 extended stage resources (GS/HS/DS, bound via `stage_ex`), and
//! - internal emulation helpers (vertex pulling, expanded draws, etc).
//!
//! D3D11 extended stages (GS/HS/DS) do not exist in WebGPU, but the guest can still update their
//! per-stage binding tables (textures/samplers/constant buffers). To keep the bind-group count
//! within the WebGPU baseline limit (`maxBindGroups >= 4`), those extended-stage resources are
//! mapped to `@group(3)` (see `runtime::bindings::ShaderStage::as_bind_group_index`). This group is
//! used by the compute-emulated GS/HS/DS paths.
//!
//! Internal translation/emulation helpers bind resources in [`BIND_GROUP_INTERNAL_EMULATION`] and use
//! `@binding` numbers at or above [`BINDING_BASE_INTERNAL`] so their bindings stay disjoint from the
//! D3D11 register-space ranges (`b#`/`t#`/`s#`/`u#`).
//!
//! Within each group, D3D register spaces are mapped into disjoint `@binding`
//! ranges so `b#`, `t#`, `s#`, and SM5 `u#` can coexist.

/// Base `@binding` offset for `b#` constant buffers.
pub const BINDING_BASE_CBUFFER: u32 = 0;
/// Base `@binding` offset for `t#` SRVs (textures and buffers).
pub const BINDING_BASE_TEXTURE: u32 = 32;
/// Base `@binding` offset for `s#` samplers.
pub const BINDING_BASE_SAMPLER: u32 = 160;
/// Base `@binding` offset for SM5 `u#` UAVs (unordered access views).
///
/// This starts immediately after the sampler range (`[BINDING_BASE_SAMPLER, BINDING_BASE_UAV)`),
/// keeping the binding model disjoint:
///
/// - `b#`: `[BINDING_BASE_CBUFFER, BINDING_BASE_TEXTURE)`
/// - `t#`: `[BINDING_BASE_TEXTURE, BINDING_BASE_SAMPLER)`
/// - `s#`: `[BINDING_BASE_SAMPLER, BINDING_BASE_UAV)`
/// - `u#`: `[BINDING_BASE_UAV, BINDING_BASE_UAV + MAX_UAV_SLOTS)`
///
/// It is safe to place UAVs after samplers because D3D11 enforces a fixed sampler register count
/// per stage (`s0..s15`), so any valid sampler binding is strictly below `BINDING_BASE_UAV`.
///
/// Note: UAVs are currently modeled as WGSL storage buffers/textures.
pub const BINDING_BASE_UAV: u32 = BINDING_BASE_SAMPLER + MAX_SAMPLER_SLOTS;

/// Base `@binding` offset reserved for internal emulation/translation resources.
///
/// Internal pipelines (such as vertex pulling / geometry expansion) use bindings at or above this
/// base (typically in [`BIND_GROUP_INTERNAL_EMULATION`]). The range is also kept disjoint from the
/// D3D11 register-space ranges so internal resources can share a bind group with stage-scoped
/// bindings when needed.
///
/// This constant must remain strictly above the D3D register-space ranges covered by
/// [`BINDING_BASE_CBUFFER`], [`BINDING_BASE_TEXTURE`], [`BINDING_BASE_SAMPLER`], and
/// [`BINDING_BASE_UAV`] (+ [`MAX_UAV_SLOTS`]).
pub const BINDING_BASE_INTERNAL: u32 = 256;

/// Maximum number of constant buffer slots that can be represented without colliding with the
/// texture binding range.
///
/// Valid slots are `0..MAX_CBUFFER_SLOTS` (inclusive max slot is `MAX_CBUFFER_SLOTS - 1`).
pub const MAX_CBUFFER_SLOTS: u32 = BINDING_BASE_TEXTURE - BINDING_BASE_CBUFFER;

/// D3D10/11 exposes 14 constant buffer slots per shader stage (`b0..b13`).
///
/// Note: This is stricter than [`MAX_CBUFFER_SLOTS`], which is derived from the binding-number
/// scheme (and exists to prevent `@binding` range collisions).
pub const D3D11_MAX_CONSTANT_BUFFER_SLOTS: u32 = 14;

/// Maximum number of SRV (`t#`) slots that can be represented without colliding with the sampler
/// binding range.
///
/// Valid slots are `0..MAX_TEXTURE_SLOTS` (inclusive max slot is `MAX_TEXTURE_SLOTS - 1`).
pub const MAX_TEXTURE_SLOTS: u32 = BINDING_BASE_SAMPLER - BINDING_BASE_TEXTURE;

/// D3D11 exposes 16 sampler slots per shader stage (`s0..s15`).
pub const MAX_SAMPLER_SLOTS: u32 = 16;

/// D3D11 exposes 8 UAV slots to SM5 shaders (`u0..u7`) in the compute stage.
///
/// This matches `D3D11_PS_CS_UAV_REGISTER_COUNT` (8). We reserve exactly this many bindings so
/// future SM5 translation/execution can map `u#` registers without colliding with other resource
/// spaces.
pub const MAX_UAV_SLOTS: u32 = 8;

/// Bind group index used for internal emulation helpers (vertex pulling, expanded geometry output,
/// counters, indirect args, etc).
///
/// WebGPU guarantees `maxBindGroups >= 4`, so AeroGPU's D3D11 executor reserves `@group(0..=2)` for
/// VS/PS/CS resources and uses `@group(3)` for both:
/// - Extended D3D11 stage resources (GS/HS/DS)
/// - Internal emulation helpers (vertex pulling, expanded draws, etc)
///
/// Internal-only bindings within this group must use `@binding >= BINDING_BASE_INTERNAL` to avoid
/// colliding with the D3D11 register-space mappings (`b#`/`t#`/`s#`/`u#`).
pub const BIND_GROUP_INTERNAL_EMULATION: u32 = 3;
