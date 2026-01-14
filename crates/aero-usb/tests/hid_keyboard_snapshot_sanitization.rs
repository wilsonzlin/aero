use aero_io_snapshot::io::state::codec::Encoder;
use aero_io_snapshot::io::state::{IoSnapshot, SnapshotWriter};
use aero_usb::hid::{UsbCompositeHidInputHandle, UsbHidKeyboardHandle};
use aero_usb::{ControlResponse, SetupPacket, UsbDeviceModel, UsbInResult};

#[test]
fn hid_keyboard_snapshot_load_filters_out_of_range_pressed_keys() {
    // Snapshot tag numbers are part of the stable snapshot format.
    const TAG_CONFIGURATION: u16 = 2;
    const TAG_PRESSED_KEYS: u16 = 11;

    let pressed = vec![0x04, 0x90]; // 0x90 exceeds the descriptor usage max (0x89)

    let mut w = SnapshotWriter::new(
        <UsbHidKeyboardHandle as IoSnapshot>::DEVICE_ID,
        <UsbHidKeyboardHandle as IoSnapshot>::DEVICE_VERSION,
    );
    w.field_u8(TAG_CONFIGURATION, 1);
    w.field_bytes(TAG_PRESSED_KEYS, Encoder::new().vec_u8(&pressed).finish());
    let snap = w.finish();

    let mut kb = UsbHidKeyboardHandle::new();
    kb.load_state(&snap).unwrap();

    let resp = kb.handle_control_request(
        SetupPacket {
            bm_request_type: 0xa1,
            b_request: 0x01,    // HID_REQUEST_GET_REPORT
            w_value: 1u16 << 8, // Input report
            w_index: 0,
            w_length: 8,
        },
        None,
    );
    assert_eq!(
        resp,
        ControlResponse::Data(vec![0, 0, 0x04, 0, 0, 0, 0, 0])
    );
}

#[test]
fn hid_keyboard_snapshot_load_sanitizes_pending_reports() {
    const TAG_CONFIGURATION: u16 = 2;
    const TAG_PENDING_REPORTS: u16 = 13;

    let pending = vec![vec![0x00, 0xff, 0x04, 0, 0, 0, 0, 0x90]];

    let mut w = SnapshotWriter::new(
        <UsbHidKeyboardHandle as IoSnapshot>::DEVICE_ID,
        <UsbHidKeyboardHandle as IoSnapshot>::DEVICE_VERSION,
    );
    w.field_u8(TAG_CONFIGURATION, 1);
    w.field_bytes(TAG_PENDING_REPORTS, Encoder::new().vec_bytes(&pending).finish());
    let snap = w.finish();

    let mut kb = UsbHidKeyboardHandle::new();
    kb.load_state(&snap).unwrap();

    let report = match kb.handle_in_transfer(0x81, 8) {
        UsbInResult::Data(data) => data,
        other => panic!("expected restored keyboard report data, got {other:?}"),
    };
    assert_eq!(report, vec![0x00, 0x00, 0x04, 0, 0, 0, 0, 0]);
    assert!(matches!(kb.handle_in_transfer(0x81, 8), UsbInResult::Nak));
}

#[test]
fn hid_composite_keyboard_snapshot_load_filters_out_of_range_pressed_keys() {
    const TAG_CONFIGURATION: u16 = 2;
    const TAG_KBD_PRESSED_KEYS: u16 = 14;

    let pressed = vec![0x04, 0x90];

    let mut w = SnapshotWriter::new(
        <UsbCompositeHidInputHandle as IoSnapshot>::DEVICE_ID,
        <UsbCompositeHidInputHandle as IoSnapshot>::DEVICE_VERSION,
    );
    w.field_u8(TAG_CONFIGURATION, 1);
    w.field_bytes(TAG_KBD_PRESSED_KEYS, Encoder::new().vec_u8(&pressed).finish());
    let snap = w.finish();

    let mut hid = UsbCompositeHidInputHandle::new();
    hid.load_state(&snap).unwrap();

    let resp = hid.handle_control_request(
        SetupPacket {
            bm_request_type: 0xa1,
            b_request: 0x01,
            w_value: 1u16 << 8, // Input report
            w_index: 0,         // keyboard interface
            w_length: 8,
        },
        None,
    );
    assert_eq!(
        resp,
        ControlResponse::Data(vec![0, 0, 0x04, 0, 0, 0, 0, 0])
    );
}

#[test]
fn hid_composite_keyboard_snapshot_load_sanitizes_pending_reports() {
    const TAG_CONFIGURATION: u16 = 2;
    const TAG_KBD_PENDING_REPORTS: u16 = 16;

    let pending = vec![vec![0x00, 0xff, 0x04, 0, 0, 0, 0, 0x90]];

    let mut w = SnapshotWriter::new(
        <UsbCompositeHidInputHandle as IoSnapshot>::DEVICE_ID,
        <UsbCompositeHidInputHandle as IoSnapshot>::DEVICE_VERSION,
    );
    w.field_u8(TAG_CONFIGURATION, 1);
    w.field_bytes(
        TAG_KBD_PENDING_REPORTS,
        Encoder::new().vec_bytes(&pending).finish(),
    );
    let snap = w.finish();

    let mut hid = UsbCompositeHidInputHandle::new();
    hid.load_state(&snap).unwrap();

    let report = match hid.handle_in_transfer(0x81, 8) {
        UsbInResult::Data(data) => data,
        other => panic!("expected restored keyboard report data, got {other:?}"),
    };
    assert_eq!(report, vec![0x00, 0x00, 0x04, 0, 0, 0, 0, 0]);
    assert!(matches!(hid.handle_in_transfer(0x81, 8), UsbInResult::Nak));
}

