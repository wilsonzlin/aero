use std::collections::HashMap;

use crate::tier2::ir::{BinOp, Instr, Operand, TraceIr, ValueId};

pub fn run(trace: &mut TraceIr) -> bool {
    let mut changed = false;
    let mut addr_defs: HashMap<ValueId, (Operand, Operand, u8, i64)> = HashMap::new();

    for inst in trace.iter_instrs_mut() {
        // Maintain Addr definitions seen so far, so we can fold Add/Sub constants into existing
        // address computations (disp updates).
        if let Some(dst) = inst.dst() {
            if !matches!(*inst, Instr::Addr { .. }) {
                addr_defs.remove(&dst);
            }
        }

        match *inst {
            Instr::Addr {
                dst,
                base,
                index,
                scale,
                disp,
            } => {
                addr_defs.insert(dst, (base, index, scale, disp));
            }
            Instr::BinOp {
                dst,
                op: BinOp::Mul,
                lhs,
                rhs,
                flags,
            } => {
                // Keep semantics simple: only rewrite pure arithmetic (no flag updates).
                if !flags.is_empty() {
                    continue;
                }

                let (x, c) = match (lhs, rhs) {
                    (Operand::Const(c), x) | (x, Operand::Const(c)) => (x, c),
                    _ => continue,
                };

                if !c.is_power_of_two() {
                    continue;
                }

                let sh = c.trailing_zeros() as u64;
                *inst = Instr::BinOp {
                    dst,
                    op: BinOp::Shl,
                    lhs: x,
                    rhs: Operand::Const(sh),
                    flags,
                };
                changed = true;
            }
            Instr::BinOp {
                dst,
                op: BinOp::Add,
                lhs,
                rhs,
                flags,
            } => {
                if !flags.is_empty() {
                    continue;
                }

                let (x, c) = match (lhs, rhs) {
                    (Operand::Const(c), x) | (x, Operand::Const(c)) => (x, c),
                    _ => continue,
                };

                if let Operand::Value(v) = x {
                    if let Some((base, index, scale, disp)) = addr_defs.get(&v).copied() {
                        let new_disp = ((disp as u64).wrapping_add(c)) as i64;
                        *inst = Instr::Addr {
                            dst,
                            base,
                            index,
                            scale,
                            disp: new_disp,
                        };
                        addr_defs.insert(dst, (base, index, scale, new_disp));
                        changed = true;
                        continue;
                    }
                }

                let disp = c as i64;
                *inst = Instr::Addr {
                    dst,
                    base: x,
                    index: Operand::Const(0),
                    scale: 1,
                    disp,
                };
                addr_defs.insert(dst, (x, Operand::Const(0), 1, disp));
                changed = true;
            }
            Instr::BinOp {
                dst,
                op: BinOp::Sub,
                lhs,
                rhs: Operand::Const(c),
                flags,
            } => {
                if !flags.is_empty() {
                    continue;
                }

                if let Operand::Value(v) = lhs {
                    if let Some((base, index, scale, disp)) = addr_defs.get(&v).copied() {
                        let new_disp = ((disp as u64).wrapping_sub(c)) as i64;
                        *inst = Instr::Addr {
                            dst,
                            base,
                            index,
                            scale,
                            disp: new_disp,
                        };
                        addr_defs.insert(dst, (base, index, scale, new_disp));
                        changed = true;
                        continue;
                    }
                }

                // x - c == x + (-c) in wrapping arithmetic.
                let disp = (0u64.wrapping_sub(c)) as i64;
                *inst = Instr::Addr {
                    dst,
                    base: lhs,
                    index: Operand::Const(0),
                    scale: 1,
                    disp,
                };
                addr_defs.insert(dst, (lhs, Operand::Const(0), 1, disp));
                changed = true;
            }
            _ => {}
        }
    }

    changed
}
