use aero_perf::{retire_from_step_outcome, InstructionRetirement, PerfCounters, PerfWorker};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FakeTier {
    Interpreter,
    Jit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FakeStepOutcome {
    InterruptDelivered,
    Block {
        tier: FakeTier,
        instructions_retired: u64,
    },
}

impl InstructionRetirement for FakeStepOutcome {
    fn instructions_retired(&self) -> u64 {
        match *self {
            FakeStepOutcome::InterruptDelivered => 0,
            FakeStepOutcome::Block {
                instructions_retired,
                ..
            } => instructions_retired,
        }
    }
}

#[test]
fn retire_from_step_outcome_counts_only_retired_instructions() {
    let shared = Arc::new(PerfCounters::new());
    let mut perf = PerfWorker::new(shared.clone());

    let outcomes = [
        FakeStepOutcome::InterruptDelivered,
        FakeStepOutcome::Block {
            tier: FakeTier::Interpreter,
            instructions_retired: 3,
        },
        // Rollback-style exit (committed=false) should retire 0 instructions.
        FakeStepOutcome::Block {
            tier: FakeTier::Jit,
            instructions_retired: 0,
        },
        FakeStepOutcome::Block {
            tier: FakeTier::Jit,
            instructions_retired: 5,
        },
        FakeStepOutcome::InterruptDelivered,
        FakeStepOutcome::Block {
            tier: FakeTier::Interpreter,
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
