use crate::shader_limits::MAX_D3D9_SHADER_CONTROL_FLOW_NESTING;
use crate::sm3::decode::{ResultShift, SrcModifier, TextureType};
use crate::sm3::ir::{
    Block, CompareOp, Cond, Dst, IrOp, RegFile, ShaderIr, Src, Stmt, TexSampleKind,
};
use crate::sm3::types::ShaderStage;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyError {
    pub message: String,
}

impl std::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "IR verify error: {}", self.message)
    }
}

impl std::error::Error for VerifyError {}

pub fn verify_ir(ir: &ShaderIr) -> Result<(), VerifyError> {
    // Reject unknown sampler texture types early. These are invalid DCL encodings and should not
    // be treated as a fallbackable "unsupported feature" (otherwise malformed shaders could use
    // legacy fallback as an escape hatch).
    for sampler in &ir.samplers {
        if let TextureType::Unknown(v) = sampler.texture_type {
            return Err(VerifyError {
                message: format!(
                    "unknown sampler texture type value {v} declared for s{}",
                    sampler.index
                ),
            });
        }
    }
    verify_block(&ir.body, ir.version.stage, 0, 0, false)?;
    for body in ir.subroutines.values() {
        verify_block(body, ir.version.stage, 0, 0, true)?;
    }
    Ok(())
}

fn verify_block(
    block: &Block,
    stage: ShaderStage,
    depth: usize,
    loop_depth: usize,
    in_subroutine: bool,
) -> Result<(), VerifyError> {
    if depth > MAX_D3D9_SHADER_CONTROL_FLOW_NESTING {
        return Err(VerifyError {
            message: format!(
                "control flow nesting exceeds maximum {MAX_D3D9_SHADER_CONTROL_FLOW_NESTING} levels"
            ),
        });
    }
    for stmt in &block.stmts {
        match stmt {
            Stmt::Op(op) => verify_op(op, stage)?,
            Stmt::If {
                cond,
                then_block,
                else_block,
            } => {
                verify_cond(cond, stage)?;
                verify_block(then_block, stage, depth + 1, loop_depth, in_subroutine)?;
                if let Some(else_block) = else_block {
                    verify_block(else_block, stage, depth + 1, loop_depth, in_subroutine)?;
                }
            }
            Stmt::Loop { init, body } => {
                if init.loop_reg.file != RegFile::Loop {
                    return Err(VerifyError {
                        message: "loop init refers to a non-loop register".to_owned(),
                    });
                }
                if init.ctrl_reg.file != RegFile::ConstInt {
                    return Err(VerifyError {
                        message: "loop init refers to a non-integer-constant register".to_owned(),
                    });
                }
                verify_block(body, stage, depth + 1, loop_depth + 1, in_subroutine)?;
            }
            Stmt::Rep { count_reg, body } => {
                if count_reg.file != RegFile::ConstInt {
                    return Err(VerifyError {
                        message: "rep init refers to a non-integer-constant register".to_owned(),
                    });
                }
                verify_block(body, stage, depth + 1, loop_depth + 1, in_subroutine)?;
            }
            Stmt::Break => {
                if loop_depth == 0 {
                    return Err(VerifyError {
                        message: "break outside of a loop".to_owned(),
                    });
                }
            }
            Stmt::BreakIf { cond } => {
                if loop_depth == 0 {
                    return Err(VerifyError {
                        message: "breakc outside of a loop".to_owned(),
                    });
                }
                verify_cond(cond, stage)?;
            }
            Stmt::Discard { src } => {
                if stage != ShaderStage::Pixel {
                    return Err(VerifyError {
                        message: "discard/texkill is only valid in pixel shaders".to_owned(),
                    });
                }
                verify_src(src, stage)?;
            }
            Stmt::Call { .. } => {}
            Stmt::Return => {
                if !in_subroutine {
                    return Err(VerifyError {
                        message: "ret/return statement outside of a subroutine".to_owned(),
                    });
                }
            }
        }
    }
    Ok(())
}

