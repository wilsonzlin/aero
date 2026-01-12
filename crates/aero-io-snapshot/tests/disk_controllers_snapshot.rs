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

