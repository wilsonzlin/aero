use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt;

use thiserror::Error;

use crate::dxbc;
use crate::shader;
use crate::shader_limits::{MAX_D3D9_SHADER_BLOB_BYTES, MAX_D3D9_SHADER_BYTECODE_BYTES};
use crate::sm3;
use crate::sm3::decode::TextureType;
use crate::vertex::{AdaptiveLocationMap, DeclUsage, VertexLocationMap};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShaderTranslateBackend {
    /// Translation used the stricter SM3 pipeline (`sm3::decode` + `sm3::build_ir`).
    Sm3,
    /// Translation fell back to the legacy translator (`shader.rs`) after the SM3
    /// pipeline rejected an unsupported feature.
    LegacyFallback,
}

#[derive(Debug, Clone)]
pub struct ShaderTranslation {
    pub backend: ShaderTranslateBackend,
    pub version: shader::ShaderVersion,
    pub wgsl: String,
    pub entry_point: &'static str,
    pub uses_semantic_locations: bool,
    /// Semantic → WGSL location mapping derived from vertex shader `dcl_*` declarations when
    /// [`ShaderTranslation::uses_semantic_locations`] is true.
    ///
    /// This metadata is used by the host-side D3D9 executor to bind vertex buffers consistently
    /// with the remapping performed during shader translation. Some translation paths (or legacy
    /// cached artifacts) may omit it, in which case callers should fall back to
    /// [`crate::vertex::StandardLocationMap`] for the common semantics.
    pub semantic_locations: Vec<shader::SemanticLocation>,
    pub used_samplers: BTreeSet<u16>,
    pub sampler_texture_types: HashMap<u16, TextureType>,
    /// When `backend == LegacyFallback`, describes the SM3 pipeline failure that
    /// triggered fallback.
    pub fallback_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShaderCacheLookupSource {
    /// The shader was already present in the in-memory cache.
    Memory,
    /// The translator ran and the output was inserted into the in-memory cache.
    Translated,
}

#[derive(Debug)]
pub struct ShaderCacheLookup<'a> {
    pub source: ShaderCacheLookupSource,
    shader: &'a ShaderTranslation,
}

impl std::ops::Deref for ShaderCacheLookup<'_> {
    type Target = ShaderTranslation;

    fn deref(&self) -> &Self::Target {
        self.shader
    }
}

/// In-memory cache for the high-level SM3-first D3D9 shader translator.
pub struct ShaderCache {
    map: HashMap<blake3::Hash, ShaderTranslation>,
    wgsl_options: shader::WgslOptions,
}

impl ShaderCache {
    pub fn new(wgsl_options: shader::WgslOptions) -> Self {
        Self {
            map: HashMap::new(),
            wgsl_options,
        }
    }

    pub fn wgsl_options(&self) -> shader::WgslOptions {
        self.wgsl_options
    }

    pub fn set_wgsl_options(&mut self, wgsl_options: shader::WgslOptions) {
        if self.wgsl_options != wgsl_options {
            self.wgsl_options = wgsl_options;
            self.map.clear();
        }
    }

    pub fn get_or_translate(
        &mut self,
        bytes: &[u8],
    ) -> Result<ShaderCacheLookup<'_>, ShaderTranslateError> {
        use std::collections::hash_map::Entry;

        if bytes.len() > MAX_D3D9_SHADER_BLOB_BYTES {
            return Err(ShaderTranslateError::Malformed(format!(
                "shader blob length {} exceeds maximum {} bytes",
                bytes.len(),
                MAX_D3D9_SHADER_BLOB_BYTES
            )));
        }

        let hash = blake3::hash(bytes);
        match self.map.entry(hash) {
            Entry::Occupied(e) => Ok(ShaderCacheLookup {
                source: ShaderCacheLookupSource::Memory,
                shader: e.into_mut(),
            }),
            Entry::Vacant(e) => {
                let translated = translate_d3d9_shader_to_wgsl(bytes, self.wgsl_options)?;
                Ok(ShaderCacheLookup {
                    source: ShaderCacheLookupSource::Translated,
                    shader: e.insert(translated),
                })
            }
        }
    }
}

