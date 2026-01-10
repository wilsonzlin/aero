use std::collections::HashSet;

use crate::t2_ir::{Instr, Operand, TraceIr, TraceKind, ValueId, REG_COUNT};

pub fn run(trace: &mut TraceIr) -> bool {
    if trace.kind != TraceKind::Loop {
        return false;
    }

    let written = trace.body_regs_written();

    let mut invariant_values: HashSet<ValueId> =
        trace.prologue.iter().filter_map(|i| i.dst()).collect();

    let mut hoisted: Vec<Instr> = Vec::new();
    let mut new_body: Vec<Instr> = Vec::with_capacity(trace.body.len());

    for inst in trace.body.iter() {
        if is_hoistable(inst, &invariant_values, &written) {
            if let Some(dst) = inst.dst() {
                invariant_values.insert(dst);
            }
            hoisted.push(inst.clone());
        } else {
            new_body.push(inst.clone());
        }
    }

    if hoisted.is_empty() {
        return false;
    }

    trace.prologue.extend(hoisted);
    trace.body = new_body;
    true
}

fn is_hoistable(
    inst: &Instr,
    invariant_values: &HashSet<ValueId>,
    written_regs: &[bool; REG_COUNT],
) -> bool {
    if inst.has_side_effects() {
        return false;
    }
    match inst {
        Instr::Const { .. } => true,
        Instr::Addr { base, index, .. } => operands_invariant([*base, *index], invariant_values),
        Instr::BinOp {
            lhs, rhs, flags, ..
        } => {
            if !flags.is_empty() {
                return false;
            }
            operands_invariant([*lhs, *rhs], invariant_values)
        }
        Instr::LoadReg { reg, .. } => !written_regs[reg.index()],
        Instr::LoadFlag { .. } => false,
        _ => false,
    }
}

fn operands_invariant<const N: usize>(ops: [Operand; N], invariant: &HashSet<ValueId>) -> bool {
    ops.iter().all(|op| match op {
        Operand::Const(_) => true,
        Operand::Value(v) => invariant.contains(v),
    })
}
