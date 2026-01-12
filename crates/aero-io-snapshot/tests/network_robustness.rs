use aero_io_snapshot::io::network::state::NetworkStackState;
use aero_io_snapshot::io::state::{IoSnapshot, SnapshotError, SnapshotWriter};

#[test]
fn network_snapshot_rejects_excessive_nat_entry_count() {
    const TAG_NAT: u16 = 3;

    let mut w = SnapshotWriter::new(
        NetworkStackState::DEVICE_ID,
        NetworkStackState::DEVICE_VERSION,
    );
    w.field_bytes(TAG_NAT, u32::MAX.to_le_bytes().to_vec());

    let mut state = NetworkStackState::default();
    let err = state
        .load_state(&w.finish())
        .expect_err("snapshot should reject excessive NAT count");
    assert_eq!(
        err,
        SnapshotError::InvalidFieldEncoding("too many nat entries")
    );
}

#[test]
fn network_snapshot_rejects_excessive_tcp_connection_count() {
    const TAG_TCP_CONNS: u16 = 5;

    let mut w = SnapshotWriter::new(
        NetworkStackState::DEVICE_ID,
        NetworkStackState::DEVICE_VERSION,
    );
    w.field_bytes(TAG_TCP_CONNS, u32::MAX.to_le_bytes().to_vec());

    let mut state = NetworkStackState::default();
    let err = state
        .load_state(&w.finish())
        .expect_err("snapshot should reject excessive TCP connection count");
    assert_eq!(
        err,
        SnapshotError::InvalidFieldEncoding("too many tcp proxy connections")
    );
}
