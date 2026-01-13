//! Shader Model 2.0 / 3.0 (D3D9) bytecode decoding and IR lowering.

pub mod decode;
pub mod ir;
pub mod ir_builder;
pub mod software;
pub mod translate;
pub mod types;
pub mod verify;
pub mod wgsl;

pub use decode::{decode_u32_tokens, decode_u8_le_bytes, DecodedShader};
pub use ir::ShaderIr;
pub use ir_builder::build_ir;
pub use translate::{
    translate_dxbc_to_wgsl, CachedShader, ShaderCache, ShaderCacheLookup, ShaderCacheLookupSource,
    TranslateError, TranslatedShader,
};
pub use types::{ShaderStage, ShaderVersion};
pub use verify::verify_ir;
pub use wgsl::{generate_wgsl, BindGroupLayout, Sm3WgslError, WgslError, WgslOutput, WgslTranslation};