impl Default for ShaderCache {
    fn default() -> Self {
        Self::new(shader::WgslOptions::default())
    }
}

#[derive(Debug, Error)]
pub enum ShaderTranslateError {
    #[error("dxbc error: {0}")]
    Dxbc(#[from] dxbc::DxbcError),
    #[error("malformed shader bytecode: {0}")]
    Malformed(String),
    #[error("shader translation failed: {0}")]
    Translation(String),
}

impl ShaderTranslation {
    pub fn stage(&self) -> shader::ShaderStage {
        self.version.stage
    }

    /// Bind group index used for texture/sampler bindings in this shader stage.
    ///
    /// Contract:
    /// - group(0): constants shared by VS/PS (bindings 0/1/2 for float/int/bool constants)
    /// - group(1): VS texture/sampler bindings
    /// - group(2): PS texture/sampler bindings
    /// - group(3): optional half-pixel-center uniform buffer (VS only)
    pub fn sampler_group(&self) -> u32 {
        match self.stage() {
            shader::ShaderStage::Vertex => 1,
            shader::ShaderStage::Pixel => 2,
        }
    }

    /// Compute a compact mask of D3D9 sampler registers referenced by the translated WGSL.
    ///
    /// Only sampler indices `0..=15` participate in the mask.
    pub fn used_samplers_mask(&self) -> u16 {
        let mut mask = 0u16;
        for &s in &self.used_samplers {
            if s < 16 {
                mask |= 1u16 << s;
            }
        }
        mask
    }

    /// Binding numbers used for `@group(self.sampler_group()) @binding(n)` declarations for the
    /// given D3D9 sampler register `s`.
    ///
    /// Contract:
    /// - texture binding = `2*s`
    /// - sampler binding = `2*s + 1`
    pub fn sampler_binding_pair(s: u16) -> (u32, u32) {
        let tex_binding = u32::from(s) * 2;
        (tex_binding, tex_binding + 1)
    }

