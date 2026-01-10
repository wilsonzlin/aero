use std::sync::Arc;

use aero_time::{FakeHostClock, Speed, TimeSource};

#[test]
fn time_source_pause_resume_and_speed() {
    let host = Arc::new(FakeHostClock::new(0));
    let time = TimeSource::new(host.clone());

    assert_eq!(time.now_ns(), 0);
    host.advance_ns(100);
    assert_eq!(time.now_ns(), 100);

    time.pause();
    host.advance_ns(50);
    assert_eq!(time.now_ns(), 100);

    time.resume();
    host.advance_ns(50);
    assert_eq!(time.now_ns(), 150);

    time.set_speed(Speed::from_ratio(2, 1));
    host.advance_ns(10);
    assert_eq!(time.now_ns(), 170);

    let sleep = time.host_duration_until_guest_ns(190).unwrap();
    assert_eq!(sleep.as_nanos(), 10);
}
