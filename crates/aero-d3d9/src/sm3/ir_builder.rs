use crate::sm3::decode::{
    DecodedInstruction, DecodedShader, DclInfo, DclUsage, Operand, Opcode, RegisterFile, ResultShift, SrcModifier,
};
use crate::sm3::ir::{
    Block, CompareOp, Cond, ConstDefF32, Dst, InstModifiers, IoDecl, IrOp, PredicateRef, RegFile, RegRef, RelativeRef, SamplerDecl,
    Semantic, ShaderIr, Src, Stmt, TexSampleKind,
};
use crate::sm3::types::ShaderStage;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildError {
    pub location: crate::sm3::decode::InstructionLocation,
    pub opcode: Opcode,
    pub message: String,
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "IR build error at instruction {} (token {}), opcode {}: {}",
            self.location.instruction_index,
            self.location.token_index,
            self.opcode.name(),
            self.message
        )
    }
}

impl std::error::Error for BuildError {}

pub fn build_ir(shader: &DecodedShader) -> Result<ShaderIr, BuildError> {
    let version = shader.version;

    let mut inputs = Vec::new();
    let mut outputs = Vec::new();
    let mut samplers = Vec::new();
    let mut const_defs_f32 = Vec::new();

    // Pass 1: declarations and constant defs.
    for inst in &shader.instructions {
        match inst.opcode {
            Opcode::Dcl => {
                if let Some(dcl) = &inst.dcl {
                    handle_dcl(inst, &version, dcl, &mut inputs, &mut outputs, &mut samplers)?;
                }
            }
            Opcode::Def => {
                handle_def_f32(inst, &mut const_defs_f32)?;
            }
            _ => {}
        }
    }

    // Pass 2: structured control-flow + ops.
    let mut stack: Vec<Frame> = vec![Frame::Root(Block::new())];

    for inst in &shader.instructions {
        match inst.opcode {
            Opcode::Comment | Opcode::Dcl | Opcode::Def | Opcode::DefI | Opcode::DefB => continue,
            Opcode::End => break,
            Opcode::Nop => {}
            Opcode::If => stack.push(Frame::If {
                cond: build_if_cond(inst)?,
                then_block: Block::new(),
                else_block: None,
                in_else: false,
            }),
            Opcode::Ifc => stack.push(Frame::If {
                cond: build_ifc_cond(inst)?,
                then_block: Block::new(),
                else_block: None,
                in_else: false,
            }),
            Opcode::Else => match stack.last_mut() {
                Some(Frame::If {
                    else_block,
                    in_else,
                    ..
                }) => {
                    *in_else = true;
                    if else_block.is_none() {
                        *else_block = Some(Block::new());
                    }
                }
                _ => return Err(err(inst, "else without matching if")),
            },
            Opcode::EndIf => {
                let frame = stack.pop().ok_or_else(|| err(inst, "endif without matching if"))?;
                let (cond, then_block, else_block) = match frame {
                    Frame::If {
                        cond,
                        then_block,
                        else_block,
                        ..
                    } => (cond, then_block, else_block),
                    _ => return Err(err(inst, "endif without matching if")),
                };
                push_stmt(&mut stack, Stmt::If {
                    cond,
                    then_block,
                    else_block,
                })?;
            }
            Opcode::Loop => {
                stack.push(Frame::Loop { body: Block::new() });
            }
            Opcode::EndLoop => {
                let frame = stack.pop().ok_or_else(|| err(inst, "endloop without matching loop"))?;
                let body = match frame {
                    Frame::Loop { body } => body,
                    _ => return Err(err(inst, "endloop without matching loop")),
                };
                push_stmt(&mut stack, Stmt::Loop { body })?;
            }
            Opcode::Break => push_stmt(&mut stack, Stmt::Break)?,
            Opcode::Breakc => {
                let cond = build_cmp_cond(inst)?;
                push_stmt(&mut stack, Stmt::BreakIf { cond })?;
            }
            Opcode::TexKill => {
                let src = extract_src(inst, 0)?;
                push_stmt(&mut stack, Stmt::Discard { src })?;
            }

            Opcode::Mov => {
                let dst = extract_dst(inst, 0)?;
                let src = extract_src(inst, 1)?;
                let modifiers = build_modifiers(inst)?;
                push_stmt(&mut stack, Stmt::Op(IrOp::Mov { dst, src, modifiers }))?;
            }
            Opcode::Add => push_binop(&mut stack, inst, |dst, src0, src1, modifiers| IrOp::Add {
                dst,
                src0,
                src1,
                modifiers,
            })?,
            Opcode::Sub => push_binop(&mut stack, inst, |dst, src0, src1, modifiers| IrOp::Sub {
                dst,
                src0,
                src1,
                modifiers,
            })?,
            Opcode::Mul => push_binop(&mut stack, inst, |dst, src0, src1, modifiers| IrOp::Mul {
                dst,
                src0,
                src1,
                modifiers,
            })?,
            Opcode::Mad => {
                let dst = extract_dst(inst, 0)?;
                let src0 = extract_src(inst, 1)?;
                let src1 = extract_src(inst, 2)?;
                let src2 = extract_src(inst, 3)?;
                let modifiers = build_modifiers(inst)?;
                push_stmt(
                    &mut stack,
                    Stmt::Op(IrOp::Mad {
                        dst,
                        src0,
                        src1,
                        src2,
                        modifiers,
                    }),
                )?;
            }
            Opcode::Dp3 => push_binop(&mut stack, inst, |dst, src0, src1, modifiers| IrOp::Dp3 {
                dst,
                src0,
                src1,
                modifiers,
            })?,
            Opcode::Dp4 => push_binop(&mut stack, inst, |dst, src0, src1, modifiers| IrOp::Dp4 {
                dst,
                src0,
                src1,
                modifiers,
            })?,
            Opcode::Rcp => push_unop(&mut stack, inst, |dst, src, modifiers| IrOp::Rcp {
                dst,
                src,
                modifiers,
            })?,
            Opcode::Rsq => push_unop(&mut stack, inst, |dst, src, modifiers| IrOp::Rsq {
                dst,
                src,
                modifiers,
            })?,
            Opcode::Min => push_binop(&mut stack, inst, |dst, src0, src1, modifiers| IrOp::Min {
                dst,
                src0,
                src1,
                modifiers,
            })?,
            Opcode::Max => push_binop(&mut stack, inst, |dst, src0, src1, modifiers| IrOp::Max {
                dst,
                src0,
                src1,
                modifiers,
            })?,

            Opcode::Sge => push_cmpop(&mut stack, inst, CompareOp::Ge)?,
            Opcode::Slt => push_cmpop(&mut stack, inst, CompareOp::Lt)?,
            Opcode::Seq => push_cmpop(&mut stack, inst, CompareOp::Eq)?,
            Opcode::Sne => push_cmpop(&mut stack, inst, CompareOp::Ne)?,

            Opcode::Setp => push_setp(&mut stack, inst)?,

            Opcode::Tex => push_texld(&mut stack, inst)?,
            Opcode::TexLdl => push_texldl(&mut stack, inst)?,
            Opcode::TexLdd => push_texldd(&mut stack, inst)?,

            Opcode::Call | Opcode::Ret => return Err(err(inst, "call/ret not supported")),

            Opcode::Unknown(op) => {
                return Err(err(
                    inst,
                    &format!("unsupported opcode 0x{op:04x}"),
                ))
            }
        }
    }

    let body = match stack.pop() {
        Some(Frame::Root(block)) if stack.is_empty() => block,
        _ => {
            return Err(BuildError {
                location: crate::sm3::decode::InstructionLocation {
                    instruction_index: 0,
                    token_index: 0,
                },
                opcode: Opcode::Unknown(0),
                message: "unbalanced control-flow stack".to_owned(),
            })
        }
    };

    Ok(ShaderIr {
        version,
        inputs,
        outputs,
        samplers,
        const_defs_f32,
        body,
    })
}

