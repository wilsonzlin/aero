use std::collections::{BTreeSet, HashMap};
use std::fmt;

use thiserror::Error;

use crate::dxbc;
use crate::shader;
use crate::shader_limits::MAX_D3D9_SHADER_BLOB_BYTES;
use crate::sm3;
use crate::sm3::decode::TextureType;

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
    /// Semantic→location mapping produced by shader translation when `uses_semantic_locations` is
    /// true.
    ///
    /// Some translation paths use the fixed [`crate::vertex::StandardLocationMap`] and therefore do
    /// not need to return an explicit mapping; in those cases this vector is empty and host-side
    /// executors should fall back to the standard map.
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

    match try_translate_sm3(token_stream, options) {
        Ok(ok) => Ok(ok),
        Err(err) => {
            if !err.is_fallbackable() {
                return Err(ShaderTranslateError::Malformed(err.to_string()));
            }

            // Fallback to the legacy translator. Use the extracted token stream so malformed DXBC
            // (already handled above) can't be silently bypassed.
            //
            // Note: the SM3 decoder uses the real SM2/3 instruction length encoding, while the
            // legacy bring-up parser expects a simplified "operand count" length field. If the
            // direct parse fails, retry after rewriting instruction length fields.
            let program = match shader::parse(token_stream) {
                Ok(p) => p,
                Err(parse_err) => {
                    let converted = reencode_sm3_instruction_lengths_for_legacy(token_stream)
                        .map_err(ShaderTranslateError::Malformed)?;
                    shader::parse(&converted).map_err(|e| {
                        ShaderTranslateError::Translation(format!(
                            "{parse_err}; after SM3-length reencode: {e}"
                        ))
                    })?
                }
            };
            let ir = shader::to_ir(&program);
            let wgsl = shader::generate_wgsl_with_options(&ir, options)
                .map_err(|e| ShaderTranslateError::Translation(e.to_string()))?;
            Ok(ShaderTranslation {
                backend: ShaderTranslateBackend::LegacyFallback,
                version: program.version,
                wgsl: wgsl.wgsl,
                entry_point: wgsl.entry_point,
                uses_semantic_locations: ir.uses_semantic_locations,
                semantic_locations: ir.semantic_locations,
                used_samplers: ir.used_samplers,
                sampler_texture_types: ir.sampler_texture_types,
                fallback_reason: Some(err.to_string()),
            })
        }
    }
}

