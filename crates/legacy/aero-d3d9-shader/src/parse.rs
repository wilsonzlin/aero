use crate::error::ShaderParseError;
use crate::opcode::{decode_opcode, OPCODE_COMMENT, OPCODE_DCL, OPCODE_END};
use crate::reg::{
    CommentBlock, Decl, Instruction, RegisterType, SamplerTextureType, ShaderModel, ShaderStage,
    ShaderStats, Usage,
};

use crate::reg::{decode_dst, decode_src};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct D3d9Shader {
    pub stage: ShaderStage,
    pub model: ShaderModel,
    pub declarations: Vec<Decl>,
    pub instructions: Vec<Instruction>,
    pub comments: Vec<CommentBlock>,
    pub stats: ShaderStats,
}

/// Maximum accepted D3D9 shader bytecode length in bytes.
///
/// This crate is primarily used for debugging/disassembly, but the shader blob is still treated as
/// untrusted input. The limit prevents pathological blobs from causing large allocations while
/// converting raw bytes into a `Vec<u32>` token stream.
const MAX_D3D9_SHADER_BYTECODE_BYTES: usize = 256 * 1024; // 256 KiB

pub fn parse_shader(blob: &[u8]) -> Result<D3d9Shader, ShaderParseError> {
    let raw = if blob.starts_with(b"DXBC") {
        let dxbc = aero_dxbc::DxbcFile::parse(blob)?;
        dxbc.find_first_shader_chunk()
            .ok_or(ShaderParseError::DxbcMissingShaderChunk)?
            .data
    } else {
        blob
    };

    if raw.is_empty() {
        return Err(ShaderParseError::Empty);
    }
    if raw.len() % 4 != 0 {
        return Err(ShaderParseError::InvalidByteLength { len: raw.len() });
    }
    if raw.len() > MAX_D3D9_SHADER_BYTECODE_BYTES {
        return Err(ShaderParseError::BytecodeTooLarge {
            len: raw.len(),
            max: MAX_D3D9_SHADER_BYTECODE_BYTES,
        });
    }

    let mut tokens = Vec::with_capacity(raw.len() / 4);
    for chunk in raw.chunks_exact(4) {
        tokens.push(u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }

    match parse_tokens(&tokens) {
        Ok(ok) => Ok(ok),
        Err(err) => {
            // Retry after normalizing operand-count-encoded streams, but preserve the original
            // parse error when normalization does not help (important for tests that assert on
            // specific error shapes).
            if let Some(normalized) =
                crate::len_normalize::normalize_sm2_sm3_instruction_lengths(&tokens)
            {
                if let Ok(ok) = parse_tokens(&normalized) {
                    return Ok(ok);
                }
            }
            Err(err)
        }
    }
}

fn parse_tokens(tokens: &[u32]) -> Result<D3d9Shader, ShaderParseError> {
    let mut r = TokenReader::new(tokens);

    let version_token = r.read().ok_or(ShaderParseError::Empty)?;
    let shader_type = (version_token >> 16) as u16;
    let major = ((version_token >> 8) & 0xFF) as u8;
    let minor = (version_token & 0xFF) as u8;

    let stage = match shader_type {
        0xFFFE => ShaderStage::Vertex,
        0xFFFF => ShaderStage::Pixel,
        _ => {
            return Err(ShaderParseError::InvalidVersionToken {
                token: version_token,
            })
        }
    };

    let model = ShaderModel { major, minor };

    let mut declarations = Vec::new();
    let mut instructions = Vec::new();
    let mut comments = Vec::new();
    let mut stats = ShaderStats::default();

    while r.remaining() > 0 {
        let at_token = r.pos;
        let opcode_token = r.read().ok_or(ShaderParseError::Truncated { at_token })?;
        let opcode_raw = (opcode_token & 0xFFFF) as u16;

        if opcode_raw == OPCODE_END {
            break;
        }

        if opcode_raw == OPCODE_COMMENT {
            let comment_len = ((opcode_token >> 16) & 0x7FFF) as usize;
            if r.remaining() < comment_len {
                return Err(ShaderParseError::TruncatedInstruction {
                    opcode: opcode_raw,
                    at_token,
                    needed_tokens: comment_len,
                    remaining_tokens: r.remaining(),
                });
            }
            let data = r.read_many(comment_len).unwrap();
            let mut bytes = Vec::with_capacity(data.len() * 4);
            for w in data {
                bytes.extend_from_slice(&w.to_le_bytes());
            }
            comments.push(CommentBlock { bytes });
            continue;
        }

        // D3D9 SM2/SM3 encode the *total* instruction length in DWORD tokens (including the
        // opcode token itself) in bits 24..27. Some operand-less instructions encode this value
        // as `0`, which is interpreted as a 1-token instruction.
        let mut len = ((opcode_token >> 24) & 0x0F) as usize;
        if len == 0 {
            len = 1;
        }
        let operand_len = len.saturating_sub(1);
        if r.remaining() < operand_len {
            return Err(ShaderParseError::TruncatedInstruction {
                opcode: opcode_raw,
                at_token,
                needed_tokens: operand_len,
                remaining_tokens: r.remaining(),
            });
        }
        let operands = r.read_many(operand_len).unwrap();

        if opcode_raw == OPCODE_DCL {
            // D3D9 `DCL` encoding differs between toolchains:
            // - Modern compilers may use a single destination register token, with usage / texture
            //   type encoded in the opcode token.
            // - Older assemblers encode a second "decl token" operand containing usage / sampler
            //   type information.
            let (decl_token, dst_token, dst_token_idx) = match operands {
                // Modern form: `dcl <dst>`
                [dst_token] => (None, *dst_token, 0usize),
                // Legacy form: `dcl <decl_token>, <dst>`
                [decl_token, dst_token, ..] => (Some(*decl_token), *dst_token, 1usize),
                _ => {
                    return Err(ShaderParseError::TruncatedInstruction {
                        opcode: opcode_raw,
                        at_token,
                        needed_tokens: 1,
                        remaining_tokens: operands.len(),
                    })
                }
            };

            let dst = decode_dst(dst_token, stage);
            if matches!(dst.reg.ty, RegisterType::Unknown(_)) {
                return Err(ShaderParseError::InvalidRegisterEncoding {
                    token: dst_token,
                    at_token: at_token + 1 + dst_token_idx,
                });
            }

            match dst.reg.ty {
                RegisterType::Sampler => {
                    // Legacy form: texture type in decl_token[27..31].
                    // Modern form: texture type in opcode_token[16..20].
                    let tex_ty_raw = match decl_token {
                        Some(decl_token) => ((decl_token >> 27) & 0xF) as u8,
                        None => ((opcode_token >> 16) & 0xF) as u8,
                    };
                    declarations.push(Decl::Sampler {
                        reg: dst.reg,
                        texture_type: SamplerTextureType::from_raw(tex_ty_raw),
                    });
                }
                _ => {
                    // Vertex-stage DCLs carry semantic usage information. Pixel-stage DCLs are
                    // typically just register declarations (e.g. `dcl t0`), so we infer texcoord
                    // usage for texture register declarations and fall back to opcode/declaration
                    // token bits otherwise.
                    let (usage_raw, usage_index) = if stage == ShaderStage::Pixel
                        && matches!(dst.reg.ty, RegisterType::Texture)
                    {
                        (5u8, dst.reg.num as u8)
                    } else if let Some(decl_token) = decl_token {
                        ((decl_token & 0x1F) as u8, ((decl_token >> 16) & 0xF) as u8)
                    } else {
                        (
                            ((opcode_token >> 16) & 0xF) as u8,
                            ((opcode_token >> 20) & 0xF) as u8,
                        )
                    };
                    declarations.push(Decl::Dcl {
                        reg: dst.reg,
                        usage: Usage::from_raw(usage_raw),
                        usage_index,
                    });
                }
            }
            continue;
        }

        let predicated = opcode_token & 0x1000_0000 != 0;
        let coissue = opcode_token & 0x4000_0000 != 0;
        let specific = ((opcode_token >> 16) & 0xFF) as u8;

        if let Some(opcode) = decode_opcode(opcode_raw, specific) {
            let (operands, predicate) = if predicated {
                if operands.is_empty() {
                    return Err(ShaderParseError::TruncatedInstruction {
                        opcode: opcode_raw,
                        at_token,
                        needed_tokens: 1,
                        remaining_tokens: 0,
                    });
                }
                let pred_token_idx = operands.len() - 1;
                let pred_slice = &operands[pred_token_idx..];
                // Predicate is encoded as a source parameter token at the end of the operand
                // stream. If the predicate token requests relative addressing, it requires an
                // extra token, which cannot be present because the predicate is already the last
                // token.
                if pred_slice[0] & 0x0000_2000 != 0 {
                    return Err(ShaderParseError::TruncatedInstruction {
                        opcode: opcode_raw,
                        at_token,
                        needed_tokens: operand_len + 1,
                        remaining_tokens: operand_len,
                    });
                }
                let mut pred_idx = 0usize;
                let pred = decode_src(pred_slice, &mut pred_idx, stage);
                validate_src(&pred, pred_slice, at_token + 1 + pred_token_idx, 0)?;
                (&operands[..pred_token_idx], Some(pred))
            } else {
                (operands, None)
            };

            let mut idx = 0usize;
            // The predicate register token (when present) is part of the operand stream, but is
            // treated separately from normal dst/src operands.
            let pred_operand_tokens = if predicated { 1usize } else { 0 };

            let has_dst = matches!(
                opcode,
                crate::Opcode::Mov
                    | crate::Opcode::Add
                    | crate::Opcode::Mul
                    | crate::Opcode::Mad
                    | crate::Opcode::Lrp
                    | crate::Opcode::Dp2
                    | crate::Opcode::Dp2Add
                    | crate::Opcode::Dp3
                    | crate::Opcode::Dp4
                    | crate::Opcode::M4x4
                    | crate::Opcode::Min
                    | crate::Opcode::Max
                    | crate::Opcode::Rcp
                    | crate::Opcode::Rsq
                    | crate::Opcode::Frc
                    | crate::Opcode::Exp
                    | crate::Opcode::Log
                    | crate::Opcode::Lit
                    | crate::Opcode::Dst
                    | crate::Opcode::Cmp
                    | crate::Opcode::Slt
                    | crate::Opcode::Sge
                    | crate::Opcode::Dsx
                    | crate::Opcode::Dsy
                    | crate::Opcode::Texld
                    | crate::Opcode::Texldp
                    | crate::Opcode::Texldb
                    | crate::Opcode::Texldd
                    | crate::Opcode::Texldl
                    | crate::Opcode::Mova
                    | crate::Opcode::Setp
            );

            let dst = if has_dst {
                if idx >= operands.len() {
                    return Err(ShaderParseError::TruncatedInstruction {
                        opcode: opcode_raw,
                        at_token,
                        needed_tokens: idx + 1 + pred_operand_tokens,
                        remaining_tokens: operand_len,
                    });
                }
                let dst_token_idx = idx;
                let dst_token = operands[dst_token_idx];
                let dst = crate::reg::decode_dst(dst_token, stage);
                if matches!(dst.reg.ty, RegisterType::Unknown(_)) {
                    return Err(ShaderParseError::InvalidRegisterEncoding {
                        token: dst_token,
                        at_token: at_token + 1 + dst_token_idx,
                    });
                }
                idx = idx.saturating_add(1);
                Some(dst)
            } else {
                None
            };

            let requires_src =
                has_dst || matches!(opcode, crate::Opcode::Texkill | crate::Opcode::If);

            // Some opcodes have mandatory operands even when the instruction length nibble is
            // malformed (too small). Treat these as truncated encodings rather than silently
            // producing an under-specified instruction.
            if requires_src && idx >= operands.len() {
                return Err(ShaderParseError::TruncatedInstruction {
                    opcode: opcode_raw,
                    at_token,
                    needed_tokens: idx + 1 + pred_operand_tokens,
                    remaining_tokens: operand_len,
                });
            }

            let mut src = Vec::new();
            while idx < operands.len() {
                let token_idx = idx;
                // Relative-addressed source parameters require an extra token.
                if operands[token_idx] & 0x0000_2000 != 0 && token_idx + 1 >= operands.len() {
                    return Err(ShaderParseError::TruncatedInstruction {
                        opcode: opcode_raw,
                        at_token,
                        needed_tokens: token_idx + 2 + pred_operand_tokens,
                        remaining_tokens: operand_len,
                    });
                }
                let s = decode_src(operands, &mut idx, stage);
                validate_src(&s, operands, at_token + 1, token_idx)?;
                src.push(s);
            }

            let inst = Instruction::Op {
                opcode,
                predicated,
                coissue,
                predicate,
                dst,
                src,
            };
            inst.observe_stats(&mut stats);
            instructions.push(inst);
        } else {
            return Err(ShaderParseError::UnknownOpcode {
                opcode: opcode_raw,
                specific,
                at_token,
            });
        }
    }

    Ok(D3d9Shader {
        stage,
        model,
        declarations,
        instructions,
        comments,
        stats,
    })
}

struct TokenReader<'a> {
    tokens: &'a [u32],
    pos: usize,
}

