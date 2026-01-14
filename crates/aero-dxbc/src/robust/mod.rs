//! Higher-level DXBC parsing helpers.
//!
//! This module contains the "robust" DXBC container/reflection implementation
//! that originally lived in `aero-d3d9`. It is exposed behind the `robust`
//! feature so both the D3D9 and D3D11 stacks can share a single implementation.

#![allow(missing_docs)]

mod byte_reader;
mod chunks;
mod container;
mod disasm;
mod reflection;
mod signature;

use std::fmt;

const MAX_DXBC_CHUNK_COUNT: u32 = 4096;

pub use chunks::{DxbcChunk, FourCc};
pub use container::DxbcContainer;
pub use disasm::disassemble_sm2_sm3;
pub use reflection::{
    DxbcConstantBuffer, DxbcReflection, DxbcResourceBinding, DxbcType, DxbcVariable,
};
pub use signature::{DxbcSignature, DxbcSignatureParameter};

use self::byte_reader::ByteReaderError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DxbcError {
    InvalidMagic {
        found: FourCc,
    },
    InvalidContainerSize {
        declared: u32,
        actual: usize,
    },
    InvalidContainerSizeTooSmall {
        declared: u32,
        minimum: usize,
    },
    ChunkCountTooLarge {
        chunk_count: u32,
        max: u32,
    },
    UnexpectedEof {
        offset: usize,
        needed: usize,
        remaining: usize,
    },
    OffsetOutOfBounds {
        offset: usize,
        len: usize,
    },
    InvalidUtf8 {
        offset: usize,
    },
    UnterminatedCString {
        offset: usize,
    },
    ChunkOffsetOutOfBounds {
        chunk_index: u32,
        offset: u32,
    },
    ChunkHeaderOutOfBounds {
        chunk_index: u32,
        offset: u32,
    },
    ChunkDataOutOfBounds {
        chunk_index: u32,
        fourcc: FourCc,
        offset: u32,
        size: u32,
        container_size: usize,
    },
    InvalidChunkSizeAlignment {
        fourcc: FourCc,
        size: u32,
    },
    MissingShaderChunk,
    InvalidShaderBytecode {
        reason: &'static str,
    },
    InvalidChunk {
        fourcc: FourCc,
        reason: &'static str,
    },
}

impl fmt::Display for DxbcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DxbcError::InvalidMagic { found } => write!(f, "DXBC magic mismatch (found {found})"),
            DxbcError::InvalidContainerSize { declared, actual } => write!(
                f,
                "DXBC container size {declared} exceeds available data ({actual} bytes)"
            ),
            DxbcError::InvalidContainerSizeTooSmall { declared, minimum } => write!(
                f,
                "DXBC container size {declared} is too small (need at least {minimum} bytes)"
            ),
            DxbcError::ChunkCountTooLarge { chunk_count, max } => {
                write!(f, "DXBC chunk count {chunk_count} exceeds maximum {max}")
            }
            DxbcError::UnexpectedEof {
                offset,
                needed,
                remaining,
            } => write!(
                f,
                "unexpected end of input at offset {offset} (needed {needed} bytes, remaining {remaining} bytes)"
            ),
            DxbcError::OffsetOutOfBounds { offset, len } => {
                write!(f, "offset {offset} out of bounds (len {len})")
            }
            DxbcError::InvalidUtf8 { offset } => write!(f, "invalid utf-8 at offset {offset}"),
            DxbcError::UnterminatedCString { offset } => {
                write!(f, "unterminated c-string at offset {offset}")
            }
            DxbcError::ChunkOffsetOutOfBounds { chunk_index, offset } => write!(
                f,
                "chunk[{chunk_index}] offset {offset} is out of bounds"
            ),
            DxbcError::ChunkHeaderOutOfBounds { chunk_index, offset } => write!(
                f,
                "chunk[{chunk_index}] header at offset {offset} is out of bounds"
            ),
            DxbcError::ChunkDataOutOfBounds {
                chunk_index,
                fourcc,
                offset,
                size,
                container_size,
            } => write!(
                f,
                "chunk[{chunk_index}] {fourcc} (offset={offset}, size={size}) exceeds container size ({container_size} bytes)"
            ),
            DxbcError::InvalidChunkSizeAlignment { fourcc, size } => write!(
                f,
                "chunk {fourcc} size {size} is not aligned as expected"
            ),
            DxbcError::MissingShaderChunk => write!(f, "missing SHDR/SHEX shader bytecode chunk"),
            DxbcError::InvalidShaderBytecode { reason } => {
                write!(f, "invalid shader bytecode: {reason}")
            }
            DxbcError::InvalidChunk { fourcc, reason } => {
                write!(f, "invalid {fourcc} chunk: {reason}")
            }
        }
    }
}

