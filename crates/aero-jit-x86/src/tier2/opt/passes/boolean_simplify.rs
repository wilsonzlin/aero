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

fn is_bool_operand(op: Operand, bool_values: &HashSet<ValueId>) -> bool {
    match op {
        Operand::Const(c) => c == 0 || c == 1,
        Operand::Value(v) => bool_values.contains(&v),
    }
}

fn eq_const(lhs: Operand, rhs: Operand) -> Option<(Operand, u64)> {
    match (lhs, rhs) {
        (Operand::Const(c), other) => Some((other, c)),
        (other, Operand::Const(c)) => Some((other, c)),
        _ => None,
    }
}

fn eq_zero_other(lhs: Operand, rhs: Operand) -> Option<Operand> {
    match eq_const(lhs, rhs) {
        Some((other, 0)) => Some(other),
        _ => None,
    }
}

pub fn run(trace: &mut TraceIr) -> bool {
    let mut changed = false;
    let uses = use_counts(trace);

    // Track simple facts while scanning top-to-bottom.
    let mut consts: HashMap<ValueId, u64> = HashMap::new();
    let mut bool_values: HashSet<ValueId> = HashSet::new();

    // For values produced by `Eq(x, 0)` (either operand order), record `x`.
    let mut eq_zero: HashMap<ValueId, Operand> = HashMap::new();
    // For values produced by `LtU(0, x)`, record `x` (i.e. value is `x != 0`).
    let mut ltu_zero: HashMap<ValueId, Operand> = HashMap::new();
    // For boolean values produced by NOT-like ops (`!b`), record `b`.
    //
    // This is used to eliminate redundant boolean negations and to simplify guards that
    // accidentally materialize a negation as a standalone value.
    let mut not_bool: HashMap<ValueId, Operand> = HashMap::new();

    for inst in trace.iter_instrs_mut() {
        let old = *inst;
        let mut new = old;

        match old {
            Instr::Guard {
                cond,
                expected,
                exit_rip,
            } => {
                // Simplify `Guard` based on already-discovered facts about `cond`.
                let mut cond2 = cond;
                let mut expected2 = expected;

                // Resolve trivial constants.
                if let Operand::Value(v) = cond2 {
                    if let Some(c) = consts.get(&v).copied() {
                        cond2 = Operand::Const(c);
                    }
                }

                // Canonicalize guards on comparisons-to-zero:
                //   guard( (x == 0), expected )  => guard(x, !expected)
                //   guard( (x != 0), expected )  => guard(x, expected)
                if let Operand::Value(v) = cond2 {
                    if let Some(x) = eq_zero.get(&v).copied() {
                        cond2 = x;
                        expected2 = !expected2;
                    } else if let Some(x) = ltu_zero.get(&v).copied() {
                        cond2 = x;
                    } else if let Some(x) = not_bool.get(&v).copied() {
                        cond2 = x;
                        expected2 = !expected2;
                    }
                }

                // Re-resolve constants after rewriting.
                if let Operand::Value(v) = cond2 {
                    if let Some(c) = consts.get(&v).copied() {
                        cond2 = Operand::Const(c);
                    }
                }

                match cond2 {
                    Operand::Const(c) => {
                        // Fold constant guards.
                        let cond_bool = c != 0;
                        if cond_bool == expected2 {
                            new = Instr::Nop;
                        } else {
                            new = Instr::SideExit { exit_rip };
                        }
                    }
                    _ => {
                        if cond2 != cond || expected2 != expected {
                            new = Instr::Guard {
                                cond: cond2,
                                expected: expected2,
                                exit_rip,
                            };
                        }
                    }
                }
            }

            Instr::BinOp {
                dst,
                op,
                lhs,
                rhs,
                flags,
            } if flags.is_empty() => {
                // Simplify boolean patterns for pure binops.
                match op {
                    BinOp::Eq => {
                        // If this is an `Eq(x, 0)` form (either operand order), it's either:
                        // - a boolean NOT if `x` is already known boolean, or
                        // - a numeric `x == 0` test for non-boolean values.
                        //
                        // We only apply boolean rewrites when `x` is known boolean.

                        // 1) Canonicalize `Eq(Eq(x,0),0)` into `x != 0` (`LtU(0,x)`),
                        // and further into `x` if `x` is already boolean.
                        if let Some(Operand::Value(inner)) = eq_zero_other(lhs, rhs) {
                            if let Some(x) = eq_zero.get(&inner).copied() {
                                if is_bool_operand(x, &bool_values) {
                                    // `x != 0` is just `x` when `x` is boolean.
                                    //
                                    // Emit an algebraically simplifiable op so `const_fold`
                                    // can turn this into a pure replacement.
                                    new = Instr::BinOp {
                                        dst,
                                        op: BinOp::Xor,
                                        lhs: x,
                                        rhs: Operand::Const(0),
                                        flags: FlagSet::EMPTY,
                                    };
                                } else {
                                    // Prefer `!(x == 0)` expressed as `xor(inner, 1)` when the
                                    // `inner = (x == 0)` value is used elsewhere (e.g. select
                                    // lowering uses both `inner` and its negation).
                                    //
                                    // Otherwise, canonicalize to `x != 0` (`lt_u(0, x)`) so DCE
                                    // can remove the `inner` comparison entirely.
                                    let inner_uses = uses.get(&inner).copied().unwrap_or(0);
                                    if inner_uses > 1 {
                                        new = Instr::BinOp {
                                            dst,
                                            op: BinOp::Xor,
                                            lhs: Operand::Value(inner),
                                            rhs: Operand::Const(1),
                                            flags: FlagSet::EMPTY,
                                        };
                                    } else {
                                        new = Instr::BinOp {
                                            dst,
                                            op: BinOp::LtU,
                                            lhs: Operand::Const(0),
                                            rhs: x,
                                            flags: FlagSet::EMPTY,
                                        };
                                    }
                                }
                            } else if let Some(x) = not_bool.get(&inner).copied() {
                                // `Eq(!b, 0)` == `b` (double negation) for boolean `b`.
                                new = Instr::BinOp {
                                    dst,
                                    op: BinOp::Xor,
                                    lhs: x,
                                    rhs: Operand::Const(0),
                                    flags: FlagSet::EMPTY,
                                };
                            }
                        }

                        // 2) Rewrite boolean NOT: `Eq(b, 0)` => `Xor(b, 1)` when `b` is boolean.
                        if new == old {
                            if let Some(other) = eq_zero_other(lhs, rhs) {
                                // Special-case `Eq(LtU(0, x), 0)` => `Eq(x, 0)` so we don't
                                // materialize a `!= 0` value only to negate it again.
                                if let Operand::Value(inner) = other {
                                    if let Some(x) = ltu_zero.get(&inner).copied() {
                                        new = Instr::BinOp {
                                            dst,
                                            op: BinOp::Eq,
                                            lhs: x,
                                            rhs: Operand::Const(0),
                                            flags: FlagSet::EMPTY,
                                        };
                                    }
                                }

                                if new == old && is_bool_operand(other, &bool_values) {
                                    new = Instr::BinOp {
                                        dst,
                                        op: BinOp::Xor,
                                        lhs: other,
                                        rhs: Operand::Const(1),
                                        flags: FlagSet::EMPTY,
                                    };
                                }
                            }
                        }

                        // 3) `Eq(b, 1)` is redundant for boolean values: it is just `b`.
                        if new == old {
                            if let Some((other, 1)) = eq_const(lhs, rhs) {
                                if is_bool_operand(other, &bool_values) {
                                    new = Instr::BinOp {
                                        dst,
                                        op: BinOp::Xor,
                                        lhs: other,
                                        rhs: Operand::Const(0),
                                        flags: FlagSet::EMPTY,
                                    };
                                }
                            }
                        }
                    }
                    BinOp::Xor => {
                        // `Xor(!b, 1)` is a redundant double-negation for boolean `b`.
                        if let Some((Operand::Value(inner), 1)) = eq_const(lhs, rhs) {
                            if let Some(x) = not_bool.get(&inner).copied() {
                                if is_bool_operand(x, &bool_values) {
                                    new = Instr::BinOp {
                                        dst,
                                        op: BinOp::Xor,
                                        lhs: x,
                                        rhs: Operand::Const(0),
                                        flags: FlagSet::EMPTY,
                                    };
                                }
                            }
                        }
                    }
                    BinOp::LtU => {
                        // `LtU(0, b)` is redundant for boolean `b` and equals `b`.
                        if lhs == Operand::Const(0) && is_bool_operand(rhs, &bool_values) {
                            new = Instr::BinOp {
                                dst,
                                op: BinOp::Xor,
                                lhs: rhs,
                                rhs: Operand::Const(0),
                                flags: FlagSet::EMPTY,
                            };
                        }

                        // For boolean `b`, `LtU(b, 1)` is `b == 0` (NOT).
                        if new == old
                            && rhs == Operand::Const(1)
                            && is_bool_operand(lhs, &bool_values)
                        {
                            new = Instr::BinOp {
                                dst,
                                op: BinOp::Xor,
                                lhs,
                                rhs: Operand::Const(1),
                                flags: FlagSet::EMPTY,
                            };
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }

        if new != old {
            *inst = new;
            changed = true;
        }

        // Update fact database based on the (possibly rewritten) instruction.
        match *inst {
            Instr::Const { dst, value } => {
                consts.insert(dst, value);
                if value == 0 || value == 1 {
                    bool_values.insert(dst);
                }
            }
            Instr::LoadFlag { dst, .. } => {
                bool_values.insert(dst);
            }
            Instr::BinOp {
                dst,
                op,
                lhs,
                rhs,
                flags,
            } => {
                if op == BinOp::Eq
                    || op == BinOp::LtU
                    || (matches!(op, BinOp::And | BinOp::Or | BinOp::Xor)
                        && is_bool_operand(lhs, &bool_values)
                        && is_bool_operand(rhs, &bool_values))
                {
                    bool_values.insert(dst);
                }

                if flags.is_empty() {
                    if op == BinOp::Eq {
                        if let Some(other) = eq_zero_other(lhs, rhs) {
                            eq_zero.insert(dst, other);
                            if is_bool_operand(other, &bool_values) {
                                not_bool.insert(dst, other);
                            }
                        }
                    } else if op == BinOp::LtU {
                        if lhs == Operand::Const(0) {
                            ltu_zero.insert(dst, rhs);
                        }
                    } else if op == BinOp::Xor {
                        if let Some((other, 1)) = eq_const(lhs, rhs) {
                            if is_bool_operand(other, &bool_values) {
                                not_bool.insert(dst, other);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    changed
}
