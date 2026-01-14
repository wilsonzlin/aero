use std::collections::{HashMap, HashSet};

use aero_types::FlagSet;

use crate::tier2::ir::{BinOp, Instr, Operand, TraceIr, ValueId};

fn use_counts(trace: &TraceIr) -> HashMap<ValueId, u32> {
    let mut uses: HashMap<ValueId, u32> = HashMap::new();
    for inst in trace.iter_instrs() {
        inst.for_each_operand(|op| {
            if let Operand::Value(v) = op {
                *uses.entry(v).or_insert(0) += 1;
            }
        });
    }
    uses
}

fn decode_bool_not(inst: &Instr) -> Option<(ValueId, ValueId)> {
    let Instr::BinOp {
        dst,
        op,
        lhs,
        rhs,
        flags,
    } = *inst
    else {
        return None;
    };
    if !flags.is_empty() {
        return None;
    }

    match op {
        BinOp::Eq => match (lhs, rhs) {
            (Operand::Value(v), Operand::Const(0)) | (Operand::Const(0), Operand::Value(v)) => {
                Some((dst, v))
            }
            _ => None,
        },
        BinOp::Xor => match (lhs, rhs) {
            (Operand::Value(v), Operand::Const(1)) | (Operand::Const(1), Operand::Value(v)) => {
                Some((dst, v))
            }
            _ => None,
        },
        _ => None,
    }
}

fn decode_mul_by_value(inst: &Instr, by: ValueId) -> Option<(ValueId, Operand)> {
    let Instr::BinOp {
        dst,
        op: BinOp::Mul,
        lhs,
        rhs,
        flags,
    } = *inst
    else {
        return None;
    };
    if !flags.is_empty() {
        return None;
    }

    match (lhs, rhs) {
        (Operand::Value(v), other) if v == by => Some((dst, other)),
        (other, Operand::Value(v)) if v == by => Some((dst, other)),
        _ => None,
    }
}

fn decode_add_pair(inst: &Instr, a: ValueId, b: ValueId) -> Option<ValueId> {
    let Instr::BinOp {
        dst,
        op: BinOp::Add,
        lhs,
        rhs,
        flags,
    } = *inst
    else {
        return None;
    };
    if !flags.is_empty() {
        return None;
    }

    match (lhs, rhs) {
        (Operand::Value(x), Operand::Value(y)) if (x == a && y == b) || (x == b && y == a) => {
            Some(dst)
        }
        _ => None,
    }
}

fn reduce_select_mul_pattern(
    instrs: &mut [Instr],
    uses: &HashMap<ValueId, u32>,
    bool_values: &HashSet<ValueId>,
) -> bool {
    // Pattern match the Tier-2 `Select` lowering sequence (after boolean_simplify may have run):
    //
    //   cond_bool = (cond_is_zero == 0) or (cond_is_zero ^ 1)
    //   then_val  = if_true  * cond_bool
    //   else_val  = if_false * cond_is_zero
    //   sum       = then_val + else_val
    //
    // and rewrite to avoid MUL:
    //   mask      = 0 - cond_is_zero                  // 0 or -1
    //   diff      = if_false - if_true
    //   masked    = diff & mask
    //   sum       = if_true + masked
    //
    // This keeps the same instruction count but replaces `i64.mul` with cheaper ops.
    let mut changed = false;
    let mut i = 0usize;
    while i + 3 < instrs.len() {
        let Some((cond_bool, cond_is_zero)) = decode_bool_not(&instrs[i]) else {
            i += 1;
            continue;
        };

        // `cond_is_zero` must be known boolean (0/1); otherwise `Eq(cond_is_zero, 0)` is a
        // comparison-to-zero test rather than a boolean NOT.
        if !bool_values.contains(&cond_is_zero) {
            i += 1;
            continue;
        }

        if uses.get(&cond_bool).copied().unwrap_or(0) != 1 {
            i += 1;
            continue;
        }

        let Some((then_dst, if_true)) = decode_mul_by_value(&instrs[i + 1], cond_bool) else {
            i += 1;
            continue;
        };
        let Some((else_dst, if_false)) = decode_mul_by_value(&instrs[i + 2], cond_is_zero) else {
            i += 1;
            continue;
        };

        if uses.get(&then_dst).copied().unwrap_or(0) != 1
            || uses.get(&else_dst).copied().unwrap_or(0) != 1
        {
            i += 1;
            continue;
        }

        let Some(sum_dst) = decode_add_pair(&instrs[i + 3], then_dst, else_dst) else {
            i += 1;
            continue;
        };

        // Rewrite the 4-instruction window in-place.
        instrs[i] = Instr::BinOp {
            dst: cond_bool,
            op: BinOp::Sub,
            lhs: Operand::Const(0),
            rhs: Operand::Value(cond_is_zero),
            flags: FlagSet::EMPTY,
        };
        instrs[i + 1] = Instr::BinOp {
            dst: then_dst,
            op: BinOp::Sub,
            lhs: if_false,
            rhs: if_true,
            flags: FlagSet::EMPTY,
        };
        instrs[i + 2] = Instr::BinOp {
            dst: else_dst,
            op: BinOp::And,
            lhs: Operand::Value(then_dst),
            rhs: Operand::Value(cond_bool),
            flags: FlagSet::EMPTY,
        };
        instrs[i + 3] = Instr::BinOp {
            dst: sum_dst,
            op: BinOp::Add,
            lhs: if_true,
            rhs: Operand::Value(else_dst),
            flags: FlagSet::EMPTY,
        };

        changed = true;
        i += 4;
    }

    changed
}