fn handle_dcl(
    inst: &DecodedInstruction,
    version: &crate::sm3::types::ShaderVersion,
    dcl: &DclInfo,
    inputs: &mut Vec<IoDecl>,
    outputs: &mut Vec<IoDecl>,
    samplers: &mut Vec<SamplerDecl>,
) -> Result<(), BuildError> {
    let dst = match inst.operands.get(0) {
        Some(Operand::Dst(dst)) => dst,
        _ => return Err(err(inst, "dcl missing destination operand")),
    };

    let reg = to_ir_reg(inst, &dst.reg)?;
    let mask = dst.mask;

    match dcl.usage {
        DclUsage::TextureType(texture_type) => {
            if reg.file != RegFile::Sampler {
                return Err(err(inst, "sampler dcl applied to non-sampler register"));
            }
            samplers.push(SamplerDecl {
                index: reg.index,
                texture_type,
            });
        }
        _ => {
            let semantic = map_semantic(&dcl.usage, dcl.usage_index, version.stage);
            match reg.file {
                RegFile::Input | RegFile::Texture => inputs.push(IoDecl { reg, semantic, mask }),
                RegFile::RastOut
                | RegFile::AttrOut
                | RegFile::TexCoordOut
                | RegFile::Output
                | RegFile::ColorOut
                | RegFile::DepthOut => outputs.push(IoDecl { reg, semantic, mask }),
                _ => {
                    // Some decls apply to special register files; ignore them for now.
                }
            }
        }
    }
    Ok(())
}