fn verify_op(op: &IrOp, stage: ShaderStage) -> Result<(), VerifyError> {
    verify_dst(op_dst(op))?;

    // Some operations have stricter modifier rules depending on destination register type.
    if let IrOp::SetCmp { dst, modifiers, .. } = op {
        if dst.reg.file == RegFile::Predicate
            && (modifiers.saturate || modifiers.shift != ResultShift::None)
        {
            return Err(VerifyError {
                message: "predicate register writes cannot use saturate/shift modifiers".to_owned(),
            });
        }
    }

    if let IrOp::Abs {
        dst,
        src,
        modifiers,
    } = op
    {
        if is_int_reg_file(dst.reg.file)
            && is_int_reg_file(src.reg.file)
            && (modifiers.saturate || modifiers.shift != ResultShift::None)
        {
            return Err(VerifyError {
                message: "integer abs cannot use saturate/shift modifiers".to_owned(),
            });
        }
    }

    match op {
        IrOp::Mov {
            dst: _,
            src,
            modifiers,
        }
        | IrOp::Rcp {
            dst: _,
            src,
            modifiers,
        }
        | IrOp::Rsq {
            dst: _,
            src,
            modifiers,
        }
        | IrOp::Frc {
            dst: _,
            src,
            modifiers,
        }
        | IrOp::Abs {
            dst: _,
            src,
            modifiers,
        }
        | IrOp::Sgn {
            dst: _,
            src,
            modifiers,
        }
        | IrOp::Exp {
            dst: _,
            src,
            modifiers,
        }
        | IrOp::Log {
            dst: _,
            src,
            modifiers,
        }
        | IrOp::Ddx {
            dst: _,
            src,
            modifiers,
        }
        | IrOp::Ddy {
            dst: _,
            src,
            modifiers,
        }
        | IrOp::Nrm {
            dst: _,
            src,
            modifiers,
        }
        | IrOp::Lit {
            dst: _,
            src,
            modifiers,
        } => {
            if matches!(op, IrOp::Ddx { .. } | IrOp::Ddy { .. }) && stage != ShaderStage::Pixel {
                return Err(VerifyError {
                    message: "derivative instructions are only valid in pixel shaders".to_owned(),
                });
            }
            verify_src(src, stage)?;
            verify_modifiers(modifiers)?;
        }
        IrOp::Mova {
            dst,
            src,
            modifiers,
        } => {
            if dst.reg.file != RegFile::Addr {
                return Err(VerifyError {
                    message: "mova destination is not an address register".to_owned(),
                });
            }
            verify_src(src, stage)?;
            verify_modifiers(modifiers)?;
        }
        IrOp::Add {
            dst: _,
            src0,
            src1,
            modifiers,
        }
        | IrOp::Sub {
            dst: _,
            src0,
            src1,
            modifiers,
        }
        | IrOp::Mul {
            dst: _,
            src0,
            src1,
            modifiers,
        }
        | IrOp::Min {
            dst: _,
            src0,
            src1,
            modifiers,
        }
        | IrOp::Max {
            dst: _,
            src0,
            src1,
            modifiers,
        }
        | IrOp::Dp2 {
            dst: _,
            src0,
            src1,
            modifiers,
        }
        | IrOp::Dp3 {
            dst: _,
            src0,
            src1,
            modifiers,
        }
        | IrOp::Dp4 {
            dst: _,
            src0,
            src1,
            modifiers,
        }
        | IrOp::Dst {
            dst: _,
            src0,
            src1,
            modifiers,
        }
        | IrOp::Crs {
            dst: _,
            src0,
            src1,
            modifiers,
        }
        | IrOp::SetCmp {
            op: _,
            dst: _,
            src0,
            src1,
            modifiers,
        }
        | IrOp::Pow {
            dst: _,
            src0,
            src1,
            modifiers,
        }
        | IrOp::MatrixMul {
            dst: _,
            src0,
            src1,
            m: _,
            n: _,
            modifiers,
        } => {
            verify_src(src0, stage)?;
            verify_src(src1, stage)?;
            verify_modifiers(modifiers)?;
        }
        IrOp::Select {
            dst: _,
            cond,
            src_ge,
            src_lt,
            modifiers,
        } => {
            verify_src(cond, stage)?;
            verify_src(src_ge, stage)?;
            verify_src(src_lt, stage)?;
            verify_modifiers(modifiers)?;
        }
        IrOp::Dp2Add {
            dst: _,
            src0,
            src1,
            src2,
            modifiers,
        } => {
            verify_src(src0, stage)?;
            verify_src(src1, stage)?;
            verify_src(src2, stage)?;
            verify_modifiers(modifiers)?;
        }
        IrOp::Mad {
            dst: _,
            src0,
            src1,
            src2,
            modifiers,
        } => {
            verify_src(src0, stage)?;
            verify_src(src1, stage)?;
            verify_src(src2, stage)?;
            verify_modifiers(modifiers)?;
        }
        IrOp::Lrp {
            dst: _,
            src0,
            src1,
            src2,
            modifiers,
        } => {
            verify_src(src0, stage)?;
            verify_src(src1, stage)?;
            verify_src(src2, stage)?;
            verify_modifiers(modifiers)?;
        }
        IrOp::SinCos {
            dst: _,
            src,
            src1,
            src2,
            modifiers,
        } => {
            verify_src(src, stage)?;
            if let Some(src1) = src1 {
                verify_src(src1, stage)?;
            }
            if let Some(src2) = src2 {
                verify_src(src2, stage)?;
            }
            verify_modifiers(modifiers)?;
        }
        IrOp::TexSample {
            kind,
            dst: _,
            coord,
            ddx,
            ddy,
            sampler: _,
            modifiers,
        } => {
            if *kind == TexSampleKind::Grad && stage != ShaderStage::Pixel {
                return Err(VerifyError {
                    message: "texldd/Grad texture sampling is only valid in pixel shaders"
                        .to_owned(),
                });
            }
            if *kind == TexSampleKind::Bias && stage != ShaderStage::Pixel {
                return Err(VerifyError {
                    message: "texldb/Bias texture sampling is only valid in pixel shaders"
                        .to_owned(),
                });
            }
            verify_src(coord, stage)?;
            match kind {
                TexSampleKind::Grad => {
                    let ddx = ddx.as_ref().ok_or_else(|| VerifyError {
                        message: "texldd/Grad texture sampling is missing ddx operand".to_owned(),
                    })?;
                    let ddy = ddy.as_ref().ok_or_else(|| VerifyError {
                        message: "texldd/Grad texture sampling is missing ddy operand".to_owned(),
                    })?;
                    verify_src(ddx, stage)?;
                    verify_src(ddy, stage)?;
                }
                TexSampleKind::ImplicitLod { .. }
                | TexSampleKind::Bias
                | TexSampleKind::ExplicitLod => {
                    if ddx.is_some() || ddy.is_some() {
                        return Err(VerifyError {
                            message: "non-gradient texture sampling includes gradient operands"
                                .to_owned(),
                        });
                    }
                }
            }
            verify_modifiers(modifiers)?;
        }
    }
    Ok(())
}

