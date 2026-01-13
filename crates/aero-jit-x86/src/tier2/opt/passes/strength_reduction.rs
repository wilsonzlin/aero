use crate::tier2::ir::{BinOp, Instr, Operand, TraceIr};

pub fn run(trace: &mut TraceIr) -> bool {
    let mut changed = false;

    for inst in trace.iter_instrs_mut() {
        let Instr::BinOp {
            dst,
            op: BinOp::Mul,
            lhs,
            rhs,
            flags,
        } = *inst
        else {
            continue;
        };

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

    changed
}
