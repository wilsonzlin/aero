use std::collections::HashMap;

use crate::t2_ir::{eval_binop, Instr, Operand, TraceIr, ValueId};

fn resolve_const(op: Operand, consts: &HashMap<ValueId, u64>) -> Operand {
    match op {
        Operand::Value(v) => consts.get(&v).copied().map_or(op, Operand::Const),
        Operand::Const(_) => op,
    }
}

pub fn run(trace: &mut TraceIr) -> bool {
    let mut changed = false;
    let mut consts: HashMap<ValueId, u64> = HashMap::new();
    let mut addr_defs: HashMap<ValueId, (Operand, Operand, u8, i64)> = HashMap::new();

    for inst in trace.iter_instrs_mut() {
        match inst {
            Instr::Const { dst, value } => {
                consts.insert(*dst, *value);
            }
            Instr::BinOp {
                dst,
                op,
                lhs,
                rhs,
                flags,
            } if flags.is_empty() => {
                let dst = *dst;
                let op = *op;
                let lhs2 = resolve_const(*lhs, &consts);
                let rhs2 = resolve_const(*rhs, &consts);
                if lhs2 != *lhs || rhs2 != *rhs {
                    changed = true;
                    *lhs = lhs2;
                    *rhs = rhs2;
                }
                if let (Operand::Const(a), Operand::Const(b)) = (lhs2, rhs2) {
                    let (res, _) = eval_binop(op, a, b);
                    *inst = Instr::Const { dst, value: res };
                    consts.insert(dst, res);
                    changed = true;
                }
            }
            Instr::Addr {
                dst,
                base,
                index,
                scale,
                disp,
            } => {
                let mut base2 = resolve_const(*base, &consts);
                let mut index2 = resolve_const(*index, &consts);
                let mut scale2 = *scale;
                let mut disp2 = *disp;

                if scale2 == 0 {
                    scale2 = 1;
                    index2 = Operand::Const(0);
                }

                if let Operand::Value(v) = base2 {
                    if let Some((inner_base, inner_index, inner_scale, inner_disp)) =
                        addr_defs.get(&v).copied()
                    {
                        if matches!(inner_index, Operand::Const(0)) || inner_scale == 0 {
                            base2 = inner_base;
                            disp2 = disp2.wrapping_add(inner_disp);
                            changed = true;
                        }
                    }
                }

                if let Operand::Const(idx) = index2 {
                    let folded = (idx.wrapping_mul(scale2 as u64)).wrapping_add(disp2 as u64);
                    disp2 = folded as i64;
                    index2 = Operand::Const(0);
                    scale2 = 1;
                    changed = true;
                }

                if let Operand::Const(b) = base2 {
                    let folded = b.wrapping_add(disp2 as u64);
                    disp2 = folded as i64;
                    base2 = Operand::Const(0);
                    changed = true;
                }

                if let (Operand::Const(b), Operand::Const(i)) = (base2, index2) {
                    let addr = b
                        .wrapping_add(i.wrapping_mul(scale2 as u64))
                        .wrapping_add(disp2 as u64);
                    let dst = *dst;
                    *inst = Instr::Const { dst, value: addr };
                    consts.insert(dst, addr);
                    addr_defs.remove(&dst);
                    changed = true;
                    continue;
                }

                if *base != base2 || *index != index2 || *scale != scale2 || *disp != disp2 {
                    *base = base2;
                    *index = index2;
                    *scale = scale2;
                    *disp = disp2;
                    changed = true;
                }

                addr_defs.insert(*dst, (base2, index2, scale2, disp2));
            }
            _ => {}
        }
    }

    changed
}
