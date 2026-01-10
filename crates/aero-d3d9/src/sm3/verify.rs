use crate::sm3::decode::{ResultShift, SrcModifier};
use crate::sm3::ir::{Block, CompareOp, Cond, IrOp, ShaderIr, Src, Stmt};

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
    verify_block(&ir.body)
}

fn verify_block(block: &Block) -> Result<(), VerifyError> {
    for stmt in &block.stmts {
        match stmt {
            Stmt::Op(op) => verify_op(op)?,
            Stmt::If {
                cond,
                then_block,
                else_block,
            } => {
                verify_cond(cond)?;
                verify_block(then_block)?;
                if let Some(else_block) = else_block {
                    verify_block(else_block)?;
                }
            }
            Stmt::Loop { body } => verify_block(body)?,
            Stmt::Break => {}
            Stmt::BreakIf { cond } => verify_cond(cond)?,
            Stmt::Discard { src } => verify_src(src)?,
        }
    }
    Ok(())
}

fn verify_op(op: &IrOp) -> Result<(), VerifyError> {
    match op {
        IrOp::Mov { dst: _, src, modifiers }
        | IrOp::Rcp { dst: _, src, modifiers }
        | IrOp::Rsq { dst: _, src, modifiers } => {
            verify_src(src)?;
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
        | IrOp::Cmp {
            op: _,
            dst: _,
            src0,
            src1,
            modifiers,
        } => {
            verify_src(src0)?;
            verify_src(src1)?;
            verify_modifiers(modifiers)?;
        }
        IrOp::Mad {
            dst: _,
            src0,
            src1,
            src2,
            modifiers,
        } => {
            verify_src(src0)?;
            verify_src(src1)?;
            verify_src(src2)?;
            verify_modifiers(modifiers)?;
        }
        IrOp::TexSample {
            kind: _,
            dst: _,
            coord,
            ddx,
            ddy,
            sampler: _,
            modifiers,
        } => {
            verify_src(coord)?;
            if let Some(ddx) = ddx {
                verify_src(ddx)?;
            }
            if let Some(ddy) = ddy {
                verify_src(ddy)?;
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

fn verify_cond(cond: &Cond) -> Result<(), VerifyError> {
    match cond {
        Cond::NonZero { src } => verify_src(src),
        Cond::Compare { op, src0, src1 } => {
            if matches!(op, CompareOp::Unknown(_)) {
                return Err(VerifyError {
                    message: format!("unknown comparison op in condition: {op:?}"),
                });
            }
            verify_src(src0)?;
            verify_src(src1)?;
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

fn verify_src(src: &Src) -> Result<(), VerifyError> {
    if matches!(src.modifier, SrcModifier::Unknown(_)) {
        return Err(VerifyError {
            message: "unknown source modifier in IR".to_owned(),
        });
    }
    Ok(())
}