fn handle_def_f32(inst: &DecodedInstruction, out: &mut Vec<ConstDefF32>) -> Result<(), BuildError> {
    let dst = match inst.operands.get(0) {
        Some(Operand::Dst(dst)) => dst,
        _ => return Err(err(inst, "def missing destination operand")),
    };
    if dst.reg.file != RegisterFile::Const {
        return Err(err(inst, "def destination is not a float constant register"));
    }

    let mut vals = [0f32; 4];
    for i in 0..4 {
        let token = match inst.operands.get(1 + i) {
            Some(Operand::Imm32(v)) => *v,
            _ => return Err(err(inst, "def missing immediate constant tokens")),
        };
        vals[i] = f32::from_bits(token);
    }

    out.push(ConstDefF32 {
        index: dst.reg.index,
        value: vals,
    });
    Ok(())
}

fn map_semantic(usage: &DclUsage, index: u8, stage: ShaderStage) -> Semantic {
    match usage {
        DclUsage::Position => Semantic::Position(index),
        DclUsage::Color => Semantic::Color(index),
        DclUsage::TexCoord => Semantic::TexCoord(index),
        DclUsage::Normal => Semantic::Normal(index),
        DclUsage::Fog => Semantic::Fog(index),
        DclUsage::PointSize => Semantic::PointSize(index),
        DclUsage::Depth => Semantic::Depth(index),
        DclUsage::Unknown(u) => Semantic::Other { usage: *u, index },
        DclUsage::TextureType(_) => Semantic::Other { usage: 0xFE, index },
        _ => {
            let fallback_usage = match stage {
                ShaderStage::Vertex => 0xFF,
                ShaderStage::Pixel => 0xFE,
            };
            Semantic::Other {
                usage: fallback_usage,
                index,
            }
        }
    }
}

fn build_modifiers(inst: &DecodedInstruction) -> Result<InstModifiers, BuildError> {
    if matches!(inst.result_modifier.shift, ResultShift::Unknown(_)) {
        return Err(err(inst, "unknown result shift modifier"));
    }

    let predicate = match &inst.predicate {
        Some(pred) => Some(PredicateRef {
            reg: to_ir_reg(inst, &pred.reg)?,
            component: pred.component,
            negate: pred.negate,
        }),
        None => None,
    };

    Ok(InstModifiers {
        saturate: inst.result_modifier.saturate,
        shift: inst.result_modifier.shift,
        coissue: inst.coissue,
        predicate,
    })
}

