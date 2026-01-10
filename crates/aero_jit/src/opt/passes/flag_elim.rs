use crate::t2_ir::{FlagMask, Instr, TraceIr};

pub fn run(trace: &mut TraceIr) -> bool {
    let mut changed = false;

    // Treat trace boundaries and side exits as observing full flags.
    let mut live = FlagMask::ALL;

    for inst in trace
        .body
        .iter_mut()
        .rev()
        .chain(trace.prologue.iter_mut().rev())
    {
        if matches!(
            inst,
            Instr::Guard { .. } | Instr::GuardCodeVersion { .. } | Instr::SideExit { .. }
        ) {
            live |= FlagMask::ALL;
        }

        let defs = inst.flags_written();
        if !defs.is_empty() {
            let needed = defs.intersection(live);
            if needed != defs {
                changed = true;
                match inst {
                    Instr::BinOp { flags, .. } => *flags = needed,
                    Instr::SetFlags { mask, .. } => *mask = needed,
                    _ => {}
                }
            }
            live.remove(needed);
        }

        live |= inst.flags_read();
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
