use aero_io_snapshot::io::state::codec::Encoder;
use aero_io_snapshot::io::state::{IoSnapshot, SnapshotVersion, SnapshotWriter};
use aero_usb::hid::composite::UsbCompositeHidInputHandle;
use aero_usb::{UsbDeviceModel, UsbInResult};

const MOUSE_INTERRUPT_IN_EP: u8 = 0x82;

#[test]
fn composite_snapshot_load_accepts_legacy_mouse_report_without_hwheel_byte() {
    // Snapshot tag numbers are part of the stable snapshot format.
    const TAG_CONFIGURATION: u16 = 2;
    const TAG_MOUSE_PROTOCOL: u16 = 21;
    const TAG_MOUSE_PENDING_REPORTS: u16 = 26;

    let pending = vec![vec![0x03, 1u8, 2u8, 3u8]]; // buttons, x, y, wheel (no hwheel byte)

    let mut w = SnapshotWriter::new(
        <UsbCompositeHidInputHandle as IoSnapshot>::DEVICE_ID,
        SnapshotVersion::new(1, 0),
    );
    // Mark the composite device configured so interrupt endpoints are active.
    w.field_u8(TAG_CONFIGURATION, 1);
    // Ensure we use report protocol formatting for the mouse (5-byte reports).
    w.field_u8(TAG_MOUSE_PROTOCOL, 1);
    w.field_bytes(
        TAG_MOUSE_PENDING_REPORTS,
        Encoder::new().vec_bytes(&pending).finish(),
    );
    let snap = w.finish();

    let mut dev = UsbCompositeHidInputHandle::new();
    dev.load_state(&snap).unwrap();

    let report = match dev.handle_in_transfer(MOUSE_INTERRUPT_IN_EP, 5) {
        UsbInResult::Data(data) => data,
        other => panic!("expected restored mouse report data, got {other:?}"),
    };
    assert_eq!(
        report,
        vec![0x03, 1, 2, 3, 0],
        "expected missing legacy hwheel byte to restore as 0"
    );
    assert!(matches!(
        dev.handle_in_transfer(MOUSE_INTERRUPT_IN_EP, 5),
        UsbInResult::Nak
    ));
}
