use aero_types::FlagSet;

use crate::t2_ir::{flag_to_set, Instr, TraceIr};

pub fn run(trace: &mut TraceIr) -> bool {
    let mut changed = false;

    // Treat trace boundaries and side exits as observing full flags.
    let mut live = FlagSet::ALU;

    for inst in trace
        .body
        .iter_mut()
        .rev()
        .chain(trace.prologue.iter_mut().rev())
    {
        if matches!(
            inst,
            Instr::Guard { .. } | Instr::SideExit { .. }
        ) {
            live = live.union(FlagSet::ALU);
        }

        let defs = inst.flags_written();
        if !defs.is_empty() {
            let needed = intersect_flagset(defs, live);
            if needed != defs {
                changed = true;
                match inst {
                    Instr::BinOp { flags, .. } => *flags = needed,
                    Instr::SetFlags { mask, .. } => *mask = needed,
                    _ => {}
                }
            }
            live = live.without(needed);
        }

        live = live.union(inst.flags_read());
    }

    if changed {
        for inst in trace.iter_instrs_mut() {
            if let Instr::SetFlags { mask, .. } = inst {
                if mask.is_empty() {
                    *inst = Instr::Nop;
                }
            }
        }
    }

    changed
}

fn intersect_flagset(a: FlagSet, b: FlagSet) -> FlagSet {
    if a.is_empty() || b.is_empty() {
        return FlagSet::EMPTY;
    }
    let mut out = FlagSet::EMPTY;
    for flag in a.iter() {
        let bit = flag_to_set(flag);
        if b.contains(bit) {
            out = out.union(bit);
        }
    }
    out
}