fn extract_dst(inst: &DecodedInstruction, idx: usize) -> Result<Dst, BuildError> {
    let dst = match inst.operands.get(idx) {
        Some(Operand::Dst(dst)) => dst,
        _ => return Err(err(inst, &format!("missing dst operand {idx}"))),
    };
    Ok(Dst {
        reg: to_ir_reg(inst, &dst.reg)?,
        mask: dst.mask,
    })
}

fn extract_src(inst: &DecodedInstruction, idx: usize) -> Result<Src, BuildError> {
    let src = match inst.operands.get(idx) {
        Some(Operand::Src(src)) => src,
        _ => return Err(err(inst, &format!("missing src operand {idx}"))),
    };
    if matches!(src.modifier, SrcModifier::Unknown(_)) {
        return Err(err(inst, "unsupported source modifier"));
    }
    Ok(Src {
        reg: to_ir_reg(inst, &src.reg)?,
        swizzle: src.swizzle,
        modifier: src.modifier,
    })
}

fn build_if_cond(inst: &DecodedInstruction) -> Result<Cond, BuildError> {
    if let Some(pred) = &inst.predicate {
        return Ok(Cond::Predicate {
            pred: PredicateRef {
                reg: to_ir_reg(inst, &pred.reg)?,
                component: pred.component,
                negate: pred.negate,
            },
        });
    }
    Ok(Cond::NonZero {
        src: extract_src(inst, 0)?,
    })
}

fn build_ifc_cond(inst: &DecodedInstruction) -> Result<Cond, BuildError> {
    if let Some(pred) = &inst.predicate {
        return Ok(Cond::Predicate {
            pred: PredicateRef {
                reg: to_ir_reg(inst, &pred.reg)?,
                component: pred.component,
                negate: pred.negate,
            },
        });
    }
    build_cmp_cond(inst)
}

fn build_cmp_cond(inst: &DecodedInstruction) -> Result<Cond, BuildError> {
    let src0 = extract_src(inst, 0)?;
    let src1 = extract_src(inst, 1)?;
    let cmp_code = match inst.operands.get(2) {
        Some(Operand::Imm32(v)) => (*v & 0xFF) as u8,
        _ => return Err(err(inst, "missing comparison code immediate")),
    };
    Ok(Cond::Compare {
        op: decode_compare_op(cmp_code),
        src0,
        src1,
    })
}

fn decode_compare_op(code: u8) -> CompareOp {
    // D3D9 "comparison" encoding used by `ifc`, `breakc`, `setp`.
    match code {
        0 => CompareOp::Gt,
        1 => CompareOp::Eq,
        2 => CompareOp::Ge,
        3 => CompareOp::Lt,
        4 => CompareOp::Ne,
        5 => CompareOp::Le,
        other => CompareOp::Unknown(other),
    }
}

fn push_binop<F>(stack: &mut Vec<Frame>, inst: &DecodedInstruction, ctor: F) -> Result<(), BuildError>
where
    F: FnOnce(Dst, Src, Src, InstModifiers) -> IrOp,
{
    let dst = extract_dst(inst, 0)?;
    let src0 = extract_src(inst, 1)?;
    let src1 = extract_src(inst, 2)?;
    let modifiers = build_modifiers(inst)?;
    push_stmt(stack, Stmt::Op(ctor(dst, src0, src1, modifiers)))
}

fn push_unop<F>(stack: &mut Vec<Frame>, inst: &DecodedInstruction, ctor: F) -> Result<(), BuildError>
where
    F: FnOnce(Dst, Src, InstModifiers) -> IrOp,
{
    let dst = extract_dst(inst, 0)?;
    let src = extract_src(inst, 1)?;
    let modifiers = build_modifiers(inst)?;
    push_stmt(stack, Stmt::Op(ctor(dst, src, modifiers)))
}

