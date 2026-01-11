//! WGSL output for SM4/SM5 shaders.
//!
//! This module is kept for backwards compatibility with earlier bootstrap
//! versions of the crate. The actual translation logic lives in
//! [`crate::shader_translate`].

pub use crate::shader_translate::{
    translate_sm4_module_to_wgsl as translate_sm4_to_wgsl, Binding, BindingKind, Builtin, IoParam,
    ShaderReflection, ShaderTranslateError as WgslError, ShaderTranslation as WgslTranslation,
};

