use std::collections::{HashMap, HashSet};

use super::ir::{Function, Instr, Operand, Terminator, TraceIr, ValueId};

/// Verify basic structural invariants of a Tier-2 trace.
///
/// This intentionally errs on the side of catching "obviously wrong" IR rather than attempting to
/// be a full semantic verifier.
pub fn verify_trace(trace: &TraceIr) -> Result<(), String> {
    verify_no_instrs_after_side_exit(&trace.prologue, "trace.prologue")?;
    verify_no_instrs_after_side_exit(&trace.body, "trace.body")?;
    if trace
        .prologue
        .iter()
        .any(|i| matches!(i, Instr::SideExit { .. }))
        && !trace.body.is_empty()
    {
        return Err(
            "trace contains a SideExit in the prologue but also has body instructions".to_string(),
        );
    }

    let mut defined_so_far: HashSet<ValueId> = HashSet::new();
    let mut def_sites: HashMap<ValueId, String> = HashMap::new();

    for (seg, instrs) in [("prologue", &trace.prologue), ("body", &trace.body)] {
        for (idx, inst) in instrs.iter().enumerate() {
            verify_instr_operands_defined(inst, &defined_so_far)
                .map_err(|e| format!("{seg}[{idx}] {inst:?}: {e}"))?;

            if let Some(dst) = inst.dst() {
                if let Some(prev) = def_sites.get(&dst) {
                    return Err(format!(
                        "{seg}[{idx}] {inst:?}: value {dst:?} defined multiple times (previous: {prev})"
                    ));
                }
                def_sites.insert(dst, format!("{seg}[{idx}] {inst:?}"));
                defined_so_far.insert(dst);
            }
        }
    }

    Ok(())
}

/// Verify basic structural invariants of a Tier-2 CFG [`Function`].
pub fn verify_function(func: &Function) -> Result<(), String> {
    if func.entry.index() >= func.blocks.len() {
        return Err(format!(
            "function entry {:?} out of range (blocks.len() = {})",
            func.entry,
            func.blocks.len()
        ));
    }

    for (idx, block) in func.blocks.iter().enumerate() {
        if block.id.index() != idx {
            return Err(format!(
                "block id mismatch: blocks[{idx}].id = {:?}",
                block.id
            ));
        }
    }

    let mut def_sites: HashMap<ValueId, String> = HashMap::new();

    for block in &func.blocks {
        verify_no_instrs_after_side_exit(&block.instrs, &format!("block {:?}.instrs", block.id))?;

        let mut defined_so_far: HashSet<ValueId> = HashSet::new();
        for (idx, inst) in block.instrs.iter().enumerate() {
            verify_instr_operands_defined(inst, &defined_so_far).map_err(|e| {
                format!("block {:?} instr[{idx}] {inst:?}: {e}", block.id)
            })?;

            if let Some(dst) = inst.dst() {
                if let Some(prev) = def_sites.get(&dst) {
                    return Err(format!(
                        "block {:?} instr[{idx}] {inst:?}: value {dst:?} defined multiple times (previous: {prev})",
                        block.id
                    ));
                }
                def_sites.insert(dst, format!("block {:?} instr[{idx}] {inst:?}", block.id));
                defined_so_far.insert(dst);
            }
        }

        // Verify terminator operands and branch targets.
        match &block.term {
            Terminator::Jump(t) => {
                if t.index() >= func.blocks.len() {
                    return Err(format!(
                        "block {:?} terminator Jump to out-of-range block {:?}",
                        block.id, t
                    ));
                }
            }
            Terminator::Branch {
                cond,
                then_bb,
                else_bb,
            } => {
                if then_bb.index() >= func.blocks.len() {
                    return Err(format!(
                        "block {:?} terminator Branch then_bb out of range: {:?}",
                        block.id, then_bb
                    ));
                }
                if else_bb.index() >= func.blocks.len() {
                    return Err(format!(
                        "block {:?} terminator Branch else_bb out of range: {:?}",
                        block.id, else_bb
                    ));
                }
                if let Operand::Value(v) = *cond {
                    if !defined_so_far.contains(&v) {
                        return Err(format!(
                            "block {:?} terminator Branch uses undefined value {:?}",
                            block.id, v
                        ));
                    }
                }
            }
            Terminator::SideExit { .. } | Terminator::Return => {}
        }
    }

    Ok(())
}

fn verify_no_instrs_after_side_exit(instrs: &[Instr], what: &str) -> Result<(), String> {
    if let Some(pos) = instrs
        .iter()
        .position(|i| matches!(i, Instr::SideExit { .. }))
    {
        if pos + 1 != instrs.len() {
            return Err(format!(
                "{what} contains instructions after SideExit (side exit at index {pos})"
            ));
        }
    }
    Ok(())
}

fn verify_instr_operands_defined(inst: &Instr, defined: &HashSet<ValueId>) -> Result<(), String> {
    let mut err: Option<String> = None;
    inst.for_each_operand(|op| {
        if err.is_some() {
            return;
        }
        if let Operand::Value(v) = op {
            if !defined.contains(&v) {
                err = Some(format!("use of undefined value {v:?}"));
            }
        }
    });
    err.map_or(Ok(()), Err)
}
