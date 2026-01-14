use aero_io_snapshot::io::state::codec::Encoder;
use aero_io_snapshot::io::state::{IoSnapshot, SnapshotWriter};
use aero_usb::hid::{UsbCompositeHidInputHandle, UsbHidMouseHandle};
use aero_usb::{UsbDeviceModel, UsbInResult};

#[test]
fn hid_mouse_snapshot_load_clamps_pending_report_axes() {
    // Snapshot tag numbers are part of the stable snapshot format.
    const TAG_CONFIGURATION: u16 = 2;
    const TAG_PROTOCOL: u16 = 8;
    const TAG_PENDING_REPORTS: u16 = 13;

    let pending = vec![vec![0x00, 0x80, 0x80, 0x80, 0x80]]; // -128 for all axes

    let mut w = SnapshotWriter::new(
        <UsbHidMouseHandle as IoSnapshot>::DEVICE_ID,
        <UsbHidMouseHandle as IoSnapshot>::DEVICE_VERSION,
    );
    w.field_u8(TAG_CONFIGURATION, 1);
    w.field_u8(TAG_PROTOCOL, 1); // report protocol (5-byte reports)
    w.field_bytes(TAG_PENDING_REPORTS, Encoder::new().vec_bytes(&pending).finish());
    let snap = w.finish();

    let mut mouse = UsbHidMouseHandle::new();
    mouse.load_state(&snap).unwrap();

    let report = match mouse.handle_in_transfer(0x81, 5) {
        UsbInResult::Data(data) => data,
        other => panic!("expected restored mouse report data, got {other:?}"),
    };
    assert_eq!(report, vec![0x00, 0x81, 0x81, 0x81, 0x81]);
    assert!(matches!(mouse.handle_in_transfer(0x81, 5), UsbInResult::Nak));
}

#[test]
fn hid_composite_mouse_snapshot_load_clamps_pending_report_axes() {
    // Snapshot tag numbers are part of the stable snapshot format.
    const TAG_CONFIGURATION: u16 = 2;
    const TAG_MOUSE_PROTOCOL: u16 = 21;
    const TAG_MOUSE_PENDING_REPORTS: u16 = 26;

    let pending = vec![vec![0x00, 0x80, 0x80, 0x80, 0x80]]; // -128 for all axes

    let mut w = SnapshotWriter::new(
        <UsbCompositeHidInputHandle as IoSnapshot>::DEVICE_ID,
        <UsbCompositeHidInputHandle as IoSnapshot>::DEVICE_VERSION,
    );
    w.field_u8(TAG_CONFIGURATION, 1);
    w.field_u8(TAG_MOUSE_PROTOCOL, 1); // report protocol (5-byte reports)
    w.field_bytes(
        TAG_MOUSE_PENDING_REPORTS,
        Encoder::new().vec_bytes(&pending).finish(),
    );
    let snap = w.finish();

    let mut hid = UsbCompositeHidInputHandle::new();
    hid.load_state(&snap).unwrap();

    let report = match hid.handle_in_transfer(0x82, 5) {
        UsbInResult::Data(data) => data,
        other => panic!("expected restored mouse report data, got {other:?}"),
    };
    assert_eq!(report, vec![0x00, 0x81, 0x81, 0x81, 0x81]);
    assert!(matches!(hid.handle_in_transfer(0x82, 5), UsbInResult::Nak));
}