    /// Returns the `TextureType` declared for `s#` (`dcl_* s#`) when present, defaulting to 2D.
    pub fn sampler_texture_type(&self, s: u16) -> TextureType {
        self.sampler_texture_types
            .get(&s)
            .copied()
            .unwrap_or(TextureType::Texture2D)
    }
}

/// High-level D3D9 shader translation entrypoint with a best-effort compatibility fallback.
///
/// Policy:
/// - Try the strict SM3 translator first (`aero_d3d9::sm3`).
/// - If it fails with an "unsupported feature" style error (opcode/modifier/register file),
///   fall back to the legacy translator (`aero_d3d9::shader`), which skips unknown opcodes.
/// - If the bytecode is malformed (truncated token stream, out-of-bounds DXBC), return an error.
pub fn translate_d3d9_shader_to_wgsl(
    bytes: &[u8],
    options: shader::WgslOptions,
) -> Result<ShaderTranslation, ShaderTranslateError> {
    if bytes.len() > MAX_D3D9_SHADER_BLOB_BYTES {
        return Err(ShaderTranslateError::Malformed(format!(
            "shader blob length {} exceeds maximum {} bytes",
            bytes.len(),
            MAX_D3D9_SHADER_BLOB_BYTES
        )));
    }
    let token_stream = dxbc::extract_shader_bytecode(bytes)?;
    if token_stream.len() > MAX_D3D9_SHADER_BYTECODE_BYTES {
        return Err(ShaderTranslateError::Malformed(format!(
            "shader bytecode length {} exceeds maximum {} bytes",
            token_stream.len(),
            MAX_D3D9_SHADER_BYTECODE_BYTES
        )));
    }
    let token_stream = crate::token_stream::normalize_sm2_sm3_instruction_lengths(token_stream)
        .map_err(ShaderTranslateError::Malformed)?;

    match try_translate_sm3(token_stream.as_ref(), options) {
        Ok(ok) => {
            validate_sampler_texture_types(&ok.used_samplers, &ok.sampler_texture_types)?;
            Ok(ok)
        }
        Err(err) => {
            if !err.is_fallbackable() {
                if err.is_malformed() {
                    return Err(ShaderTranslateError::Malformed(err.to_string()));
                }
                return Err(ShaderTranslateError::Translation(err.to_string()));
            }

            // Fallback to the legacy translator. Use the extracted token stream so malformed DXBC
            // (already handled above) can't be silently bypassed.
            let program = shader::parse(token_stream.as_ref()).map_err(|e| match e {
                // Treat obvious truncation/shape issues as malformed input rather than a generic
                // translation failure.
                shader::ShaderError::TokenStreamTooSmall
                | shader::ShaderError::TokenCountTooLarge { .. }
                | shader::ShaderError::BytecodeTooLarge { .. }
                | shader::ShaderError::UnexpectedEof
                // Invalid enum encodings / control-flow structure are malformed input.
                | shader::ShaderError::UnsupportedSrcModifier(_)
                | shader::ShaderError::UnsupportedCompareOp(_)
                | shader::ShaderError::UnsupportedVersion(_)
                | shader::ShaderError::InvalidControlFlow(_)
                | shader::ShaderError::InvalidDstRegisterFile { .. }
                | shader::ShaderError::InvalidSrcRegisterFile { .. }
                // Out-of-range indices / unknown enum values are malformed input.
                | shader::ShaderError::RegisterIndexTooLarge { .. }
                | shader::ShaderError::UnsupportedSamplerTextureType { .. } => {
                    ShaderTranslateError::Malformed(e.to_string())
                }
                other => ShaderTranslateError::Translation(other.to_string()),
            })?;
            let ir = shader::to_ir(&program);
            let wgsl = shader::generate_wgsl_with_options(&ir, options)
                .map_err(|e| ShaderTranslateError::Translation(e.to_string()))?;
            let shader::ShaderIr {
                uses_semantic_locations,
                semantic_locations,
                used_samplers,
                sampler_texture_types,
                ..
            } = ir;
            let out = ShaderTranslation {
                backend: ShaderTranslateBackend::LegacyFallback,
                version: program.version,
                wgsl: wgsl.wgsl,
                entry_point: wgsl.entry_point,
                uses_semantic_locations,
                semantic_locations,
                used_samplers,
                sampler_texture_types,
                fallback_reason: Some(err.to_string()),
            };
            validate_sampler_texture_types(&out.used_samplers, &out.sampler_texture_types)?;
            Ok(out)
        }
    }
}

fn validate_sampler_texture_types(
    used_samplers: &BTreeSet<u16>,
    sampler_texture_types: &HashMap<u16, TextureType>,
) -> Result<(), ShaderTranslateError> {
    // D3D9 supports 1D/2D/3D/Cube sampler declarations.
    //
    // The D3D9 executor does not currently support binding 1D/3D textures, but unknown sampler
    // texture type encodings are treated as malformed bytecode and must still be rejected.
    for &sampler in used_samplers {
        let ty = sampler_texture_types
            .get(&sampler)
            .copied()
            .unwrap_or(TextureType::Texture2D);
        match ty {
            TextureType::Texture2D | TextureType::TextureCube => {}
            // The runtime can tolerate unsupported sampler declarations as long as the sampler is
            // never used. However, once a sampler is actually referenced by a texture instruction
            // we must reject unsupported dimensions so callers don't assume the command stream can
            // bind them.
            TextureType::Texture1D | TextureType::Texture3D => {
                return Err(ShaderTranslateError::Translation(format!(
                    "unsupported sampler texture type {ty:?} for s{sampler} (supported: 2D/Cube)"
                )));
            }
            // Unknown texture type encodings are treated as malformed input (not a fallbackable
            // unsupported feature).
            TextureType::Unknown(_) => {
                return Err(ShaderTranslateError::Malformed(format!(
                    "unsupported sampler texture type {ty:?} for s{sampler} (supported: 2D/Cube)"
                )));
            }
        }
    }
    Ok(())
}
#[derive(Debug)]
enum Sm3TranslateFailure {
    Decode(sm3::decode::DecodeError),
    Build(sm3::ir_builder::BuildError),
    Verify(sm3::verify::VerifyError),
    Wgsl(sm3::wgsl::WgslError),
}

impl Sm3TranslateFailure {
    fn is_fallbackable(&self) -> bool {
        // Fallback is intended for *valid* shaders that use features not yet supported by the
        // strict SM3 pipeline (missing opcode/register/modifier coverage, WGSL lowering gaps).
        //
        // Do **not** use broad substring matching on the formatted error string: decode errors in
        // particular can contain phrases like "not supported" for *malformed* bytecode (e.g. nested
        // relative addressing), and falling back would allow hostile inputs to bypass the stricter
        // decoder/validator.
        match self {
            // Most decode errors indicate malformed/untrusted bytecode. The one exception we
            // intentionally fall back on is an unknown opcode: the legacy translator skips unknown
            // opcodes so we can keep games running while the strict pipeline gains coverage.
            Sm3TranslateFailure::Decode(e) => e
                .message
                .to_ascii_lowercase()
                .contains("unsupported opcode"),

            // IR build errors are generally higher-level semantic issues. We treat explicit
            // "not supported" messages as fallbackable feature gaps.
            Sm3TranslateFailure::Build(e) => {
                // Unknown opcodes are treated as feature gaps: the legacy translator skips unknown
                // opcodes so we can keep games running while the strict pipeline gains coverage.
                //
                // Prefer matching the structured opcode value instead of substring matching on
                // `BuildError::message` so future message changes don't silently alter fallback
                // policy.
                if matches!(e.opcode, sm3::decode::Opcode::Unknown(_)) {
                    return true;
                }

                let msg = e.message.to_ascii_lowercase();
                // Similar to WGSL lowering errors: relative addressing failures are treated as
                // malformed bytecode, not a fallbackable "unsupported feature". Otherwise hostile
                // inputs could use legacy fallback as an escape hatch.
                if msg.contains("relative") && msg.contains("address") {
                    return false;
                }
                // Do **not** fall back for opcode-specific encoding errors (e.g. invalid TEX opcode
                // "specific" fields). Those indicate malformed bytecode and the legacy translator
                // is intentionally more permissive, which could make fallback an escape hatch.
                msg.contains("not supported")
            }

            // Verify errors represent malformed IR and should not fall back.
            Sm3TranslateFailure::Verify(_) => false,

            // WGSL lowering errors can be either feature gaps or malformed IR. Treat explicit
            // "unsupported"/"not supported" messages as fallbackable, except for relative-addressing
            // failures (these are treated as malformed to avoid using fallback as an escape hatch).
            Sm3TranslateFailure::Wgsl(e) => {
                let msg = e.message.to_ascii_lowercase();
                if msg.contains("unsupported sampler texture type") {
                    return false;
                }
                // Be permissive in matching: we want to catch both "relative addressing" and
                // phrases like "relative register addressing".
                if msg.contains("relative") && msg.contains("address") {
                    return false;
                }
                msg.contains("unsupported") || msg.contains("not supported")
            }
        }
    }

