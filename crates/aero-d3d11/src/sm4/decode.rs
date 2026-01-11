use core::fmt;

use crate::sm4_ir::{
    DstOperand, OperandModifier, RegFile, RegisterRef, SamplerRef, Sm4Decl, Sm4Inst, Sm4Module,
    SrcKind, SrcOperand, Swizzle, TextureRef, WriteMask,
};

use super::opcode::*;
use super::Sm4Program;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sm4DecodeError {
    pub at_dword: usize,
    pub kind: Sm4DecodeErrorKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sm4DecodeErrorKind {
    UnexpectedEof {
        wanted: usize,
        remaining: usize,
    },
    InvalidDeclaredLength {
        declared: usize,
        available: usize,
    },
    InstructionLengthZero,
    InstructionOutOfBounds {
        start: usize,
        len: usize,
        available: usize,
    },
    UnsupportedOperand(&'static str),
    UnsupportedOperandType {
        ty: u32,
    },
    UnsupportedIndexDimension {
        dim: u32,
    },
    UnsupportedIndexRepresentation {
        rep: u32,
    },
    UnsupportedExtendedOperand {
        ty: u32,
    },
    InvalidRegisterIndices {
        ty: u32,
        indices: Vec<u32>,
    },
}

impl fmt::Display for Sm4DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SM4/5 decode error at dword {}: ", self.at_dword)?;
        match &self.kind {
            Sm4DecodeErrorKind::UnexpectedEof { wanted, remaining } => write!(
                f,
                "unexpected end of token stream (wanted {wanted} dwords, {remaining} remaining)"
            ),
            Sm4DecodeErrorKind::InvalidDeclaredLength {
                declared,
                available,
            } => write!(
                f,
                "declared program length {declared} is out of bounds (available {available})"
            ),
            Sm4DecodeErrorKind::InstructionLengthZero => write!(f, "instruction length is zero"),
            Sm4DecodeErrorKind::InstructionOutOfBounds {
                start,
                len,
                available,
            } => write!(
                f,
                "instruction at {start} with length {len} overruns program (available {available})"
            ),
            Sm4DecodeErrorKind::UnsupportedOperand(msg) => write!(f, "unsupported operand: {msg}"),
            Sm4DecodeErrorKind::UnsupportedOperandType { ty } => {
                write!(f, "unsupported operand type {ty}")
            }
            Sm4DecodeErrorKind::UnsupportedIndexDimension { dim } => {
                write!(f, "unsupported operand index dimension {dim}")
            }
            Sm4DecodeErrorKind::UnsupportedIndexRepresentation { rep } => {
                write!(f, "unsupported operand index representation {rep}")
            }
            Sm4DecodeErrorKind::UnsupportedExtendedOperand { ty } => {
                write!(f, "unsupported extended operand token type {ty}")
            }
            Sm4DecodeErrorKind::InvalidRegisterIndices { ty, indices } => write!(
                f,
                "invalid register index encoding for operand type {ty} (indices={indices:?})"
            ),
        }
    }
}

impl std::error::Error for Sm4DecodeError {}

pub fn decode_program(program: &Sm4Program) -> Result<Sm4Module, Sm4DecodeError> {
    let declared_len = *program.tokens.get(1).unwrap_or(&0) as usize;
    if declared_len < 2 || declared_len > program.tokens.len() {
        return Err(Sm4DecodeError {
            at_dword: 1,
            kind: Sm4DecodeErrorKind::InvalidDeclaredLength {
                declared: declared_len,
                available: program.tokens.len(),
            },
        });
    }

    let toks = &program.tokens[..declared_len];

    let mut decls = Vec::new();
    let mut instructions = Vec::new();

    let mut i = 2usize;
    let mut in_decls = true;
    while i < toks.len() {
        let opcode_token = toks[i];
        let opcode = opcode_token & OPCODE_MASK;
        let len = ((opcode_token >> OPCODE_LEN_SHIFT) & OPCODE_LEN_MASK) as usize;
        if len == 0 {
            return Err(Sm4DecodeError {
                at_dword: i,
                kind: Sm4DecodeErrorKind::InstructionLengthZero,
            });
        }
        if i + len > toks.len() {
            return Err(Sm4DecodeError {
                at_dword: i,
                kind: Sm4DecodeErrorKind::InstructionOutOfBounds {
                    start: i,
                    len,
                    available: toks.len(),
                },
            });
        }

        let inst_toks = &toks[i..i + len];

        // All declarations are required to come before the instruction stream. Unknown
        // declarations are preserved as `Sm4Decl::Unknown` so later stages can still decide
        // whether they're important.
        if in_decls && !is_supported_instruction_opcode(opcode) {
            let decl = decode_decl(opcode, inst_toks, i).unwrap_or(Sm4Decl::Unknown { opcode });
            decls.push(decl);
            i += len;
            continue;
        }
        in_decls = false;

        instructions.push(decode_instruction(opcode, inst_toks, i)?);
        i += len;
    }

    Ok(Sm4Module {
        stage: program.stage,
        model: program.model,
        decls,
        instructions,
    })
}

