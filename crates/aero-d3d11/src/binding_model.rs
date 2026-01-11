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
