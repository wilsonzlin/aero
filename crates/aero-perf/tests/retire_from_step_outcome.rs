use aero_cpu_core::exec::{ExecutedTier, StepOutcome};
use aero_perf::{retire_from_step_outcome, PerfCounters, PerfWorker};
use std::sync::Arc;

#[test]
fn retire_from_step_outcome_counts_only_retired_instructions() {
    let shared = Arc::new(PerfCounters::new());
    let mut perf = PerfWorker::new(shared.clone());

    let outcomes = [
        StepOutcome::InterruptDelivered,
        StepOutcome::Block {
            tier: ExecutedTier::Interpreter,
            entry_rip: 0,
            next_rip: 1,
            instructions_retired: 3,
        },
        // Rollback-style exit (committed=false) should retire 0 instructions.
        StepOutcome::Block {
            tier: ExecutedTier::Jit,
            entry_rip: 1,
            next_rip: 2,
            instructions_retired: 0,
        },
        StepOutcome::Block {
            tier: ExecutedTier::Jit,
            entry_rip: 2,
            next_rip: 3,
            instructions_retired: 5,
        },
        StepOutcome::InterruptDelivered,
        StepOutcome::Block {
            tier: ExecutedTier::Interpreter,
            entry_rip: 3,
            next_rip: 4,
            instructions_retired: 1,
        },
    ];

    for outcome in outcomes {
        retire_from_step_outcome(&mut perf, &outcome);
    }

    assert_eq!(perf.lifetime_snapshot().instructions_executed, 9);

    // Also validate that flushing writes the same total into the shared atomics
    // (the default flush threshold is much larger than this test).
    perf.flush();
    assert_eq!(shared.instructions_executed(), 9);
}

