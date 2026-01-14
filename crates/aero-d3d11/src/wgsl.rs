//! WGSL output for SM4/SM5 shaders.
//!
//! The long-term direction is:
//! `DXBC bytes -> (parse signatures + decode SM4/SM5 IR) -> WGSL + reflection`.
//!
//! The fully-featured translation logic lives in [`crate::shader_translate`].

pub use crate::shader_translate::{
    translate_sm4_module_to_wgsl as translate_sm4_to_wgsl,
    translate_sm4_module_to_wgsl_ds_eval as translate_sm4_to_wgsl_ds_eval, Binding, BindingKind,
    Builtin, IoParam, ShaderReflection, ShaderTranslateError as WgslError,
    ShaderTranslation as WgslTranslation,
};