fn is_supported_instruction_opcode(opcode: u32) -> bool {
    matches!(
        opcode,
        OPCODE_MOV
            | OPCODE_ADD
            | OPCODE_MUL
            | OPCODE_MAD
            | OPCODE_DP3
            | OPCODE_DP4
            | OPCODE_MIN
            | OPCODE_MAX
            | OPCODE_RCP
            | OPCODE_RSQ
            | OPCODE_RET
            | OPCODE_SAMPLE
            | OPCODE_SAMPLE_L
    )
}

fn decode_instruction(
    opcode: u32,
    inst_toks: &[u32],
    at: usize,
) -> Result<Sm4Inst, Sm4DecodeError> {
    let mut r = InstrReader::new(inst_toks, at);
    let opcode_token = r.read_u32()?;
    let saturate = decode_extended_opcode_modifiers(&mut r, opcode_token)?;

    match opcode {
        OPCODE_MOV => {
            let mut dst = decode_dst(&mut r)?;
            dst.saturate = saturate;
            let src = decode_src(&mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::Mov { dst, src })
        }
        OPCODE_ADD => {
            let mut dst = decode_dst(&mut r)?;
            dst.saturate = saturate;
            let a = decode_src(&mut r)?;
            let b = decode_src(&mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::Add { dst, a, b })
        }
        OPCODE_MUL => {
            let mut dst = decode_dst(&mut r)?;
            dst.saturate = saturate;
            let a = decode_src(&mut r)?;
            let b = decode_src(&mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::Mul { dst, a, b })
        }
        OPCODE_MAD => {
            let mut dst = decode_dst(&mut r)?;
            dst.saturate = saturate;
            let a = decode_src(&mut r)?;
            let b = decode_src(&mut r)?;
            let c = decode_src(&mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::Mad { dst, a, b, c })
        }
        OPCODE_DP3 => {
            let mut dst = decode_dst(&mut r)?;
            dst.saturate = saturate;
            let a = decode_src(&mut r)?;
            let b = decode_src(&mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::Dp3 { dst, a, b })
        }
        OPCODE_DP4 => {
            let mut dst = decode_dst(&mut r)?;
            dst.saturate = saturate;
            let a = decode_src(&mut r)?;
            let b = decode_src(&mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::Dp4 { dst, a, b })
        }
        OPCODE_MIN => {
            let mut dst = decode_dst(&mut r)?;
            dst.saturate = saturate;
            let a = decode_src(&mut r)?;
            let b = decode_src(&mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::Min { dst, a, b })
        }
        OPCODE_MAX => {
            let mut dst = decode_dst(&mut r)?;
            dst.saturate = saturate;
            let a = decode_src(&mut r)?;
            let b = decode_src(&mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::Max { dst, a, b })
        }
        OPCODE_RCP => {
            let mut dst = decode_dst(&mut r)?;
            dst.saturate = saturate;
            let src = decode_src(&mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::Rcp { dst, src })
        }
        OPCODE_RSQ => {
            let mut dst = decode_dst(&mut r)?;
            dst.saturate = saturate;
            let src = decode_src(&mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::Rsq { dst, src })
        }
        OPCODE_RET => {
            r.expect_eof()?;
            Ok(Sm4Inst::Ret)
        }
        OPCODE_SAMPLE | OPCODE_SAMPLE_L => decode_sample_like(opcode, saturate, &mut r),
        other => {
            // Structural fallback for sample/sample_l when opcode IDs differ.
            if let Some(sample) = try_decode_sample_like(saturate, inst_toks, at)? {
                return Ok(sample);
            }
            Ok(Sm4Inst::Unknown { opcode: other })
        }
    }
}

