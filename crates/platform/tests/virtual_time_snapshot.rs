use aero_io_snapshot::io::state::IoSnapshot;
use aero_platform::time::{TimerEvent, VirtualTime};

fn collect_events(time: &mut VirtualTime, steps: &[u64]) -> Vec<TimerEvent> {
    let mut events = Vec::new();
    for &step in steps {
        events.extend(time.advance(step));
    }
    events
}

#[test]
fn snapshot_bytes_are_deterministic_for_unchanged_virtual_time() {
    let mut time = VirtualTime::new();

    let periodic = time.timers_mut().alloc_timer();
    let rational = time.timers_mut().alloc_timer();
    let one_shot = time.timers_mut().alloc_timer();

    time.timers_mut().arm_periodic(periodic, 10, 50).unwrap();
    let now_ns = time.now_ns();
    time.timers_mut()
        .arm_periodic_rational_from_now_ns(rational, now_ns, 25, 10)
        .unwrap();
    time.timers_mut().arm_one_shot(one_shot, 160).unwrap();

    let bytes1 = IoSnapshot::save_state(&time);
    let bytes2 = IoSnapshot::save_state(&time);

    assert_eq!(bytes1, bytes2);
}

#[test]
fn virtual_time_snapshot_round_trip_preserves_future_timer_events() {
    let mut baseline = VirtualTime::new();

    let periodic = baseline.timers_mut().alloc_timer();
    let rational = baseline.timers_mut().alloc_timer();
    let one_shot = baseline.timers_mut().alloc_timer();

    baseline
        .timers_mut()
        .arm_periodic(periodic, 10, 50)
        .unwrap();
    let now_ns = baseline.now_ns();
    baseline
        .timers_mut()
        .arm_periodic_rational_from_now_ns(rational, now_ns, 25, 10)
        .unwrap();
    baseline.timers_mut().arm_one_shot(one_shot, 160).unwrap();

    // Advance to a checkpoint using a chunked pattern.
    let pre_steps = [7, 13, 29, 51, 23]; // -> now=123
    collect_events(&mut baseline, &pre_steps);

    let snapshot = IoSnapshot::save_state(&baseline);

    // Continue advancing and record all future events.
    let post_steps = [1, 2, 3, 5, 8, 13, 21, 34];
    let baseline_events = collect_events(&mut baseline, &post_steps);

    // Restore from snapshot and ensure the future events match.
    let mut restored = VirtualTime::new();
    restored.load_state(&snapshot).unwrap();

    let restored_events = collect_events(&mut restored, &post_steps);
    assert_eq!(baseline_events, restored_events);
}
