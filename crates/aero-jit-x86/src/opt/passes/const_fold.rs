use std::collections::HashMap;

use crate::t2_ir::{eval_binop, BinOp, Instr, Operand, TraceIr, ValueId};

fn resolve_operand(
    mut op: Operand,
    repl: &HashMap<ValueId, Operand>,
    consts: &HashMap<ValueId, u64>,
) -> Operand {
    loop {
        match op {
            Operand::Const(_) => return op,
            Operand::Value(v) => {
                if let Some(rep) = repl.get(&v).copied() {
                    op = rep;
                    continue;
                }
                if let Some(c) = consts.get(&v).copied() {
                    return Operand::Const(c);
                }
                return Operand::Value(v);
            }
        }
    }
}

pub fn run(trace: &mut TraceIr) -> bool {
    let mut changed = false;
    let mut repl: HashMap<ValueId, Operand> = HashMap::new();
    let mut consts: HashMap<ValueId, u64> = HashMap::new();

    let mut new_prologue = Vec::with_capacity(trace.prologue.len());
    let mut new_body = Vec::with_capacity(trace.body.len());

    let mut stop = false;
    for inst in trace.prologue.iter() {
        if stop {
            changed = true;
            break;
        }
        stop |= fold_inst(
            inst,
            &mut new_prologue,
            &mut repl,
            &mut consts,
            &mut changed,
        );
    }

    if stop {
        trace.prologue = new_prologue;
        trace.body.clear();
        trace.kind = crate::t2_ir::TraceKind::Linear;
        return true;
    }

    for inst in trace.body.iter() {
        if stop {
            changed = true;
            break;
        }
        stop |= fold_inst(inst, &mut new_body, &mut repl, &mut consts, &mut changed);
    }

    if changed {
        trace.prologue = new_prologue;
        trace.body = new_body;
    }
    changed
}

fn fold_inst(
    inst: &Instr,
    out: &mut Vec<Instr>,
    repl: &mut HashMap<ValueId, Operand>,
    consts: &mut HashMap<ValueId, u64>,
    changed: &mut bool,
) -> bool {
    match inst {
        Instr::Nop => {
            *changed = true;
        }
        Instr::Const { dst, value } => {
            consts.insert(*dst, *value);
            out.push(inst.clone());
        }
        Instr::LoadReg { .. } | Instr::LoadFlag { .. } => {
            out.push(inst.clone());
        }
        Instr::LoadMem { dst, addr, width } => {
            let addr2 = resolve_operand(*addr, repl, consts);
            if addr2 != *addr {
                *changed = true;
            }
            out.push(Instr::LoadMem {
                dst: *dst,
                addr: addr2,
                width: *width,
            });
        }
        Instr::SetFlags { mask, values } => {
            if mask.is_empty() {
                *changed = true;
                return false;
            }
            out.push(Instr::SetFlags {
                mask: *mask,
                values: *values,
            });
        }
        Instr::StoreReg { reg, src } => {
            let src2 = resolve_operand(*src, repl, consts);
            if src2 != *src {
                *changed = true;
            }
            out.push(Instr::StoreReg {
                reg: *reg,
                src: src2,
            });
        }
        Instr::StoreMem { addr, src, width } => {
            let addr2 = resolve_operand(*addr, repl, consts);
            let src2 = resolve_operand(*src, repl, consts);
            if addr2 != *addr || src2 != *src {
                *changed = true;
            }
            out.push(Instr::StoreMem {
                addr: addr2,
                src: src2,
                width: *width,
            });
        }
        Instr::Addr {
            dst,
            base,
            index,
            scale,
            disp,
        } => {
            let base2 = resolve_operand(*base, repl, consts);
            let index2 = resolve_operand(*index, repl, consts);
            if let (Operand::Const(b), Operand::Const(i)) = (base2, index2) {
                let addr = b
                    .wrapping_add(i.wrapping_mul(*scale as u64))
                    .wrapping_add(*disp as u64);
                consts.insert(*dst, addr);
                out.push(Instr::Const {
                    dst: *dst,
                    value: addr,
                });
                *changed = true;
            } else {
                if base2 != *base || index2 != *index {
                    *changed = true;
                }
                out.push(Instr::Addr {
                    dst: *dst,
                    base: base2,
                    index: index2,
                    scale: *scale,
                    disp: *disp,
                });
            }
        }
        Instr::BinOp {
            dst,
            op,
            lhs,
            rhs,
            flags,
        } => {
            let lhs2 = resolve_operand(*lhs, repl, consts);
            let rhs2 = resolve_operand(*rhs, repl, consts);

            let (lhs2, rhs2) = if op.is_commutative() {
                match (lhs2, rhs2) {
                    (Operand::Const(_), Operand::Value(_)) => (rhs2, lhs2),
                    _ => (lhs2, rhs2),
                }
            } else {
                (lhs2, rhs2)
            };

            if flags.is_empty() {
                if let Some(replacement) = algebraic_simplify(*op, lhs2, rhs2) {
                    repl.insert(*dst, replacement);
                    *changed = true;
                    return false;
                }
            }

            if let (Operand::Const(a), Operand::Const(b)) = (lhs2, rhs2) {
                let (res, computed_flags) = eval_binop(*op, a, b);
                consts.insert(*dst, res);
                out.push(Instr::Const {
                    dst: *dst,
                    value: res,
                });
                if !flags.is_empty() {
                    out.push(Instr::SetFlags {
                        mask: *flags,
                        values: computed_flags,
                    });
                }
                *changed = true;
                return false;
            }

            if lhs2 != *lhs || rhs2 != *rhs {
                *changed = true;
            }
            out.push(Instr::BinOp {
                dst: *dst,
                op: *op,
                lhs: lhs2,
                rhs: rhs2,
                flags: *flags,
            });
        }
        Instr::Guard {
            cond,
            expected,
            exit_rip,
        } => {
            let cond2 = resolve_operand(*cond, repl, consts);
            if let Operand::Const(c) = cond2 {
                let cond_bool = c != 0;
                if cond_bool == *expected {
                    *changed = true;
                    return false;
                }
                out.push(Instr::SideExit {
                    exit_rip: *exit_rip,
                });
                *changed = true;
                return true;
            }
            if cond2 != *cond {
                *changed = true;
            }
            out.push(Instr::Guard {
                cond: cond2,
                expected: *expected,
                exit_rip: *exit_rip,
            });
        }
        Instr::GuardCodeVersion {
            page,
            expected,
            exit_rip,
        } => {
            out.push(Instr::GuardCodeVersion {
                page: *page,
                expected: *expected,
                exit_rip: *exit_rip,
            });
        }
        Instr::SideExit { exit_rip } => {
            out.push(Instr::SideExit {
                exit_rip: *exit_rip,
            });
            return true;
        }
    }
    false
}