fn decode_decl(opcode: u32, inst_toks: &[u32], at: usize) -> Result<Sm4Decl, Sm4DecodeError> {
    let mut r = InstrReader::new(inst_toks, at);
    let opcode_token = r.read_u32()?;
    // Declarations can also have extended opcode tokens; consume them even if we don't
    // understand the contents.
    let _ = decode_extended_opcode_modifiers(&mut r, opcode_token)?;

    if r.is_eof() {
        return Ok(Sm4Decl::Unknown { opcode });
    }

    let op = decode_raw_operand(&mut r)?;
    if op.imm32.is_some() {
        return Ok(Sm4Decl::Unknown { opcode });
    }

    let mask = match op.selection_mode {
        OPERAND_SEL_MASK => WriteMask((op.component_sel & 0xF) as u8),
        _ => WriteMask::XYZW,
    };

    match op.ty {
        OPERAND_TYPE_INPUT => {
            let reg = one_index(op.ty, &op.indices, r.base_at)?;
            if r.is_eof() {
                return Ok(Sm4Decl::Input { reg, mask });
            }
            if r.toks.len().saturating_sub(r.pos) == 1 {
                let sys_value = r.read_u32()?;
                r.expect_eof()?;
                return Ok(Sm4Decl::InputSiv {
                    reg,
                    mask,
                    sys_value,
                });
            }
        }
        OPERAND_TYPE_OUTPUT => {
            let reg = one_index(op.ty, &op.indices, r.base_at)?;
            if r.is_eof() {
                return Ok(Sm4Decl::Output { reg, mask });
            }
            if r.toks.len().saturating_sub(r.pos) == 1 {
                let sys_value = r.read_u32()?;
                r.expect_eof()?;
                return Ok(Sm4Decl::OutputSiv {
                    reg,
                    mask,
                    sys_value,
                });
            }
        }
        OPERAND_TYPE_CONSTANT_BUFFER => {
            if let [slot, reg_count] = op.indices.as_slice() {
                return Ok(Sm4Decl::ConstantBuffer {
                    slot: *slot,
                    reg_count: *reg_count,
                });
            }
        }
        OPERAND_TYPE_SAMPLER => {
            let slot = one_index(op.ty, &op.indices, r.base_at)?;
            return Ok(Sm4Decl::Sampler { slot });
        }
        OPERAND_TYPE_RESOURCE => {
            let slot = one_index(op.ty, &op.indices, r.base_at)?;
            return Ok(Sm4Decl::ResourceTexture2D { slot });
        }
        _ => {}
    }

    Ok(Sm4Decl::Unknown { opcode })
}

fn decode_sample_like(
    opcode: u32,
    saturate: bool,
    r: &mut InstrReader<'_>,
) -> Result<Sm4Inst, Sm4DecodeError> {
    match opcode {
        OPCODE_SAMPLE => {
            let mut dst = decode_dst(r)?;
            dst.saturate = saturate;
            let coord = decode_src(r)?;
            let texture = decode_texture_ref(r)?;
            let sampler = decode_sampler_ref(r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::Sample {
                dst,
                coord,
                texture,
                sampler,
            })
        }
        OPCODE_SAMPLE_L => {
            let mut dst = decode_dst(r)?;
            dst.saturate = saturate;
            let coord = decode_src(r)?;
            let texture = decode_texture_ref(r)?;
            let sampler = decode_sampler_ref(r)?;
            let lod = decode_src(r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::SampleL {
                dst,
                coord,
                texture,
                sampler,
                lod,
            })
        }
        _ => unreachable!("decode_sample_like called with non-sample opcode"),
    }
}