fn is_bool_operand(op: Operand, bool_values: &HashSet<ValueId>) -> bool {
    match op {
        Operand::Const(c) => c == 0 || c == 1,
        Operand::Value(v) => bool_values.contains(&v),
    }
}

fn compute_bool_values(trace: &TraceIr) -> HashSet<ValueId> {
    let mut bool_values = HashSet::new();
    for inst in trace.iter_instrs() {
        let Some(dst) = inst.dst() else { continue };
        let is_bool = match *inst {
            Instr::Const { value, .. } => value == 0 || value == 1,
            Instr::LoadFlag { .. } => true,
            Instr::BinOp { op, lhs, rhs, .. } => match op {
                BinOp::Eq | BinOp::LtU => true,
                BinOp::And | BinOp::Or | BinOp::Xor => {
                    is_bool_operand(lhs, &bool_values) && is_bool_operand(rhs, &bool_values)
                }
                _ => false,
            },
            _ => false,
        };
        if is_bool {
            bool_values.insert(dst);
        }
    }
    bool_values
}

pub fn run(trace: &mut TraceIr) -> bool {
    let mut changed = false;
    let mut addr_defs: HashMap<ValueId, (Operand, Operand, u8, i64)> = HashMap::new();
    // Track SSA values that are simple shifts by 0..3 (scales 1,2,4,8) so we can fold
    // `base + (x << k)` into an `Addr` with an index scale.
    let mut scaled_defs: HashMap<ValueId, (Operand, u8)> = HashMap::new();
    // Track `dst = x & mask` so we can collapse nested constant masks:
    //   (x & m1) & m2  =>  x & (m1 & m2)
    // This is especially common after lowering of narrow-width operations.
    let mut and_defs: HashMap<ValueId, (Operand, u64)> = HashMap::new();

    let bool_values_pre = compute_bool_values(trace);
    let uses = use_counts(trace);
    changed |= reduce_select_mul_pattern(&mut trace.prologue, &uses, &bool_values_pre);
    changed |= reduce_select_mul_pattern(&mut trace.body, &uses, &bool_values_pre);

    let bool_values = compute_bool_values(trace);

    for inst in trace.iter_instrs_mut() {
        // Maintain Addr definitions seen so far, so we can fold Add/Sub constants into existing
        // address computations (disp updates).
        if let Some(dst) = inst.dst() {
            // In well-formed traces ValueIds are SSA, but keep maps conservative in case a buggy
            // transform introduces redefinitions.
            scaled_defs.remove(&dst);
            and_defs.remove(&dst);
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

                // Multiplication of two boolean values (0/1) is equivalent to AND, which is
                // typically cheaper than a MUL in WASM.
                if is_bool_operand(lhs, &bool_values) && is_bool_operand(rhs, &bool_values) {
                    *inst = Instr::BinOp {
                        dst,
                        op: BinOp::And,
                        lhs,
                        rhs,
                        flags,
                    };
                    changed = true;
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
                if sh <= 3 {
                    scaled_defs.insert(dst, (x, 1u8 << (sh as u8)));
                }
                changed = true;
            }
            Instr::BinOp {
                dst,
                op: BinOp::Shl,
                lhs,
                rhs: Operand::Const(k),
                flags,
            } => {
                if flags.is_empty() {
                    let sh = (k & 63) as u8;
                    if sh <= 3 {
                        scaled_defs.insert(dst, (lhs, 1u8 << sh));
                    }
                }
            }
            Instr::BinOp {
                dst,
                op: BinOp::And,
                lhs,
                rhs,
                flags,
            } => {
                // Only rewrite pure arithmetic (no flag updates).
                if !flags.is_empty() {
                    continue;
                }

                // Only handle constant masks for now.
                let (x, mask) = match (lhs, rhs) {
                    (Operand::Const(mask), x) | (x, Operand::Const(mask)) => (x, mask),
                    _ => continue,
                };

                // Collapse nested constant masks: (x & m1) & m2 => x & (m1 & m2).
                let (base, new_mask) = match x {
                    Operand::Value(v) => match and_defs.get(&v).copied() {
                        Some((base, inner_mask)) => (base, inner_mask & mask),
                        None => (Operand::Value(v), mask),
                    },
                    _ => (x, mask),
                };

                if new_mask == 0 {
                    *inst = Instr::Const { dst, value: 0 };
                    changed = true;
                    continue;
                }

                if base != x || new_mask != mask {
                    *inst = Instr::BinOp {
                        dst,
                        op: BinOp::And,
                        lhs: base,
                        rhs: Operand::Const(new_mask),
                        flags,
                    };
                    changed = true;
                }

                and_defs.insert(dst, (base, new_mask));
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

                // Case 1: x + const => Addr-style add (base + disp), with folding into existing
                // Addr displacements when possible.
                if let Some((x, c)) = match (lhs, rhs) {
                    (Operand::Const(c), x) | (x, Operand::Const(c)) => Some((x, c)),
                    _ => None,
                } {
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

                        if let Some((inner, scale)) = scaled_defs.get(&v).copied() {
                            let disp = c as i64;
                            *inst = Instr::Addr {
                                dst,
                                base: Operand::Const(0),
                                index: inner,
                                scale,
                                disp,
                            };
                            addr_defs.insert(dst, (Operand::Const(0), inner, scale, disp));
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
                    continue;
                }

                // Case 2: base + (x << k) => Addr-style add (base + index*scale).
                let mut scaled: Option<(Operand, Operand, u8)> = None;
                if let Operand::Value(v) = lhs {
                    if let Some((inner, scale)) = scaled_defs.get(&v).copied() {
                        scaled = Some((rhs, inner, scale));
                    }
                }
                if scaled.is_none() {
                    if let Operand::Value(v) = rhs {
                        if let Some((inner, scale)) = scaled_defs.get(&v).copied() {
                            scaled = Some((lhs, inner, scale));
                        }
                    }
                }

                if let Some((base, index, scale)) = scaled {
                    *inst = Instr::Addr {
                        dst,
                        base,
                        index,
                        scale,
                        disp: 0,
                    };
                    addr_defs.insert(dst, (base, index, scale, 0));
                    changed = true;
                }
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

                    if let Some((inner, scale)) = scaled_defs.get(&v).copied() {
                        let disp = (0u64.wrapping_sub(c)) as i64;
                        *inst = Instr::Addr {
                            dst,
                            base: Operand::Const(0),
                            index: inner,
                            scale,
                            disp,
                        };
                        addr_defs.insert(dst, (Operand::Const(0), inner, scale, disp));
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
