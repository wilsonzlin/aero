//! Shared binding model between SM4/SM5→WGSL translation and the AeroGPU D3D11
//! command-stream executor.
//!
//! The executor uses stage-scoped bind groups:
//! - `@group(0)` = vertex shader stage resources
//! - `@group(1)` = pixel shader stage resources
//! - `@group(2)` = compute shader stage resources
//!
//! Within each group, D3D register spaces are mapped into disjoint `@binding`
//! ranges so `b#`, `t#`, and `s#` can coexist.

/// Base `@binding` offset for `b#` constant buffers.
pub const BINDING_BASE_CBUFFER: u32 = 0;
/// Base `@binding` offset for `t#` textures.
pub const BINDING_BASE_TEXTURE: u32 = 32;
/// Base `@binding` offset for `s#` samplers.
pub const BINDING_BASE_SAMPLER: u32 = 160;

/// Maximum number of constant buffer slots that can be represented without colliding with the
/// texture binding range.
///
/// Valid slots are `0..MAX_CBUFFER_SLOTS` (inclusive max slot is `MAX_CBUFFER_SLOTS - 1`).
pub const MAX_CBUFFER_SLOTS: u32 = BINDING_BASE_TEXTURE - BINDING_BASE_CBUFFER;

/// Maximum number of texture slots that can be represented without colliding with the sampler
/// binding range.
///
/// Valid slots are `0..MAX_TEXTURE_SLOTS` (inclusive max slot is `MAX_TEXTURE_SLOTS - 1`).
pub const MAX_TEXTURE_SLOTS: u32 = BINDING_BASE_SAMPLER - BINDING_BASE_TEXTURE;

/// D3D11 exposes 16 sampler slots per shader stage (`s0..s15`).
pub const MAX_SAMPLER_SLOTS: u32 = 16;