fn try_decode_sample_like(
    saturate: bool,
    inst_toks: &[u32],
    at: usize,
) -> Result<Option<Sm4Inst>, Sm4DecodeError> {
    let mut r = InstrReader::new(inst_toks, at);
    let opcode_token = r.read_u32()?;
    let _ = decode_extended_opcode_modifiers(&mut r, opcode_token)?;

    let mut dst = match decode_dst(&mut r) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    dst.saturate = saturate;
    let coord = match decode_src(&mut r) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    let texture = match decode_texture_ref(&mut r) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    let sampler = match decode_sampler_ref(&mut r) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };

    if r.is_eof() {
        return Ok(Some(Sm4Inst::Sample {
            dst,
            coord,
            texture,
            sampler,
        }));
    }

    let lod = match decode_src(&mut r) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    if r.is_eof() {
        return Ok(Some(Sm4Inst::SampleL {
            dst,
            coord,
            texture,
            sampler,
            lod,
        }));
    }

    Ok(None)
}

// ---- Operand decoding ----

#[derive(Debug, Clone)]
struct RawOperand {
    ty: u32,
    selection_mode: u32,
    component_sel: u32,
    modifier: OperandModifier,
    indices: Vec<u32>,
    imm32: Option<[u32; 4]>,
}

fn decode_dst(r: &mut InstrReader<'_>) -> Result<DstOperand, Sm4DecodeError> {
    let op = decode_raw_operand(r)?;
    if op.imm32.is_some() {
        return Err(Sm4DecodeError {
            at_dword: r.base_at + r.pos.saturating_sub(1),
            kind: Sm4DecodeErrorKind::UnsupportedOperand("destination cannot be immediate"),
        });
    }

    let (file, index) = match op.ty {
        OPERAND_TYPE_TEMP => (RegFile::Temp, one_index(op.ty, &op.indices, r.base_at)?),
        OPERAND_TYPE_OUTPUT => (RegFile::Output, one_index(op.ty, &op.indices, r.base_at)?),
        other => {
            return Err(Sm4DecodeError {
                at_dword: r.base_at + r.pos.saturating_sub(1),
                kind: Sm4DecodeErrorKind::UnsupportedOperandType { ty: other },
            })
        }
    };

    let mask = match op.selection_mode {
        OPERAND_SEL_MASK => WriteMask((op.component_sel & 0xF) as u8),
        _ => WriteMask::XYZW,
    };

    Ok(DstOperand {
        reg: RegisterRef { file, index },
        mask,
        saturate: false,
    })
}

fn decode_src(r: &mut InstrReader<'_>) -> Result<SrcOperand, Sm4DecodeError> {
    let op = decode_raw_operand(r)?;

    let swizzle = match op.selection_mode {
        OPERAND_SEL_SWIZZLE => decode_swizzle(op.component_sel),
        OPERAND_SEL_SELECT1 => {
            let c = (op.component_sel & 0x3) as u8;
            Swizzle([c, c, c, c])
        }
        OPERAND_SEL_MASK => Swizzle::XYZW,
        _ => {
            return Err(Sm4DecodeError {
                at_dword: r.base_at + r.pos.saturating_sub(1),
                kind: Sm4DecodeErrorKind::UnsupportedOperand("unknown component selection mode"),
            })
        }
    };

    let kind = if let Some(imm) = op.imm32 {
        SrcKind::ImmediateF32(imm)
    } else {
        match op.ty {
            OPERAND_TYPE_TEMP => SrcKind::Register(RegisterRef {
                file: RegFile::Temp,
                index: one_index(op.ty, &op.indices, r.base_at)?,
            }),
            OPERAND_TYPE_INPUT => SrcKind::Register(RegisterRef {
                file: RegFile::Input,
                index: one_index(op.ty, &op.indices, r.base_at)?,
            }),
            OPERAND_TYPE_OUTPUT => SrcKind::Register(RegisterRef {
                file: RegFile::Output,
                index: one_index(op.ty, &op.indices, r.base_at)?,
            }),
            OPERAND_TYPE_CONSTANT_BUFFER => match op.indices.as_slice() {
                [slot, reg] => SrcKind::ConstantBuffer {
                    slot: *slot,
                    reg: *reg,
                },
                _ => {
                    return Err(Sm4DecodeError {
                        at_dword: r.base_at + r.pos.saturating_sub(1),
                        kind: Sm4DecodeErrorKind::InvalidRegisterIndices {
                            ty: op.ty,
                            indices: op.indices,
                        },
                    })
                }
            },
            other => {
                return Err(Sm4DecodeError {
                    at_dword: r.base_at + r.pos.saturating_sub(1),
                    kind: Sm4DecodeErrorKind::UnsupportedOperandType { ty: other },
                })
            }
        }
    };

    Ok(SrcOperand {
        kind,
        swizzle,
        modifier: op.modifier,
    })
}

