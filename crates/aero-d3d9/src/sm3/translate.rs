use std::collections::HashMap;

use blake3::Hash;
use thiserror::Error;

use crate::dxbc;
use crate::shader;
use crate::sm3::{build_ir, decode_u8_le_bytes, verify_ir};

use super::wgsl;

/// Successful SM2/SM3 (D3D9) shader translation result.
#[derive(Debug, Clone)]
pub struct TranslatedShader {
    pub stage: shader::ShaderStage,
    pub wgsl: String,
    pub entry_point: &'static str,
    /// True when vertex shader input registers were remapped from raw `v#` indices to canonical
    /// WGSL `@location(n)` values based on `dcl_*` semantics.
    pub uses_semantic_locations: bool,
    /// Bitmask of D3D9 sampler registers used by this shader.
    ///
    /// Only sampler indices `0..=15` participate in the mask.
    pub used_samplers_mask: u16,
    /// Bind group index used for texture/sampler bindings in this shader stage.
    ///
    /// Contract:
    /// - group(0): constants shared by VS/PS (bindings 0/1/2 for float/int/bool constants)
    /// - group(1): VS texture/sampler bindings
    /// - group(2): PS texture/sampler bindings
    /// - group(3): optional half-pixel-center uniform buffer (VS only)
    pub sampler_group: u32,
    /// Binding numbers used for sampler-related `@group(sampler_group) @binding(n)` declarations.
    ///
    /// The map keys are D3D9 sampler register indices (`s#`).
    pub sampler_bindings: HashMap<u32, (u32, u32)>,
}

#[derive(Debug, Clone)]
pub struct CachedShader {
    pub hash: Hash,
    pub translated: TranslatedShader,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShaderCacheLookupSource {
    /// The shader was already present in the in-memory cache.
    Memory,
    /// The translator ran and the output was inserted into the in-memory cache.
    Translated,
}

#[derive(Debug, Clone, Copy)]
pub struct ShaderCacheLookup<'a> {
    pub source: ShaderCacheLookupSource,
    shader: &'a CachedShader,
}

impl std::ops::Deref for ShaderCacheLookup<'_> {
    type Target = CachedShader;

    fn deref(&self) -> &Self::Target {
        self.shader
    }
}

pub struct ShaderCache {
    map: HashMap<Hash, CachedShader>,
    wgsl_options: wgsl::WgslOptions,
}

impl ShaderCache {
    pub fn new(wgsl_options: wgsl::WgslOptions) -> Self {
        Self {
            map: HashMap::new(),
            wgsl_options,
        }
    }

    pub fn wgsl_options(&self) -> wgsl::WgslOptions {
        self.wgsl_options
    }

    pub fn set_wgsl_options(&mut self, wgsl_options: wgsl::WgslOptions) {
        if self.wgsl_options != wgsl_options {
            self.wgsl_options = wgsl_options;
            self.map.clear();
        }
    }

    pub fn get_or_translate(
        &mut self,
        bytes: &[u8],
    ) -> Result<ShaderCacheLookup<'_>, TranslateError> {
        use std::collections::hash_map::Entry;

        let hash = blake3::hash(bytes);
        match self.map.entry(hash) {
            Entry::Occupied(e) => Ok(ShaderCacheLookup {
                source: ShaderCacheLookupSource::Memory,
                shader: e.into_mut(),
            }),
            Entry::Vacant(e) => {
                let translated = translate_dxbc_to_wgsl_with_options(bytes, self.wgsl_options)?;
                let hash = *e.key();
                Ok(ShaderCacheLookup {
                    source: ShaderCacheLookupSource::Translated,
                    shader: e.insert(CachedShader { hash, translated }),
                })
            }
        }
    }
}

impl Default for ShaderCache {
    fn default() -> Self {
        Self::new(wgsl::WgslOptions::default())
    }
}

#[derive(Debug, Error)]
pub enum TranslateError {
    #[error("dxbc error: {0}")]
    Dxbc(#[from] dxbc::DxbcError),
    #[error(transparent)]
    Decode(#[from] crate::sm3::decode::DecodeError),
    #[error(transparent)]
    BuildIr(#[from] crate::sm3::ir_builder::BuildError),
    #[error(transparent)]
    VerifyIr(#[from] crate::sm3::verify::VerifyError),
    #[error(transparent)]
    Wgsl(#[from] wgsl::WgslError),
}

/// Translate SM2/SM3 (D3D9) shader bytecode to WGSL.
///
/// The input `bytes` may be:
/// - a DXBC container (starts with ASCII `DXBC`), or
/// - a raw D3D9 DWORD token stream (starts with a version token like `0xFFFE0200`).
///
/// Returned WGSL uses the fixed bind group layout expected by the AeroGPU D3D9 executor:
/// - group(0): shader constants shared by VS/PS (packed to keep bindings stable across stages)
///   - `@binding(0)`: float4 constants (`c#`) as `array<vec4<f32>, 512>`
///   - `@binding(1)`: int4 constants (`i#`) as `array<vec4<i32>, 512>`
///   - `@binding(2)`: bool constants (`b#`) as `array<vec4<u32>, 512>`
/// - group(1): vertex shader samplers, bindings `(2*s, 2*s+1)` for D3D9 sampler register `s#`
/// - group(2): pixel shader samplers, bindings `(2*s, 2*s+1)` for D3D9 sampler register `s#`
/// - group(3): optional half-pixel-center uniform buffer (VS only)
pub fn translate_dxbc_to_wgsl(bytes: &[u8]) -> Result<TranslatedShader, TranslateError> {
    translate_dxbc_to_wgsl_with_options(bytes, wgsl::WgslOptions::default())
}

pub fn translate_dxbc_to_wgsl_with_options(
    bytes: &[u8],
    options: wgsl::WgslOptions,
) -> Result<TranslatedShader, TranslateError> {
    let token_stream = dxbc::extract_shader_bytecode(bytes)?;

    let token_stream = crate::token_stream::normalize_sm2_sm3_instruction_lengths(token_stream)
        .map_err(|message| crate::sm3::decode::DecodeError {
            token_index: 0,
            message,
        })?;
    let decoded = decode_u8_le_bytes(token_stream.as_ref())?;
    let ir = build_ir(&decoded)?;
    verify_ir(&ir)?;

    let wgsl_out = wgsl::generate_wgsl_with_options(&ir, options)?;

    // Build a compact used-samplers bitmask for the executor.
    let mut used_samplers_mask = 0u16;
    for &s in wgsl_out.bind_group_layout.sampler_bindings.keys() {
        if s < 16 {
            used_samplers_mask |= 1u16 << s;
        }
    }

    let stage = match decoded.version.stage {
        crate::sm3::types::ShaderStage::Vertex => shader::ShaderStage::Vertex,
        crate::sm3::types::ShaderStage::Pixel => shader::ShaderStage::Pixel,
    };

    Ok(TranslatedShader {
        stage,
        wgsl: wgsl_out.wgsl,
        entry_point: wgsl_out.entry_point,
        uses_semantic_locations: ir.uses_semantic_locations && stage == shader::ShaderStage::Vertex,
        used_samplers_mask,
        sampler_group: wgsl_out.bind_group_layout.sampler_group,
        sampler_bindings: wgsl_out.bind_group_layout.sampler_bindings,
    })
}
