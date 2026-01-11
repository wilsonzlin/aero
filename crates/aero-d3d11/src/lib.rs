//! Direct3D 10/11 translation primitives.
//!
//! This crate currently contains two layers:
//!
//! - [`runtime`]: a wgpu-backed executor for the guest D3D11 command stream.
//! - [`sm4`] / [`signature`] / [`sm4_ir`] / [`shader_translate`]: DXBC parsing +
//!   minimal SM4/SM5 → WGSL translation suitable for FL10_0 bring-up.

pub mod input_layout;
pub mod runtime;
pub mod shader_translate;
pub mod signature;
pub mod sm4;
pub mod sm4_ir;
pub mod wgsl;
mod wgsl_bootstrap;

pub use aero_dxbc::{DxbcChunk, DxbcError, DxbcFile, FourCC};
pub use shader_translate::{
    translate_sm4_module_to_wgsl, Binding, BindingKind, Builtin, IoParam, ShaderReflection,
    ShaderTranslateError, ShaderTranslation,
};
pub use signature::{
    parse_signature_chunk, parse_signatures, DxbcSignature, DxbcSignatureParameter,
    ShaderSignatures, SignatureError,
};
pub use sm4::{ShaderModel, ShaderStage, Sm4DecodeError, Sm4Error, Sm4Program};
pub use sm4_ir::{
    DstOperand, OperandModifier, RegFile, RegisterRef, SamplerRef, Sm4Decl, Sm4Inst, Sm4Module,
    SrcKind, SrcOperand, Swizzle, TextureRef, WriteMask,
};
pub use wgsl::{translate_sm4_to_wgsl, WgslError, WgslTranslation};
pub use wgsl_bootstrap::{
    translate_sm4_to_wgsl_bootstrap, WgslBootstrapError, WgslBootstrapTranslation,
};