fn decode_texture_ref(r: &mut InstrReader<'_>) -> Result<TextureRef, Sm4DecodeError> {
    let op = decode_raw_operand(r)?;
    if op.imm32.is_some() {
        return Err(Sm4DecodeError {
            at_dword: r.base_at + r.pos.saturating_sub(1),
            kind: Sm4DecodeErrorKind::UnsupportedOperand("texture operand cannot be immediate"),
        });
    }
    if op.ty != OPERAND_TYPE_RESOURCE {
        return Err(Sm4DecodeError {
            at_dword: r.base_at + r.pos.saturating_sub(1),
            kind: Sm4DecodeErrorKind::UnsupportedOperand("expected resource operand"),
        });
    }
    let slot = one_index(op.ty, &op.indices, r.base_at)?;
    Ok(TextureRef { slot })
}

fn decode_sampler_ref(r: &mut InstrReader<'_>) -> Result<SamplerRef, Sm4DecodeError> {
    let op = decode_raw_operand(r)?;
    if op.imm32.is_some() {
        return Err(Sm4DecodeError {
            at_dword: r.base_at + r.pos.saturating_sub(1),
            kind: Sm4DecodeErrorKind::UnsupportedOperand("sampler operand cannot be immediate"),
        });
    }
    if op.ty != OPERAND_TYPE_SAMPLER {
        return Err(Sm4DecodeError {
            at_dword: r.base_at + r.pos.saturating_sub(1),
            kind: Sm4DecodeErrorKind::UnsupportedOperand("expected sampler operand"),
        });
    }
    let slot = one_index(op.ty, &op.indices, r.base_at)?;
    Ok(SamplerRef { slot })
}

fn one_index(ty: u32, indices: &[u32], at: usize) -> Result<u32, Sm4DecodeError> {
    match indices {
        [idx] => Ok(*idx),
        _ => Err(Sm4DecodeError {
            at_dword: at,
            kind: Sm4DecodeErrorKind::InvalidRegisterIndices {
                ty,
                indices: indices.to_vec(),
            },
        }),
    }
}

fn decode_swizzle(sel: u32) -> Swizzle {
    let x = (sel & 0x3) as u8;
    let y = ((sel >> 2) & 0x3) as u8;
    let z = ((sel >> 4) & 0x3) as u8;
    let w = ((sel >> 6) & 0x3) as u8;
    Swizzle([x, y, z, w])
}