fn reencode_sm3_instruction_lengths_for_legacy(token_stream: &[u8]) -> Result<Vec<u8>, String> {
    if !token_stream.len().is_multiple_of(4) {
        return Err(format!(
            "token stream length {} is not a multiple of 4",
            token_stream.len()
        ));
    }
    if token_stream.len() < 4 {
        return Err("token stream too small".to_owned());
    }

    let mut tokens: Vec<u32> = token_stream
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
        .collect();

    let mut idx = 1usize;
    while idx < tokens.len() {
        let token = tokens[idx];
        let opcode = (token & 0xFFFF) as u16;

        // Comments are variable-length data blocks that should be skipped.
        // Layout: opcode=0xFFFE, length in DWORDs in bits 16..30.
        if opcode == 0xFFFE {
            let comment_len = ((token >> 16) & 0x7FFF) as usize;
            let total_len = 1usize
                .checked_add(comment_len)
                .ok_or_else(|| "comment length overflow".to_owned())?;
            if idx + total_len > tokens.len() {
                return Err(format!(
                    "comment length {comment_len} exceeds remaining tokens {}",
                    tokens.len() - idx
                ));
            }
            idx += total_len;
            continue;
        }

        if opcode == 0xFFFF {
            break;
        }

        // In SM2/3 bytecode, bits 24..27 encode the *total* instruction length (in DWORD tokens),
        // including the opcode token itself. A value of 0 is treated as a 1-token instruction.
        let mut length = ((token >> 24) & 0x0F) as usize;
        if length == 0 {
            length = 1;
        }
        if idx + length > tokens.len() {
            return Err(format!(
                "instruction length {length} exceeds remaining tokens {}",
                tokens.len() - idx
            ));
        }

        // Legacy shader parser expects the length field to contain the number of operand tokens
        // (excluding the opcode token), so rewrite length from `N` to `N-1`.
        let operand_count = (length - 1) as u32;
        tokens[idx] = (token & !(0x0F << 24)) | ((operand_count & 0x0F) << 24);

        idx += length;
    }

    let mut out = Vec::with_capacity(token_stream.len());
    for t in tokens {
        out.extend_from_slice(&t.to_le_bytes());
    }
    Ok(out)
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
        let msg = self.to_string().to_ascii_lowercase();
        // These errors are typically due to incomplete SM3 opcode/register/modifier coverage or
        // WGSL lowering limitations. In these cases, falling back to the legacy translator is
        // preferable to hard-failing the entire draw stream.
        msg.contains("unsupported") || msg.contains("not supported")
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
    let wgsl = sm3::generate_wgsl(&ir).map_err(Sm3TranslateFailure::Wgsl)?;
    let used_samplers = collect_used_samplers_sm3(&ir);
    let sampler_texture_types = collect_sampler_texture_types_sm3(&ir);

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

    let mut wgsl_str = wgsl.wgsl;
    if stage == shader::ShaderStage::Vertex && options.half_pixel_center {
        inject_half_pixel_center_sm3_vertex_wgsl(&mut wgsl_str).map_err(|message| {
            Sm3TranslateFailure::Wgsl(sm3::wgsl::WgslError { message })
        })?;
    }

    Ok(ShaderTranslation {
        backend: ShaderTranslateBackend::Sm3,
        version,
        wgsl: wgsl_str,
        entry_point: wgsl.entry_point,
        uses_semantic_locations: ir.uses_semantic_locations,
        // SM3 translation currently uses StandardLocationMap for semantic remapping and therefore
        // does not need to return an explicit mapping.
        semantic_locations: Vec::new(),
        used_samplers,
        sampler_texture_types,
        fallback_reason: None,
    })
}

fn collect_used_samplers_sm3(ir: &sm3::ir::ShaderIr) -> BTreeSet<u16> {
    let mut out = BTreeSet::new();
    collect_used_samplers_block(&ir.body, &mut out);
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
            sm3::ir::Stmt::Break
            | sm3::ir::Stmt::BreakIf { .. }
            | sm3::ir::Stmt::Discard { .. } => {}
        }
    }
}

fn inject_half_pixel_center_sm3_vertex_wgsl(wgsl: &mut String) -> Result<(), String> {
    // Match `shader::generate_wgsl_with_options`' half-pixel declarations so the executor's bind
    // group layout (group(3) binding(0) uniform buffer with 16 bytes) is compatible across both
    // translation backends.
    const DECL: &str =
        "struct HalfPixel { inv_viewport: vec2<f32>, _pad: vec2<f32>, };\n@group(3) @binding(0) var<uniform> half_pixel: HalfPixel;\n\n";

    if !wgsl.contains("@group(3) @binding(0) var<uniform> half_pixel") {
        let insert_at = wgsl
            .find("struct VsInput")
            .or_else(|| wgsl.find("struct VsOut"))
            .ok_or_else(|| "half-pixel injection failed: could not find vertex interface structs".to_owned())?;
        wgsl.insert_str(insert_at, DECL);
    }

    let marker = "  out.pos = oPos;\n";
    let Some(pos) = wgsl.find(marker) else {
        return Err("half-pixel injection failed: could not find out.pos assignment".to_owned());
    };
    let insert_at = pos + marker.len();
    if wgsl.contains("half_pixel.inv_viewport") {
        // Already injected.
        return Ok(());
    }

    wgsl.insert_str(
        insert_at,
        "  // D3D9 half-pixel center adjustment: emulate the D3D9 viewport transform's\n  // -0.5 window-space bias by nudging clip-space XY by (-1/width, +1/height) * w.\n  out.pos.x = out.pos.x - half_pixel.inv_viewport.x * out.pos.w;\n  out.pos.y = out.pos.y + half_pixel.inv_viewport.y * out.pos.w;\n",
    );
    Ok(())
}
