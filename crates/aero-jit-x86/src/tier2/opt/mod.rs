//! Tier-2 IR optimization pipeline.
//!
//! The baseline Tier-1 JIT (`ir::IrBlock` → `wasm`) compiles single basic blocks quickly.
//! Tier-2 targets hot regions/traces and is allowed to spend more compilation time to
//! optimize the IR before lowering to WASM.

use super::ir::TraceIr;

pub mod passes;

pub use passes::regalloc::RegAllocPlan;

#[cfg(debug_assertions)]
use super::verify::verify_trace;

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
    #[cfg(debug_assertions)]
    {
        if let Err(e) = verify_trace(trace) {
            panic!("Tier-2 IR verification failed (pre-opt): {e}");
        }
    }

    // Compact ValueIds before running other passes so later passes don't allocate based on a sparse
    // `max(ValueId)`.
    passes::value_compact::run(trace);
    #[cfg(debug_assertions)]
    {
        if let Err(e) = verify_trace(trace) {
            panic!("Tier-2 IR verification failed (post-value-compact-pre): {e}");
        }
    }

    for _ in 0..cfg.max_iters {
        let mut changed = false;
        changed |= passes::addr_simplify::run(trace);
        changed |= passes::licm::run(trace);
        changed |= passes::flag_elim::run(trace);
        changed |= passes::boolean_simplify::run(trace);
        changed |= passes::const_fold::run(trace);
        changed |= passes::strength_reduction::run(trace);
        changed |= passes::cse::run(trace);
        changed |= passes::dce::run(trace);

        #[cfg(debug_assertions)]
        {
            if let Err(e) = verify_trace(trace) {
                panic!("Tier-2 IR verification failed (post-opt-iter): {e}");
            }
        }

        if !changed {
            break;
        }
    }

    // Some passes (especially DCE) can remove many values, leaving gaps. Compact again so codegen
    // allocates the minimum number of WASM locals for values.
    passes::value_compact::run(trace);

    #[cfg(debug_assertions)]
    {
        if let Err(e) = verify_trace(trace) {
            panic!("Tier-2 IR verification failed (post-value-compact-post): {e}");
        }
    }

    debug_assert!(trace.validate().is_ok());

    let regalloc = passes::regalloc::run(trace);

    #[cfg(debug_assertions)]
    {
        if let Err(e) = verify_trace(trace) {
            panic!("Tier-2 IR verification failed (post-regalloc): {e}");
        }
    }

    OptResult { regalloc }
}