fn push_cmpop(stack: &mut Vec<Frame>, inst: &DecodedInstruction, op: CompareOp) -> Result<(), BuildError> {
    let dst = extract_dst(inst, 0)?;
    let src0 = extract_src(inst, 1)?;
    let src1 = extract_src(inst, 2)?;
    let modifiers = build_modifiers(inst)?;
    push_stmt(
        stack,
        Stmt::Op(IrOp::Cmp {
            op,
            dst,
            src0,
            src1,
            modifiers,
        }),
    )
}

fn push_setp(stack: &mut Vec<Frame>, inst: &DecodedInstruction) -> Result<(), BuildError> {
    let dst = extract_dst(inst, 0)?;
    if dst.reg.file != RegFile::Predicate {
        return Err(err(inst, "setp destination must be a predicate register"));
    }
    let src0 = extract_src(inst, 1)?;
    let src1 = extract_src(inst, 2)?;
    let cmp_code = match inst.operands.get(3) {
        Some(Operand::Imm32(v)) => (*v & 0xFF) as u8,
        _ => return Err(err(inst, "setp missing comparison code")),
    };
    let op = decode_compare_op(cmp_code);
    let modifiers = build_modifiers(inst)?;
    push_stmt(
        stack,
        Stmt::Op(IrOp::Cmp {
            op,
            dst,
            src0,
            src1,
            modifiers,
        }),
    )
}

fn push_texld(stack: &mut Vec<Frame>, inst: &DecodedInstruction) -> Result<(), BuildError> {
    // `tex` (SM2/3) is `texld` / `texldp` depending on a flag we captured as an immediate.
    let dst = extract_dst(inst, 0)?;
    let coord = extract_src(inst, 1)?;
    let sampler_src = extract_src(inst, 2)?;
    if sampler_src.reg.file != RegFile::Sampler {
        return Err(err(inst, "tex sampler operand is not a sampler register"));
    }
    let project = match inst.operands.last() {
        Some(Operand::Imm32(v)) => (*v & 0x1) != 0,
        _ => false,
    };
    let modifiers = build_modifiers(inst)?;
    push_stmt(
        stack,
        Stmt::Op(IrOp::TexSample {
            kind: TexSampleKind::ImplicitLod { project },
            dst,
            coord,
            ddx: None,
            ddy: None,
            sampler: sampler_src.reg.index,
            modifiers,
        }),
    )
}

fn push_texldl(stack: &mut Vec<Frame>, inst: &DecodedInstruction) -> Result<(), BuildError> {
    let dst = extract_dst(inst, 0)?;
    let coord = extract_src(inst, 1)?;
    let sampler_src = extract_src(inst, 2)?;
    if sampler_src.reg.file != RegFile::Sampler {
        return Err(err(inst, "texldl sampler operand is not a sampler register"));
    }
    let modifiers = build_modifiers(inst)?;
    push_stmt(
        stack,
        Stmt::Op(IrOp::TexSample {
            kind: TexSampleKind::ExplicitLod,
            dst,
            coord,
            ddx: None,
            ddy: None,
            sampler: sampler_src.reg.index,
            modifiers,
        }),
    )
}

fn push_texldd(stack: &mut Vec<Frame>, inst: &DecodedInstruction) -> Result<(), BuildError> {
    let dst = extract_dst(inst, 0)?;
    let coord = extract_src(inst, 1)?;
    let ddx = extract_src(inst, 2)?;
    let ddy = extract_src(inst, 3)?;
    let sampler_src = extract_src(inst, 4)?;
    if sampler_src.reg.file != RegFile::Sampler {
        return Err(err(inst, "texldd sampler operand is not a sampler register"));
    }
    let modifiers = build_modifiers(inst)?;
    push_stmt(
        stack,
        Stmt::Op(IrOp::TexSample {
            kind: TexSampleKind::Grad,
            dst,
            coord,
            ddx: Some(ddx),
            ddy: Some(ddy),
            sampler: sampler_src.reg.index,
            modifiers,
        }),
    )
}

