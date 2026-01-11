//! Direct3D 10/11 translation primitives.
//!
//! This crate currently contains two layers:
//!
//! - [`runtime`]: a wgpu-backed executor for the guest D3D11 command stream.
//! - [`sm4`] / [`signature`] / [`sm4_ir`] / [`shader_translate`]: DXBC parsing +
//!   minimal SM4/SM5 → WGSL translation suitable for FL10_0 bring-up.

pub mod runtime;
pub mod sm4;
pub mod sm4_ir;
pub mod signature;
pub mod shader_translate;
pub mod wgsl;
pub mod input_layout;

pub use aero_dxbc::{DxbcChunk, DxbcError, DxbcFile, FourCC};
pub use sm4::{ShaderModel, ShaderStage, Sm4Error, Sm4Program};
pub use sm4_ir::{
    DstOperand, OperandModifier, RegFile, RegisterRef, Sm4Inst, Sm4Module, SamplerRef, SrcKind,
    SrcOperand, Swizzle, TextureRef, WriteMask,
};
pub use signature::{
    parse_signature_chunk, parse_signatures, DxbcSignature, DxbcSignatureParameter, ShaderSignatures,
    SignatureError,
};
pub use shader_translate::{
    translate_sm4_module_to_wgsl, Binding, BindingKind, Builtin, IoParam, ShaderReflection,
    ShaderTranslateError, ShaderTranslation,
};
pub use wgsl::{translate_sm4_to_wgsl, WgslError, WgslTranslation};
