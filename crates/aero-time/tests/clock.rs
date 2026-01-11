use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Barrier};
use std::time::Duration;

use aero_time::{FakeHostClock, HostClock, Speed, TimeSource, Tsc};

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

#[derive(Debug)]
struct ScriptedHostClock {
    calls: AtomicUsize,
    first_call_entered: mpsc::Sender<()>,
    release_first_call: Arc<Barrier>,
}

impl HostClock for ScriptedHostClock {
    fn now_ns(&self) -> u64 {
        match self.calls.fetch_add(1, Ordering::SeqCst) {
            // `TimeSource::new()` anchor.
            0 => 0,
            // `host_duration_until_guest_ns()` host clock read: block so another
            // thread can observe a later `now_ns()` value first.
            1 => {
                self.first_call_entered
                    .send(())
                    .expect("first_call_entered receiver dropped");
                self.release_first_call.wait();
                100
            }
            // `now_ns()` call from the main thread.
            2 => 200,
            n => panic!("unexpected now_ns call {n}"),
        }
    }
}

#[test]
fn host_duration_clamps_to_monotonic_now_ns() {
    let (entered_tx, entered_rx) = mpsc::channel();
    let release = Arc::new(Barrier::new(2));
    let host = Arc::new(ScriptedHostClock {
        calls: AtomicUsize::new(0),
        first_call_entered: entered_tx,
        release_first_call: release.clone(),
    });
    let time = Arc::new(TimeSource::new(host));

    let time_for_thread = time.clone();
    let (sleep_tx, sleep_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let sleep = time_for_thread
            .host_duration_until_guest_ns(150)
            .expect("clock should not be paused");
        sleep_tx.send(sleep).expect("sleep receiver dropped");
    });

    // Wait until the first host clock read is blocked.
    entered_rx.recv().expect("first call notification dropped");

    // Another thread observes later time and bumps the monotonic now value.
    assert_eq!(time.now_ns(), 200);

    // Release the blocked host clock read; without clamping, the computed guest
    // "now" would lag behind the monotonic value and return a non-zero sleep.
    release.wait();

    let sleep = sleep_rx.recv().expect("sleep sender dropped");
    assert_eq!(sleep, Duration::ZERO);
}

#[test]
fn speed_conversions_saturate_on_overflow() {
    // Ensure fixed-point conversions never wrap on overflow; guest nanoseconds are modeled as a
    // monotonic u64 and should saturate at u64::MAX rather than truncating.
    let fast = Speed::from_ratio(2, 1);
    assert_eq!(fast.host_delta_to_guest_ns(u64::MAX), u64::MAX);

    let slow = Speed::from_ratio(1, 2);
    assert_eq!(slow.guest_delta_to_host_ns_ceil(u64::MAX), u64::MAX);
}

#[test]
fn tsc_guest_ns_for_tsc_saturates_on_overflow() {
    // With an unrealistically low TSC frequency, mapping a large TSC delta back into guest
    // nanoseconds can overflow u64. The conversion should saturate instead of wrapping.
    let tsc = Tsc::new(1);
    let ns = tsc.guest_ns_for_tsc(u64::MAX).expect("freq_hz != 0");
    assert_eq!(ns, u64::MAX);
}