fn verify_modifiers(mods: &crate::sm3::ir::InstModifiers) -> Result<(), VerifyError> {
    if let ResultShift::Unknown(v) = mods.shift {
        return Err(VerifyError {
            message: format!("unknown result shift modifier value {v}"),
        });
    }
    if let Some(pred) = &mods.predicate {
        if pred.reg.file != crate::sm3::ir::RegFile::Predicate {
            return Err(VerifyError {
                message: "predicate modifier refers to non-predicate register".to_owned(),
            });
        }
    }
    Ok(())
}

fn verify_cond(cond: &Cond, stage: ShaderStage) -> Result<(), VerifyError> {
    match cond {
        Cond::NonZero { src } => verify_src(src, stage),
        Cond::Compare { op, src0, src1 } => {
            if matches!(op, CompareOp::Unknown(_)) {
                return Err(VerifyError {
                    message: format!("unknown comparison op in condition: {op:?}"),
                });
            }
            verify_src(src0, stage)?;
            verify_src(src1, stage)?;
            Ok(())
        }
        Cond::Predicate { pred } => {
            if pred.reg.file != crate::sm3::ir::RegFile::Predicate {
                return Err(VerifyError {
                    message: "condition predicate refers to non-predicate register".to_owned(),
                });
            }
            Ok(())
        }
    }
}