fn push_stmt(stack: &mut Vec<Frame>, stmt: Stmt) -> Result<(), BuildError> {
    match stack.last_mut() {
        Some(Frame::Root(block)) => block.stmts.push(stmt),
        Some(Frame::Loop { body }) => body.stmts.push(stmt),
        Some(Frame::If {
            then_block,
            else_block,
            in_else,
            ..
        }) => {
            if *in_else {
                else_block
                    .as_mut()
                    .ok_or_else(|| BuildError {
                        location: crate::sm3::decode::InstructionLocation {
                            instruction_index: 0,
                            token_index: 0,
                        },
                        opcode: Opcode::Unknown(0),
                        message: "internal error: missing else_block".to_owned(),
                    })?
                    .stmts
                    .push(stmt);
            } else {
                then_block.stmts.push(stmt);
            }
        }
        None => return Err(err_internal("empty block stack")),
    }
    Ok(())
}

fn to_ir_reg(inst: &DecodedInstruction, reg: &crate::sm3::decode::RegisterRef) -> Result<RegRef, BuildError> {
    let (file, index) = match reg.file {
        RegisterFile::Temp => (RegFile::Temp, reg.index),
        RegisterFile::Input => (RegFile::Input, reg.index),
        RegisterFile::Const => (RegFile::Const, reg.index),
        RegisterFile::Addr => (RegFile::Addr, reg.index),
        RegisterFile::Texture => (RegFile::Texture, reg.index),
        RegisterFile::Sampler => (RegFile::Sampler, reg.index),
        RegisterFile::Predicate => (RegFile::Predicate, reg.index),
        RegisterFile::RastOut => (RegFile::RastOut, reg.index),
        RegisterFile::AttrOut => (RegFile::AttrOut, reg.index),
        RegisterFile::TexCoordOut => (RegFile::TexCoordOut, reg.index),
        RegisterFile::Output => (RegFile::Output, reg.index),
        RegisterFile::ColorOut => (RegFile::ColorOut, reg.index),
        RegisterFile::DepthOut => (RegFile::DepthOut, reg.index),
        RegisterFile::ConstInt => (RegFile::ConstInt, reg.index),
        RegisterFile::ConstBool => (RegFile::ConstBool, reg.index),
        RegisterFile::Loop => (RegFile::Loop, reg.index),
        RegisterFile::Label => (RegFile::Label, reg.index),
        RegisterFile::MiscType => (RegFile::MiscType, reg.index),

        other => {
            return Err(BuildError {
                location: inst.location,
                opcode: inst.opcode,
                message: format!("unsupported register file {other:?}"),
            })
        }
    };

    let relative = match &reg.relative {
        Some(rel) => Some(Box::new(RelativeRef {
            reg: Box::new(to_ir_reg(inst, &rel.reg)?),
            component: rel.component,
        })),
        None => None,
    };

    Ok(RegRef {
        file,
        index,
        relative,
    })
}

fn err(inst: &DecodedInstruction, message: &str) -> BuildError {
    BuildError {
        location: inst.location,
        opcode: inst.opcode,
        message: message.to_owned(),
    }
}

fn err_internal(message: &str) -> BuildError {
    BuildError {
        location: crate::sm3::decode::InstructionLocation {
            instruction_index: 0,
            token_index: 0,
        },
        opcode: Opcode::Unknown(0),
        message: message.to_owned(),
    }
}

#[derive(Debug)]
enum Frame {
    Root(Block),
    If {
        cond: Cond,
        then_block: Block,
        else_block: Option<Block>,
        in_else: bool,
    },
    Loop {
        body: Block,
    },
}
