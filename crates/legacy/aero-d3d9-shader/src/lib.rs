//! Legacy parser + debug disassembler for Direct3D 9 SM2/SM3 shader token streams.
//!
//! This crate is kept for reference and debugging, but it is no longer used by the runtime D3D9
//! translation pipeline. Prefer the canonical implementation in `crates/aero-d3d9/src/sm3`.

mod disasm;
mod error;
mod len_normalize;
mod opcode;
mod parse;
mod reg;
mod token;

pub use crate::error::ShaderParseError;
pub use crate::opcode::Opcode;
pub use crate::parse::D3d9Shader;
pub use crate::reg::{
    CommentBlock, Decl, DstParam, Instruction, Register, RegisterType, SamplerTextureType,
    ShaderModel, ShaderStage, ShaderStats, SrcModifier, SrcParam, Swizzle, Usage,
};

impl D3d9Shader {
    /// Parse a shader blob.
    ///
    /// `blob` may either be a raw DWORD token stream or a DXBC container
    /// produced by `D3DCompile` (in which case we extract `SHEX`/`SHDR`).
    pub fn parse(blob: &[u8]) -> Result<Self, ShaderParseError> {
        parse::parse_shader(blob)
    }

    /// Produce a stable, debug-friendly disassembly.
    pub fn disassemble(&self) -> String {
        disasm::disassemble(self)
    }
}
