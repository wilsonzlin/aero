//! Tier-2 IR optimization pipeline.
//!
//! The baseline Tier-1 JIT (`ir::IrBlock` → `wasm`) compiles single basic blocks quickly.
//! Tier-2 targets hot regions/traces and is allowed to spend more compilation time to
//! optimize the IR before lowering to WASM.

use crate::t2_ir::TraceIr;

pub mod passes;

pub use passes::regalloc::RegAllocPlan;

#[derive(Clone, Debug)]
pub struct OptConfig {
    /// Maximum fixed-point iterations.
    pub max_iters: usize,
}

impl Default for OptConfig {
    fn default() -> Self {
        Self { max_iters: 5 }
    }
}

#[derive(Clone, Debug)]
pub struct OptResult {
    pub regalloc: RegAllocPlan,
}

pub fn optimize_trace(trace: &mut TraceIr, cfg: &OptConfig) -> OptResult {
    for _ in 0..cfg.max_iters {
        let mut changed = false;
        changed |= passes::addr_simplify::run(trace);
        changed |= passes::licm::run(trace);
        changed |= passes::flag_elim::run(trace);
        changed |= passes::const_fold::run(trace);
        changed |= passes::cse::run(trace);
        changed |= passes::dce::run(trace);
        if !changed {
            break;
        }
    }

    let regalloc = passes::regalloc::run(trace);
    OptResult { regalloc }
}
