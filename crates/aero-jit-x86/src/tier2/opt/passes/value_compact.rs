use std::collections::HashMap;

use crate::tier2::ir::{Instr, Operand, TraceIr, ValueId};

/// Renumber sparse [`ValueId`]s in a trace into a dense range `[0..n)`.
///
/// Tier-2 traces can be formed by concatenating multiple blocks. The CFG builder ensures that
/// [`ValueId`]s are globally unique across blocks (typically by applying an offset per block),
/// which can make traces very sparse.
///
/// A sparse `max(ValueId)` has a large cost across the Tier-2 pipeline:
/// - `Tier2WasmCodegen` sizes its locals by `max(ValueId) + 1`.
/// - Some optimization passes allocate `Vec`s sized to `max(ValueId) + 1` (e.g. DCE liveness).
pub fn run(trace: &mut TraceIr) -> bool {
    let mut next: u32 = 0;
    let mut map: HashMap<ValueId, ValueId> = HashMap::new();

    let intern = |v: ValueId, map: &mut HashMap<ValueId, ValueId>, next: &mut u32| {
        map.entry(v).or_insert_with(|| {
            let new = ValueId(*next);
            *next += 1;
            new
        });
    };

    // Build a stable mapping by first occurrence order (prologue then body).
    for inst in trace.iter_instrs() {
        if let Some(dst) = inst.dst() {
            intern(dst, &mut map, &mut next);
        }
        inst.for_each_operand(|op| {
            if let Operand::Value(v) = op {
                intern(v, &mut map, &mut next);
            }
        });
    }

    // Fast-path: already compact.
    let mut changed = false;
    for (old, new) in &map {
        if old != new {
            changed = true;
            break;
        }
    }
    if !changed {
        return false;
    }

    for inst in trace.iter_instrs_mut() {
        match inst {
            Instr::Const { dst, .. }
            | Instr::LoadReg { dst, .. }
            | Instr::LoadMem { dst, .. }
            | Instr::LoadFlag { dst, .. }
            | Instr::BinOp { dst, .. }
            | Instr::Addr { dst, .. } => {
                *dst = map
                    .get(dst)
                    .copied()
                    .expect("ValueId present in trace but missing from compaction map");
            }
            Instr::Nop
            | Instr::StoreReg { .. }
            | Instr::StoreMem { .. }
            | Instr::SetFlags { .. }
            | Instr::Guard { .. }
            | Instr::GuardCodeVersion { .. }
            | Instr::SideExit { .. } => {}
        }

        inst.for_each_operand_mut(|op| {
            if let Operand::Value(v) = op {
                *v = map
                    .get(v)
                    .copied()
                    .expect("ValueId present in trace but missing from compaction map");
            }
        });
    }

    true
}
