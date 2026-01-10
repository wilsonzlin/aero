use aero_devices::pit8254::{Pit8254, PIT_CH0, PIT_CMD};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

#[test]
fn irq0_callback_is_invoked() {
    let mut pit = Pit8254::new();
    let seen = Arc::new(AtomicU64::new(0));
    let seen_clone = Arc::clone(&seen);
    pit.connect_irq0(move || {
        seen_clone.fetch_add(1, Ordering::Relaxed);
    });

    // Program channel 0: mode2, lobyte/hibyte, divisor=3.
    pit.port_write(PIT_CMD, 1, 0x34);
    pit.port_write(PIT_CH0, 1, 3);
    pit.port_write(PIT_CH0, 1, 0);

    pit.advance_ticks(9);
    assert_eq!(seen.load(Ordering::Relaxed), 3);
}
