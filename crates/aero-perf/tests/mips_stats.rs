use aero_perf::{compute_mips, MipsWindow, PerfMonitor, PerfSnapshot};
use std::time::{Duration, Instant};

#[test]
fn perf_snapshot_delta_saturates_when_called_with_newer_snapshot_as_earlier() {
    let earlier = PerfSnapshot {
        instructions_executed: 10,
        rep_iterations: 5,
    };
    let later = PerfSnapshot {
        instructions_executed: 7,
        rep_iterations: 2,
    };
    let delta = later.delta_since(earlier);
    assert_eq!(delta.instructions_executed, 0);
    assert_eq!(delta.rep_iterations, 0);
}

#[test]
fn compute_mips_uses_million_instructions_per_second() {
    let mips = compute_mips(2_000_000, Duration::from_secs(2));
    assert!((mips - 1.0).abs() < 1e-9);
}

#[test]
fn mips_window_avg_and_p95() {
    let mut window = MipsWindow::new(10);
    for sample in [1.0, 2.0, 3.0, 4.0, 5.0] {
        window.push(sample);
    }

    assert!((window.avg() - 3.0).abs() < 1e-9);
    assert!((window.p95() - 5.0).abs() < 1e-9);
}

#[test]
fn perf_monitor_tracks_window_stats() {
    let t0 = Instant::now();
    let mut monitor = PerfMonitor::new(5, PerfSnapshot::default(), t0);

    let t1 = t0 + Duration::from_secs(1);
    let s1 = monitor.update(
        PerfSnapshot {
            instructions_executed: 2_000_000,
            rep_iterations: 0,
        },
        t1,
    );
    assert!((s1.mips - 2.0).abs() < 1e-9);
    assert!((s1.mips_avg - 2.0).abs() < 1e-9);
    assert!((s1.mips_p95 - 2.0).abs() < 1e-9);

    let t2 = t1 + Duration::from_secs(1);
    let s2 = monitor.update(
        PerfSnapshot {
            instructions_executed: 3_000_000,
            rep_iterations: 0,
        },
        t2,
    );
    assert!((s2.mips - 1.0).abs() < 1e-9);
    assert!((s2.mips_avg - 1.5).abs() < 1e-9);
    assert!((s2.mips_p95 - 2.0).abs() < 1e-9);
}

#[test]
fn perf_monitor_handles_non_monotonic_timestamps() {
    let t0 = Instant::now();
    // Start with a baseline that is *after* the timestamps we will pass to update.
    let t1 = t0 + Duration::from_secs(1);
    let mut monitor = PerfMonitor::new(5, PerfSnapshot::default(), t1);

    let sample = monitor.update(
        PerfSnapshot {
            instructions_executed: 1,
            rep_iterations: 0,
        },
        t0,
    );

    assert_eq!(sample.wall_time_delta, Duration::ZERO);
    assert_eq!(sample.mips, 0.0);
}
