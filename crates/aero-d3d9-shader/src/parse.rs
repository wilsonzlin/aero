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

    let mut tokens = Vec::with_capacity(raw.len() / 4);
    for chunk in raw.chunks_exact(4) {
        tokens.push(u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }

    let mut r = TokenReader::new(&tokens);

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

        let len = ((opcode_token >> 24) & 0x0F) as usize;
        if r.remaining() < len {
            return Err(ShaderParseError::TruncatedInstruction {
                opcode: opcode_raw,
                at_token,
                needed_tokens: len,
                remaining_tokens: r.remaining(),
            });
        }
        let operands = r.read_many(len).unwrap();

        if opcode_raw == OPCODE_DCL {
            if operands.len() < 2 {
                return Err(ShaderParseError::TruncatedInstruction {
                    opcode: opcode_raw,
                    at_token,
                    needed_tokens: 2,
                    remaining_tokens: operands.len(),
                });
            }
            let decl_token = operands[0];
            let dst = decode_dst(operands[1]);

            match dst.reg.ty {
                RegisterType::Sampler => {
                    let tex_ty_raw = ((decl_token >> 27) & 0xF) as u8;
                    declarations.push(Decl::Sampler {
                        reg: dst.reg,
                        texture_type: SamplerTextureType::from_raw(tex_ty_raw),
                    });
                }
                _ => {
                    let usage_raw = (decl_token & 0x1F) as u8;
                    let usage_index = ((decl_token >> 16) & 0xF) as u8;
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
            let mut idx = 0usize;
            let predicate = if predicated {
                Some(decode_src(operands, &mut idx))
            } else {
                None
            };

            let has_dst = matches!(
                opcode,
                crate::Opcode::Mov
                    | crate::Opcode::Add
                    | crate::Opcode::Mul
                    | crate::Opcode::Mad
                    | crate::Opcode::Dp3
                    | crate::Opcode::Dp4
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
                    | crate::Opcode::Texld
                    | crate::Opcode::Texldp
                    | crate::Opcode::Texldb
                    | crate::Opcode::Texldd
                    | crate::Opcode::Texldl
                    | crate::Opcode::Mova
                    | crate::Opcode::Setp
            );

            let dst = if has_dst && idx < operands.len() {
                let dst = crate::reg::decode_dst(operands[idx]);
                idx = idx.saturating_add(1);
                Some(dst)
            } else {
                None
            };

            let mut src = Vec::new();
            while idx < operands.len() {
                src.push(decode_src(operands, &mut idx));
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
            let mut all_tokens = Vec::with_capacity(1 + operands.len());
            all_tokens.push(opcode_token);
            all_tokens.extend_from_slice(operands);
            instructions.push(Instruction::Unknown {
                opcode_raw,
                tokens: all_tokens,
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