    fn is_malformed(&self) -> bool {
        // Structural corruption should not trigger fallback. In practice this is caught at decode.
        //
        // Some feature gaps are treated as *malformed* rather than a generic translation failure
        // because we do not want callers to fall back to the legacy translator as an escape hatch.
        // Relative addressing is the main case today: it can be legal SM3, but our WGSL lowering
        // cannot represent it safely/portably.
        matches!(self, Sm3TranslateFailure::Decode(_))
            // Verify errors indicate that the decoded shader produced invalid IR. This is treated
            // as malformed input rather than a recoverable translation failure, and should never
            // trigger legacy fallback.
            || matches!(self, Sm3TranslateFailure::Verify(_))
            || matches!(self, Sm3TranslateFailure::Build(e) if {
                let msg = e.message.to_ascii_lowercase();
                msg.contains("unsupported source modifier")
                    || msg.contains("unknown result shift modifier")
                    || msg.contains("unsupported register file")
                    || msg.contains("unsupported encoding")
                    || (msg.contains("relative") && msg.contains("address"))
                    || msg.contains("missing ")
            })
            || matches!(self, Sm3TranslateFailure::Wgsl(e) if {
                let msg = e.message.to_ascii_lowercase();
                (msg.contains("relative") && msg.contains("address"))
                    // Unknown `dcl_* s#` texture-type encodings are malformed input.
                    || (msg.contains("unsupported sampler texture type") && msg.contains("unknown("))
            })
    }
}

impl fmt::Display for Sm3TranslateFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Sm3TranslateFailure::Decode(e) => e.fmt(f),
            Sm3TranslateFailure::Build(e) => e.fmt(f),
            Sm3TranslateFailure::Verify(e) => e.fmt(f),
            Sm3TranslateFailure::Wgsl(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for Sm3TranslateFailure {}

fn try_translate_sm3(
    token_stream: &[u8],
    options: shader::WgslOptions,
) -> Result<ShaderTranslation, Sm3TranslateFailure> {
    let decoded = sm3::decode_u8_le_bytes(token_stream).map_err(Sm3TranslateFailure::Decode)?;
    let ir = sm3::build_ir(&decoded).map_err(Sm3TranslateFailure::Build)?;
    sm3::verify_ir(&ir).map_err(Sm3TranslateFailure::Verify)?;
    let wgsl = sm3::generate_wgsl_with_options(
        &ir,
        sm3::wgsl::WgslOptions {
            half_pixel_center: options.half_pixel_center,
        },
    )
    .map_err(Sm3TranslateFailure::Wgsl)?;
    let used_samplers = collect_used_samplers_sm3(&ir);
    let sampler_texture_types = collect_sampler_texture_types_sm3(&ir);
    let semantic_locations = collect_semantic_locations_sm3(&ir);

    let stage = match decoded.version.stage {
        sm3::types::ShaderStage::Vertex => shader::ShaderStage::Vertex,
        sm3::types::ShaderStage::Pixel => shader::ShaderStage::Pixel,
    };
    let version = shader::ShaderVersion {
        stage,
        model: shader::ShaderModel {
            major: decoded.version.major,
            minor: decoded.version.minor,
        },
    };

    Ok(ShaderTranslation {
        backend: ShaderTranslateBackend::Sm3,
        version,
        wgsl: wgsl.wgsl,
        entry_point: wgsl.entry_point,
        uses_semantic_locations: ir.uses_semantic_locations,
        semantic_locations,
        used_samplers,
        sampler_texture_types,
        fallback_reason: None,
    })
}

fn semantic_to_decl_usage(semantic: &sm3::ir::Semantic) -> Option<(DeclUsage, u8)> {
    use sm3::ir::Semantic;

    let (usage, index) = match semantic {
        Semantic::Position(i) => (DeclUsage::Position, *i),
        Semantic::BlendWeight(i) => (DeclUsage::BlendWeight, *i),
        Semantic::BlendIndices(i) => (DeclUsage::BlendIndices, *i),
        Semantic::Normal(i) => (DeclUsage::Normal, *i),
        Semantic::Tangent(i) => (DeclUsage::Tangent, *i),
        Semantic::Binormal(i) => (DeclUsage::Binormal, *i),
        Semantic::Color(i) => (DeclUsage::Color, *i),
        Semantic::TexCoord(i) => (DeclUsage::TexCoord, *i),
        Semantic::PositionT(i) => (DeclUsage::PositionT, *i),
        Semantic::PointSize(i) => (DeclUsage::PSize, *i),
        Semantic::Fog(i) => (DeclUsage::Fog, *i),
        Semantic::Depth(i) => (DeclUsage::Depth, *i),
        Semantic::Sample(i) => (DeclUsage::Sample, *i),
        Semantic::TessFactor(i) => (DeclUsage::TessFactor, *i),
        Semantic::Other { usage, index } => {
            let usage = DeclUsage::from_u8(*usage).ok()?;
            return Some((usage, *index));
        }
    };
    Some((usage, index))
}

fn collect_semantic_locations_sm3(ir: &sm3::ir::ShaderIr) -> Vec<shader::SemanticLocation> {
    // Only vertex shaders use semantic-based vertex attribute remapping.
    if ir.version.stage != sm3::types::ShaderStage::Vertex || !ir.uses_semantic_locations {
        return Vec::new();
    }

    // Reconstruct the semantic->location mapping from the full declared DCL list.
    //
    // Note: The SM3 IR builder remaps only the input registers that are actually referenced by the
    // instruction stream. Declared-but-unused inputs must still participate in the mapping so the
    // host can bind vertex buffers consistently and avoid location collisions in vertex
    // declarations (even when the shader doesn't read those attributes).
    //
    // Build an adaptive map from the ordered DCL semantic list (deduped by first occurrence),
    // matching the IR builder's allocation policy.
    let mut dcl_semantics = Vec::<(DeclUsage, u8)>::new();
    let mut seen_semantics = HashSet::<(DeclUsage, u8)>::new();
    for decl in &ir.inputs {
        if decl.reg.file != sm3::ir::RegFile::Input {
            continue;
        }
        let Some(pair) = semantic_to_decl_usage(&decl.semantic) else {
            continue;
        };
        if seen_semantics.insert(pair) {
            dcl_semantics.push(pair);
        }
    }

    match AdaptiveLocationMap::new(dcl_semantics.iter().copied()) {
        Ok(map) => dcl_semantics
            .into_iter()
            .filter_map(|(usage, usage_index)| {
                let location = map.location_for(usage, usage_index).ok()?;
                Some(shader::SemanticLocation {
                    usage,
                    usage_index,
                    location,
                })
            })
            .collect(),

        Err(_) => {
            // Should not happen: `uses_semantic_locations` implies the IR builder was able to
            // construct a location map.
            //
            // Fall back to exposing the post-build input register indices (which will still be
            // correct for inputs the shader actually reads).
            let mut out = Vec::<shader::SemanticLocation>::new();
            let mut seen = HashSet::<(DeclUsage, u8)>::new();
            for decl in &ir.inputs {
                if decl.reg.file != sm3::ir::RegFile::Input {
                    continue;
                }
                let Some((usage, usage_index)) = semantic_to_decl_usage(&decl.semantic) else {
                    continue;
                };
                if !seen.insert((usage, usage_index)) {
                    continue;
                }
                out.push(shader::SemanticLocation {
                    usage,
                    usage_index,
                    location: decl.reg.index,
                });
            }
            out
        }
    }
}

fn collect_used_samplers_sm3(ir: &sm3::ir::ShaderIr) -> BTreeSet<u16> {
    let mut out = BTreeSet::new();
    collect_used_samplers_block(&ir.body, &mut out);
    for body in ir.subroutines.values() {
        collect_used_samplers_block(body, &mut out);
    }
    out
}

fn collect_sampler_texture_types_sm3(ir: &sm3::ir::ShaderIr) -> HashMap<u16, TextureType> {
    let mut out = HashMap::new();
    for sampler in &ir.samplers {
        let Ok(index) = u16::try_from(sampler.index) else {
            continue;
        };
        out.insert(index, sampler.texture_type);
    }
    out
}

fn collect_used_samplers_block(block: &sm3::ir::Block, out: &mut BTreeSet<u16>) {
    for stmt in &block.stmts {
        match stmt {
            sm3::ir::Stmt::Op(op) => {
                if let sm3::ir::IrOp::TexSample { sampler, .. } = op {
                    if let Ok(s) = u16::try_from(*sampler) {
                        out.insert(s);
                    }
                }
            }
            sm3::ir::Stmt::If {
                then_block,
                else_block,
                ..
            } => {
                collect_used_samplers_block(then_block, out);
                if let Some(else_block) = else_block {
                    collect_used_samplers_block(else_block, out);
                }
            }
            sm3::ir::Stmt::Loop { body, .. } => collect_used_samplers_block(body, out),
            sm3::ir::Stmt::Rep { body, .. } => collect_used_samplers_block(body, out),
            sm3::ir::Stmt::Break
            | sm3::ir::Stmt::BreakIf { .. }
            | sm3::ir::Stmt::Discard { .. }
            | sm3::ir::Stmt::Call { .. }
            | sm3::ir::Stmt::Return => {}
        }
    }
}
