use aero_devices::pit8254::Pit8254;
use aero_io_snapshot::io::state::codec::Encoder;
use aero_io_snapshot::io::state::{IoSnapshot, SnapshotError, SnapshotWriter};

#[test]
fn pit_snapshot_rejects_excessive_channel_count() {
    const TAG_CHANNELS: u16 = 3;
    const MAX_CHANNELS: u32 = 32;

    let channels = Encoder::new().u32(MAX_CHANNELS + 1).finish();
    let mut w = SnapshotWriter::new(Pit8254::DEVICE_ID, Pit8254::DEVICE_VERSION);
    w.field_bytes(TAG_CHANNELS, channels);
    let bytes = w.finish();

    let mut pit = Pit8254::new();
    let err = pit.load_state(&bytes).unwrap_err();
    match err {
        SnapshotError::InvalidFieldEncoding(_) => {}
        other => panic!("expected InvalidFieldEncoding, got {other:?}"),
    }
}
