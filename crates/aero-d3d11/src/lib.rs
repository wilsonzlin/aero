//! Direct3D 10/11 translation primitives.
//!
//! This crate currently contains two layers:
//!
//! - [`runtime`]: a wgpu-backed executor for the guest D3D11 command stream.
//! - [`sm4`] / [`signature`] / [`sm4_ir`] / [`shader_translate`]: DXBC parsing +
//!   minimal SM4/SM5 → WGSL translation suitable for FL10_0 bring-up.

// On wasm32, many `wgpu` handle types are `!Send + !Sync` due to JS thread-affinity. We still use
// `Arc<T>` widely for shared ownership (matching native builds), but those `Arc<T>`s are never sent
// across threads on wasm. Clippy warns about this pattern; silence it for wasm32 builds.
#![cfg_attr(target_arch = "wasm32", allow(clippy::arc_with_non_send_sync))]

pub mod binding_model;
pub mod input_layout;
pub mod runtime;
pub mod shader_lib;
pub mod shader_translate;
pub mod signature;
pub mod sm4;
pub mod sm4_ir;
pub mod vertex_pulling;
pub mod wgsl;
mod wgsl_bootstrap;

pub use aero_dxbc::{DxbcChunk, DxbcError, DxbcFile, FourCC};
pub use shader_translate::{
    translate_sm4_module_to_wgsl, translate_sm4_module_to_wgsl_ds_eval, Binding, BindingKind,
    Builtin, IoParam, ShaderReflection, ShaderTranslateError, ShaderTranslation,
    StorageTextureFormat,
};
pub use signature::{
    parse_signature_chunk, parse_signatures, DxbcSignature, DxbcSignatureParameter,
    ShaderSignatures, SignatureError,
};
pub use sm4::{ShaderModel, ShaderStage, Sm4DecodeError, Sm4Error, Sm4Program};
pub use sm4_ir::{
    BufferKind, BufferRef, CmpOp, CmpType, DstOperand, GsInputPrimitive, GsOutputTopology,
    HsDomain, HsOutputTopology, HsPartitioning, HullShaderPhase, OperandModifier,
    PredicateDstOperand, PredicateOperand, PredicateRef, RegFile, RegisterRef, SamplerRef,
    Sm4CmpOp, Sm4Decl, Sm4Inst, Sm4Module, Sm4TestBool, SrcKind, SrcOperand, Swizzle, TextureRef,
    UavRef, WriteMask,
};
pub use wgsl::{translate_sm4_to_wgsl, translate_sm4_to_wgsl_ds_eval, WgslError, WgslTranslation};
pub use wgsl_bootstrap::{
    translate_sm4_to_wgsl_bootstrap, WgslBootstrapError, WgslBootstrapTranslation,
};