fn validate_src(
    src: &crate::reg::SrcParam,
    operands: &[u32],
    operands_base_token: usize,
    token_idx: usize,
) -> Result<(), ShaderParseError> {
    match src {
        crate::reg::SrcParam::Immediate(_) => Ok(()),
        crate::reg::SrcParam::Register { reg, relative, .. } => {
            if matches!(reg.ty, RegisterType::Unknown(_)) {
                return Err(ShaderParseError::InvalidRegisterEncoding {
                    token: operands[token_idx],
                    at_token: operands_base_token + token_idx,
                });
            }
            if let Some(rel) = relative {
                if matches!(rel.reg.ty, RegisterType::Unknown(_)) {
                    // The relative address register token is always immediately after the main
                    // source token.
                    if let Some(token) = operands.get(token_idx + 1) {
                        return Err(ShaderParseError::InvalidRegisterEncoding {
                            token: *token,
                            at_token: operands_base_token + token_idx + 1,
                        });
                    }
                }
            }
            Ok(())
        }
    }
}

impl<'a> TokenReader<'a> {
    fn new(tokens: &'a [u32]) -> Self {
        Self { tokens, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.tokens.len().saturating_sub(self.pos)
    }

    fn read(&mut self) -> Option<u32> {
        let out = self.tokens.get(self.pos).copied();
        if out.is_some() {
            self.pos += 1;
        }
        out
    }

    fn read_many(&mut self, n: usize) -> Option<&'a [u32]> {
        if self.remaining() < n {
            return None;
        }
        let start = self.pos;
        let end = start + n;
        self.pos = end;
        Some(&self.tokens[start..end])
    }
}