fn algebraic_simplify(op: BinOp, lhs: Operand, rhs: Operand) -> Option<Operand> {
    match op {
        BinOp::Add => match (lhs, rhs) {
            (x, Operand::Const(0)) => Some(x),
            (Operand::Const(0), x) => Some(x),
            _ => None,
        },
        BinOp::Sub => match (lhs, rhs) {
            (x, Operand::Const(0)) => Some(x),
            (x, y) if x == y => Some(Operand::Const(0)),
            _ => None,
        },
        BinOp::Mul => match (lhs, rhs) {
            (_, Operand::Const(0)) | (Operand::Const(0), _) => Some(Operand::Const(0)),
            (x, Operand::Const(1)) => Some(x),
            (Operand::Const(1), x) => Some(x),
            _ => None,
        },
        BinOp::And => match (lhs, rhs) {
            (_, Operand::Const(0)) | (Operand::Const(0), _) => Some(Operand::Const(0)),
            (x, Operand::Const(u64::MAX)) => Some(x),
            (Operand::Const(u64::MAX), x) => Some(x),
            _ => None,
        },
        BinOp::Or => match (lhs, rhs) {
            (x, Operand::Const(0)) => Some(x),
            (Operand::Const(0), x) => Some(x),
            _ => None,
        },
        BinOp::Xor => match (lhs, rhs) {
            (x, Operand::Const(0)) => Some(x),
            (Operand::Const(0), x) => Some(x),
            (x, y) if x == y => Some(Operand::Const(0)),
            _ => None,
        },
        BinOp::Shl | BinOp::Shr => match rhs {
            Operand::Const(0) => Some(lhs),
            _ => None,
        },
        BinOp::Eq => (lhs == rhs).then_some(Operand::Const(1)),
        BinOp::LtU => (lhs == rhs).then_some(Operand::Const(0)),
    }
}
