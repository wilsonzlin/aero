use aero_io_snapshot::io::state::codec::Encoder;
use aero_io_snapshot::io::state::{IoSnapshot, SnapshotError, SnapshotWriter};
use aero_io_snapshot::io::storage::state::{
    AhciControllerState, DiskControllersSnapshot, NvmeControllerState,
};

#[test]
fn disk_controllers_snapshot_roundtrip_is_deterministic() {
    let ahci = AhciControllerState::default().save_state();
    let nvme = NvmeControllerState::default().save_state();

    let mut a = DiskControllersSnapshot::new();
    a.insert(0x0200, nvme.clone());
    a.insert(0x0100, ahci.clone());

    let mut b = DiskControllersSnapshot::new();
    b.insert(0x0100, ahci);
    b.insert(0x0200, nvme);

    let bytes_a = a.save_state();
    let bytes_b = b.save_state();
    assert_eq!(bytes_a, bytes_b, "encoding must be deterministic");

    let mut restored = DiskControllersSnapshot::default();
    restored
        .load_state(&bytes_a)
        .expect("snapshot should decode");
    assert_eq!(restored, a);
}

#[test]
fn disk_controllers_snapshot_rejects_duplicate_bdf() {
    const TAG_CONTROLLERS: u16 = 1;

    let bdf = 0x0100u16;
    let ahci = AhciControllerState::default().save_state();
    let nvme = NvmeControllerState::default().save_state();

    let mut entries = Vec::new();
    let mut e1 = Vec::new();
    e1.extend_from_slice(&bdf.to_le_bytes());
    e1.extend_from_slice(&ahci);
    entries.push(e1);

    let mut e2 = Vec::new();
    e2.extend_from_slice(&bdf.to_le_bytes());
    e2.extend_from_slice(&nvme);
    entries.push(e2);

    let controllers = Encoder::new().vec_bytes(&entries).finish();
    let mut w = SnapshotWriter::new(
        DiskControllersSnapshot::DEVICE_ID,
        DiskControllersSnapshot::DEVICE_VERSION,
    );
    w.field_bytes(TAG_CONTROLLERS, controllers);
    let bytes = w.finish();

    let mut state = DiskControllersSnapshot::default();
    let err = state
        .load_state(&bytes)
        .expect_err("snapshot should reject duplicate bdf");
    assert_eq!(
        err,
        SnapshotError::InvalidFieldEncoding("disk controller duplicate bdf")
    );
}

#[test]
fn disk_controllers_snapshot_rejects_excessive_controller_count() {
    const TAG_CONTROLLERS: u16 = 1;

    // Keep in sync with `MAX_DISK_CONTROLLER_COUNT` in `aero-io-snapshot`.
    let controllers = Encoder::new().u32(257).finish();

    let mut w = SnapshotWriter::new(
        DiskControllersSnapshot::DEVICE_ID,
        DiskControllersSnapshot::DEVICE_VERSION,
    );
    w.field_bytes(TAG_CONTROLLERS, controllers);

    let mut state = DiskControllersSnapshot::default();
    let err = state
        .load_state(&w.finish())
        .expect_err("snapshot should reject excessive disk controller count");
    assert_eq!(
        err,
        SnapshotError::InvalidFieldEncoding("disk controller count")
    );
}

#[test]
fn disk_controllers_snapshot_rejects_entry_too_short() {
    const TAG_CONTROLLERS: u16 = 1;

    // One entry, declared length=1 (<2), and no payload bytes (should fail on the length check
    // before attempting to read the entry data).
    let controllers = Encoder::new().u32(1).u32(1).finish();

    let mut w = SnapshotWriter::new(
        DiskControllersSnapshot::DEVICE_ID,
        DiskControllersSnapshot::DEVICE_VERSION,
    );
    w.field_bytes(TAG_CONTROLLERS, controllers);

    let mut state = DiskControllersSnapshot::default();
    let err = state
        .load_state(&w.finish())
        .expect_err("snapshot should reject disk controller entry that is too short");
    assert_eq!(
        err,
        SnapshotError::InvalidFieldEncoding("disk controller entry too short")
    );
}

#[test]
fn disk_controllers_snapshot_rejects_excessive_nested_snapshot_size() {
    const TAG_CONTROLLERS: u16 = 1;

    // Declare a single entry with an absurd length, which implies a nested snapshot larger than
    // `MAX_DISK_CONTROLLER_SNAPSHOT_BYTES`. This should be rejected without allocating.
    let controllers = Encoder::new().u32(1).u32(u32::MAX).finish();

    let mut w = SnapshotWriter::new(
        DiskControllersSnapshot::DEVICE_ID,
        DiskControllersSnapshot::DEVICE_VERSION,
    );
    w.field_bytes(TAG_CONTROLLERS, controllers);

    let mut state = DiskControllersSnapshot::default();
    let err = state
        .load_state(&w.finish())
        .expect_err("snapshot should reject excessive disk controller snapshot size");
    assert_eq!(
        err,
        SnapshotError::InvalidFieldEncoding("disk controller snapshot too large")
    );
}

#[test]
fn disk_controllers_snapshot_rejects_truncated_entry_payload() {
    const TAG_CONTROLLERS: u16 = 1;

    // One entry, declared length=2, but provide only 1 byte of entry payload.
    let controllers = Encoder::new().u32(1).u32(2).u8(0xAA).finish();

    let mut w = SnapshotWriter::new(
        DiskControllersSnapshot::DEVICE_ID,
        DiskControllersSnapshot::DEVICE_VERSION,
    );
    w.field_bytes(TAG_CONTROLLERS, controllers);

    let mut state = DiskControllersSnapshot::default();
    let err = state
        .load_state(&w.finish())
        .expect_err("snapshot should reject truncated disk controller entry payload");
    assert_eq!(err, SnapshotError::UnexpectedEof);
}

#[test]
fn disk_controllers_snapshot_rejects_trailing_bytes() {
    const TAG_CONTROLLERS: u16 = 1;

    // Encode count=0 but include an extra byte. The decoder should reject the trailing data.
    let controllers = Encoder::new().u32(0).u8(0xAA).finish();

    let mut w = SnapshotWriter::new(
        DiskControllersSnapshot::DEVICE_ID,
        DiskControllersSnapshot::DEVICE_VERSION,
    );
    w.field_bytes(TAG_CONTROLLERS, controllers);

    let mut state = DiskControllersSnapshot::default();
    let err = state
        .load_state(&w.finish())
        .expect_err("snapshot should reject trailing bytes");
    assert_eq!(err, SnapshotError::InvalidFieldEncoding("trailing bytes"));
}
