use aero_io_snapshot::io::state::{IoSnapshot, SnapshotError, SnapshotWriter};
use aero_io_snapshot::io::storage::state::NvmeControllerState;

#[test]
fn nvme_snapshot_rejects_excessive_io_queue_count() {
    const TAG_IO_QUEUES: u16 = 3;

    let mut w = SnapshotWriter::new(
        NvmeControllerState::DEVICE_ID,
        NvmeControllerState::DEVICE_VERSION,
    );
    w.field_bytes(TAG_IO_QUEUES, u32::MAX.to_le_bytes().to_vec());

    let mut state = NvmeControllerState::default();
    let err = state
        .load_state(&w.finish())
        .expect_err("snapshot should reject excessive IO queue count");
    assert_eq!(
        err,
        SnapshotError::InvalidFieldEncoding("nvme io queue count")
    );
}

#[test]
fn nvme_snapshot_rejects_excessive_in_flight_command_count() {
    const TAG_IN_FLIGHT: u16 = 4;

    let mut w = SnapshotWriter::new(
        NvmeControllerState::DEVICE_ID,
        NvmeControllerState::DEVICE_VERSION,
    );
    w.field_bytes(TAG_IN_FLIGHT, u32::MAX.to_le_bytes().to_vec());

    let mut state = NvmeControllerState::default();
    let err = state
        .load_state(&w.finish())
        .expect_err("snapshot should reject excessive in-flight command count");
    assert_eq!(
        err,
        SnapshotError::InvalidFieldEncoding("nvme in_flight count")
    );
}
