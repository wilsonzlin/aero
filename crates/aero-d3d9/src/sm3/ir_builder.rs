use crate::shader_limits::MAX_D3D9_SHADER_CONTROL_FLOW_NESTING;
use crate::sm3::decode::{
    DclInfo, DclUsage, DecodedInstruction, DecodedShader, Opcode, Operand, RegisterFile,
    ResultShift, SrcModifier,
};
use crate::sm3::ir::{
    Block, CompareOp, Cond, ConstDefBool, ConstDefF32, ConstDefI32, Dst, InstModifiers, IoDecl,
    IrOp, LoopInit, PredicateRef, RegFile, RegRef, RelativeRef, SamplerDecl, Semantic, ShaderIr,
    Src, Stmt, TexSampleKind,
};
use crate::sm3::types::ShaderStage;
use crate::vertex::{AdaptiveLocationMap, DeclUsage, VertexLocationMap};
use std::collections::{BTreeSet, HashMap};

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
    let mut const_defs_i32 = Vec::new();
    let mut const_defs_bool = Vec::new();

    // Pass 1: declarations and constant defs.
    for inst in &shader.instructions {
        match inst.opcode {
            Opcode::Dcl => {
                if let Some(dcl) = &inst.dcl {
                    handle_dcl(
                        inst,
                        &version,
                        dcl,
                        &mut inputs,
                        &mut outputs,
                        &mut samplers,
                    )?;
                }
            }
            Opcode::Def => {
                handle_def_f32(inst, &mut const_defs_f32)?;
            }
            Opcode::DefI => {
                handle_def_i32(inst, &mut const_defs_i32)?;
            }
            Opcode::DefB => {
                handle_def_bool(inst, &mut const_defs_bool)?;
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
            Opcode::Ret => {
                // Some real-world SM3 shaders terminate the main program with an explicit `ret`
                // instruction before the final END token. Since we don't currently support SM3
                // subroutines (`call`/`callnz`/`label`), treat a top-level `ret` as end-of-shader.
                //
                // `ret` inside structured control flow would imply an early exit and requires a
                // dedicated IR construct; reject that for now.
                if stack.len() != 1 {
                    return Err(err(inst, "ret inside control flow is not supported"));
                }
                break;
            }
            Opcode::Nop => {}
            Opcode::If => {
                if stack.len() > MAX_D3D9_SHADER_CONTROL_FLOW_NESTING {
                    return Err(err(
                        inst,
                        format!(
                            "control flow nesting exceeds maximum {MAX_D3D9_SHADER_CONTROL_FLOW_NESTING} levels"
                        ),
                    ));
                }
                stack.push(Frame::If {
                    cond: build_if_cond(inst)?,
                    then_block: Block::new(),
                    else_block: None,
                    in_else: false,
                })
            }
            Opcode::Ifc => {
                if stack.len() > MAX_D3D9_SHADER_CONTROL_FLOW_NESTING {
                    return Err(err(
                        inst,
                        format!(
                            "control flow nesting exceeds maximum {MAX_D3D9_SHADER_CONTROL_FLOW_NESTING} levels"
                        ),
                    ));
                }
                stack.push(Frame::If {
                    cond: build_ifc_cond(inst)?,
                    then_block: Block::new(),
                    else_block: None,
                    in_else: false,
                })
            }
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
                let frame = stack
                    .pop()
                    .ok_or_else(|| err(inst, "endif without matching if"))?;
                let (cond, then_block, else_block) = match frame {
                    Frame::If {
                        cond,
                        then_block,
                        else_block,
                        ..
                    } => (cond, then_block, else_block),
                    _ => return Err(err(inst, "endif without matching if")),
                };
                push_stmt(
                    &mut stack,
                    Stmt::If {
                        cond,
                        then_block,
                        else_block,
                    },
                )?;
            }
            Opcode::Loop => {
                if stack.len() > MAX_D3D9_SHADER_CONTROL_FLOW_NESTING {
                    return Err(err(
                        inst,
                        format!(
                            "control flow nesting exceeds maximum {MAX_D3D9_SHADER_CONTROL_FLOW_NESTING} levels"
                        ),
                    ));
                }
                stack.push(Frame::Loop {
                    init: build_loop_init(inst)?,
                    body: Block::new(),
                })
            }
            Opcode::EndLoop => {
                let frame = stack
                    .pop()
                    .ok_or_else(|| err(inst, "endloop without matching loop"))?;
                let (init, body) = match frame {
                    Frame::Loop { init, body } => (init, body),
                    _ => return Err(err(inst, "endloop without matching loop")),
                };
                push_stmt(&mut stack, Stmt::Loop { init, body })?;
            }
            Opcode::Break => push_stmt(&mut stack, Stmt::Break)?,
            Opcode::Breakc => {
                let cond = build_cmp_cond(inst)?;
                push_stmt(&mut stack, Stmt::BreakIf { cond })?;
            }
            Opcode::TexKill => {
                let src = extract_src(inst, 0)?;
                if let Some(pred) = &inst.predicate {
                    let cond = Cond::Predicate {
                        pred: PredicateRef {
                            reg: to_ir_reg(inst, &pred.reg)?,
                            component: pred.component,
                            negate: pred.negate,
                        },
                    };
                    let then_block = Block {
                        stmts: vec![Stmt::Discard { src }],
                    };
                    push_stmt(
                        &mut stack,
                        Stmt::If {
                            cond,
                            then_block,
                            else_block: None,
                        },
                    )?;
                } else {
                    push_stmt(&mut stack, Stmt::Discard { src })?;
                }
            }

            Opcode::Mov => {
                let dst = extract_dst(inst, 0)?;
                let src = extract_src(inst, 1)?;
                let modifiers = build_modifiers(inst)?;
                push_stmt(
                    &mut stack,
                    Stmt::Op(IrOp::Mov {
                        dst,
                        src,
                        modifiers,
                    }),
                )?;
            }
            Opcode::Mova => {
                let dst = extract_dst(inst, 0)?;
                if dst.reg.file != RegFile::Addr {
                    return Err(err(inst, "mova destination must be an address register"));
                }
                let src = extract_src(inst, 1)?;
                let modifiers = build_modifiers(inst)?;
                push_stmt(
                    &mut stack,
                    Stmt::Op(IrOp::Mova {
                        dst,
                        src,
                        modifiers,
                    }),
                )?;
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
            Opcode::Lrp => {
                let dst = extract_dst(inst, 0)?;
                let src0 = extract_src(inst, 1)?;
                let src1 = extract_src(inst, 2)?;
                let src2 = extract_src(inst, 3)?;
                let modifiers = build_modifiers(inst)?;
                push_stmt(
                    &mut stack,
                    Stmt::Op(IrOp::Lrp {
                        dst,
                        src0,
                        src1,
                        src2,
                        modifiers,
                    }),
                )?;
            }
            Opcode::Dp2 => push_binop(&mut stack, inst, |dst, src0, src1, modifiers| IrOp::Dp2 {
                dst,
                src0,
                src1,
                modifiers,
            })?,
            Opcode::Dp2Add => {
                let dst = extract_dst(inst, 0)?;
                let src0 = extract_src(inst, 1)?;
                let src1 = extract_src(inst, 2)?;
                let src2 = extract_src(inst, 3)?;
                let modifiers = build_modifiers(inst)?;
                push_stmt(
                    &mut stack,
                    Stmt::Op(IrOp::Dp2Add {
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
            Opcode::Dst => push_binop(&mut stack, inst, |dst, src0, src1, modifiers| IrOp::Dst {
                dst,
                src0,
                src1,
                modifiers,
            })?,
            Opcode::Crs => push_binop(&mut stack, inst, |dst, src0, src1, modifiers| IrOp::Crs {
                dst,
                src0,
                src1,
                modifiers,
            })?,
            Opcode::M4x4 => push_matmul(&mut stack, inst, 4, 4)?,
            Opcode::M4x3 => push_matmul(&mut stack, inst, 4, 3)?,
            Opcode::M3x4 => push_matmul(&mut stack, inst, 3, 4)?,
            Opcode::M3x3 => push_matmul(&mut stack, inst, 3, 3)?,
            Opcode::M3x2 => push_matmul(&mut stack, inst, 3, 2)?,
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
            Opcode::Frc => push_unop(&mut stack, inst, |dst, src, modifiers| IrOp::Frc {
                dst,
                src,
                modifiers,
            })?,
            Opcode::Abs => push_unop(&mut stack, inst, |dst, src, modifiers| IrOp::Abs {
                dst,
                src,
                modifiers,
            })?,
            Opcode::Sgn => push_unop(&mut stack, inst, |dst, src, modifiers| IrOp::Sgn {
                dst,
                src,
                modifiers,
            })?,
            Opcode::Exp => push_unop(&mut stack, inst, |dst, src, modifiers| IrOp::Exp {
                dst,
                src,
                modifiers,
            })?,
            Opcode::Log => push_unop(&mut stack, inst, |dst, src, modifiers| IrOp::Log {
                dst,
                src,
                modifiers,
            })?,
            Opcode::Dsx => {
                if version.stage != ShaderStage::Pixel {
                    return Err(err(inst, "dsx is only valid in pixel shaders"));
                }
                push_unop(&mut stack, inst, |dst, src, modifiers| IrOp::Ddx {
                    dst,
                    src,
                    modifiers,
                })?
            }
            Opcode::Dsy => {
                if version.stage != ShaderStage::Pixel {
                    return Err(err(inst, "dsy is only valid in pixel shaders"));
                }
                push_unop(&mut stack, inst, |dst, src, modifiers| IrOp::Ddy {
                    dst,
                    src,
                    modifiers,
                })?
            }
            Opcode::Nrm => push_unop(&mut stack, inst, |dst, src, modifiers| IrOp::Nrm {
                dst,
                src,
                modifiers,
            })?,
            Opcode::Lit => push_unop(&mut stack, inst, |dst, src, modifiers| IrOp::Lit {
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
            Opcode::Pow => push_binop(&mut stack, inst, |dst, src0, src1, modifiers| IrOp::Pow {
                dst,
                src0,
                src1,
                modifiers,
            })?,
            Opcode::SinCos => {
                let dst = extract_dst(inst, 0)?;
                let src = extract_src(inst, 1)?;
                let src1 = if inst.operands.len() > 2 {
                    Some(extract_src(inst, 2)?)
                } else {
                    None
                };
                let src2 = if inst.operands.len() > 3 {
                    Some(extract_src(inst, 3)?)
                } else {
                    None
                };
                let modifiers = build_modifiers(inst)?;
                push_stmt(
                    &mut stack,
                    Stmt::Op(IrOp::SinCos {
                        dst,
                        src,
                        src1,
                        src2,
                        modifiers,
                    }),
                )?;
            }

            Opcode::Sge => push_cmpop(&mut stack, inst, CompareOp::Ge)?,
            Opcode::Slt => push_cmpop(&mut stack, inst, CompareOp::Lt)?,
            Opcode::Seq => push_cmpop(&mut stack, inst, CompareOp::Eq)?,
            Opcode::Sne => push_cmpop(&mut stack, inst, CompareOp::Ne)?,

            Opcode::Setp => push_setp(&mut stack, inst)?,

            Opcode::Cmp => {
                // D3D9 `cmp`: dst, cond, src_ge, src_lt
                let dst = extract_dst(inst, 0)?;
                let cond = extract_src(inst, 1)?;
                let src_ge = extract_src(inst, 2)?;
                let src_lt = extract_src(inst, 3)?;
                let modifiers = build_modifiers(inst)?;
                push_stmt(
                    &mut stack,
                    Stmt::Op(IrOp::Select {
                        dst,
                        cond,
                        src_ge,
                        src_lt,
                        modifiers,
                    }),
                )?;
            }

            Opcode::Tex => push_texld(&mut stack, inst)?,
            Opcode::TexLdl => push_texldl(&mut stack, inst)?,
            Opcode::TexLdd => push_texldd(&mut stack, inst)?,

            Opcode::Call => return Err(err(inst, "call/callnz not supported")),

            Opcode::Unknown(op) => return Err(err(inst, format!("unsupported opcode 0x{op:04x}"))),
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

    let mut ir = ShaderIr {
        version,
        inputs,
        outputs,
        samplers,
        const_defs_f32,
        const_defs_i32,
        const_defs_bool,
        body,
        uses_semantic_locations: false,
    };

    apply_vertex_input_remap(&mut ir)?;

    Ok(ir)
}

fn handle_dcl(
    inst: &DecodedInstruction,
    _version: &crate::sm3::types::ShaderVersion,
    dcl: &DclInfo,
    inputs: &mut Vec<IoDecl>,
    outputs: &mut Vec<IoDecl>,
    samplers: &mut Vec<SamplerDecl>,
) -> Result<(), BuildError> {
    let dst = match inst.operands.first() {
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
            let semantic = map_semantic(&dcl.usage, dcl.usage_index);
            match reg.file {
                RegFile::Input | RegFile::Texture => inputs.push(IoDecl {
                    reg,
                    semantic,
                    mask,
                }),
                RegFile::RastOut
                | RegFile::AttrOut
                | RegFile::TexCoordOut
                | RegFile::Output
                | RegFile::ColorOut
                | RegFile::DepthOut => outputs.push(IoDecl {
                    reg,
                    semantic,
                    mask,
                }),
                _ => {
                    // Some decls apply to special register files; ignore them for now.
                }
            }
        }
    }
    Ok(())
}

fn handle_def_f32(inst: &DecodedInstruction, out: &mut Vec<ConstDefF32>) -> Result<(), BuildError> {
    let dst = match inst.operands.first() {
        Some(Operand::Dst(dst)) => dst,
        _ => return Err(err(inst, "def missing destination operand")),
    };
    if dst.reg.file != RegisterFile::Const {
        return Err(err(
            inst,
            "def destination is not a float constant register",
        ));
    }

    let mut vals = [0f32; 4];
    for (i, val) in vals.iter_mut().enumerate() {
        let token = match inst.operands.get(1 + i) {
            Some(Operand::Imm32(v)) => *v,
            _ => return Err(err(inst, "def missing immediate constant tokens")),
        };
        *val = f32::from_bits(token);
    }

    out.push(ConstDefF32 {
        index: dst.reg.index,
        value: vals,
    });
    Ok(())
}

fn handle_def_i32(inst: &DecodedInstruction, out: &mut Vec<ConstDefI32>) -> Result<(), BuildError> {
    let dst = match inst.operands.first() {
        Some(Operand::Dst(dst)) => dst,
        _ => return Err(err(inst, "defi missing destination operand")),
    };
    if dst.reg.file != RegisterFile::ConstInt {
        return Err(err(
            inst,
            "defi destination is not an integer constant register",
        ));
    }

    let mut vals = [0i32; 4];
    for (i, val) in vals.iter_mut().enumerate() {
        let token = match inst.operands.get(1 + i) {
            Some(Operand::Imm32(v)) => *v,
            _ => return Err(err(inst, "defi missing immediate constant tokens")),
        };
        *val = token as i32;
    }

    out.push(ConstDefI32 {
        index: dst.reg.index,
        value: vals,
    });
    Ok(())
}

fn handle_def_bool(
    inst: &DecodedInstruction,
    out: &mut Vec<ConstDefBool>,
) -> Result<(), BuildError> {
    let dst = match inst.operands.first() {
        Some(Operand::Dst(dst)) => dst,
        _ => return Err(err(inst, "defb missing destination operand")),
    };
    if dst.reg.file != RegisterFile::ConstBool {
        return Err(err(
            inst,
            "defb destination is not a boolean constant register",
        ));
    }

    let token = match inst.operands.get(1) {
        Some(Operand::Imm32(v)) => *v,
        _ => return Err(err(inst, "defb missing immediate constant token")),
    };

    out.push(ConstDefBool {
        index: dst.reg.index,
        value: token != 0,
    });
    Ok(())
}

fn build_loop_init(inst: &DecodedInstruction) -> Result<LoopInit, BuildError> {
    let loop_reg = match inst.operands.first() {
        Some(Operand::Src(src)) => src,
        _ => return Err(err(inst, "loop missing loop register operand")),
    };
    if loop_reg.reg.file != RegisterFile::Loop {
        return Err(err(inst, "loop first operand must be a loop register"));
    }
    if !matches!(loop_reg.modifier, SrcModifier::None) {
        return Err(err(
            inst,
            "loop register operand cannot have a source modifier",
        ));
    }

    let ctrl_reg = match inst.operands.get(1) {
        Some(Operand::Src(src)) => src,
        _ => return Err(err(inst, "loop missing integer constant operand")),
    };
    if ctrl_reg.reg.file != RegisterFile::ConstInt {
        return Err(err(
            inst,
            "loop second operand must be an integer constant register",
        ));
    }
    if !matches!(ctrl_reg.modifier, SrcModifier::None) {
        return Err(err(
            inst,
            "loop integer constant operand cannot have a source modifier",
        ));
    }

    Ok(LoopInit {
        loop_reg: to_ir_reg(inst, &loop_reg.reg)?,
        ctrl_reg: to_ir_reg(inst, &ctrl_reg.reg)?,
    })
}

fn map_semantic(usage: &DclUsage, index: u8) -> Semantic {
    match usage {
        DclUsage::Position => Semantic::Position(index),
        DclUsage::BlendWeight => Semantic::BlendWeight(index),
        DclUsage::BlendIndices => Semantic::BlendIndices(index),
        DclUsage::Color => Semantic::Color(index),
        DclUsage::TexCoord => Semantic::TexCoord(index),
        DclUsage::Normal => Semantic::Normal(index),
        DclUsage::Fog => Semantic::Fog(index),
        DclUsage::PointSize => Semantic::PointSize(index),
        DclUsage::Depth => Semantic::Depth(index),
        DclUsage::Tangent => Semantic::Tangent(index),
        DclUsage::Binormal => Semantic::Binormal(index),
        DclUsage::TessFactor => Semantic::TessFactor(index),
        DclUsage::PositionT => Semantic::PositionT(index),
        DclUsage::Sample => Semantic::Sample(index),
        DclUsage::Unknown(u) => Semantic::Other { usage: *u, index },
        DclUsage::TextureType(_) => Semantic::Other { usage: 0xFE, index },
    }
}

fn semantic_to_decl_usage(semantic: &Semantic) -> Option<(DeclUsage, u8)> {
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

fn collect_used_input_regs_block(block: &Block, out: &mut BTreeSet<u32>) {
    for stmt in &block.stmts {
        match stmt {
            Stmt::Op(op) => collect_used_input_regs_op(op, out),
            Stmt::If {
                cond,
                then_block,
                else_block,
            } => {
                collect_used_input_regs_cond(cond, out);
                collect_used_input_regs_block(then_block, out);
                if let Some(else_block) = else_block {
                    collect_used_input_regs_block(else_block, out);
                }
            }
            Stmt::Loop { init: _, body } => collect_used_input_regs_block(body, out),
            Stmt::Break => {}
            Stmt::BreakIf { cond } => collect_used_input_regs_cond(cond, out),
            Stmt::Discard { src } => collect_used_input_regs_src(src, out),
        }
    }
}

fn collect_used_input_regs_op(op: &IrOp, out: &mut BTreeSet<u32>) {
    match op {
        IrOp::Mov {
            dst,
            src,
            modifiers,
        }
        | IrOp::Mova {
            dst,
            src,
            modifiers,
        }
        | IrOp::Rcp {
            dst,
            src,
            modifiers,
        }
        | IrOp::Frc {
            dst,
            src,
            modifiers,
        }
        | IrOp::Rsq {
            dst,
            src,
            modifiers,
        }
        | IrOp::Abs {
            dst,
            src,
            modifiers,
        }
        | IrOp::Sgn {
            dst,
            src,
            modifiers,
        }
        | IrOp::Exp {
            dst,
            src,
            modifiers,
        }
        | IrOp::Log {
            dst,
            src,
            modifiers,
        }
        | IrOp::Ddx {
            dst,
            src,
            modifiers,
        }
        | IrOp::Ddy {
            dst,
            src,
            modifiers,
        }
        | IrOp::Nrm {
            dst,
            src,
            modifiers,
        }
        | IrOp::Lit {
            dst,
            src,
            modifiers,
        } => {
            collect_used_input_regs_dst(dst, out);
            collect_used_input_regs_src(src, out);
            collect_used_input_regs_modifiers(modifiers, out);
        }
        IrOp::SinCos {
            dst,
            src,
            src1,
            src2,
            modifiers,
        } => {
            collect_used_input_regs_dst(dst, out);
            collect_used_input_regs_src(src, out);
            if let Some(src1) = src1 {
                collect_used_input_regs_src(src1, out);
            }
            if let Some(src2) = src2 {
                collect_used_input_regs_src(src2, out);
            }
            collect_used_input_regs_modifiers(modifiers, out);
        }
        IrOp::Add {
            dst,
            src0,
            src1,
            modifiers,
        }
        | IrOp::Sub {
            dst,
            src0,
            src1,
            modifiers,
        }
        | IrOp::Mul {
            dst,
            src0,
            src1,
            modifiers,
        }
        | IrOp::Min {
            dst,
            src0,
            src1,
            modifiers,
        }
        | IrOp::Max {
            dst,
            src0,
            src1,
            modifiers,
        }
        | IrOp::Dp2 {
            dst,
            src0,
            src1,
            modifiers,
        }
        | IrOp::Dp3 {
            dst,
            src0,
            src1,
            modifiers,
        }
        | IrOp::Dp4 {
            dst,
            src0,
            src1,
            modifiers,
        }
        | IrOp::Dst {
            dst,
            src0,
            src1,
            modifiers,
        }
        | IrOp::Crs {
            dst,
            src0,
            src1,
            modifiers,
        }
        | IrOp::SetCmp {
            dst,
            src0,
            src1,
            modifiers,
            ..
        }
        | IrOp::Pow {
            dst,
            src0,
            src1,
            modifiers,
        } => {
            collect_used_input_regs_dst(dst, out);
            collect_used_input_regs_src(src0, out);
            collect_used_input_regs_src(src1, out);
            collect_used_input_regs_modifiers(modifiers, out);
        }
        IrOp::MatrixMul {
            dst,
            src0,
            src1,
            n,
            modifiers,
            ..
        } => {
            collect_used_input_regs_dst(dst, out);
            collect_used_input_regs_src(src0, out);
            // Matrix helper ops implicitly read `src1 + column_index` for 0..n.
            for col in 0..*n {
                let mut column = src1.clone();
                if let Some(idx) = column.reg.index.checked_add(u32::from(col)) {
                    column.reg.index = idx;
                }
                collect_used_input_regs_src(&column, out);
            }
            collect_used_input_regs_modifiers(modifiers, out);
        }
        IrOp::Select {
            dst,
            cond,
            src_ge,
            src_lt,
            modifiers,
        } => {
            collect_used_input_regs_dst(dst, out);
            collect_used_input_regs_src(cond, out);
            collect_used_input_regs_src(src_ge, out);
            collect_used_input_regs_src(src_lt, out);
            collect_used_input_regs_modifiers(modifiers, out);
        }
        IrOp::Dp2Add {
            dst,
            src0,
            src1,
            src2,
            modifiers,
        } => {
            collect_used_input_regs_dst(dst, out);
            collect_used_input_regs_src(src0, out);
            collect_used_input_regs_src(src1, out);
            collect_used_input_regs_src(src2, out);
            collect_used_input_regs_modifiers(modifiers, out);
        }
        IrOp::Mad {
            dst,
            src0,
            src1,
            src2,
            modifiers,
        } => {
            collect_used_input_regs_dst(dst, out);
            collect_used_input_regs_src(src0, out);
            collect_used_input_regs_src(src1, out);
            collect_used_input_regs_src(src2, out);
            collect_used_input_regs_modifiers(modifiers, out);
        }
        IrOp::Lrp {
            dst,
            src0,
            src1,
            src2,
            modifiers,
        } => {
            collect_used_input_regs_dst(dst, out);
            collect_used_input_regs_src(src0, out);
            collect_used_input_regs_src(src1, out);
            collect_used_input_regs_src(src2, out);
            collect_used_input_regs_modifiers(modifiers, out);
        }
        IrOp::TexSample {
            dst,
            coord,
            ddx,
            ddy,
            modifiers,
            ..
        } => {
            collect_used_input_regs_dst(dst, out);
            collect_used_input_regs_src(coord, out);
            if let Some(ddx) = ddx {
                collect_used_input_regs_src(ddx, out);
            }
            if let Some(ddy) = ddy {
                collect_used_input_regs_src(ddy, out);
            }
            collect_used_input_regs_modifiers(modifiers, out);
        }
    }
}

fn collect_used_input_regs_dst(dst: &Dst, out: &mut BTreeSet<u32>) {
    collect_used_input_regs_reg(&dst.reg, out);
}

fn collect_used_input_regs_src(src: &Src, out: &mut BTreeSet<u32>) {
    collect_used_input_regs_reg(&src.reg, out);
}

fn collect_used_input_regs_cond(cond: &Cond, out: &mut BTreeSet<u32>) {
    match cond {
        Cond::NonZero { src } => collect_used_input_regs_src(src, out),
        Cond::Compare { src0, src1, .. } => {
            collect_used_input_regs_src(src0, out);
            collect_used_input_regs_src(src1, out);
        }
        Cond::Predicate { pred } => collect_used_input_regs_reg(&pred.reg, out),
    }
}

fn collect_used_input_regs_modifiers(mods: &InstModifiers, out: &mut BTreeSet<u32>) {
    if let Some(pred) = &mods.predicate {
        collect_used_input_regs_reg(&pred.reg, out);
    }
}

fn collect_used_input_regs_reg(reg: &RegRef, out: &mut BTreeSet<u32>) {
    if reg.file == RegFile::Input {
        out.insert(reg.index);
    }
    if let Some(rel) = &reg.relative {
        collect_used_input_regs_reg(&rel.reg, out);
    }
}

fn remap_input_regs_in_block(block: &mut Block, remap: &HashMap<u32, u32>) {
    for stmt in &mut block.stmts {
        match stmt {
            Stmt::Op(op) => remap_input_regs_in_op(op, remap),
            Stmt::If {
                cond,
                then_block,
                else_block,
            } => {
                remap_input_regs_in_cond(cond, remap);
                remap_input_regs_in_block(then_block, remap);
                if let Some(else_block) = else_block {
                    remap_input_regs_in_block(else_block, remap);
                }
            }
            Stmt::Loop { init: _, body } => remap_input_regs_in_block(body, remap),
            Stmt::Break => {}
            Stmt::BreakIf { cond } => remap_input_regs_in_cond(cond, remap),
            Stmt::Discard { src } => remap_input_regs_in_src(src, remap),
        }
    }
}

fn remap_input_regs_in_op(op: &mut IrOp, remap: &HashMap<u32, u32>) {
    match op {
        IrOp::Mov {
            dst,
            src,
            modifiers,
        }
        | IrOp::Mova {
            dst,
            src,
            modifiers,
        }
        | IrOp::Rcp {
            dst,
            src,
            modifiers,
        }
        | IrOp::Frc {
            dst,
            src,
            modifiers,
        }
        | IrOp::Rsq {
            dst,
            src,
            modifiers,
        }
        | IrOp::Sgn {
            dst,
            src,
            modifiers,
        }
        | IrOp::Abs {
            dst,
            src,
            modifiers,
        }
        | IrOp::Exp {
            dst,
            src,
            modifiers,
        }
        | IrOp::Log {
            dst,
            src,
            modifiers,
        }
        | IrOp::Ddx {
            dst,
            src,
            modifiers,
        }
        | IrOp::Ddy {
            dst,
            src,
            modifiers,
        }
        | IrOp::Nrm {
            dst,
            src,
            modifiers,
        }
        | IrOp::Lit {
            dst,
            src,
            modifiers,
        } => {
            remap_input_regs_in_dst(dst, remap);
            remap_input_regs_in_src(src, remap);
            remap_input_regs_in_modifiers(modifiers, remap);
        }
        IrOp::SinCos {
            dst,
            src,
            src1,
            src2,
            modifiers,
        } => {
            remap_input_regs_in_dst(dst, remap);
            remap_input_regs_in_src(src, remap);
            if let Some(src1) = src1 {
                remap_input_regs_in_src(src1, remap);
            }
            if let Some(src2) = src2 {
                remap_input_regs_in_src(src2, remap);
            }
            remap_input_regs_in_modifiers(modifiers, remap);
        }
        IrOp::Add {
            dst,
            src0,
            src1,
            modifiers,
        }
        | IrOp::Sub {
            dst,
            src0,
            src1,
            modifiers,
        }
        | IrOp::Mul {
            dst,
            src0,
            src1,
            modifiers,
        }
        | IrOp::Min {
            dst,
            src0,
            src1,
            modifiers,
        }
        | IrOp::Max {
            dst,
            src0,
            src1,
            modifiers,
        }
        | IrOp::Dp2 {
            dst,
            src0,
            src1,
            modifiers,
        }
        | IrOp::Dp3 {
            dst,
            src0,
            src1,
            modifiers,
        }
        | IrOp::Dp4 {
            dst,
            src0,
            src1,
            modifiers,
        }
        | IrOp::Dst {
            dst,
            src0,
            src1,
            modifiers,
        }
        | IrOp::Crs {
            dst,
            src0,
            src1,
            modifiers,
        }
        | IrOp::SetCmp {
            dst,
            src0,
            src1,
            modifiers,
            ..
        }
        | IrOp::MatrixMul {
            dst,
            src0,
            src1,
            modifiers,
            ..
        }
        | IrOp::Pow {
            dst,
            src0,
            src1,
            modifiers,
        } => {
            remap_input_regs_in_dst(dst, remap);
            remap_input_regs_in_src(src0, remap);
            remap_input_regs_in_src(src1, remap);
            remap_input_regs_in_modifiers(modifiers, remap);
        }
        IrOp::Select {
            dst,
            cond,
            src_ge,
            src_lt,
            modifiers,
        } => {
            remap_input_regs_in_dst(dst, remap);
            remap_input_regs_in_src(cond, remap);
            remap_input_regs_in_src(src_ge, remap);
            remap_input_regs_in_src(src_lt, remap);
            remap_input_regs_in_modifiers(modifiers, remap);
        }
        IrOp::Mad {
            dst,
            src0,
            src1,
            src2,
            modifiers,
        } => {
            remap_input_regs_in_dst(dst, remap);
            remap_input_regs_in_src(src0, remap);
            remap_input_regs_in_src(src1, remap);
            remap_input_regs_in_src(src2, remap);
            remap_input_regs_in_modifiers(modifiers, remap);
        }
        IrOp::Lrp {
            dst,
            src0,
            src1,
            src2,
            modifiers,
        }
        | IrOp::Dp2Add {
            dst,
            src0,
            src1,
            src2,
            modifiers,
        } => {
            remap_input_regs_in_dst(dst, remap);
            remap_input_regs_in_src(src0, remap);
            remap_input_regs_in_src(src1, remap);
            remap_input_regs_in_src(src2, remap);
            remap_input_regs_in_modifiers(modifiers, remap);
        }
        IrOp::TexSample {
            dst,
            coord,
            ddx,
            ddy,
            modifiers,
            ..
        } => {
            remap_input_regs_in_dst(dst, remap);
            remap_input_regs_in_src(coord, remap);
            if let Some(ddx) = ddx {
                remap_input_regs_in_src(ddx, remap);
            }
            if let Some(ddy) = ddy {
                remap_input_regs_in_src(ddy, remap);
            }
            remap_input_regs_in_modifiers(modifiers, remap);
        }
    }
}

fn remap_input_regs_in_dst(dst: &mut Dst, remap: &HashMap<u32, u32>) {
    remap_input_regs_in_reg(&mut dst.reg, remap);
}

fn remap_input_regs_in_src(src: &mut Src, remap: &HashMap<u32, u32>) {
    remap_input_regs_in_reg(&mut src.reg, remap);
}

fn remap_input_regs_in_cond(cond: &mut Cond, remap: &HashMap<u32, u32>) {
    match cond {
        Cond::NonZero { src } => remap_input_regs_in_src(src, remap),
        Cond::Compare { src0, src1, .. } => {
            remap_input_regs_in_src(src0, remap);
            remap_input_regs_in_src(src1, remap);
        }
        Cond::Predicate { pred } => remap_input_regs_in_reg(&mut pred.reg, remap),
    }
}

fn remap_input_regs_in_modifiers(mods: &mut InstModifiers, remap: &HashMap<u32, u32>) {
    if let Some(pred) = &mut mods.predicate {
        remap_input_regs_in_reg(&mut pred.reg, remap);
    }
}

fn remap_input_regs_in_reg(reg: &mut RegRef, remap: &HashMap<u32, u32>) {
    if reg.file == RegFile::Input {
        if let Some(&new_idx) = remap.get(&reg.index) {
            reg.index = new_idx;
        }
    }
    if let Some(rel) = &mut reg.relative {
        remap_input_regs_in_reg(&mut rel.reg, remap);
    }
}

fn apply_vertex_input_remap(ir: &mut ShaderIr) -> Result<(), BuildError> {
    if ir.version.stage != ShaderStage::Vertex {
        return Ok(());
    }

    let mut used_vs_inputs = BTreeSet::<u32>::new();
    collect_used_input_regs_block(&ir.body, &mut used_vs_inputs);

    if used_vs_inputs.is_empty() {
        return Ok(());
    }

    // Only enable semantic remapping when we have DCL declarations for all used input registers.
    // Otherwise, fall back to raw v# indices (legacy behavior).
    let mut dcl_map = HashMap::<u32, (DeclUsage, u8)>::new();
    let mut input_dcl_order = Vec::<(DeclUsage, u8)>::new();
    for decl in &ir.inputs {
        if decl.reg.file != RegFile::Input {
            continue;
        }
        let Some((usage, usage_index)) = semantic_to_decl_usage(&decl.semantic) else {
            continue;
        };
        dcl_map.insert(decl.reg.index, (usage, usage_index));
        input_dcl_order.push((usage, usage_index));
    }

    let map = AdaptiveLocationMap::new(input_dcl_order)
        .map_err(|e| err_internal(&format!("failed to build vertex input location map: {e}")))?;
    let mut remap = HashMap::<u32, u32>::new();
    let mut used_locations = HashMap::<u32, u32>::new();

    // Bail out if any used `v#` register is missing a semantic declaration.
    for &v in &used_vs_inputs {
        if !dcl_map.contains_key(&v) {
            return Ok(());
        }
    }

    // Build a v# -> @location(n) remap table for *all* declared input registers, not just the ones
    // referenced by the instruction stream.
    //
    // Even unused declarations must be remapped so host-side semantic reflection (`semantic_locations`)
    // stays consistent and doesn't observe collisions between remapped used regs and raw unused regs.
    for decl in &ir.inputs {
        if decl.reg.file != RegFile::Input {
            continue;
        }
        let Some((usage, usage_index)) = semantic_to_decl_usage(&decl.semantic) else {
            continue;
        };
        let v = decl.reg.index;
        let loc = map
            .location_for(usage, usage_index)
            .map_err(|e| err_internal(&format!("failed to map vertex input semantic: {e}")))?;
        if let Some(prev_v) = used_locations.insert(loc, v) {
            if prev_v != v {
                return Err(err_internal(&format!(
                    "vertex shader input DCL declarations map multiple input registers to WGSL @location({loc}): v{prev_v} and v{v}"
                )));
            }
        }
        remap.insert(v, loc);
    }

    // Rewrite all declared input registers (including unused ones) so that
    // `ShaderTranslation::semantic_locations` can report canonical WGSL locations for the full
    // `dcl_*` set. This avoids collisions between remapped and non-remapped declarations on the
    // host side (see `translate_entrypoint_sm3_remaps_unused_declared_semantics` regression test).
    for decl in &mut ir.inputs {
        if decl.reg.file != RegFile::Input || decl.reg.relative.is_some() {
            continue;
        }
        let Some((usage, usage_index)) = semantic_to_decl_usage(&decl.semantic) else {
            continue;
        };
        let loc = map
            .location_for(usage, usage_index)
            .map_err(|e| err_internal(&format!("failed to map vertex input semantic: {e}")))?;
        decl.reg.index = loc;
    }

    remap_input_regs_in_block(&mut ir.body, &remap);
    ir.uses_semantic_locations = true;
    Ok(())
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
        _ => return Err(err(inst, format!("missing dst operand {idx}"))),
    };
    Ok(Dst {
        reg: to_ir_reg(inst, &dst.reg)?,
        mask: dst.mask,
    })
}

fn extract_src(inst: &DecodedInstruction, idx: usize) -> Result<Src, BuildError> {
    let src = match inst.operands.get(idx) {
        Some(Operand::Src(src)) => src,
        _ => return Err(err(inst, format!("missing src operand {idx}"))),
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

fn push_binop<F>(stack: &mut [Frame], inst: &DecodedInstruction, ctor: F) -> Result<(), BuildError>
where
    F: FnOnce(Dst, Src, Src, InstModifiers) -> IrOp,
{
    let dst = extract_dst(inst, 0)?;
    let src0 = extract_src(inst, 1)?;
    let src1 = extract_src(inst, 2)?;
    let modifiers = build_modifiers(inst)?;
    push_stmt(stack, Stmt::Op(ctor(dst, src0, src1, modifiers)))
}

fn push_unop<F>(stack: &mut [Frame], inst: &DecodedInstruction, ctor: F) -> Result<(), BuildError>
where
    F: FnOnce(Dst, Src, InstModifiers) -> IrOp,
{
    let dst = extract_dst(inst, 0)?;
    let src = extract_src(inst, 1)?;
    let modifiers = build_modifiers(inst)?;
    push_stmt(stack, Stmt::Op(ctor(dst, src, modifiers)))
}

fn push_matmul(
    stack: &mut [Frame],
    inst: &DecodedInstruction,
    m: u8,
    n: u8,
) -> Result<(), BuildError> {
    let dst = extract_dst(inst, 0)?;
    let src0 = extract_src(inst, 1)?;
    let src1 = extract_src(inst, 2)?;
    let modifiers = build_modifiers(inst)?;
    push_stmt(
        stack,
        Stmt::Op(IrOp::MatrixMul {
            dst,
            src0,
            src1,
            m,
            n,
            modifiers,
        }),
    )
}

fn push_cmpop(
    stack: &mut [Frame],
    inst: &DecodedInstruction,
    op: CompareOp,
) -> Result<(), BuildError> {
    let dst = extract_dst(inst, 0)?;
    let src0 = extract_src(inst, 1)?;
    let src1 = extract_src(inst, 2)?;
    let modifiers = build_modifiers(inst)?;
    push_stmt(
        stack,
        Stmt::Op(IrOp::SetCmp {
            op,
            dst,
            src0,
            src1,
            modifiers,
        }),
    )
}

fn push_setp(stack: &mut [Frame], inst: &DecodedInstruction) -> Result<(), BuildError> {
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
        Stmt::Op(IrOp::SetCmp {
            op,
            dst,
            src0,
            src1,
            modifiers,
        }),
    )
}

fn push_texld(stack: &mut [Frame], inst: &DecodedInstruction) -> Result<(), BuildError> {
    // `tex` (SM2/3) is `texld` / `texldp` depending on a flag we captured as an immediate.
    let dst = extract_dst(inst, 0)?;
    let coord = extract_src(inst, 1)?;
    let sampler_src = extract_src(inst, 2)?;
    if sampler_src.reg.file != RegFile::Sampler {
        return Err(err(inst, "tex sampler operand is not a sampler register"));
    }
    let specific = match inst.operands.last() {
        Some(Operand::Imm32(v)) => (*v & 0xF) as u8,
        _ => 0,
    };
    let kind = match specific {
        0 => TexSampleKind::ImplicitLod { project: false },
        1 => TexSampleKind::ImplicitLod { project: true },
        2 => TexSampleKind::Bias,
        other => {
            return Err(err(
                inst,
                format!("tex has unsupported encoding (specific=0x{other:x})"),
            ))
        }
    };
    let modifiers = build_modifiers(inst)?;
    push_stmt(
        stack,
        Stmt::Op(IrOp::TexSample {
            kind,
            dst,
            coord,
            ddx: None,
            ddy: None,
            sampler: sampler_src.reg.index,
            modifiers,
        }),
    )
}

fn push_texldl(stack: &mut [Frame], inst: &DecodedInstruction) -> Result<(), BuildError> {
    let dst = extract_dst(inst, 0)?;
    let coord = extract_src(inst, 1)?;
    let sampler_src = extract_src(inst, 2)?;
    if sampler_src.reg.file != RegFile::Sampler {
        return Err(err(
            inst,
            "texldl sampler operand is not a sampler register",
        ));
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

fn push_texldd(stack: &mut [Frame], inst: &DecodedInstruction) -> Result<(), BuildError> {
    let dst = extract_dst(inst, 0)?;
    let coord = extract_src(inst, 1)?;
    let ddx = extract_src(inst, 2)?;
    let ddy = extract_src(inst, 3)?;
    let sampler_src = extract_src(inst, 4)?;
    if sampler_src.reg.file != RegFile::Sampler {
        return Err(err(
            inst,
            "texldd sampler operand is not a sampler register",
        ));
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

fn push_stmt(stack: &mut [Frame], stmt: Stmt) -> Result<(), BuildError> {
    match stack.last_mut() {
        Some(Frame::Root(block)) => block.stmts.push(stmt),
        Some(Frame::Loop { body, .. }) => body.stmts.push(stmt),
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

fn to_ir_reg(
    inst: &DecodedInstruction,
    reg: &crate::sm3::decode::RegisterRef,
) -> Result<RegRef, BuildError> {
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

fn err(inst: &DecodedInstruction, message: impl Into<String>) -> BuildError {
    BuildError {
        location: inst.location,
        opcode: inst.opcode,
        message: message.into(),
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
        init: LoopInit,
        body: Block,
    },
}