impl std::error::Error for DxbcError {}

impl From<ByteReaderError> for DxbcError {
    fn from(value: ByteReaderError) -> Self {
        match value {
            ByteReaderError::UnexpectedEof {
                offset,
                needed,
                remaining,
            } => DxbcError::UnexpectedEof {
                offset,
                needed,
                remaining,
            },
            ByteReaderError::OffsetOutOfBounds { offset, len } => {
                DxbcError::OffsetOutOfBounds { offset, len }
            }
            ByteReaderError::InvalidUtf8 { offset } => DxbcError::InvalidUtf8 { offset },
            ByteReaderError::UnterminatedCString { offset } => {
                DxbcError::UnterminatedCString { offset }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShaderType {
    Vertex,
    Pixel,
}

impl ShaderType {
    pub fn short(self) -> &'static str {
        match self {
            ShaderType::Vertex => "vs",
            ShaderType::Pixel => "ps",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ShaderModel {
    pub major: u8,
    pub minor: u8,
}

impl fmt::Display for ShaderModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ShaderKey(pub u64);

impl ShaderKey {
    pub fn from_tokens(tokens: &[u32]) -> Self {
        // A simple, stable 64-bit FNV-1a hash over the token stream.
        const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
        const FNV_PRIME: u64 = 0x100000001b3;

        let mut hash = FNV_OFFSET_BASIS;
        for &token in tokens {
            for b in token.to_le_bytes() {
                hash ^= u64::from(b);
                hash = hash.wrapping_mul(FNV_PRIME);
            }
        }
        ShaderKey(hash)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DxbcInstructionStream {
    /// Raw 32-bit instruction tokens as stored in the `SHDR`/`SHEX` chunk.
    pub tokens: Vec<u32>,
    /// Placeholder for a higher-level instruction parser.
    pub parsed: Option<()>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DxbcShader {
    pub shader_type: ShaderType,
    pub shader_model: ShaderModel,
    pub key: ShaderKey,
    pub bytecode: DxbcInstructionStream,
    pub reflection: Option<DxbcReflection>,
    pub input_signature: Option<DxbcSignature>,
    pub output_signature: Option<DxbcSignature>,
    pub patch_constant_signature: Option<DxbcSignature>,
    pub stats: Option<Vec<u32>>,
    pub unknown_chunks: Vec<FourCc>,
}

impl DxbcShader {
    pub fn parse(container_bytes: &[u8]) -> Result<Self, DxbcError> {
        let container = DxbcContainer::parse(container_bytes)?;

        let shader_chunk = container
            .find_first(&FourCc::from("SHDR"))
            .or_else(|| container.find_first(&FourCc::from("SHEX")))
            .ok_or(DxbcError::MissingShaderChunk)?;

        let tokens = parse_u32_token_stream(shader_chunk.data, shader_chunk.fourcc)?;

        let (shader_type, shader_model) = parse_version_token(tokens.first().copied()).ok_or(
            DxbcError::InvalidShaderBytecode {
                reason: "missing/invalid version token",
            },
        )?;

        let reflection = container
            .find_first(&FourCc::from("RDEF"))
            .map(|c| reflection::parse_rdef(c.data))
            .transpose()?;

        fn parse_signature_variants<'a>(
            container: &DxbcContainer<'a>,
            kinds: &[FourCc],
        ) -> Option<Result<DxbcSignature, DxbcError>> {
            let mut any = false;
            let mut first_err = None;
            for &kind in kinds {
                for chunk in container.chunks.iter().filter(|c| c.fourcc == kind) {
                    any = true;
                    match signature::parse_signature(chunk.fourcc, chunk.data) {
                        Ok(sig) => return Some(Ok(sig)),
                        Err(err) => {
                            if first_err.is_none() {
                                first_err = Some(err);
                            }
                        }
                    }
                }
            }
            if !any {
                None
            } else {
                // `any == true` implies we attempted to parse at least one signature chunk, so
                // `first_err` must have been populated.
                Some(Err(
                    first_err.expect("signature parse did not record an error")
                ))
            }
        }

        // Prefer the `*SG1` variants but accept `*SGN` when needed. Some toolchains emit duplicate
        // signature chunks; iterate in file order and take the first one that parses successfully.
        let input_signature =
            parse_signature_variants(&container, &[FourCc::from("ISG1"), FourCc::from("ISGN")])
                .transpose()?;

        let output_signature =
            parse_signature_variants(&container, &[FourCc::from("OSG1"), FourCc::from("OSGN")])
                .transpose()?;

        // Tessellation uses two signature spellings for patch-constant IO:
        // - `PCSG` / `PCG1` for hull shader patch-constant outputs
        // - `PSGN` / `PSG1` for domain shader patch-constant inputs
        //
        // Prefer the dedicated patch-constant signature but fall back to the patch signature.
        let patch_constant_signature = parse_signature_variants(
            &container,
            &[
                FourCc::from("PCG1"),
                FourCc::from("PCSG"),
                FourCc::from("PSG1"),
                FourCc::from("PSGN"),
            ],
        )
        .transpose()?;

        let stats = container
            .find_first(&FourCc::from("STAT"))
            .map(|c| parse_u32_list(c.data, c.fourcc))
            .transpose()?;

        let mut unknown_chunks = Vec::new();
        for chunk in &container.chunks {
            match chunk.fourcc.as_bytes() {
                b"SHDR" | b"SHEX" | b"RDEF" | b"STAT" => {}
                b"ISGN" | b"ISG1" | b"OSGN" | b"OSG1" | b"PSGN" | b"PSG1" | b"PCSG" | b"PCG1" => {}
                _ => unknown_chunks.push(chunk.fourcc),
            }
        }

        Ok(Self {
            shader_type,
            shader_model,
            key: ShaderKey::from_tokens(&tokens),
            bytecode: DxbcInstructionStream {
                tokens,
                parsed: None,
            },
            reflection,
            input_signature,
            output_signature,
            patch_constant_signature,
            stats,
            unknown_chunks,
        })
    }

    pub fn disassemble(&self) -> String {
        disassemble_sm2_sm3(self.shader_type, &self.bytecode.tokens)
    }

    pub fn dump(&self) -> String {
        use std::fmt::Write as _;

        let mut out = String::new();
        let _ = writeln!(
            out,
            "DXBC shader: {}_{}_{} (key=0x{:016x})",
            self.shader_type.short(),
            self.shader_model.major,
            self.shader_model.minor,
            self.key.0
        );

        if let Some(refl) = &self.reflection {
            let _ = writeln!(out, "Reflection:");
            let _ = writeln!(out, "  creator: {:?}", refl.creator);
            let _ = writeln!(out, "  constant_buffers: {}", refl.constant_buffers.len());
            for cb in &refl.constant_buffers {
                let _ = writeln!(
                    out,
                    "    - {} (size={} bytes, vars={})",
                    cb.name,
                    cb.size,
                    cb.variables.len()
                );
                for var in &cb.variables {
                    let _ = writeln!(
                        out,
                        "      * {} @{} ({} bytes) {:?}",
                        var.name, var.offset, var.size, var.ty
                    );
                }
            }

            let _ = writeln!(out, "  resources: {}", refl.resources.len());
            for res in &refl.resources {
                let _ = writeln!(
                    out,
                    "    - {} (type={}, bind_point={}, bind_count={})",
                    res.name, res.input_type, res.bind_point, res.bind_count
                );
            }
        }

        if let Some(sig) = &self.input_signature {
            let _ = writeln!(out, "Input signature: {} params", sig.parameters.len());
            for p in &sig.parameters {
                let _ = writeln!(
                    out,
                    "  - {}{} r{} mask=0x{:x}",
                    p.semantic_name, p.semantic_index, p.register, p.mask
                );
            }
        }

        if let Some(sig) = &self.output_signature {
            let _ = writeln!(out, "Output signature: {} params", sig.parameters.len());
            for p in &sig.parameters {
                let _ = writeln!(
                    out,
                    "  - {}{} r{} mask=0x{:x}",
                    p.semantic_name, p.semantic_index, p.register, p.mask
                );
            }
        }

        if !self.unknown_chunks.is_empty() {
            let _ = writeln!(out, "Unknown chunks: {:?}", self.unknown_chunks);
        }

        out
    }
}

fn parse_u32_list(bytes: &[u8], fourcc: FourCc) -> Result<Vec<u32>, DxbcError> {
    if !bytes.len().is_multiple_of(4) {
        return Err(DxbcError::InvalidChunkSizeAlignment {
            fourcc,
            size: bytes.len() as u32,
        });
    }

    let mut out = Vec::with_capacity(bytes.len() / 4);
    for chunk in bytes.chunks_exact(4) {
        out.push(u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    Ok(out)
}

fn parse_u32_token_stream(bytes: &[u8], fourcc: FourCc) -> Result<Vec<u32>, DxbcError> {
    parse_u32_list(bytes, fourcc)
}

fn parse_version_token(token: Option<u32>) -> Option<(ShaderType, ShaderModel)> {
    let token = token?;
    let major = ((token >> 8) & 0xff) as u8;
    let minor = (token & 0xff) as u8;
    let high = (token >> 16) as u16;
    let shader_type = match high {
        0xfffe => ShaderType::Vertex,
        0xffff => ShaderType::Pixel,
        _ => return None,
    };
    Some((shader_type, ShaderModel { major, minor }))
}
