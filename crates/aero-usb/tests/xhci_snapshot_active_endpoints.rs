use aero_io_snapshot::io::state::codec::Encoder;
use aero_io_snapshot::io::state::{IoSnapshot, SnapshotError, SnapshotVersion, SnapshotWriter};
use aero_usb::xhci::XhciController;

#[test]
fn xhci_snapshot_load_rejects_duplicate_active_endpoints() {
    const TAG_ACTIVE_ENDPOINTS: u16 = 22;

    let active = Encoder::new().u32(2).u8(1).u8(1).u8(1).u8(1).finish();

    let snapshot = {
        let mut w = SnapshotWriter::new(*b"XHCI", SnapshotVersion::new(0, 8));
        w.field_bytes(TAG_ACTIVE_ENDPOINTS, active);
        w.finish()
    };

    let mut ctrl = XhciController::new();
    match ctrl.load_state(&snapshot) {
        Err(SnapshotError::InvalidFieldEncoding(
            "xhci active endpoint duplicate",
        )) => {}
        other => panic!("expected InvalidFieldEncoding, got {other:?}"),
    }
}
