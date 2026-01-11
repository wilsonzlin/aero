use crate::t2_ir::{Instr, Operand, TraceIr};

fn max_value_id(trace: &TraceIr) -> usize {
    let mut max_id: Option<u32> = None;
    for inst in trace.iter_instrs() {
        if let Some(dst) = inst.dst() {
            max_id = Some(max_id.map_or(dst.0, |cur| cur.max(dst.0)));
        }
        inst.for_each_operand(|op| {
            if let Operand::Value(v) = op {
                max_id = Some(max_id.map_or(v.0, |cur| cur.max(v.0)));
            }
        });
    }
    max_id.map_or(0, |v| v as usize + 1)
}

fn truncate_after_side_exit(prologue: &mut Vec<Instr>, body: &mut Vec<Instr>) -> bool {
    if let Some(pos) = prologue
        .iter()
        .position(|i| matches!(i, Instr::SideExit { .. }))
    {
        prologue.truncate(pos + 1);
        body.clear();
        return true;
    }
    if let Some(pos) = body
        .iter()
        .position(|i| matches!(i, Instr::SideExit { .. }))
    {
        body.truncate(pos + 1);
        return true;
    }
    false
}

pub fn run(trace: &mut TraceIr) -> bool {
    let mut changed = truncate_after_side_exit(&mut trace.prologue, &mut trace.body);

    let slots = max_value_id(trace).max(1);
    let mut live = vec![false; slots];

    let mut new_body_rev: Vec<Instr> = Vec::with_capacity(trace.body.len());
    for inst in trace.body.iter().rev() {
        if keep_inst(inst, &mut live) {
            new_body_rev.push(inst.clone());
        } else {
            changed = true;
        }
    }
    new_body_rev.reverse();

    let mut new_prologue_rev: Vec<Instr> = Vec::with_capacity(trace.prologue.len());
    for inst in trace.prologue.iter().rev() {
        if keep_inst(inst, &mut live) {
            new_prologue_rev.push(inst.clone());
        } else {
            changed = true;
        }
    }
    new_prologue_rev.reverse();

    if changed {
        trace.body = new_body_rev;
        trace.prologue = new_prologue_rev;
    }
    changed
}

fn keep_inst(inst: &Instr, live: &mut [bool]) -> bool {
    let dst = inst.dst();
    let needed = inst.has_side_effects()
        || dst.map_or(false, |d| live.get(d.index()).copied().unwrap_or(false));

    if !needed {
        return false;
    }

    inst.for_each_operand(|op| {
        if let Operand::Value(v) = op {
            if let Some(slot) = live.get_mut(v.index()) {
                *slot = true;
            }
        }
    });

    if let Some(d) = dst {
        if let Some(slot) = live.get_mut(d.index()) {
            *slot = false;
        }
    }

    true
}
