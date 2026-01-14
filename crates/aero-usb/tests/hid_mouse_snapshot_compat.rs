use aero_io_snapshot::io::state::codec::Encoder;
use aero_io_snapshot::io::state::{IoSnapshot, SnapshotVersion, SnapshotWriter};
use aero_usb::hid::UsbHidMouseHandle;
use aero_usb::{UsbDeviceModel, UsbInResult};

const INTERRUPT_IN_EP: u8 = 0x81;

#[test]
fn mouse_snapshot_load_accepts_legacy_report_without_hwheel_byte() {
    // Snapshot tag numbers are part of the stable snapshot format.
    const TAG_CONFIGURATION: u16 = 2;
    const TAG_PROTOCOL: u16 = 8;
    const TAG_PENDING_REPORTS: u16 = 13;

    let pending = vec![vec![0x03, 1u8, 2u8, 3u8]]; // buttons, x, y, wheel (no hwheel byte)

    let mut w = SnapshotWriter::new(
        <UsbHidMouseHandle as IoSnapshot>::DEVICE_ID,
        SnapshotVersion::new(1, 1),
    );
    // Mark the device configured so interrupt endpoints are active.
    w.field_u8(TAG_CONFIGURATION, 1);
    // Ensure we use report protocol formatting for the mouse (5-byte reports).
    w.field_u8(TAG_PROTOCOL, 1);
    w.field_bytes(
        TAG_PENDING_REPORTS,
        Encoder::new().vec_bytes(&pending).finish(),
    );
    let snap = w.finish();

    let mut mouse = UsbHidMouseHandle::new();
    mouse.load_state(&snap).unwrap();

    let report = match mouse.handle_in_transfer(INTERRUPT_IN_EP, 5) {
        UsbInResult::Data(data) => data,
        other => panic!("expected restored mouse report data, got {other:?}"),
    };
    assert_eq!(
        report,
        vec![0x03, 1, 2, 3, 0],
        "expected missing legacy hwheel byte to restore as 0"
    );
    assert!(matches!(
        mouse.handle_in_transfer(INTERRUPT_IN_EP, 5),
        UsbInResult::Nak
    ));
}
