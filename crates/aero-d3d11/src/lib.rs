//! Direct3D 10/11 translation primitives.
//!
//! This crate currently contains two layers:
//!
//! - [`runtime`]: a wgpu-backed executor for the guest D3D11 command stream.
//! - [`sm4`] / [`wgsl`]: early SM4/SM5 DXBC token decoding and a tiny DXBC→WGSL
//!   bootstrap translator (currently only `mov`/`ret`), intended to grow into a
//!   full shader translation pipeline.

pub mod runtime;
pub mod sm4;
pub mod wgsl;
pub mod input_layout;

pub use aero_dxbc::{DxbcChunk, DxbcError, DxbcFile, FourCC};
pub use sm4::{ShaderModel, ShaderStage, Sm4Error, Sm4Program};
pub use wgsl::{translate_sm4_to_wgsl, WgslError, WgslTranslation};
