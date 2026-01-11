use std::collections::HashMap;

use crate::t2_ir::{BinOp, Instr, Operand, TraceIr, TraceKind, ValueId, REG_COUNT};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum ExprKey {
    Const(u64),
    Bin {
        op: BinOp,
        lhs: Operand,
        rhs: Operand,
    },
    Addr {
        base: Operand,
        index: Operand,
        scale: u8,
        disp: i64,
    },
}

fn operand_key(op: Operand) -> (u8, u64) {
    match op {
        Operand::Const(c) => (0, c),
        Operand::Value(v) => (1, v.0 as u64),
    }
}

fn resolve_operand(mut op: Operand, repl: &HashMap<ValueId, Operand>) -> Operand {
    loop {
        match op {
            Operand::Const(_) => return op,
            Operand::Value(v) => match repl.get(&v).copied() {
                Some(next) => op = next,
                None => return Operand::Value(v),
            },
        }
    }
}

pub fn run(trace: &mut TraceIr) -> bool {
    let mut changed = false;
    let mut repl: HashMap<ValueId, Operand> = HashMap::new();
    let mut exprs: HashMap<ExprKey, ValueId> = HashMap::new();

    let mut reg_state: [Option<Operand>; REG_COUNT] = [None; REG_COUNT];

    let mut new_prologue = Vec::with_capacity(trace.prologue.len());
    let mut new_body = Vec::with_capacity(trace.body.len());

    let stop = process_list(
        &trace.prologue,
        &mut new_prologue,
        &mut repl,
        &mut exprs,
        &mut reg_state,
        &mut changed,
    );
    if stop {
        trace.prologue = new_prologue;
        trace.body.clear();
        trace.kind = TraceKind::Linear;
        return true;
    }

    if trace.kind == TraceKind::Loop {
        let written = trace.body_regs_written();
        for (idx, w) in written.into_iter().enumerate() {
            if w {
                reg_state[idx] = None;
            }
        }
    }

    let stop = process_list(
        &trace.body,
        &mut new_body,
        &mut repl,
        &mut exprs,
        &mut reg_state,
        &mut changed,
    );
    if stop {
        trace.kind = TraceKind::Linear;
    }

    if changed {
        trace.prologue = new_prologue;
        trace.body = new_body;
    }

    changed
}

fn process_list(
    input: &[Instr],
    output: &mut Vec<Instr>,
    repl: &mut HashMap<ValueId, Operand>,
    exprs: &mut HashMap<ExprKey, ValueId>,
    reg_state: &mut [Option<Operand>; REG_COUNT],
    changed: &mut bool,
) -> bool {
    for inst in input {
        match inst {
            Instr::Nop => *changed = true,
            Instr::SideExit { exit_rip } => {
                output.push(Instr::SideExit {
                    exit_rip: *exit_rip,
                });
                return true;
            }
            Instr::Const { dst, value } => {
                let key = ExprKey::Const(*value);
                if let Some(existing) = exprs.get(&key).copied() {
                    repl.insert(*dst, Operand::Value(existing));
                    *changed = true;
                    continue;
                }
                exprs.insert(key, *dst);
                output.push(inst.clone());
            }
            Instr::LoadReg { dst, reg } => {
                let idx = reg.as_u8() as usize;
                if let Some(current) = reg_state[idx] {
                    repl.insert(*dst, current);
                    *changed = true;
                    continue;
                }
                output.push(inst.clone());
                reg_state[idx] = Some(Operand::Value(*dst));
            }
            Instr::StoreReg { reg, src } => {
                let src2 = resolve_operand(*src, repl);
                if src2 != *src {
                    *changed = true;
                }
                output.push(Instr::StoreReg {
                    reg: *reg,
                    src: src2,
                });
                reg_state[reg.as_u8() as usize] = Some(src2);
                reg_state[reg.as_u8() as usize] = Some(src2);
            }
            Instr::LoadMem { dst, addr, width } => {
                let addr2 = resolve_operand(*addr, repl);
                if addr2 != *addr {
                    *changed = true;
                }
                output.push(Instr::LoadMem {
                    dst: *dst,
                    addr: addr2,
                    width: *width,
                });
                // Treat memory operations as barriers (no expression CSE across them).
                exprs.clear();
            }
            Instr::StoreMem { addr, src, width } => {
                let addr2 = resolve_operand(*addr, repl);
                let src2 = resolve_operand(*src, repl);
                if addr2 != *addr || src2 != *src {
                    *changed = true;
                }
                output.push(Instr::StoreMem {
                    addr: addr2,
                    src: src2,
                    width: *width,
                });
                // Treat memory operations as barriers (no expression CSE across them).
                exprs.clear();
            }
            Instr::LoadFlag { dst, flag } => {
                output.push(Instr::LoadFlag {
                    dst: *dst,
                    flag: *flag,
                });
            }
            Instr::SetFlags { mask, values } => output.push(Instr::SetFlags {
                mask: *mask,
                values: *values,
            }),
            Instr::BinOp {
                dst,
                op,
                lhs,
                rhs,
                flags,
            } => {
                let mut lhs2 = resolve_operand(*lhs, repl);
                let mut rhs2 = resolve_operand(*rhs, repl);
                if op.is_commutative() && operand_key(lhs2) > operand_key(rhs2) {
                    std::mem::swap(&mut lhs2, &mut rhs2);
                }

                if !flags.is_empty() {
                    if lhs2 != *lhs || rhs2 != *rhs {
                        *changed = true;
                    }
                    output.push(Instr::BinOp {
                        dst: *dst,
                        op: *op,
                        lhs: lhs2,
                        rhs: rhs2,
                        flags: *flags,
                    });
                    continue;
                }

                let key = ExprKey::Bin {
                    op: *op,
                    lhs: lhs2,
                    rhs: rhs2,
                };
                if let Some(existing) = exprs.get(&key).copied() {
                    repl.insert(*dst, Operand::Value(existing));
                    *changed = true;
                    continue;
                }
                exprs.insert(key, *dst);
                if lhs2 != *lhs || rhs2 != *rhs {
                    *changed = true;
                }
                output.push(Instr::BinOp {
                    dst: *dst,
                    op: *op,
                    lhs: lhs2,
                    rhs: rhs2,
                    flags: *flags,
                });
            }
            Instr::Addr {
                dst,
                base,
                index,
                scale,
                disp,
            } => {
                let base2 = resolve_operand(*base, repl);
                let index2 = resolve_operand(*index, repl);
                let key = ExprKey::Addr {
                    base: base2,
                    index: index2,
                    scale: *scale,
                    disp: *disp,
                };
                if let Some(existing) = exprs.get(&key).copied() {
                    repl.insert(*dst, Operand::Value(existing));
                    *changed = true;
                    continue;
                }
                exprs.insert(key, *dst);
                if base2 != *base || index2 != *index {
                    *changed = true;
                }
                output.push(Instr::Addr {
                    dst: *dst,
                    base: base2,
                    index: index2,
                    scale: *scale,
                    disp: *disp,
                });
            }
            Instr::Guard {
                cond,
                expected,
                exit_rip,
            } => {
                let cond2 = resolve_operand(*cond, repl);
                if cond2 != *cond {
                    *changed = true;
                }
                output.push(Instr::Guard {
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
                output.push(Instr::GuardCodeVersion {
                    page: *page,
                    expected: *expected,
                    exit_rip: *exit_rip,
                });
            }
        }
    }
    false
}
