use crate::t2_ir::{Instr, TraceIr, REG_COUNT};
use aero_types::Gpr;

/// A simple guest-register allocation plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegAllocPlan {
    pub cached: [bool; REG_COUNT],
    pub local_for_reg: [Option<u32>; REG_COUNT],
    pub local_count: u32,
}

impl Default for RegAllocPlan {
    fn default() -> Self {
        Self {
            cached: [false; REG_COUNT],
            local_for_reg: [None; REG_COUNT],
            local_count: 0,
        }
    }
}

impl RegAllocPlan {
    pub fn is_cached(&self, reg: Gpr) -> bool {
        self.cached[reg.as_u8() as usize]
    }
}

pub fn run(trace: &TraceIr) -> RegAllocPlan {
    let mut used = [false; REG_COUNT];
    for inst in trace.iter_instrs() {
        match *inst {
            Instr::LoadReg { reg, .. } | Instr::StoreReg { reg, .. } => {
                used[reg.as_u8() as usize] = true;
            }
            _ => {}
        }
    }

    let mut plan = RegAllocPlan::default();
    let mut next_local: u32 = 0;
    for (idx, u) in used.into_iter().enumerate() {
        if u {
            plan.cached[idx] = true;
            plan.local_for_reg[idx] = Some(next_local);
            next_local += 1;
        }
    }
    plan.local_count = next_local;
    plan
}
