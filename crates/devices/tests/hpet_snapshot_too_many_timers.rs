use aero_devices::clock::ManualClock;
use aero_devices::hpet::Hpet;
use aero_io_snapshot::io::state::codec::Encoder;
use aero_io_snapshot::io::state::{IoSnapshot, SnapshotError, SnapshotWriter};

#[test]
fn hpet_snapshot_rejects_excessive_timer_count() {
    const TAG_TIMERS: u16 = 5;
    const MAX_HPETS: u32 = 32;

    let timers = Encoder::new().u32(MAX_HPETS + 1).finish();
    let mut w = SnapshotWriter::new(Hpet::<ManualClock>::DEVICE_ID, Hpet::<ManualClock>::DEVICE_VERSION);
    w.field_bytes(TAG_TIMERS, timers);
    let bytes = w.finish();

    let clock = ManualClock::new();
    let mut hpet = Hpet::new_default(clock);
    let err = hpet.load_state(&bytes).unwrap_err();
    match err {
        SnapshotError::InvalidFieldEncoding(_) => {}
        other => panic!("expected InvalidFieldEncoding, got {other:?}"),
    }
}

