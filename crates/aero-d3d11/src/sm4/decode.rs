use core::fmt;

use crate::sm4_ir::{
    BufferKind, BufferRef, DstOperand, OperandModifier, RegFile, RegisterRef, SamplerRef, Sm4Decl,
    Sm4Inst, Sm4Module, SrcKind, SrcOperand, Swizzle, TextureRef, UavRef, WriteMask,
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

const DECLARATION_OPCODE_MIN: u32 = 0x100;
const CUSTOMDATA_CLASS_COMMENT: u32 = 0;

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

        // `customdata` blocks are non-executable and can legally appear both in the declaration
        // region and interspersed within the executable instruction stream (comments, debug data,
        // immediate constant buffers, etc.). Treat them as declarations/metadata regardless of
        // their class so they never poison the instruction stream.
        if opcode == OPCODE_CUSTOMDATA {
            // Custom-data blocks can (in theory) also have extended opcode tokens. Skip over them
            // to find the class token so the metadata we record is accurate.
            let mut class_pos = 1usize;
            let mut extended = (opcode_token & OPCODE_EXTENDED_BIT) != 0;
            while extended {
                let Some(ext) = inst_toks.get(class_pos).copied() else {
                    break;
                };
                class_pos += 1;
                extended = (ext & OPCODE_EXTENDED_BIT) != 0;
            }
            let class = inst_toks
                .get(class_pos)
                .copied()
                .unwrap_or(CUSTOMDATA_CLASS_COMMENT);
            decls.push(Sm4Decl::CustomData {
                class,
                len_dwords: len as u32,
            });
            i += len;
            continue;
        }

        // `nop` can appear in both the declaration section and the executable instruction stream.
        // It has no effect and should not influence where we split declarations from
        // instructions.
        if opcode == OPCODE_NOP {
            i += len;
            continue;
        }

        // All declarations are required to come before the instruction stream. Unknown
        // declarations are preserved as `Sm4Decl::Unknown` so later stages can still decide
        // whether they're important.
        if in_decls && opcode >= DECLARATION_OPCODE_MIN {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sm4::{ShaderModel, ShaderStage};

    fn opcode_token(opcode: u32, len_dwords: u32) -> u32 {
        opcode | (len_dwords << OPCODE_LEN_SHIFT)
    }

    #[test]
    fn customdata_non_comment_is_skipped_from_instructions() {
        // Build a minimal ps_5_0 token stream:
        // - a declaration
        // - a non-comment customdata block (class=1)
        // - mov r0, l(0,0,0,0)
        // - ret
        let version_token = 0x50u32; // ps_5_0

        // dcl (any opcode >= DECLARATION_OPCODE_MIN is treated as a declaration by the decoder)
        let decl_opcode = DECLARATION_OPCODE_MIN;
        let decl_len = 3u32;
        let decl_operand_token = 0x10F012u32; // v0.xyzw
        let decl_register = 0u32;

        // customdata block: opcode + class + 2 payload dwords
        let customdata_len = 4u32;
        let customdata_class = 1u32;

        // mov r0.xyzw, l(0,0,0,0)
        let mov_len = 8u32;
        let mov_dst_token = 0x10F002u32; // r0.xyzw
        let mov_dst_index = 0u32;
        let mov_src_imm_token = 0x42u32; // immediate32 vec4
        let imm = 0u32; // 0.0f bits

        // ret
        let ret_len = 1u32;

        let mut tokens = vec![
            version_token,
            0, // declared length patched below
            opcode_token(decl_opcode, decl_len),
            decl_operand_token,
            decl_register,
            opcode_token(OPCODE_CUSTOMDATA, customdata_len),
            customdata_class,
            0x1234_5678,
            0x9abc_def0,
            opcode_token(OPCODE_MOV, mov_len),
            mov_dst_token,
            mov_dst_index,
            mov_src_imm_token,
            imm,
            imm,
            imm,
            imm,
            opcode_token(OPCODE_RET, ret_len),
        ];
        tokens[1] = tokens.len() as u32;

        let program = Sm4Program {
            stage: ShaderStage::Pixel,
            model: ShaderModel { major: 5, minor: 0 },
            tokens,
        };

        let module = super::decode_program(&program).expect("decode should succeed");
        assert_eq!(module.instructions.len(), 2);
        assert!(matches!(module.instructions[0], Sm4Inst::Mov { .. }));
        assert!(matches!(module.instructions[1], Sm4Inst::Ret));
        assert!(
            !module
                .instructions
                .iter()
                .any(|i| matches!(i, Sm4Inst::Unknown { opcode: OPCODE_CUSTOMDATA })),
            "customdata must not be decoded as an executable instruction"
        );

        assert!(
            module
                .decls
                .iter()
                .any(|d| matches!(d, Sm4Decl::CustomData { class: 1, .. })),
            "customdata block should be preserved as a declaration for diagnostics"
        );
    }

    #[test]
    fn customdata_inside_instruction_stream_is_skipped() {
        // Ensure `customdata` blocks that appear after the decoder has entered the executable
        // instruction stream are still treated as non-executable and do not yield `Unknown`
        // instructions.
        let version_token = 0x50u32; // ps_5_0

        // dcl (any opcode >= DECLARATION_OPCODE_MIN is treated as a declaration by the decoder)
        let decl_opcode = DECLARATION_OPCODE_MIN;
        let decl_len = 3u32;
        let decl_operand_token = 0x10F012u32; // v0.xyzw
        let decl_register = 0u32;

        // mov r0.xyzw, l(0,0,0,0)
        let mov_len = 8u32;
        let mov_dst_token = 0x10F002u32; // r0.xyzw
        let mov_dst_index = 0u32;
        let mov_src_imm_token = 0x42u32; // immediate32 vec4
        let imm = 0u32; // 0.0f bits

        // customdata block: opcode + class + 2 payload dwords
        let customdata_len = 4u32;
        let customdata_class = 1u32;

        // ret
        let ret_len = 1u32;

        let mut tokens = vec![
            version_token,
            0, // declared length patched below
            opcode_token(decl_opcode, decl_len),
            decl_operand_token,
            decl_register,
            opcode_token(OPCODE_MOV, mov_len),
            mov_dst_token,
            mov_dst_index,
            mov_src_imm_token,
            imm,
            imm,
            imm,
            imm,
            opcode_token(OPCODE_CUSTOMDATA, customdata_len),
            customdata_class,
            0x1111_1111,
            0x2222_2222,
            opcode_token(OPCODE_RET, ret_len),
        ];
        tokens[1] = tokens.len() as u32;

        let program = Sm4Program {
            stage: ShaderStage::Pixel,
            model: ShaderModel { major: 5, minor: 0 },
            tokens,
        };

        let module = decode_program(&program).expect("decode should succeed");
        assert_eq!(module.instructions.len(), 2);
        assert!(matches!(module.instructions[0], Sm4Inst::Mov { .. }));
        assert!(matches!(module.instructions[1], Sm4Inst::Ret));
        assert!(
            !module
                .instructions
                .iter()
                .any(|i| matches!(i, Sm4Inst::Unknown { opcode: OPCODE_CUSTOMDATA })),
            "customdata must not be decoded as an executable instruction"
        );

        assert!(
            module
                .decls
                .iter()
                .any(|d| matches!(d, Sm4Decl::CustomData { class: 1, .. })),
            "customdata block should be preserved as a declaration for diagnostics"
        );
    }

    #[test]
    fn customdata_out_of_bounds_length_errors() {
        // Customdata blocks still participate in the normal instruction-length bounds checks.
        // If the block overruns the declared token stream, decoding should fail.
        let version_token = 0x50u32; // ps_5_0

        let bogus_len = 16u32;
        let tokens = vec![
            version_token,
            4, // declared length
            opcode_token(OPCODE_CUSTOMDATA, bogus_len),
            1, // class
        ];

        let program = Sm4Program {
            stage: ShaderStage::Pixel,
            model: ShaderModel { major: 5, minor: 0 },
            tokens,
        };

        let err = decode_program(&program).expect_err("decode should fail");
        assert!(
            matches!(err.kind, Sm4DecodeErrorKind::InstructionOutOfBounds { start: 2, .. }),
            "expected InstructionOutOfBounds at dword 2, got {err:?}"
        );
    }

    #[test]
    fn customdata_zero_length_errors() {
        // Zero-length instructions are invalid and should always error, even for customdata.
        let version_token = 0x50u32; // ps_5_0

        let tokens = vec![
            version_token,
            3, // declared length
            opcode_token(OPCODE_CUSTOMDATA, 0),
        ];

        let program = Sm4Program {
            stage: ShaderStage::Pixel,
            model: ShaderModel { major: 5, minor: 0 },
            tokens,
        };

        let err = decode_program(&program).expect_err("decode should fail");
        assert!(
            matches!(err.kind, Sm4DecodeErrorKind::InstructionLengthZero),
            "expected InstructionLengthZero, got {err:?}"
        );
        assert_eq!(err.at_dword, 2);
    }

    #[test]
    fn customdata_records_class_after_extended_opcode_tokens() {
        // Extended opcode tokens (rare for `customdata`, but allowed by the token format) should
        // not be mistaken for the customdata class token.
        let version_token = 0x50u32; // ps_5_0

        // customdata block: opcode (extended) + ext token + class token
        let customdata_len = 3u32;
        let customdata_class = 1u32;

        let customdata_opcode =
            opcode_token(OPCODE_CUSTOMDATA, customdata_len) | OPCODE_EXTENDED_BIT;
        let ext_token = 0u32; // terminates extended opcode token chain

        let ret_len = 1u32;

        let mut tokens = vec![
            version_token,
            0, // declared length patched below
            customdata_opcode,
            ext_token,
            customdata_class,
            opcode_token(OPCODE_RET, ret_len),
        ];
        tokens[1] = tokens.len() as u32;

        let program = Sm4Program {
            stage: ShaderStage::Pixel,
            model: ShaderModel { major: 5, minor: 0 },
            tokens,
        };

        let module = decode_program(&program).expect("decode should succeed");
        assert!(matches!(module.instructions.as_slice(), [Sm4Inst::Ret]));
        assert!(
            module.decls.iter().any(|d| matches!(
                d,
                Sm4Decl::CustomData {
                    class: 1,
                    len_dwords: 3
                }
            )),
            "expected customdata class to be recorded after extended tokens, got decls={:?}",
            module.decls
        );
    }
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
        OPCODE_MOVC => {
            let mut dst = decode_dst(&mut r)?;
            dst.saturate = saturate;
            let cond = decode_src(&mut r)?;
            let a = decode_src(&mut r)?;
            let b = decode_src(&mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::Movc { dst, cond, a, b })
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
        OPCODE_LD => decode_ld(saturate, &mut r),
        OPCODE_LD_RAW => decode_ld_raw(saturate, &mut r),
        OPCODE_STORE_RAW => decode_store_raw(&mut r),
        OPCODE_LD_STRUCTURED => decode_ld_structured(saturate, &mut r),
        OPCODE_STORE_STRUCTURED => decode_store_structured(&mut r),
        other => {
            // Structural fallback for sample/sample_l when opcode IDs differ.
            if let Some(sample) = try_decode_sample_like(saturate, inst_toks, at)? {
                return Ok(sample);
            }
            // Structural fallback for texture load (ld) when opcode IDs differ.
            if let Some(ld) = try_decode_ld_like(saturate, inst_toks, at)? {
                return Ok(ld);
            }
            Ok(Sm4Inst::Unknown { opcode: other })
        }
    }
}

fn decode_ld(saturate: bool, r: &mut InstrReader<'_>) -> Result<Sm4Inst, Sm4DecodeError> {
    let mut dst = decode_dst(r)?;
    dst.saturate = saturate;
    let coord = decode_src(r)?;
    let texture = decode_texture_ref(r)?;

    // Some `ld` forms may include an explicit LOD operand, but for the common
    // `Texture2D.Load(int3(x,y,mip))` encoding it is part of `coord.z`. Model that
    // by defaulting the LOD operand to the third component of the coordinate.
    let default_lod_sel = coord.swizzle.0[2];
    let mut lod = coord.clone();
    lod.swizzle = Swizzle([default_lod_sel; 4]);

    if r.is_eof() {
        return Ok(Sm4Inst::Ld {
            dst,
            coord,
            texture,
            lod,
        });
    }

    // Optional explicit LOD operand.
    let explicit_lod = decode_src(r)?;
    if r.is_eof() {
        // Treat vector swizzles as offset-like operands (e.g. `Texture2D.Load(..., offset)`)
        // rather than an explicit LOD. Explicit LOD operands are scalar in practice (replicated
        // swizzle), while offsets require multiple components.
        let swz = explicit_lod.swizzle.0;
        let is_scalar = swz[0] == swz[1] && swz[0] == swz[2] && swz[0] == swz[3];
        if !is_scalar {
            return Ok(Sm4Inst::Unknown { opcode: OPCODE_LD });
        }
        return Ok(Sm4Inst::Ld {
            dst,
            coord,
            texture,
            lod: explicit_lod,
        });
    }

    // Unsupported `ld` variant (e.g. with offsets); preserve as unknown.
    Ok(Sm4Inst::Unknown { opcode: OPCODE_LD })
}

fn decode_ld_raw(saturate: bool, r: &mut InstrReader<'_>) -> Result<Sm4Inst, Sm4DecodeError> {
    let mut dst = decode_dst(r)?;
    dst.saturate = saturate;
    let addr = decode_src(r)?;
    let buffer = decode_buffer_ref(r)?;
    if !r.is_eof() {
        return Ok(Sm4Inst::Unknown {
            opcode: OPCODE_LD_RAW,
        });
    }
    Ok(Sm4Inst::LdRaw { dst, addr, buffer })
}

fn decode_store_raw(r: &mut InstrReader<'_>) -> Result<Sm4Inst, Sm4DecodeError> {
    let (uav, mask) = decode_uav_ref(r)?;
    let addr = decode_src(r)?;
    let value = decode_src(r)?;
    if !r.is_eof() {
        return Ok(Sm4Inst::Unknown {
            opcode: OPCODE_STORE_RAW,
        });
    }
    Ok(Sm4Inst::StoreRaw {
        uav,
        addr,
        value,
        mask,
    })
}

fn decode_ld_structured(saturate: bool, r: &mut InstrReader<'_>) -> Result<Sm4Inst, Sm4DecodeError> {
    let mut dst = decode_dst(r)?;
    dst.saturate = saturate;
    let index = decode_src(r)?;
    let offset = decode_src(r)?;
    let buffer = decode_buffer_ref(r)?;
    if !r.is_eof() {
        return Ok(Sm4Inst::Unknown {
            opcode: OPCODE_LD_STRUCTURED,
        });
    }
    Ok(Sm4Inst::LdStructured {
        dst,
        index,
        offset,
        buffer,
    })
}

fn decode_store_structured(r: &mut InstrReader<'_>) -> Result<Sm4Inst, Sm4DecodeError> {
    let (uav, mask) = decode_uav_ref(r)?;
    let index = decode_src(r)?;
    let offset = decode_src(r)?;
    let value = decode_src(r)?;
    if !r.is_eof() {
        return Ok(Sm4Inst::Unknown {
            opcode: OPCODE_STORE_STRUCTURED,
        });
    }
    Ok(Sm4Inst::StoreStructured {
        uav,
        index,
        offset,
        value,
        mask,
    })
}
fn decode_decl(opcode: u32, inst_toks: &[u32], at: usize) -> Result<Sm4Decl, Sm4DecodeError> {
    let mut r = InstrReader::new(inst_toks, at);
    let opcode_token = r.read_u32()?;
    // Declarations can also have extended opcode tokens; consume them even if we don't
    // understand the contents.
    let _ = decode_extended_opcode_modifiers(&mut r, opcode_token)?;

    if opcode == OPCODE_DCL_THREAD_GROUP {
        let x = r.read_u32()?;
        let y = r.read_u32()?;
        let z = r.read_u32()?;
        r.expect_eof()?;
        return Ok(Sm4Decl::ThreadGroupSize { x, y, z });
    }

    if r.is_eof() {
        return Ok(Sm4Decl::Unknown { opcode });
    }

    // `dcl_thread_group x, y, z` is a compute-stage declaration that encodes the thread-group
    // dimensions as three immediate DWORDs (no operand token). Most other declarations begin with an
    // operand token, which has index-dimension bits set at bit 20 and therefore tends to be a large
    // value (>= 1<<20). Use this to distinguish thread-group-size declarations from
    // resource/sampler/etc declarations that can also be 4 DWORDs long.
    //
    // Note: This is intentionally heuristic (we don't currently key off the declaration opcode),
    // but it is sufficient for real-world DXBC where the dimensions are small constants.
    if r.toks.len().saturating_sub(r.pos) == 3 {
        let x = r.toks[r.pos];
        let y = r.toks[r.pos + 1];
        let z = r.toks[r.pos + 2];
        if x < (1 << 20) && y < (1 << 20) && z < (1 << 20) {
            let x = r.read_u32()?;
            let y = r.read_u32()?;
            let z = r.read_u32()?;
            r.expect_eof()?;
            return Ok(Sm4Decl::ThreadGroupSize { x, y, z });
        }
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
            match opcode {
                OPCODE_DCL_RESOURCE_RAW => {
                    return Ok(Sm4Decl::ResourceBuffer {
                        slot,
                        stride: 0,
                        kind: BufferKind::Raw,
                    });
                }
                OPCODE_DCL_RESOURCE_STRUCTURED => {
                    let stride = if r.is_eof() {
                        return Ok(Sm4Decl::Unknown { opcode });
                    } else {
                        r.read_u32()?
                    };
                    return Ok(Sm4Decl::ResourceBuffer {
                        slot,
                        stride,
                        kind: BufferKind::Structured,
                    });
                }
                _ => {
                    // Typed resources encode their dimensionality in an extra token. We only model
                    // `Texture2D` today; other dimensions are preserved as `Unknown` so later stages
                    // can decide whether they matter.
                    let dim = if r.is_eof() { None } else { Some(r.read_u32()?) };
                    if dim == Some(2) {
                        return Ok(Sm4Decl::ResourceTexture2D { slot });
                    }
                }
            }
        }
        OPERAND_TYPE_UNORDERED_ACCESS_VIEW => {
            let slot = one_index(op.ty, &op.indices, r.base_at)?;
            match opcode {
                OPCODE_DCL_UAV_RAW => {
                    return Ok(Sm4Decl::UavBuffer {
                        slot,
                        stride: 0,
                        kind: BufferKind::Raw,
                    });
                }
                OPCODE_DCL_UAV_STRUCTURED => {
                    let stride = if r.is_eof() {
                        return Ok(Sm4Decl::Unknown { opcode });
                    } else {
                        r.read_u32()?
                    };
                    return Ok(Sm4Decl::UavBuffer {
                        slot,
                        stride,
                        kind: BufferKind::Structured,
                    });
                }
                _ => {}
            }
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

fn try_decode_ld_like(
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
    // `ld` expects at least a 2D coordinate. Avoid misclassifying other
    // instructions with a `dst, scalar, resource` operand pattern (e.g. resinfo)
    // as texture loads.
    if coord.swizzle.0.iter().all(|&c| c == coord.swizzle.0[0]) {
        return Ok(None);
    }
    let texture = match decode_texture_ref(&mut r) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };

    if r.is_eof() {
        let default_lod_sel = coord.swizzle.0[2];
        let mut lod = coord.clone();
        lod.swizzle = Swizzle([default_lod_sel; 4]);
        return Ok(Some(Sm4Inst::Ld {
            dst,
            coord,
            texture,
            lod,
        }));
    }

    // Optional explicit LOD operand.
    let explicit_lod = match decode_src(&mut r) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    if r.is_eof() {
        return Ok(Some(Sm4Inst::Ld {
            dst,
            coord,
            texture,
            lod: explicit_lod,
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

fn decode_buffer_ref(r: &mut InstrReader<'_>) -> Result<BufferRef, Sm4DecodeError> {
    let op = decode_raw_operand(r)?;
    if op.imm32.is_some() {
        return Err(Sm4DecodeError {
            at_dword: r.base_at + r.pos.saturating_sub(1),
            kind: Sm4DecodeErrorKind::UnsupportedOperand("buffer operand cannot be immediate"),
        });
    }
    if op.ty != OPERAND_TYPE_RESOURCE {
        return Err(Sm4DecodeError {
            at_dword: r.base_at + r.pos.saturating_sub(1),
            kind: Sm4DecodeErrorKind::UnsupportedOperand("expected resource operand"),
        });
    }
    let slot = one_index(op.ty, &op.indices, r.base_at)?;
    Ok(BufferRef { slot })
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

fn decode_uav_ref(r: &mut InstrReader<'_>) -> Result<(UavRef, WriteMask), Sm4DecodeError> {
    let op = decode_raw_operand(r)?;
    if op.imm32.is_some() {
        return Err(Sm4DecodeError {
            at_dword: r.base_at + r.pos.saturating_sub(1),
            kind: Sm4DecodeErrorKind::UnsupportedOperand("uav operand cannot be immediate"),
        });
    }
    if op.ty != OPERAND_TYPE_UNORDERED_ACCESS_VIEW {
        return Err(Sm4DecodeError {
            at_dword: r.base_at + r.pos.saturating_sub(1),
            kind: Sm4DecodeErrorKind::UnsupportedOperand("expected uav operand"),
        });
    }
    let slot = one_index(op.ty, &op.indices, r.base_at)?;
    let mask = match op.selection_mode {
        OPERAND_SEL_MASK => WriteMask((op.component_sel & 0xF) as u8),
        _ => WriteMask::XYZW,
    };
    Ok((UavRef { slot }, mask))
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
        let v = self
            .toks
            .get(self.pos)
            .copied()
            .ok_or_else(|| Sm4DecodeError {
                at_dword: self.base_at + self.pos,
                kind: Sm4DecodeErrorKind::UnexpectedEof {
                    wanted: 1,
                    remaining: 0,
                },
            })?;
        self.pos += 1;
        Ok(v)
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