fn decode_raw_operand(r: &mut InstrReader<'_>) -> Result<RawOperand, Sm4DecodeError> {
    let token = r.read_u32()?;

    let num_components = token & OPERAND_NUM_COMPONENTS_MASK;
    let selection_mode = (token >> OPERAND_SELECTION_MODE_SHIFT) & OPERAND_SELECTION_MODE_MASK;
    let ty = (token >> OPERAND_TYPE_SHIFT) & OPERAND_TYPE_MASK;
    let component_sel =
        (token >> OPERAND_COMPONENT_SELECTION_SHIFT) & OPERAND_COMPONENT_SELECTION_MASK;
    let index_dim = (token >> OPERAND_INDEX_DIMENSION_SHIFT) & OPERAND_INDEX_DIMENSION_MASK;
    let idx_reps = [
        (token >> OPERAND_INDEX0_REP_SHIFT) & OPERAND_INDEX_REP_MASK,
        (token >> OPERAND_INDEX1_REP_SHIFT) & OPERAND_INDEX_REP_MASK,
        (token >> OPERAND_INDEX2_REP_SHIFT) & OPERAND_INDEX_REP_MASK,
    ];

    let mut modifier = OperandModifier::None;

    let mut extended = (token & OPERAND_EXTENDED_BIT) != 0;
    while extended {
        let ext = r.read_u32()?;
        extended = (ext & OPERAND_EXTENDED_BIT) != 0;
        let ext_ty = ext & 0x3f;
        if ext_ty != 0 {
            return Err(Sm4DecodeError {
                at_dword: r.base_at + r.pos.saturating_sub(1),
                kind: Sm4DecodeErrorKind::UnsupportedExtendedOperand { ty: ext_ty },
            });
        }
        let m = (ext >> 6) & 0x3;
        modifier = match m {
            0 => OperandModifier::None,
            1 => OperandModifier::Neg,
            2 => OperandModifier::Abs,
            3 => OperandModifier::AbsNeg,
            _ => OperandModifier::None,
        };
    }

    let dim = match index_dim {
        OPERAND_INDEX_DIMENSION_0D => 0usize,
        OPERAND_INDEX_DIMENSION_1D => 1usize,
        OPERAND_INDEX_DIMENSION_2D => 2usize,
        other => {
            return Err(Sm4DecodeError {
                at_dword: r.base_at + r.pos.saturating_sub(1),
                kind: Sm4DecodeErrorKind::UnsupportedIndexDimension { dim: other },
            })
        }
    };

    let mut indices = Vec::with_capacity(dim);
    for rep in idx_reps.iter().take(dim) {
        if *rep != OPERAND_INDEX_REP_IMMEDIATE32 {
            return Err(Sm4DecodeError {
                at_dword: r.base_at + r.pos.saturating_sub(1),
                kind: Sm4DecodeErrorKind::UnsupportedIndexRepresentation { rep: *rep },
            });
        }
        indices.push(r.read_u32()?);
    }

    let imm32 = if ty == OPERAND_TYPE_IMMEDIATE32 {
        match num_components {
            1 => {
                let v = r.read_u32()?;
                Some([v, v, v, v])
            }
            2 => Some([r.read_u32()?, r.read_u32()?, r.read_u32()?, r.read_u32()?]),
            _other => {
                return Err(Sm4DecodeError {
                    at_dword: r.base_at + r.pos.saturating_sub(1),
                    kind: Sm4DecodeErrorKind::UnsupportedOperand(
                        "immediate32 with unsupported component count",
                    ),
                })
            }
        }
    } else {
        None
    };

    Ok(RawOperand {
        ty,
        selection_mode,
        component_sel,
        modifier,
        indices,
        imm32,
    })
}

// ---- Extended opcode tokens ----

fn decode_extended_opcode_modifiers(
    r: &mut InstrReader<'_>,
    opcode_token: u32,
) -> Result<bool, Sm4DecodeError> {
    let mut saturate = false;

    let mut extended = (opcode_token & OPCODE_EXTENDED_BIT) != 0;
    while extended {
        let ext = r.read_u32()?;
        extended = (ext & OPCODE_EXTENDED_BIT) != 0;
        let ext_ty = ext & 0x3f;
        if ext_ty == 0 {
            saturate |= (ext & (1 << 13)) != 0;
        }
    }

    Ok(saturate)
}

// ---- Token reader ----

struct InstrReader<'a> {
    toks: &'a [u32],
    pos: usize,
    base_at: usize,
}

impl<'a> InstrReader<'a> {
    fn new(toks: &'a [u32], base_at: usize) -> Self {
        Self {
            toks,
            pos: 0,
            base_at,
        }
    }

    fn read_u32(&mut self) -> Result<u32, Sm4DecodeError> {
        self.toks
            .get(self.pos)
            .copied()
            .ok_or_else(|| Sm4DecodeError {
                at_dword: self.base_at + self.pos,
                kind: Sm4DecodeErrorKind::UnexpectedEof {
                    wanted: 1,
                    remaining: 0,
                },
            })
            .map(|v| {
                self.pos += 1;
                v
            })
    }

    fn is_eof(&self) -> bool {
        self.pos >= self.toks.len()
    }

    fn expect_eof(&self) -> Result<(), Sm4DecodeError> {
        if self.is_eof() {
            Ok(())
        } else {
            Err(Sm4DecodeError {
                at_dword: self.base_at + self.pos,
                kind: Sm4DecodeErrorKind::UnsupportedOperand(
                    "trailing tokens after instruction/declaration",
                ),
            })
        }
    }
}
