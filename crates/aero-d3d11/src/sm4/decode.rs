use core::fmt;
use std::collections::{BTreeMap, BTreeSet};

use crate::sm4_ir::{
    BufferKind, BufferRef, CmpOp, CmpType, ComputeBuiltin, DstOperand, GsInputPrimitive,
    GsOutputTopology, HsDomain, HsOutputTopology, HsPartitioning, HullShaderPhase, OperandModifier,
    PredicateDstOperand, PredicateOperand, PredicateRef, RegFile, RegisterRef, SamplerRef,
    Sm4CmpOp, Sm4Decl, Sm4Inst, Sm4Module, Sm4TestBool, SrcKind, SrcOperand, Swizzle, TextureRef,
    UavRef, WriteMask,
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
    /// The shader appears to use the official DXBC SM4/SM5 token encoding from the Windows SDK,
    /// which is not yet supported by Aero's SM4 decoder.
    ///
    /// This is typically detected when an opcode token has:
    /// - a zero instruction-length field in Aero's legacy encoding (bits 11..=23), but
    /// - a non-zero length in the official encoding (bits 24..=30).
    UnsupportedTokenEncoding {
        encoding: &'static str,
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
            Sm4DecodeErrorKind::UnsupportedTokenEncoding { encoding } => write!(
                f,
                "unsupported SM4/5 token encoding ({encoding}); this decoder currently expects Aero's internal token format"
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
    while i < toks.len() {
        let opcode_token = toks[i];
        let opcode = opcode_token & OPCODE_MASK;
        let len = ((opcode_token >> OPCODE_LEN_SHIFT) & OPCODE_LEN_MASK) as usize;
        if len == 0 {
            // Real DXBC encodes instruction length in bits 24..=30; our current decoder/fixtures use
            // a legacy encoding with length in bits 11..=23. Provide a targeted error when the
            // official encoding is detected so users don't have to reverse-engineer the bitfields
            // from a generic "length is zero" failure.
            let official_len = ((opcode_token >> 24) & 0x7f) as usize;
            if official_len != 0 {
                return Err(Sm4DecodeError {
                    at_dword: i,
                    kind: Sm4DecodeErrorKind::UnsupportedTokenEncoding {
                        encoding: "Windows SDK DXBC (length in bits 24..30)",
                    },
                });
            }
            return Err(Sm4DecodeError {
                at_dword: i,
                kind: Sm4DecodeErrorKind::InstructionLengthZero,
            });
        }
        if i + len > toks.len() {
            // If the official DXBC instruction-length field (bits 24..=30) looks valid, prefer a
            // targeted "unsupported encoding" error over a generic bounds failure.
            let official_len = ((opcode_token >> 24) & 0x7f) as usize;
            if official_len != 0 && i + official_len <= toks.len() {
                return Err(Sm4DecodeError {
                    at_dword: i,
                    kind: Sm4DecodeErrorKind::UnsupportedTokenEncoding {
                        encoding: "Windows SDK DXBC (length in bits 24..30)",
                    },
                });
            }
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
            let Some(class) = inst_toks.get(class_pos).copied() else {
                decls.push(Sm4Decl::CustomData {
                    class: CUSTOMDATA_CLASS_COMMENT,
                    len_dwords: len as u32,
                });
                i += len;
                continue;
            };

            if class == CUSTOMDATA_CLASS_IMMEDIATE_CONSTANT_BUFFER {
                decls.push(Sm4Decl::ImmediateConstantBuffer {
                    dwords: inst_toks.get(class_pos + 1..).unwrap_or(&[]).to_vec(),
                });
            } else {
                decls.push(Sm4Decl::CustomData {
                    class,
                    len_dwords: len as u32,
                });
            }
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

        // Hull shader phase markers (`hs_control_point_phase`, `hs_fork_phase`, `hs_join_phase`)
        // are non-executable instructions that delimit the control-point vs patch-constant code
        // paths. Preserve them as metadata rather than polluting the instruction stream.
        if program.stage == super::ShaderStage::Hull
            && len == 1
            && matches!(
                opcode,
                OPCODE_HS_CONTROL_POINT_PHASE | OPCODE_HS_FORK_PHASE | OPCODE_HS_JOIN_PHASE
            )
        {
            let phase = match opcode {
                OPCODE_HS_CONTROL_POINT_PHASE => HullShaderPhase::ControlPoint,
                OPCODE_HS_FORK_PHASE => HullShaderPhase::Fork,
                OPCODE_HS_JOIN_PHASE => HullShaderPhase::Join,
                _ => unreachable!(),
            };
            decls.push(Sm4Decl::HsPhase {
                phase,
                inst_index: instructions.len(),
            });
            i += len;
            continue;
        }

        // Declarations are encoded in a separate opcode space (>= 0x100). Most shader stages place
        // all declarations before executable instructions, but hull shaders contain per-phase
        // declaration blocks that can appear after phase markers. Treat any opcode in the
        // declaration range as a declaration regardless of its position in the token stream.
        if opcode >= DECLARATION_OPCODE_MIN {
            // Most declarations are best-effort decoded: if we can't interpret the encoding we
            // preserve them as `Unknown` and continue.
            //
            // `dcl_thread_group` is special: its payload is required for compute translation
            // (`@workgroup_size`), and its encoding is fixed (three immediate DWORDs). If it is
            // malformed, surface the decode error rather than silently dropping the declaration.
            let decl = if opcode == OPCODE_DCL_THREAD_GROUP {
                decode_decl(opcode, inst_toks, i)?
            } else {
                decode_decl(opcode, inst_toks, i).unwrap_or(Sm4Decl::Unknown { opcode })
            };
            decls.push(decl);
            i += len;
            continue;
        }

        instructions.push(decode_instruction(opcode, inst_toks, i)?);
        i += len;
    }

    // Post-processing: refine certain instruction forms using information from declarations.
    //
    // `bufinfo` output packing differs between raw and structured buffers; the instruction token
    // stream does not encode the stride directly, so we derive it from the corresponding
    // `dcl_resource_structured` / `dcl_uav_structured` declaration when available.
    //
    // This keeps `decode_instruction` context-free while still letting downstream stages (e.g. WGSL
    // translation) distinguish `ByteAddressBuffer.GetDimensions` from
    // `StructuredBuffer.GetDimensions`.
    let mut srv_buffer_decls: BTreeMap<u32, (BufferKind, u32)> = BTreeMap::new();
    let mut uav_buffer_decls: BTreeMap<u32, (BufferKind, u32)> = BTreeMap::new();
    let mut uav_typed_decls: BTreeSet<u32> = BTreeSet::new();
    for decl in &decls {
        match decl {
            Sm4Decl::ResourceBuffer { slot, stride, kind } => {
                srv_buffer_decls.insert(*slot, (*kind, *stride));
            }
            Sm4Decl::UavBuffer { slot, stride, kind } => {
                uav_buffer_decls.insert(*slot, (*kind, *stride));
            }
            Sm4Decl::UavTyped2D { slot, .. } => {
                uav_typed_decls.insert(*slot);
            }
            _ => {}
        }
    }
    if !srv_buffer_decls.is_empty() || !uav_buffer_decls.is_empty() || !uav_typed_decls.is_empty() {
        for inst in &mut instructions {
            match inst {
                Sm4Inst::BufInfoRaw { dst, buffer } => {
                    if let Some((BufferKind::Structured, stride)) =
                        srv_buffer_decls.get(&buffer.slot).copied()
                    {
                        if stride != 0 {
                            *inst = Sm4Inst::BufInfoStructured {
                                dst: dst.clone(),
                                buffer: *buffer,
                                stride_bytes: stride,
                            };
                        }
                    }
                }
                Sm4Inst::BufInfoRawUav { dst, uav } => {
                    if let Some((BufferKind::Structured, stride)) =
                        uav_buffer_decls.get(&uav.slot).copied()
                    {
                        if stride != 0 {
                            *inst = Sm4Inst::BufInfoStructuredUav {
                                dst: dst.clone(),
                                uav: *uav,
                                stride_bytes: stride,
                            };
                        }
                    }
                }
                // DXBC encodes both typed UAV stores (`store_uav_typed`) and raw UAV buffer stores
                // (`store_raw`) using the same operand shapes: `u#, coord/addr, value`.
                //
                // Some toolchains appear to use different opcode IDs, so the decoder may initially
                // classify an unknown store as one of the IR variants based purely on operand
                // structure. Refine the final variant using the corresponding UAV declaration.
                Sm4Inst::StoreRaw {
                    uav,
                    addr,
                    value,
                    mask,
                } => {
                    if uav_typed_decls.contains(&uav.slot) {
                        *inst = Sm4Inst::StoreUavTyped {
                            uav: *uav,
                            coord: addr.clone(),
                            value: value.clone(),
                            mask: *mask,
                        };
                    }
                }
                Sm4Inst::StoreUavTyped {
                    uav,
                    coord,
                    value,
                    mask,
                } => {
                    if !uav_typed_decls.contains(&uav.slot)
                        && uav_buffer_decls.contains_key(&uav.slot)
                    {
                        *inst = Sm4Inst::StoreRaw {
                            uav: *uav,
                            addr: coord.clone(),
                            value: value.clone(),
                            mask: *mask,
                        };
                    }
                }
                _ => {}
            }
        }
    }

    Ok(Sm4Module {
        stage: program.stage,
        model: program.model,
        decls,
        instructions,
    })
}

/// Decode only the declaration/metadata section of an SM4/SM5 token stream.
///
/// This is useful for stages that the translator does not implement yet (e.g. HS/DS) where we may
/// still want to extract declaration metadata (such as `dcl_inputcontrolpoints`) without requiring
/// the full instruction stream to decode successfully.
pub fn decode_program_decls(program: &Sm4Program) -> Result<Vec<Sm4Decl>, Sm4DecodeError> {
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

    let mut i = 2usize;
    while i < toks.len() {
        let opcode_token = toks[i];
        let opcode = opcode_token & OPCODE_MASK;
        let len = ((opcode_token >> OPCODE_LEN_SHIFT) & OPCODE_LEN_MASK) as usize;
        if len == 0 {
            let official_len = ((opcode_token >> 24) & 0x7f) as usize;
            if official_len != 0 {
                return Err(Sm4DecodeError {
                    at_dword: i,
                    kind: Sm4DecodeErrorKind::UnsupportedTokenEncoding {
                        encoding: "Windows SDK DXBC (length in bits 24..30)",
                    },
                });
            }
            return Err(Sm4DecodeError {
                at_dword: i,
                kind: Sm4DecodeErrorKind::InstructionLengthZero,
            });
        }
        if i + len > toks.len() {
            let official_len = ((opcode_token >> 24) & 0x7f) as usize;
            if official_len != 0 && i + official_len <= toks.len() {
                return Err(Sm4DecodeError {
                    at_dword: i,
                    kind: Sm4DecodeErrorKind::UnsupportedTokenEncoding {
                        encoding: "Windows SDK DXBC (length in bits 24..30)",
                    },
                });
            }
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

        // Treat all customdata blocks as declarations/metadata, even if interspersed inside the
        // instruction stream.
        if opcode == OPCODE_CUSTOMDATA {
            let mut class_pos = 1usize;
            let mut extended = (opcode_token & OPCODE_EXTENDED_BIT) != 0;
            while extended {
                let Some(ext) = inst_toks.get(class_pos).copied() else {
                    break;
                };
                class_pos += 1;
                extended = (ext & OPCODE_EXTENDED_BIT) != 0;
            }
            let Some(class) = inst_toks.get(class_pos).copied() else {
                decls.push(Sm4Decl::CustomData {
                    class: CUSTOMDATA_CLASS_COMMENT,
                    len_dwords: len as u32,
                });
                i += len;
                continue;
            };

            if class == CUSTOMDATA_CLASS_IMMEDIATE_CONSTANT_BUFFER {
                decls.push(Sm4Decl::ImmediateConstantBuffer {
                    dwords: inst_toks.get(class_pos + 1..).unwrap_or(&[]).to_vec(),
                });
            } else {
                decls.push(Sm4Decl::CustomData {
                    class,
                    len_dwords: len as u32,
                });
            }
            i += len;
            continue;
        }

        if opcode == OPCODE_NOP {
            i += len;
            continue;
        }

        // Declarations typically appear at the start of the token stream, but some shader stages
        // (notably HS) can interleave metadata declarations with phase markers/instructions.
        //
        // Continue scanning the full stream and collect any declaration opcodes we encounter
        // instead of stopping at the first non-declaration.
        if opcode < DECLARATION_OPCODE_MIN {
            i += len;
            continue;
        }

        let decl = decode_decl(opcode, inst_toks, i).unwrap_or(Sm4Decl::Unknown { opcode });
        decls.push(decl);
        i += len;
    }

    Ok(decls)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sm4::{ShaderModel, ShaderStage};

    fn opcode_token(opcode: u32, len_dwords: u32) -> u32 {
        opcode | (len_dwords << OPCODE_LEN_SHIFT)
    }

    #[test]
    fn dcl_thread_group_is_decoded() {
        // Build a minimal cs_5_0 token stream:
        // - dcl_thread_group 8,4,2
        // - ret
        let version_token = 0x0005_0050u32; // cs_5_0

        let decl_opcode = OPCODE_DCL_THREAD_GROUP;
        let decl_len = 4u32;

        let ret_len = 1u32;

        let mut tokens = vec![
            version_token,
            0, // declared length patched below
            opcode_token(decl_opcode, decl_len),
            8u32,
            4u32,
            2u32,
            opcode_token(OPCODE_RET, ret_len),
        ];
        tokens[1] = tokens.len() as u32;

        let program = Sm4Program {
            stage: ShaderStage::Compute,
            model: ShaderModel { major: 5, minor: 0 },
            tokens,
        };

        let module = super::decode_program(&program).expect("decode should succeed");
        assert!(matches!(
            module.decls.as_slice(),
            [Sm4Decl::ThreadGroupSize { x: 8, y: 4, z: 2 }]
        ));
        assert!(matches!(module.instructions.as_slice(), [Sm4Inst::Ret]));
    }

    #[test]
    fn dcl_inputcontrolpoints_is_decoded() {
        // Build a minimal hs_5_0 token stream:
        // - dcl_inputcontrolpoints 4
        // - ret
        let version_token = 0x0003_0050u32; // hs_5_0 (program type 3)
        let decl_len = 2u32;
        let ret_len = 1u32;
        let mut tokens = vec![
            version_token,
            0, // declared length patched below
            opcode_token(OPCODE_DCL_INPUT_CONTROL_POINT_COUNT, decl_len),
            4u32,
            opcode_token(OPCODE_RET, ret_len),
        ];
        tokens[1] = tokens.len() as u32;

        let program = Sm4Program {
            stage: ShaderStage::Hull,
            model: ShaderModel { major: 5, minor: 0 },
            tokens,
        };

        let module = super::decode_program(&program).expect("decode should succeed");
        assert!(module
            .decls
            .iter()
            .any(|d| matches!(d, Sm4Decl::InputControlPointCount { count: 4 })));
        assert!(matches!(module.instructions.as_slice(), [Sm4Inst::Ret]));
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
            !module.instructions.iter().any(|i| matches!(
                i,
                Sm4Inst::Unknown {
                    opcode: OPCODE_CUSTOMDATA
                }
            )),
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
            !module.instructions.iter().any(|i| matches!(
                i,
                Sm4Inst::Unknown {
                    opcode: OPCODE_CUSTOMDATA
                }
            )),
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
            matches!(
                err.kind,
                Sm4DecodeErrorKind::InstructionOutOfBounds { start: 2, .. }
            ),
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

    #[test]
    fn immediate_constant_buffer_records_payload_after_extended_tokens() {
        // The immediate constant buffer customdata class should be detected even when the opcode
        // uses extended opcode tokens, and its payload should start *after* the class DWORD.
        let version_token = 0x50u32; // ps_5_0

        // customdata block: opcode (extended) + ext token + class token + 4 payload DWORDs
        let customdata_len = 3u32 + 4u32;
        let customdata_opcode =
            opcode_token(OPCODE_CUSTOMDATA, customdata_len) | OPCODE_EXTENDED_BIT;
        let ext_token = 0u32; // terminates extended opcode token chain

        let payload = [0x1111_1111, 0x2222_2222, 0x3333_3333, 0x4444_4444];

        let mut tokens = vec![
            version_token,
            0, // declared length patched below
            customdata_opcode,
            ext_token,
            CUSTOMDATA_CLASS_IMMEDIATE_CONSTANT_BUFFER,
            payload[0],
            payload[1],
            payload[2],
            payload[3],
            opcode_token(OPCODE_RET, 1),
        ];
        tokens[1] = tokens.len() as u32;

        let program = Sm4Program {
            stage: ShaderStage::Pixel,
            model: ShaderModel { major: 5, minor: 0 },
            tokens,
        };

        let module = decode_program(&program).expect("decode should succeed");
        assert!(matches!(module.instructions.as_slice(), [Sm4Inst::Ret]));
        assert!(module.decls.iter().any(|d| matches!(
            d,
            Sm4Decl::ImmediateConstantBuffer { dwords } if dwords.as_slice() == payload
        )));
    }
}
#[doc(hidden)]
pub fn decode_instruction(
    opcode: u32,
    inst_toks: &[u32],
    at: usize,
) -> Result<Sm4Inst, Sm4DecodeError> {
    // SM4/SM5 predicated instructions show up in the wild in two encodings:
    // - Most commonly, as a leading predicate operand (`(+p0.x) mov ...`) placed before the
    //   regular operand list.
    // - Some blobs appear to encode predication as a *trailing* predicate operand (mirroring the
    //   legacy SM2/SM3 token stream format). This would otherwise show up as unexpected trailing
    //   tokens once we've decoded the fixed operand list for the opcode.
    //
    // Peel off a trailing predicate operand up front so the main opcode decoder sees a normal
    // operand list.
    //
    // Note: if the instruction already begins with a predicate operand (the common `(+p0.x)`
    // encoding), do *not* attempt to strip a trailing predicate operand. This avoids false
    // positives where some unrelated tail tokens happen to look like a predicate operand and would
    // otherwise be dropped from the instruction stream.
    let strip_trailing_predication = if opcode == OPCODE_SETP {
        // `setp` always starts with a predicate operand (destination), so we can't use the leading
        // operand type to infer predication encoding. Always allow the trailing predicate probe so
        // `setp ... (+p0.x)` can be recognized.
        true
    } else {
        // Skip over any extended opcode tokens to find the first operand token.
        let mut pos = 1usize;
        let mut extended = inst_toks
            .first()
            .is_some_and(|t| (t & OPCODE_EXTENDED_BIT) != 0);
        while extended && pos < inst_toks.len() {
            let ext = inst_toks[pos];
            pos += 1;
            extended = (ext & OPCODE_EXTENDED_BIT) != 0;
        }
        let leading_operand_is_predicate = inst_toks.get(pos).is_some_and(|t| {
            ((t >> OPERAND_TYPE_SHIFT) & OPERAND_TYPE_MASK) == OPERAND_TYPE_PREDICATE
        });
        !leading_operand_is_predicate
    };

    let mut inst_toks = inst_toks;
    let mut trailing_predication: Option<PredicateOperand> = None;
    if strip_trailing_predication && inst_toks.len() >= 3 {
        let len = inst_toks.len();
        // The predicate operand is always small: `operand_token + reg_index`, plus at most one
        // extended operand token for the `-p0.x` invert modifier. Probe the last few DWORDs to see
        // if they form a well-formed predicate operand that consumes the remainder of the
        // instruction.
        let min_start = len.saturating_sub(5).max(1);
        for start in (min_start..=len - 2).rev() {
            let mut rr = InstrReader::new(&inst_toks[start..], at + start);
            let Ok(pred) = decode_predicate_operand(&mut rr) else {
                continue;
            };
            if rr.is_eof() {
                trailing_predication = Some(pred);
                inst_toks = &inst_toks[..start];
                break;
            }
        }
    }

    let mut r = InstrReader::new(inst_toks, at);
    let opcode_token = r.read_u32()?;
    let saturate = decode_extended_opcode_modifiers(&mut r, opcode_token)?;

    // ---- Predication ----
    //
    // At this point we may already have stripped a trailing predicate operand. We also support the
    // leading-operand encoding by recognizing a predicate operand in the first position.
    //
    // `setp` itself writes to the predicate register file, so it can appear in both predicated and
    // non-predicated forms. For the leading-operand encoding, detect the predicated form by
    // looking for two consecutive predicate operands (first = condition, second = destination).
    let peek_operand_type = |r: &InstrReader<'_>| {
        r.peek_u32()
            .map(|t| (t >> OPERAND_TYPE_SHIFT) & OPERAND_TYPE_MASK)
    };

    let mut predication: Option<PredicateOperand> = trailing_predication;

    if opcode == OPCODE_SETP {
        // Non-predicated `setp` starts with a single predicate operand (destination).
        // Predicated `setp` starts with two predicate operands (condition, then destination).
        if peek_operand_type(&r) == Some(OPERAND_TYPE_PREDICATE) {
            let first_at = r.base_at + r.pos;
            let first = decode_raw_operand(&mut r)?;
            if peek_operand_type(&r) == Some(OPERAND_TYPE_PREDICATE) {
                predication = Some(predicate_operand_from_raw(first, first_at)?);
                // Destination follows.
                let dst = decode_predicate_dst(&mut r)?;
                let Some(op) = decode_setp_cmp_op(opcode_token) else {
                    return Ok(Sm4Inst::Unknown { opcode });
                };
                let a = decode_src(&mut r)?;
                let b = decode_src(&mut r)?;
                r.expect_eof()?;
                let inst = Sm4Inst::Setp { dst, op, a, b };
                if let Some(pred) = predication {
                    return Ok(Sm4Inst::Predicated {
                        pred,
                        inner: Box::new(inst),
                    });
                }
                return Ok(inst);
            }

            // First predicate operand is destination.
            let dst = predicate_dst_from_raw(first, first_at)?;
            let Some(op) = decode_setp_cmp_op(opcode_token) else {
                return Ok(Sm4Inst::Unknown { opcode });
            };
            let a = decode_src(&mut r)?;
            let b = decode_src(&mut r)?;
            r.expect_eof()?;
            let inst = Sm4Inst::Setp { dst, op, a, b };
            if let Some(pred) = predication {
                return Ok(Sm4Inst::Predicated {
                    pred,
                    inner: Box::new(inst),
                });
            }
            return Ok(inst);
        }

        return Ok(Sm4Inst::Unknown { opcode });
    }

    // All other instructions: if the first operand is a predicate register, treat it as the
    // predication operand.
    if peek_operand_type(&r) == Some(OPERAND_TYPE_PREDICATE) {
        if predication.is_some() {
            return Err(Sm4DecodeError {
                at_dword: r.base_at + r.pos,
                kind: Sm4DecodeErrorKind::UnsupportedOperand(
                    "multiple predicate operands found for instruction predication",
                ),
            });
        }
        predication = Some(decode_predicate_operand(&mut r)?);
    }

    let inst = match opcode {
        OPCODE_IF => {
            // Canonically, SM4/SM5 token streams use distinct opcode IDs for `if` (OPCODE_IF) and
            // compare-based `ifc` (OPCODE_IFC). In practice, some toolchains encode `ifc_*` using
            // `OPCODE_IF` with a non-boolean instruction-test value in the opcode token; handle
            // that encoding here as well.
            //
            // - 0/1: `if_z` / `if_nz`
            // - 2..=7: `ifc_*` with an in-token comparison operator.
            let test_raw = (opcode_token >> OPCODE_TEST_SHIFT) & OPCODE_TEST_MASK;
            match test_raw {
                0 => {
                    let cond = decode_src(&mut r)?;
                    r.expect_eof()?;
                    Ok(Sm4Inst::If {
                        cond,
                        test: Sm4TestBool::Zero,
                    })
                }
                1 => {
                    let cond = decode_src(&mut r)?;
                    r.expect_eof()?;
                    Ok(Sm4Inst::If {
                        cond,
                        test: Sm4TestBool::NonZero,
                    })
                }
                2..=7 => {
                    if let Some(op) = decode_flow_cmp_op(opcode_token) {
                        let a = decode_src(&mut r)?;
                        let b = decode_src(&mut r)?;
                        r.expect_eof()?;
                        Ok(Sm4Inst::IfC { op, a, b })
                    } else {
                        Ok(Sm4Inst::Unknown { opcode })
                    }
                }
                _ => Ok(Sm4Inst::Unknown { opcode }),
            }
        }
        OPCODE_IFC => {
            let Some(op) = decode_flow_cmp_op(opcode_token) else {
                return Ok(Sm4Inst::Unknown { opcode });
            };
            let a = decode_src(&mut r)?;
            let b = decode_src(&mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::IfC { op, a, b })
        }
        OPCODE_ELSE => {
            r.expect_eof()?;
            Ok(Sm4Inst::Else)
        }
        OPCODE_ENDIF => {
            r.expect_eof()?;
            Ok(Sm4Inst::EndIf)
        }
        // ---- ALU / resource ops ----
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
        OPCODE_UMUL | OPCODE_IMUL => {
            // Integer multiply does not support saturate; ignore the modifier if present.
            let signed = opcode == OPCODE_IMUL;
            let mut r_multi = r.clone();
            if let Ok(inst) = decode_int_mul(signed, &mut r_multi) {
                return Ok(inst);
            }
            decode_int_mul_single(signed, &mut r)
        }
        OPCODE_UMAD | OPCODE_IMAD => {
            // Integer multiply-add does not support saturate; ignore the modifier if present.
            let signed = opcode == OPCODE_IMAD;
            let mut r_multi = r.clone();
            if let Ok(inst) = decode_int_mad(signed, &mut r_multi) {
                return Ok(inst);
            }
            decode_int_mad_single(signed, &mut r)
        }
        OPCODE_ADD => {
            let mut dst = decode_dst(&mut r)?;
            dst.saturate = saturate;
            let a = decode_src(&mut r)?;
            let b = decode_src(&mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::Add { dst, a, b })
        }
        OPCODE_IADDC => {
            let dst_sum = decode_dst(&mut r)?;
            let dst_carry = decode_dst(&mut r)?;
            let a = decode_src(&mut r)?;
            let b = decode_src(&mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::IAddC {
                dst_sum,
                dst_carry,
                a,
                b,
            })
        }
        OPCODE_UADDC => {
            let dst_sum = decode_dst(&mut r)?;
            let dst_carry = decode_dst(&mut r)?;
            let a = decode_src(&mut r)?;
            let b = decode_src(&mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::UAddC {
                dst_sum,
                dst_carry,
                a,
                b,
            })
        }
        OPCODE_ISUBC => {
            let dst_diff = decode_dst(&mut r)?;
            let dst_carry = decode_dst(&mut r)?;
            let a = decode_src(&mut r)?;
            let b = decode_src(&mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::ISubC {
                dst_diff,
                dst_carry,
                a,
                b,
            })
        }
        OPCODE_USUBB => {
            let dst_diff = decode_dst(&mut r)?;
            let dst_borrow = decode_dst(&mut r)?;
            let a = decode_src(&mut r)?;
            let b = decode_src(&mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::USubB {
                dst_diff,
                dst_borrow,
                a,
                b,
            })
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
        OPCODE_UDIV => {
            // `udiv dst_quot, dst_rem, a, b`
            // Note: integer division does not support saturate; ignore the opcode modifier if
            // present (it should not be emitted for valid DXBC).
            let dst_quot = decode_dst(&mut r)?;
            let dst_rem = decode_dst(&mut r)?;
            let a = decode_src(&mut r)?;
            let b = decode_src(&mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::UDiv {
                dst_quot,
                dst_rem,
                a,
                b,
            })
        }
        OPCODE_IDIV => {
            // `idiv dst_quot, dst_rem, a, b`
            // Note: integer division does not support saturate; ignore the opcode modifier if
            // present (it should not be emitted for valid DXBC).
            let dst_quot = decode_dst(&mut r)?;
            let dst_rem = decode_dst(&mut r)?;
            let a = decode_src(&mut r)?;
            let b = decode_src(&mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::IDiv {
                dst_quot,
                dst_rem,
                a,
                b,
            })
        }
        OPCODE_IADD => {
            let dst = decode_dst(&mut r)?;
            let a = decode_src(&mut r)?;
            let b = decode_src(&mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::IAdd { dst, a, b })
        }
        OPCODE_ISUB => {
            let dst = decode_dst(&mut r)?;
            let a = decode_src(&mut r)?;
            let b = decode_src(&mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::ISub { dst, a, b })
        }
        OPCODE_AND => {
            let dst = decode_dst(&mut r)?;
            let a = decode_src(&mut r)?;
            let b = decode_src(&mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::And { dst, a, b })
        }
        OPCODE_OR => {
            let dst = decode_dst(&mut r)?;
            let a = decode_src(&mut r)?;
            let b = decode_src(&mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::Or { dst, a, b })
        }
        OPCODE_XOR => {
            let dst = decode_dst(&mut r)?;
            let a = decode_src(&mut r)?;
            let b = decode_src(&mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::Xor { dst, a, b })
        }
        OPCODE_NOT => {
            let dst = decode_dst(&mut r)?;
            let src = decode_src(&mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::Not { dst, src })
        }
        OPCODE_ISHL => {
            let dst = decode_dst(&mut r)?;
            let a = decode_src(&mut r)?;
            let b = decode_src(&mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::IShl { dst, a, b })
        }
        OPCODE_ISHR => {
            let dst = decode_dst(&mut r)?;
            let a = decode_src(&mut r)?;
            let b = decode_src(&mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::IShr { dst, a, b })
        }
        OPCODE_USHR => {
            let dst = decode_dst(&mut r)?;
            let a = decode_src(&mut r)?;
            let b = decode_src(&mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::UShr { dst, a, b })
        }
        OPCODE_IMIN => {
            let mut dst = decode_dst(&mut r)?;
            dst.saturate = saturate;
            let a = decode_src(&mut r)?;
            let b = decode_src(&mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::IMin { dst, a, b })
        }
        OPCODE_IMAX => {
            let mut dst = decode_dst(&mut r)?;
            dst.saturate = saturate;
            let a = decode_src(&mut r)?;
            let b = decode_src(&mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::IMax { dst, a, b })
        }
        OPCODE_UMIN => {
            let mut dst = decode_dst(&mut r)?;
            dst.saturate = saturate;
            let a = decode_src(&mut r)?;
            let b = decode_src(&mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::UMin { dst, a, b })
        }
        OPCODE_UMAX => {
            let mut dst = decode_dst(&mut r)?;
            dst.saturate = saturate;
            let a = decode_src(&mut r)?;
            let b = decode_src(&mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::UMax { dst, a, b })
        }
        OPCODE_IABS => {
            let mut dst = decode_dst(&mut r)?;
            dst.saturate = saturate;
            let src = decode_src(&mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::IAbs { dst, src })
        }
        OPCODE_INEG => {
            let mut dst = decode_dst(&mut r)?;
            dst.saturate = saturate;
            let src = decode_src(&mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::INeg { dst, src })
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
        OPCODE_ITOF => {
            let mut dst = decode_dst(&mut r)?;
            dst.saturate = saturate;
            let src = decode_src(&mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::Itof { dst, src })
        }
        OPCODE_UTOF => {
            let mut dst = decode_dst(&mut r)?;
            dst.saturate = saturate;
            let src = decode_src(&mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::Utof { dst, src })
        }
        OPCODE_FTOI => {
            // `ftoi` results are integer bit patterns stored in the untyped register file.
            // Saturate modifiers are only meaningful for float results, so ignore them here.
            let dst = decode_dst(&mut r)?;
            let src = decode_src(&mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::Ftoi { dst, src })
        }
        OPCODE_FTOU => {
            // `ftou` results are integer bit patterns stored in the untyped register file.
            // Saturate modifiers are only meaningful for float results, so ignore them here.
            let dst = decode_dst(&mut r)?;
            let src = decode_src(&mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::Ftou { dst, src })
        }
        OPCODE_BFI => {
            let mut dst = decode_dst(&mut r)?;
            dst.saturate = saturate;
            let width = decode_src(&mut r)?;
            let offset = decode_src(&mut r)?;
            let insert = decode_src(&mut r)?;
            let base = decode_src(&mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::Bfi {
                dst,
                width,
                offset,
                insert,
                base,
            })
        }
        OPCODE_UBFE => {
            let mut dst = decode_dst(&mut r)?;
            dst.saturate = saturate;
            let width = decode_src(&mut r)?;
            let offset = decode_src(&mut r)?;
            let src = decode_src(&mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::Ubfe {
                dst,
                width,
                offset,
                src,
            })
        }
        OPCODE_IBFE => {
            let mut dst = decode_dst(&mut r)?;
            dst.saturate = saturate;
            let width = decode_src(&mut r)?;
            let offset = decode_src(&mut r)?;
            let src = decode_src(&mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::Ibfe {
                dst,
                width,
                offset,
                src,
            })
        }
        OPCODE_EQ | OPCODE_NE | OPCODE_LT | OPCODE_LE | OPCODE_GT | OPCODE_GE => {
            // Float compare (`eq/ne/lt/le/gt/ge`).
            //
            // These opcodes write D3D-style boolean mask bits (0xffffffff/0) into the untyped
            // register file (modeled as `vec4<f32>` in the IR).
            //
            // Saturate modifiers are only meaningful for float *numeric* results. Applying saturate
            // here would treat predicate mask bits as floats and corrupt the value, so ignore it.
            //
            // Note: If the operand encoding doesn't match the expected `dst, a, b` pattern, fall
            // back to `Unknown` instead of failing the decode. This keeps the decoder resilient in
            // the presence of future/unknown instructions that might share numeric opcode values
            // with our small bring-up subset.
            let dst = match decode_dst(&mut r) {
                Ok(v) => v,
                Err(_) => return Ok(Sm4Inst::Unknown { opcode }),
            };
            let a = match decode_src(&mut r) {
                Ok(v) => v,
                Err(_) => return Ok(Sm4Inst::Unknown { opcode }),
            };
            let b = match decode_src(&mut r) {
                Ok(v) => v,
                Err(_) => return Ok(Sm4Inst::Unknown { opcode }),
            };
            if r.expect_eof().is_err() {
                return Ok(Sm4Inst::Unknown { opcode });
            }

            let op = match opcode {
                OPCODE_EQ => CmpOp::Eq,
                OPCODE_NE => CmpOp::Ne,
                OPCODE_LT => CmpOp::Lt,
                OPCODE_LE => CmpOp::Le,
                OPCODE_GT => CmpOp::Gt,
                OPCODE_GE => CmpOp::Ge,
                _ => unreachable!("opcode match ensures exhaustive"),
            };
            Ok(Sm4Inst::Cmp {
                dst,
                a,
                b,
                op,
                ty: CmpType::F32,
            })
        }
        OPCODE_IEQ | OPCODE_INE | OPCODE_ILT | OPCODE_IGE | OPCODE_ULT | OPCODE_UGE => {
            // Integer compares write predicate masks (0xffffffff/0) into the untyped register file.
            // Saturate would clamp the numeric float interpretation of those bits, corrupting the
            // predicate payload, so ignore it here (while still consuming any extended opcode
            // tokens above).
            let dst = decode_dst(&mut r)?;
            let a = decode_src(&mut r)?;
            let b = decode_src(&mut r)?;
            r.expect_eof()?;

            let (op, ty) = match opcode {
                OPCODE_IEQ => (CmpOp::Eq, CmpType::I32),
                OPCODE_INE => (CmpOp::Ne, CmpType::I32),
                OPCODE_ILT => (CmpOp::Lt, CmpType::I32),
                OPCODE_IGE => (CmpOp::Ge, CmpType::I32),
                OPCODE_ULT => (CmpOp::Lt, CmpType::U32),
                OPCODE_UGE => (CmpOp::Ge, CmpType::U32),
                _ => unreachable!("opcode match ensures exhaustive"),
            };

            Ok(Sm4Inst::Cmp { dst, a, b, op, ty })
        }
        OPCODE_F32TOF16 => {
            let mut dst = decode_dst(&mut r)?;
            dst.saturate = saturate;
            let src = decode_src(&mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::F32ToF16 { dst, src })
        }
        OPCODE_F16TOF32 => {
            let mut dst = decode_dst(&mut r)?;
            dst.saturate = saturate;
            let src = decode_src(&mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::F16ToF32 { dst, src })
        }
        OPCODE_SWITCH => {
            let selector = decode_src(&mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::Switch { selector })
        }
        OPCODE_CASE => {
            let op = decode_raw_operand(&mut r)?;
            if op.ty != OPERAND_TYPE_IMMEDIATE32 || op.imm32.is_none() {
                return Err(Sm4DecodeError {
                    at_dword: r.base_at + r.pos.saturating_sub(1),
                    kind: Sm4DecodeErrorKind::UnsupportedOperand(
                        "case label must be an immediate32 operand",
                    ),
                });
            }
            let value = op
                .imm32
                .expect("checked imm32 is present for immediate32")
                .first()
                .copied()
                .unwrap_or(0);
            r.expect_eof()?;
            Ok(Sm4Inst::Case { value })
        }
        OPCODE_DEFAULT => {
            r.expect_eof()?;
            Ok(Sm4Inst::Default)
        }
        OPCODE_ENDSWITCH => {
            r.expect_eof()?;
            Ok(Sm4Inst::EndSwitch)
        }
        OPCODE_LOOP => {
            r.expect_eof()?;
            Ok(Sm4Inst::Loop)
        }
        OPCODE_ENDLOOP => {
            r.expect_eof()?;
            Ok(Sm4Inst::EndLoop)
        }
        OPCODE_BREAK => {
            r.expect_eof()?;
            Ok(Sm4Inst::Break)
        }
        OPCODE_BREAKC => {
            if let Some(op) = decode_flow_cmp_op(opcode_token) {
                let a = decode_src(&mut r)?;
                let b = decode_src(&mut r)?;
                r.expect_eof()?;
                Ok(Sm4Inst::BreakC { op, a, b })
            } else {
                Ok(Sm4Inst::Unknown { opcode })
            }
        }
        OPCODE_CONTINUE => {
            r.expect_eof()?;
            Ok(Sm4Inst::Continue)
        }
        OPCODE_CONTINUEC => {
            if let Some(op) = decode_flow_cmp_op(opcode_token) {
                let a = decode_src(&mut r)?;
                let b = decode_src(&mut r)?;
                r.expect_eof()?;
                Ok(Sm4Inst::ContinueC { op, a, b })
            } else {
                Ok(Sm4Inst::Unknown { opcode })
            }
        }
        OPCODE_BFREV => {
            let mut dst = decode_dst(&mut r)?;
            dst.saturate = saturate;
            let src = decode_src(&mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::Bfrev { dst, src })
        }
        OPCODE_COUNTBITS => {
            let mut dst = decode_dst(&mut r)?;
            dst.saturate = saturate;
            let src = decode_src(&mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::CountBits { dst, src })
        }
        OPCODE_FIRSTBIT_HI => {
            let mut dst = decode_dst(&mut r)?;
            dst.saturate = saturate;
            let src = decode_src(&mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::FirstbitHi { dst, src })
        }
        OPCODE_FIRSTBIT_LO => {
            let mut dst = decode_dst(&mut r)?;
            dst.saturate = saturate;
            let src = decode_src(&mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::FirstbitLo { dst, src })
        }
        OPCODE_FIRSTBIT_SHI => {
            let mut dst = decode_dst(&mut r)?;
            dst.saturate = saturate;
            let src = decode_src(&mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::FirstbitShi { dst, src })
        }
        OPCODE_EMITTHENCUT => {
            r.expect_eof()?;
            Ok(Sm4Inst::EmitThenCut { stream: 0 })
        }
        OPCODE_EMITTHENCUT_STREAM => {
            let stream = decode_stream_index(opcode_token, &mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::EmitThenCut { stream })
        }
        OPCODE_DISCARD => {
            let test_raw = (opcode_token >> OPCODE_TEST_BOOLEAN_SHIFT) & OPCODE_TEST_BOOLEAN_MASK;
            let test = match test_raw {
                0 => Sm4TestBool::Zero,
                1 => Sm4TestBool::NonZero,
                _ => return Ok(Sm4Inst::Unknown { opcode }),
            };
            let cond = decode_src(&mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::Discard { cond, test })
        }
        OPCODE_CLIP => {
            let src = decode_src(&mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::Clip { src })
        }
        OPCODE_RET => {
            r.expect_eof()?;
            Ok(Sm4Inst::Ret)
        }
        OPCODE_EMIT => {
            r.expect_eof()?;
            Ok(Sm4Inst::Emit { stream: 0 })
        }
        OPCODE_CUT => {
            r.expect_eof()?;
            Ok(Sm4Inst::Cut { stream: 0 })
        }
        OPCODE_EMIT_STREAM => {
            let stream = decode_stream_index(opcode_token, &mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::Emit { stream })
        }
        OPCODE_CUT_STREAM => {
            let stream = decode_stream_index(opcode_token, &mut r)?;
            r.expect_eof()?;
            Ok(Sm4Inst::Cut { stream })
        }
        OPCODE_SAMPLE | OPCODE_SAMPLE_L => decode_sample_like(opcode, saturate, &mut r),
        OPCODE_RESINFO => decode_resinfo(saturate, &mut r),
        OPCODE_LD => decode_ld(saturate, &mut r),
        OPCODE_LD_RAW => decode_ld_raw(saturate, &mut r),
        OPCODE_LD_STRUCTURED => decode_ld_structured(saturate, &mut r),
        OPCODE_STORE_RAW => decode_store_raw(&mut r),
        OPCODE_STORE_STRUCTURED => decode_store_structured(&mut r),
        OPCODE_SYNC => {
            // `sync` encodes barrier/fence semantics in the opcode token control bits.
            //
            // Note: the SM5 instruction set includes both:
            // - Group-sync barriers (`*_t` variants), which require all threads to participate.
            // - Fence-only variants (no `THREAD_GROUP_SYNC`), which do not synchronize threads and
            //   therefore should not be treated as a workgroup execution barrier during
            //   translation.
            //
            // We preserve the raw flags here and let the WGSL backend pick the safest mapping.
            let sync_flags = (opcode_token >> OPCODE_CONTROL_SHIFT) & OPCODE_CONTROL_MASK;
            r.expect_eof()?;
            Ok(Sm4Inst::Sync { flags: sync_flags })
        }
        OPCODE_LD_UAV_RAW => decode_ld_uav_raw(saturate, &mut r),
        other => {
            // Structural fallback for sample/sample_l when opcode IDs differ.
            if let Some(sample) = try_decode_sample_like(saturate, inst_toks, at)? {
                Ok(sample)
            // Structural fallback for texture load (ld) when opcode IDs differ.
            } else if let Some(ld) = try_decode_ld_like(saturate, inst_toks, at)? {
                Ok(ld)
            // Structural fallback for `bufinfo` when opcode IDs differ.
            } else if let Some(bufinfo) = try_decode_bufinfo_like(saturate, inst_toks, at)? {
                Ok(bufinfo)
            // Structural fallback for UAV raw load (ld_uav_raw) when opcode IDs differ.
            } else if let Some(ld_uav_raw) = try_decode_ld_uav_raw_like(saturate, inst_toks, at)? {
                Ok(ld_uav_raw)
            // Structural fallback for structured buffer loads (`ld_structured` / `ld_uav_structured`)
            // when opcode IDs differ.
            } else if let Some(ld_struct) = try_decode_ld_structured_like(saturate, inst_toks, at)?
            {
                Ok(ld_struct)
            // Structural fallback for atomic add on UAV buffers (`InterlockedAdd`).
            } else if let Some(atomic) = try_decode_atomic_add_like(saturate, inst_toks, at)? {
                Ok(atomic)
            // Structural fallback for buffer UAV stores when opcode IDs differ.
            } else if let Some(store_struct) = try_decode_store_structured_like(inst_toks, at)? {
                Ok(store_struct)
            } else if let Some(store_raw) = try_decode_store_raw_like(inst_toks, at)? {
                Ok(store_raw)
            // Structural fallback for typed UAV stores.
            //
            // Note: some SM5 UAV store opcodes (e.g. `store_raw` / `store_structured`) have a
            // similar operand prefix (uav, addr/coord, value). We intentionally *do not* decode
            // those here to avoid misclassifying buffer stores as typed texture stores.
            } else if other != OPCODE_STORE_RAW && other != OPCODE_STORE_STRUCTURED {
                if let Some(store) = try_decode_store_uav_typed_like(inst_toks, at)? {
                    Ok(store)
                } else {
                    Ok(Sm4Inst::Unknown { opcode: other })
                }
            } else {
                Ok(Sm4Inst::Unknown { opcode: other })
            }
        }
    }?;

    if let Some(pred) = predication {
        Ok(Sm4Inst::Predicated {
            pred,
            inner: Box::new(inst),
        })
    } else {
        Ok(inst)
    }
}

fn decode_setp_cmp_op(opcode_token: u32) -> Option<Sm4CmpOp> {
    let raw = (opcode_token >> SETP_CMP_SHIFT) & SETP_CMP_MASK;
    match raw {
        0 => Some(Sm4CmpOp::Eq),
        1 => Some(Sm4CmpOp::Ne),
        2 => Some(Sm4CmpOp::Lt),
        3 => Some(Sm4CmpOp::Ge),
        4 => Some(Sm4CmpOp::Le),
        5 => Some(Sm4CmpOp::Gt),
        8 => Some(Sm4CmpOp::EqU),
        9 => Some(Sm4CmpOp::NeU),
        10 => Some(Sm4CmpOp::LtU),
        11 => Some(Sm4CmpOp::GeU),
        12 => Some(Sm4CmpOp::LeU),
        13 => Some(Sm4CmpOp::GtU),
        _ => None,
    }
}

fn decode_stream_index(opcode_token: u32, r: &mut InstrReader<'_>) -> Result<u8, Sm4DecodeError> {
    // Stream instructions (`emit_stream` / `cut_stream` / `emitthen_cut_stream`) take a stream
    // index in the range 0..=3.
    //
    // The common encoding uses an immediate32 scalar operand (replicated lanes). Some SM4 blobs
    // omit the operand entirely for stream 0; treat a missing operand as stream 0 by deriving the
    // index from the opcode token's opcode-specific control field.
    if r.is_eof() {
        return Ok(((opcode_token >> OPCODE_CONTROL_SHIFT) & 0x3) as u8);
    }
    let op = decode_raw_operand(r)?;
    let Some(imm) = op.imm32 else {
        return Err(Sm4DecodeError {
            at_dword: r.base_at + r.pos.saturating_sub(1),
            kind: Sm4DecodeErrorKind::UnsupportedOperand(
                "stream instruction expects an immediate stream index",
            ),
        });
    };
    let stream_raw = imm[0];
    let stream = u8::try_from(stream_raw).map_err(|_| Sm4DecodeError {
        at_dword: r.base_at + r.pos.saturating_sub(1),
        kind: Sm4DecodeErrorKind::UnsupportedOperand("stream index out of range"),
    })?;
    if stream > 3 {
        return Err(Sm4DecodeError {
            at_dword: r.base_at + r.pos.saturating_sub(1),
            kind: Sm4DecodeErrorKind::UnsupportedOperand("stream index out of range"),
        });
    }
    Ok(stream)
}

fn decode_bufinfo(saturate: bool, r: &mut InstrReader<'_>) -> Result<Sm4Inst, Sm4DecodeError> {
    let mut dst = decode_dst(r)?;
    dst.saturate = saturate;
    let op = decode_raw_operand(r)?;
    if op.imm32.is_some() {
        return Err(Sm4DecodeError {
            at_dword: r.base_at + r.pos.saturating_sub(1),
            kind: Sm4DecodeErrorKind::UnsupportedOperand("bufinfo operand cannot be immediate"),
        });
    }
    let slot = one_index(op.ty, &op.indices, r.base_at)?;
    r.expect_eof()?;
    match op.ty {
        OPERAND_TYPE_RESOURCE => Ok(Sm4Inst::BufInfoRaw {
            dst,
            buffer: BufferRef { slot },
        }),
        OPERAND_TYPE_UNORDERED_ACCESS_VIEW => Ok(Sm4Inst::BufInfoRawUav {
            dst,
            uav: UavRef { slot },
        }),
        other => Err(Sm4DecodeError {
            at_dword: r.base_at + r.pos.saturating_sub(1),
            kind: Sm4DecodeErrorKind::UnsupportedOperandType { ty: other },
        }),
    }
}
fn decode_int_mul(signed: bool, r: &mut InstrReader<'_>) -> Result<Sm4Inst, Sm4DecodeError> {
    let dst_lo = decode_dst(r)?;
    let dst_hi = decode_dst(r)?;
    let a = decode_src(r)?;
    let b = decode_src(r)?;
    r.expect_eof()?;
    Ok(if signed {
        Sm4Inst::IMul {
            dst_lo,
            dst_hi: Some(dst_hi),
            a,
            b,
        }
    } else {
        Sm4Inst::UMul {
            dst_lo,
            dst_hi: Some(dst_hi),
            a,
            b,
        }
    })
}

fn decode_int_mul_single(signed: bool, r: &mut InstrReader<'_>) -> Result<Sm4Inst, Sm4DecodeError> {
    let dst_lo = decode_dst(r)?;
    let a = decode_src(r)?;
    let b = decode_src(r)?;
    r.expect_eof()?;
    Ok(if signed {
        Sm4Inst::IMul {
            dst_lo,
            dst_hi: None,
            a,
            b,
        }
    } else {
        Sm4Inst::UMul {
            dst_lo,
            dst_hi: None,
            a,
            b,
        }
    })
}

fn decode_int_mad(signed: bool, r: &mut InstrReader<'_>) -> Result<Sm4Inst, Sm4DecodeError> {
    let dst_lo = decode_dst(r)?;
    let dst_hi = decode_dst(r)?;
    let a = decode_src(r)?;
    let b = decode_src(r)?;
    let c = decode_src(r)?;
    r.expect_eof()?;
    Ok(if signed {
        Sm4Inst::IMad {
            dst_lo,
            dst_hi: Some(dst_hi),
            a,
            b,
            c,
        }
    } else {
        Sm4Inst::UMad {
            dst_lo,
            dst_hi: Some(dst_hi),
            a,
            b,
            c,
        }
    })
}

fn decode_int_mad_single(signed: bool, r: &mut InstrReader<'_>) -> Result<Sm4Inst, Sm4DecodeError> {
    let dst_lo = decode_dst(r)?;
    let a = decode_src(r)?;
    let b = decode_src(r)?;
    let c = decode_src(r)?;
    r.expect_eof()?;
    Ok(if signed {
        Sm4Inst::IMad {
            dst_lo,
            dst_hi: None,
            a,
            b,
            c,
        }
    } else {
        Sm4Inst::UMad {
            dst_lo,
            dst_hi: None,
            a,
            b,
            c,
        }
    })
}

fn decode_flow_cmp_op(opcode_token: u32) -> Option<Sm4CmpOp> {
    // SM4/SM5 compare-based flow-control opcodes encode the comparison operator in the opcode
    // token's "instruction test" field.
    //
    // Per the D3D10/11 tokenized-program format, the field uses the `D3D10_SB_INSTRUCTION_TEST`
    // encoding (values are 0..=7).
    let test = ((opcode_token >> OPCODE_TEST_SHIFT) & OPCODE_TEST_MASK) as u8;
    match test {
        2 => Some(Sm4CmpOp::Eq),
        3 => Some(Sm4CmpOp::Ne),
        4 => Some(Sm4CmpOp::Gt),
        5 => Some(Sm4CmpOp::Ge),
        6 => Some(Sm4CmpOp::Lt),
        7 => Some(Sm4CmpOp::Le),
        _ => None,
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

fn decode_resinfo(saturate: bool, r: &mut InstrReader<'_>) -> Result<Sm4Inst, Sm4DecodeError> {
    let mut dst = decode_dst(r)?;
    dst.saturate = saturate;
    let mip_level = decode_src(r)?;
    let texture = decode_texture_ref(r)?;
    if !r.is_eof() {
        return Ok(Sm4Inst::Unknown {
            opcode: OPCODE_RESINFO,
        });
    }
    Ok(Sm4Inst::ResInfo {
        dst,
        mip_level,
        texture,
    })
}

fn decode_ld_raw(saturate: bool, r: &mut InstrReader<'_>) -> Result<Sm4Inst, Sm4DecodeError> {
    let mut dst = decode_dst(r)?;
    dst.saturate = saturate;
    let addr = decode_src(r)?;
    let buf_op = decode_raw_operand(r)?;
    if buf_op.imm32.is_some() {
        return Err(Sm4DecodeError {
            at_dword: r.base_at + r.pos.saturating_sub(1),
            kind: Sm4DecodeErrorKind::UnsupportedOperand("buffer operand cannot be immediate"),
        });
    }

    let slot = one_index(buf_op.ty, &buf_op.indices, r.base_at)?;
    let inst = match buf_op.ty {
        OPERAND_TYPE_RESOURCE => Sm4Inst::LdRaw {
            dst,
            addr,
            buffer: BufferRef { slot },
        },
        OPERAND_TYPE_UNORDERED_ACCESS_VIEW => Sm4Inst::LdUavRaw {
            dst,
            addr,
            uav: UavRef { slot },
        },
        _ => {
            return Err(Sm4DecodeError {
                at_dword: r.base_at + r.pos.saturating_sub(1),
                kind: Sm4DecodeErrorKind::UnsupportedOperandType { ty: buf_op.ty },
            })
        }
    };

    if !r.is_eof() {
        return Ok(Sm4Inst::Unknown {
            opcode: OPCODE_LD_RAW,
        });
    }
    Ok(inst)
}

fn decode_ld_uav_raw(saturate: bool, r: &mut InstrReader<'_>) -> Result<Sm4Inst, Sm4DecodeError> {
    let mut dst = decode_dst(r)?;
    dst.saturate = saturate;
    let addr = decode_src(r)?;
    let (uav, _mask) = decode_uav_ref(r)?;
    if !r.is_eof() {
        return Ok(Sm4Inst::Unknown {
            opcode: OPCODE_LD_UAV_RAW,
        });
    }
    Ok(Sm4Inst::LdUavRaw { dst, addr, uav })
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

fn decode_ld_structured(
    saturate: bool,
    r: &mut InstrReader<'_>,
) -> Result<Sm4Inst, Sm4DecodeError> {
    let mut dst = decode_dst(r)?;
    dst.saturate = saturate;
    let index = decode_src(r)?;
    let offset = decode_src(r)?;
    let buf_op = decode_raw_operand(r)?;
    if buf_op.imm32.is_some() {
        return Err(Sm4DecodeError {
            at_dword: r.base_at + r.pos.saturating_sub(1),
            kind: Sm4DecodeErrorKind::UnsupportedOperand("buffer operand cannot be immediate"),
        });
    }

    let slot = one_index(buf_op.ty, &buf_op.indices, r.base_at)?;
    let inst = match buf_op.ty {
        OPERAND_TYPE_RESOURCE => Sm4Inst::LdStructured {
            dst,
            index,
            offset,
            buffer: BufferRef { slot },
        },
        OPERAND_TYPE_UNORDERED_ACCESS_VIEW => Sm4Inst::LdStructuredUav {
            dst,
            index,
            offset,
            uav: UavRef { slot },
        },
        _ => {
            return Err(Sm4DecodeError {
                at_dword: r.base_at + r.pos.saturating_sub(1),
                kind: Sm4DecodeErrorKind::UnsupportedOperandType { ty: buf_op.ty },
            })
        }
    };

    if !r.is_eof() {
        return Ok(Sm4Inst::Unknown {
            opcode: OPCODE_LD_STRUCTURED,
        });
    }
    Ok(inst)
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

#[doc(hidden)]
pub fn decode_decl(opcode: u32, inst_toks: &[u32], at: usize) -> Result<Sm4Decl, Sm4DecodeError> {
    let mut r = InstrReader::new(inst_toks, at);
    let opcode_token = r.read_u32()?;
    // Declarations can also have extended opcode tokens; consume them even if we don't
    // understand the contents.
    let _ = decode_extended_opcode_modifiers(&mut r, opcode_token)?;

    if opcode == OPCODE_DCL_THREAD_GROUP {
        // dcl_thread_group x, y, z
        let x = r.read_u32()?;
        let y = r.read_u32()?;
        let z = r.read_u32()?;
        r.expect_eof()?;
        return Ok(Sm4Decl::ThreadGroupSize { x, y, z });
    }
    fn decode_hs_domain(raw: u32) -> Option<HsDomain> {
        // Values from `d3d11tokenizedprogramformat.h` (`D3D11_SB_TESSELLATOR_DOMAIN`).
        match raw {
            1 => Some(HsDomain::Isoline),
            2 => Some(HsDomain::Tri),
            3 => Some(HsDomain::Quad),
            _ => None,
        }
    }

    fn decode_hs_partitioning(raw: u32) -> Option<HsPartitioning> {
        // Values from `d3d11tokenizedprogramformat.h` (`D3D11_SB_TESSELLATOR_PARTITIONING`).
        match raw {
            1 => Some(HsPartitioning::Integer),
            2 => Some(HsPartitioning::Pow2),
            3 => Some(HsPartitioning::FractionalOdd),
            4 => Some(HsPartitioning::FractionalEven),
            _ => None,
        }
    }

    fn decode_hs_output_topology(raw: u32) -> Option<HsOutputTopology> {
        // Values from `d3d11tokenizedprogramformat.h` (`D3D11_SB_TESSELLATOR_OUTPUT_PRIMITIVE`).
        match raw {
            1 => Some(HsOutputTopology::Point),
            2 => Some(HsOutputTopology::Line),
            3 => Some(HsOutputTopology::TriangleCw),
            4 => Some(HsOutputTopology::TriangleCcw),
            _ => None,
        }
    }

    // Geometry + tessellation metadata declarations do not use an operand token; they carry
    // a small immediate payload (or no payload) instead.
    match opcode {
        OPCODE_DCL_INPUT_CONTROL_POINT_COUNT => {
            if r.is_eof() {
                return Ok(Sm4Decl::Unknown { opcode });
            }
            let count = r.read_u32()?;
            // Some toolchains appear to emit extra DWORDs after the control-point count (e.g.
            // extended metadata). We only need the count for executor validation, so decode it
            // even if trailing tokens are present.
            return Ok(Sm4Decl::InputControlPointCount { count });
        }
        OPCODE_DCL_GS_INPUT_PRIMITIVE => {
            if r.is_eof() {
                return Ok(Sm4Decl::Unknown { opcode });
            }
            let primitive = GsInputPrimitive::from_token(r.read_u32()?);
            r.expect_eof()?;
            return Ok(Sm4Decl::GsInputPrimitive { primitive });
        }
        OPCODE_DCL_GS_OUTPUT_TOPOLOGY => {
            if r.is_eof() {
                return Ok(Sm4Decl::Unknown { opcode });
            }
            let topology = GsOutputTopology::from_token(r.read_u32()?);
            r.expect_eof()?;
            return Ok(Sm4Decl::GsOutputTopology { topology });
        }
        OPCODE_DCL_GS_MAX_OUTPUT_VERTEX_COUNT => {
            if r.is_eof() {
                return Ok(Sm4Decl::Unknown { opcode });
            }
            let max = r.read_u32()?;
            r.expect_eof()?;
            return Ok(Sm4Decl::GsMaxOutputVertexCount { max });
        }
        OPCODE_DCL_GS_INSTANCE_COUNT => {
            if r.is_eof() {
                return Ok(Sm4Decl::Unknown { opcode });
            }
            let count = r.read_u32()?;
            r.expect_eof()?;
            return Ok(Sm4Decl::GsInstanceCount { count });
        }
        OPCODE_DCL_HS_DOMAIN => {
            if r.is_eof() {
                return Ok(Sm4Decl::Unknown { opcode });
            }
            let raw = r.read_u32()?;
            if !r.is_eof() {
                return Ok(Sm4Decl::Unknown { opcode });
            }
            let Some(domain) = decode_hs_domain(raw) else {
                return Ok(Sm4Decl::Unknown { opcode });
            };
            return Ok(Sm4Decl::HsDomain { domain });
        }
        OPCODE_DCL_HS_PARTITIONING => {
            if r.is_eof() {
                return Ok(Sm4Decl::Unknown { opcode });
            }
            let raw = r.read_u32()?;
            if !r.is_eof() {
                return Ok(Sm4Decl::Unknown { opcode });
            }
            let Some(partitioning) = decode_hs_partitioning(raw) else {
                return Ok(Sm4Decl::Unknown { opcode });
            };
            return Ok(Sm4Decl::HsPartitioning { partitioning });
        }
        OPCODE_DCL_HS_OUTPUT_TOPOLOGY => {
            if r.is_eof() {
                return Ok(Sm4Decl::Unknown { opcode });
            }
            let raw = r.read_u32()?;
            if !r.is_eof() {
                return Ok(Sm4Decl::Unknown { opcode });
            }
            let Some(topology) = decode_hs_output_topology(raw) else {
                return Ok(Sm4Decl::Unknown { opcode });
            };
            return Ok(Sm4Decl::HsOutputTopology { topology });
        }
        OPCODE_DCL_HS_OUTPUT_CONTROL_POINT_COUNT => {
            if r.is_eof() {
                return Ok(Sm4Decl::Unknown { opcode });
            }
            let count = r.read_u32()?;
            if !r.is_eof() {
                return Ok(Sm4Decl::Unknown { opcode });
            }
            return Ok(Sm4Decl::HsOutputControlPointCount { count });
        }
        OPCODE_DCL_HS_MAX_TESSFACTOR => {
            if r.is_eof() {
                return Ok(Sm4Decl::Unknown { opcode });
            }
            let factor = r.read_u32()?;
            if !r.is_eof() {
                return Ok(Sm4Decl::Unknown { opcode });
            }
            return Ok(Sm4Decl::HsMaxTessFactor { factor });
        }
        OPCODE_DCL_DS_DOMAIN => {
            if r.is_eof() {
                return Ok(Sm4Decl::Unknown { opcode });
            }
            let raw = r.read_u32()?;
            if !r.is_eof() {
                return Ok(Sm4Decl::Unknown { opcode });
            }
            let Some(domain) = decode_hs_domain(raw) else {
                return Ok(Sm4Decl::Unknown { opcode });
            };
            return Ok(Sm4Decl::DsDomain { domain });
        }
        _ => {}
    }

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
                    let dim = if r.is_eof() {
                        None
                    } else {
                        Some(r.read_u32()?)
                    };
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
                _ => {
                    // Typed UAV declarations encode their dimensionality in an extra token, similar
                    // to typed resources. We only model `RWTexture2D` today.
                    let dim = if r.is_eof() {
                        None
                    } else {
                        Some(r.read_u32()?)
                    };
                    if dim == Some(2) && inst_toks.len() >= 3 {
                        // Preserve the raw DXGI_FORMAT so later stages can report actionable
                        // errors for unsupported formats.
                        let format = *inst_toks.last().expect("len>=3");
                        return Ok(Sm4Decl::UavTyped2D { slot, format });
                    }
                }
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

fn try_decode_bufinfo_like(
    saturate: bool,
    inst_toks: &[u32],
    at: usize,
) -> Result<Option<Sm4Inst>, Sm4DecodeError> {
    let mut r = InstrReader::new(inst_toks, at);
    let opcode_token = r.read_u32()?;
    let _ = decode_extended_opcode_modifiers(&mut r, opcode_token)?;
    match decode_bufinfo(saturate, &mut r) {
        Ok(v) => Ok(Some(v)),
        Err(_) => Ok(None),
    }
}

fn try_decode_ld_uav_raw_like(
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
    let addr = match decode_src(&mut r) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    // Raw buffer loads take a scalar byte address; typed/structured UAV loads use vector coords.
    if !addr.swizzle.0.iter().all(|&c| c == addr.swizzle.0[0]) {
        return Ok(None);
    }
    let (uav, _mask) = match decode_uav_ref(&mut r) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    if r.is_eof() {
        return Ok(Some(Sm4Inst::LdUavRaw { dst, addr, uav }));
    }

    Ok(None)
}

fn try_decode_ld_structured_like(
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

    let index = match decode_src(&mut r) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    let offset = match decode_src(&mut r) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };

    // Structured loads use scalar element index + byte offset; typed loads use vector coords.
    if !index.swizzle.0.iter().all(|&c| c == index.swizzle.0[0]) {
        return Ok(None);
    }
    if !offset.swizzle.0.iter().all(|&c| c == offset.swizzle.0[0]) {
        return Ok(None);
    }

    let buf_op = match decode_raw_operand(&mut r) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    if buf_op.imm32.is_some() {
        return Ok(None);
    }
    let slot = match one_index(buf_op.ty, &buf_op.indices, r.base_at) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };

    if !r.is_eof() {
        return Ok(None);
    }

    match buf_op.ty {
        OPERAND_TYPE_RESOURCE => Ok(Some(Sm4Inst::LdStructured {
            dst,
            index,
            offset,
            buffer: BufferRef { slot },
        })),
        OPERAND_TYPE_UNORDERED_ACCESS_VIEW => Ok(Some(Sm4Inst::LdStructuredUav {
            dst,
            index,
            offset,
            uav: UavRef { slot },
        })),
        _ => Ok(None),
    }
}

fn try_decode_atomic_add_like(
    _saturate: bool,
    inst_toks: &[u32],
    at: usize,
) -> Result<Option<Sm4Inst>, Sm4DecodeError> {
    let mut r = InstrReader::new(inst_toks, at);
    let opcode_token = r.read_u32()?;
    let _ = decode_extended_opcode_modifiers(&mut r, opcode_token)?;

    let dst = match decode_atomic_dst(&mut r) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    let (uav, _mask) = match decode_uav_ref(&mut r) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    let addr = match decode_src(&mut r) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    let value = match decode_src(&mut r) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };

    if r.is_eof() {
        return Ok(Some(Sm4Inst::AtomicAdd {
            dst,
            uav,
            addr,
            value,
        }));
    }

    Ok(None)
}

fn try_decode_store_raw_like(
    inst_toks: &[u32],
    at: usize,
) -> Result<Option<Sm4Inst>, Sm4DecodeError> {
    let mut r = InstrReader::new(inst_toks, at);
    let opcode_token = r.read_u32()?;
    let _ = decode_extended_opcode_modifiers(&mut r, opcode_token)?;

    let (uav, mask) = match decode_uav_ref(&mut r) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    let addr = match decode_src(&mut r) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    // Raw UAV stores take a scalar byte address; typed UAV stores use vector coords.
    //
    // Note: typed stores can still legally supply replicated coordinates, so this is a heuristic
    // that trades off coverage for avoiding misclassification in the absence of declarations.
    if !addr.swizzle.0.iter().all(|&c| c == addr.swizzle.0[0]) {
        return Ok(None);
    }
    let value = match decode_src(&mut r) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };

    if r.is_eof() {
        return Ok(Some(Sm4Inst::StoreRaw {
            uav,
            addr,
            value,
            mask,
        }));
    }

    Ok(None)
}

fn try_decode_store_structured_like(
    inst_toks: &[u32],
    at: usize,
) -> Result<Option<Sm4Inst>, Sm4DecodeError> {
    let mut r = InstrReader::new(inst_toks, at);
    let opcode_token = r.read_u32()?;
    let _ = decode_extended_opcode_modifiers(&mut r, opcode_token)?;

    let (uav, mask) = match decode_uav_ref(&mut r) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    let index = match decode_src(&mut r) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    let offset = match decode_src(&mut r) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    let value = match decode_src(&mut r) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };

    if r.is_eof() {
        return Ok(Some(Sm4Inst::StoreStructured {
            uav,
            index,
            offset,
            value,
            mask,
        }));
    }

    Ok(None)
}

fn try_decode_store_uav_typed_like(
    inst_toks: &[u32],
    at: usize,
) -> Result<Option<Sm4Inst>, Sm4DecodeError> {
    let mut r = InstrReader::new(inst_toks, at);
    let opcode_token = r.read_u32()?;
    let _ = decode_extended_opcode_modifiers(&mut r, opcode_token)?;

    let uav_op = match decode_raw_operand(&mut r) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    if uav_op.imm32.is_some() {
        return Ok(None);
    }
    if uav_op.ty != OPERAND_TYPE_UNORDERED_ACCESS_VIEW {
        return Ok(None);
    }
    let slot = match one_index(uav_op.ty, &uav_op.indices, r.base_at) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };

    let mask = match uav_op.selection_mode {
        OPERAND_SEL_MASK => WriteMask((uav_op.component_sel & 0xF) as u8),
        _ => WriteMask::XYZW,
    };

    let coord = match decode_src(&mut r) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    let value = match decode_src(&mut r) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };

    if !r.is_eof() {
        return Ok(None);
    }

    Ok(Some(Sm4Inst::StoreUavTyped {
        uav: UavRef { slot },
        coord,
        value,
        mask,
    }))
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
        OPERAND_TYPE_OUTPUT_DEPTH
        | OPERAND_TYPE_OUTPUT_DEPTH_GREATER_EQUAL
        | OPERAND_TYPE_OUTPUT_DEPTH_LESS_EQUAL => {
            // `oDepth`/`oDepthGE`/`oDepthLE` operands do not necessarily encode an `o#` index; the
            // signature-driven WGSL backend maps them to the correct output register. Preserve
            // them as a dedicated register file in the IR.
            let index = match op.indices.as_slice() {
                [] => 0,
                [idx] => *idx,
                _ => {
                    return Err(Sm4DecodeError {
                        at_dword: r.base_at + r.pos.saturating_sub(1),
                        kind: Sm4DecodeErrorKind::InvalidRegisterIndices {
                            ty: op.ty,
                            indices: op.indices,
                        },
                    })
                }
            };
            (RegFile::OutputDepth, index)
        }
        other => {
            return Err(Sm4DecodeError {
                at_dword: r.base_at + r.pos.saturating_sub(1),
                kind: Sm4DecodeErrorKind::UnsupportedOperandType { ty: other },
            })
        }
    };

    let mask = match op.selection_mode {
        OPERAND_SEL_MASK => WriteMask((op.component_sel & 0xF) as u8),
        OPERAND_SEL_SELECT1 => WriteMask(1u8 << ((op.component_sel & 0x3) as u8)),
        _ => WriteMask::XYZW,
    };

    Ok(DstOperand {
        reg: RegisterRef { file, index },
        mask,
        saturate: false,
    })
}

fn decode_predicate_operand(r: &mut InstrReader<'_>) -> Result<PredicateOperand, Sm4DecodeError> {
    let at = r.base_at + r.pos;
    let op = decode_raw_operand(r)?;
    predicate_operand_from_raw(op, at)
}

fn decode_predicate_dst(r: &mut InstrReader<'_>) -> Result<PredicateDstOperand, Sm4DecodeError> {
    let at = r.base_at + r.pos;
    let op = decode_raw_operand(r)?;
    predicate_dst_from_raw(op, at)
}

fn predicate_operand_from_raw(
    op: RawOperand,
    at_dword: usize,
) -> Result<PredicateOperand, Sm4DecodeError> {
    if op.imm32.is_some() {
        return Err(Sm4DecodeError {
            at_dword,
            kind: Sm4DecodeErrorKind::UnsupportedOperand("predicate operand cannot be immediate"),
        });
    }
    if op.ty != OPERAND_TYPE_PREDICATE {
        return Err(Sm4DecodeError {
            at_dword,
            kind: Sm4DecodeErrorKind::UnsupportedOperandType { ty: op.ty },
        });
    }

    let index = one_index(op.ty, &op.indices, at_dword)?;

    // Resolve which component the predication operand selects.
    let component = match op.selection_mode {
        OPERAND_SEL_SELECT1 => (op.component_sel & 0x3) as u8,
        OPERAND_SEL_SWIZZLE => {
            let swz = decode_swizzle(op.component_sel).0;
            if swz.iter().all(|&c| c == swz[0]) {
                swz[0]
            } else {
                return Err(Sm4DecodeError {
                    at_dword,
                    kind: Sm4DecodeErrorKind::UnsupportedOperand(
                        "predicate operand must be scalar (replicated swizzle)",
                    ),
                });
            }
        }
        OPERAND_SEL_MASK => {
            let bits = (op.component_sel & 0xF) as u8;
            match bits {
                0b0001 => 0,
                0b0010 => 1,
                0b0100 => 2,
                0b1000 => 3,
                _ => {
                    return Err(Sm4DecodeError {
                        at_dword,
                        kind: Sm4DecodeErrorKind::UnsupportedOperand(
                            "predicate operand must select exactly one component",
                        ),
                    })
                }
            }
        }
        _ => {
            return Err(Sm4DecodeError {
                at_dword,
                kind: Sm4DecodeErrorKind::UnsupportedOperand(
                    "unknown component selection mode for predicate operand",
                ),
            })
        }
    };

    let invert = matches!(op.modifier, OperandModifier::Neg | OperandModifier::AbsNeg);

    Ok(PredicateOperand {
        reg: PredicateRef { index },
        component,
        invert,
    })
}

fn predicate_dst_from_raw(
    op: RawOperand,
    at_dword: usize,
) -> Result<PredicateDstOperand, Sm4DecodeError> {
    if op.imm32.is_some() {
        return Err(Sm4DecodeError {
            at_dword,
            kind: Sm4DecodeErrorKind::UnsupportedOperand(
                "predicate destination cannot be immediate",
            ),
        });
    }
    if op.ty != OPERAND_TYPE_PREDICATE {
        return Err(Sm4DecodeError {
            at_dword,
            kind: Sm4DecodeErrorKind::UnsupportedOperandType { ty: op.ty },
        });
    }

    let index = one_index(op.ty, &op.indices, at_dword)?;

    let mask = match op.selection_mode {
        OPERAND_SEL_MASK => WriteMask((op.component_sel & 0xF) as u8),
        OPERAND_SEL_SELECT1 => {
            let c = (op.component_sel & 0x3) as u8;
            WriteMask(1 << c)
        }
        _ => WriteMask::XYZW,
    };

    Ok(PredicateDstOperand {
        reg: PredicateRef { index },
        mask,
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
            OPERAND_TYPE_INPUT => match op.indices.as_slice() {
                [idx] => SrcKind::Register(RegisterRef {
                    file: RegFile::Input,
                    index: *idx,
                }),
                // Geometry shader per-vertex input (`v{reg}[{vertex}]`), encoded as a 2D-indexed
                // input operand.
                [reg, vertex] => SrcKind::GsInput {
                    reg: *reg,
                    vertex: *vertex,
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
            OPERAND_TYPE_OUTPUT => SrcKind::Register(RegisterRef {
                file: RegFile::Output,
                index: one_index(op.ty, &op.indices, r.base_at)?,
            }),
            OPERAND_TYPE_OUTPUT_DEPTH
            | OPERAND_TYPE_OUTPUT_DEPTH_GREATER_EQUAL
            | OPERAND_TYPE_OUTPUT_DEPTH_LESS_EQUAL => {
                let index = match op.indices.as_slice() {
                    [] => 0,
                    [idx] => *idx,
                    _ => {
                        return Err(Sm4DecodeError {
                            at_dword: r.base_at + r.pos.saturating_sub(1),
                            kind: Sm4DecodeErrorKind::InvalidRegisterIndices {
                                ty: op.ty,
                                indices: op.indices,
                            },
                        })
                    }
                };
                SrcKind::Register(RegisterRef {
                    file: RegFile::OutputDepth,
                    index,
                })
            }
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
            OPERAND_TYPE_INPUT_THREAD_ID => {
                if !op.indices.is_empty() {
                    return Err(Sm4DecodeError {
                        at_dword: r.base_at + r.pos.saturating_sub(1),
                        kind: Sm4DecodeErrorKind::InvalidRegisterIndices {
                            ty: op.ty,
                            indices: op.indices,
                        },
                    });
                }
                SrcKind::ComputeBuiltin(ComputeBuiltin::DispatchThreadId)
            }
            OPERAND_TYPE_INPUT_THREAD_GROUP_ID => {
                if !op.indices.is_empty() {
                    return Err(Sm4DecodeError {
                        at_dword: r.base_at + r.pos.saturating_sub(1),
                        kind: Sm4DecodeErrorKind::InvalidRegisterIndices {
                            ty: op.ty,
                            indices: op.indices,
                        },
                    });
                }
                SrcKind::ComputeBuiltin(ComputeBuiltin::GroupId)
            }
            OPERAND_TYPE_INPUT_THREAD_ID_IN_GROUP => {
                if !op.indices.is_empty() {
                    return Err(Sm4DecodeError {
                        at_dword: r.base_at + r.pos.saturating_sub(1),
                        kind: Sm4DecodeErrorKind::InvalidRegisterIndices {
                            ty: op.ty,
                            indices: op.indices,
                        },
                    });
                }
                SrcKind::ComputeBuiltin(ComputeBuiltin::GroupThreadId)
            }
            OPERAND_TYPE_INPUT_THREAD_ID_IN_GROUP_FLATTENED => {
                if !op.indices.is_empty() {
                    return Err(Sm4DecodeError {
                        at_dword: r.base_at + r.pos.saturating_sub(1),
                        kind: Sm4DecodeErrorKind::InvalidRegisterIndices {
                            ty: op.ty,
                            indices: op.indices,
                        },
                    });
                }
                SrcKind::ComputeBuiltin(ComputeBuiltin::GroupIndex)
            }
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

fn decode_atomic_dst(r: &mut InstrReader<'_>) -> Result<Option<DstOperand>, Sm4DecodeError> {
    let op = decode_raw_operand(r)?;
    if op.imm32.is_some() {
        return Err(Sm4DecodeError {
            at_dword: r.base_at + r.pos.saturating_sub(1),
            kind: Sm4DecodeErrorKind::UnsupportedOperand("destination cannot be immediate"),
        });
    }

    if op.ty == OPERAND_TYPE_NULL {
        return Ok(None);
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

    Ok(Some(DstOperand {
        reg: RegisterRef { file, index },
        mask,
        saturate: false,
    }))
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

impl<'a> Clone for InstrReader<'a> {
    fn clone(&self) -> Self {
        Self {
            toks: self.toks,
            pos: self.pos,
            base_at: self.base_at,
        }
    }
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

    fn peek_u32(&self) -> Option<u32> {
        self.toks.get(self.pos).copied()
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
