use aero_perf::{PerfCounters, PerfWorker};
use std::sync::Arc;

#[test]
fn frame_and_benchmark_deltas_use_unflushed_local_counts() {
    let shared = Arc::new(PerfCounters::new());
    let mut perf = PerfWorker::with_flush_threshold(shared, 1_000_000);

    assert_eq!(perf.begin_frame(0).instructions_executed, 0);
    perf.retire_instructions(7);
    let delta = perf.begin_frame(1);
    assert_eq!(delta.instructions_executed, 7);

    perf.begin_benchmark();
    perf.retire_instructions(10);
    assert_eq!(
        perf.benchmark_delta().unwrap().instructions_executed,
        10
    );
    assert_eq!(perf.end_benchmark().unwrap().instructions_executed, 10);
}

