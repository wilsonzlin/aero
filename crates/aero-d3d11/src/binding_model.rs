//! Shared binding model between SM4/SM5→WGSL translation and the AeroGPU D3D11
//! command-stream executor.
//!
//! The executor uses stage-scoped bind groups:
//! - `@group(0)` = vertex shader stage resources
//! - `@group(1)` = pixel shader stage resources
//! - `@group(2)` = compute shader stage resources
//! - `@group(3)` = geometry shader stage resources
//!
//! Internal translation/emulation passes may reserve additional bind groups. For example,
//! compute-side vertex pulling uses `@group(3)` (see `runtime::vertex_pulling`).
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