fn verify_src(src: &Src, stage: ShaderStage) -> Result<(), VerifyError> {
    // Sampler registers (`s#`) are not general-purpose registers: in D3D9 they may only appear as
    // the sampler operand of texture sampling instructions. The IR builder extracts sampler
    // operands into `IrOp::TexSample::sampler`, so any remaining sampler references indicate
    // malformed bytecode.
    if src.reg.file == RegFile::Sampler {
        return Err(VerifyError {
            message: "sampler register used as a source operand".to_owned(),
        });
    }
    // Label registers (`l#`) are only meaningful as operands to `call`/`label` instructions, which
    // the IR builder lowers into structured call statements. Any remaining label references would
    // result in invalid WGSL (labels are not runtime registers) and indicate malformed bytecode.
    if src.reg.file == RegFile::Label {
        return Err(VerifyError {
            message: "label register used as a source operand".to_owned(),
        });
    }
    if matches!(src.modifier, SrcModifier::Unknown(_)) {
        return Err(VerifyError {
            message: "unknown source modifier in IR".to_owned(),
        });
    }
    if src.reg.file == RegFile::MiscType {
        if stage != ShaderStage::Pixel {
            return Err(VerifyError {
                message: "MiscType (vPos/vFace) inputs are only supported in pixel shaders"
                    .to_owned(),
            });
        }
        if src.reg.index > 1 {
            return Err(VerifyError {
                message: format!(
                    "unsupported MiscType input misc{} (only misc0=vPos and misc1=vFace are supported)",
                    src.reg.index
                ),
            });
        }
    }
    Ok(())
}

fn verify_dst(dst: &Dst) -> Result<(), VerifyError> {
    // Reject writes to register files that are not writable in D3D9 SM2/SM3 and/or would produce
    // invalid WGSL output.
    if matches!(
        dst.reg.file,
        // Sampler registers (`s#`) are not writable.
        RegFile::Sampler
        // Constant registers (`c#`) are read-only at runtime (only `def` writes them, and those are
        // extracted into `ShaderIr.const_defs_*` rather than emitted as ops).
        | RegFile::Const
        // `defi`/`defb` constant registers are also read-only at runtime.
        | RegFile::ConstInt
        | RegFile::ConstBool
        // Input registers (`v#` / `t#` / MISCTYPE) are read-only at runtime.
        | RegFile::Input
        | RegFile::Texture
        | RegFile::MiscType
        // Label registers are not runtime storage.
        | RegFile::Label
    ) {
        return Err(VerifyError {
            message: format!("{:?} register used as a destination operand", dst.reg.file),
        });
    }
    Ok(())
}

fn is_int_reg_file(file: RegFile) -> bool {
    matches!(
        file,
        RegFile::Addr | RegFile::ConstInt | RegFile::Loop | RegFile::Label
    )
}

fn op_dst(op: &IrOp) -> &Dst {
    match op {
        IrOp::Mov { dst, .. }
        | IrOp::Mova { dst, .. }
        | IrOp::Add { dst, .. }
        | IrOp::Sub { dst, .. }
        | IrOp::Mul { dst, .. }
        | IrOp::Mad { dst, .. }
        | IrOp::Lrp { dst, .. }
        | IrOp::Dp2 { dst, .. }
        | IrOp::Dp2Add { dst, .. }
        | IrOp::Dp3 { dst, .. }
        | IrOp::Dp4 { dst, .. }
        | IrOp::Min { dst, .. }
        | IrOp::Max { dst, .. }
        | IrOp::MatrixMul { dst, .. }
        | IrOp::Rcp { dst, .. }
        | IrOp::Rsq { dst, .. }
        | IrOp::Frc { dst, .. }
        | IrOp::Abs { dst, .. }
        | IrOp::Dst { dst, .. }
        | IrOp::Crs { dst, .. }
        | IrOp::Sgn { dst, .. }
        | IrOp::Nrm { dst, .. }
        | IrOp::Lit { dst, .. }
        | IrOp::SinCos { dst, .. }
        | IrOp::Exp { dst, .. }
        | IrOp::Log { dst, .. }
        | IrOp::Ddx { dst, .. }
        | IrOp::Ddy { dst, .. }
        | IrOp::SetCmp { dst, .. }
        | IrOp::Select { dst, .. }
        | IrOp::Pow { dst, .. }
        | IrOp::TexSample { dst, .. } => dst,
    }
}
