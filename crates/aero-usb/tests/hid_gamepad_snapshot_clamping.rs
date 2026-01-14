use aero_io_snapshot::io::state::{IoSnapshot, SnapshotWriter};
use aero_usb::hid::{UsbCompositeHidInputHandle, UsbHidGamepadHandle};
use aero_usb::{ControlResponse, SetupPacket, UsbDeviceModel};

#[test]
fn hid_gamepad_snapshot_load_clamps_hat_and_axes() {
    // Snapshot tag numbers are part of the stable snapshot format.
    const TAG_CONFIGURATION: u16 = 2;
    const TAG_HAT: u16 = 10;
    const TAG_X: u16 = 11;
    const TAG_RY: u16 = 14;

    let mut w = SnapshotWriter::new(
        <UsbHidGamepadHandle as IoSnapshot>::DEVICE_ID,
        <UsbHidGamepadHandle as IoSnapshot>::DEVICE_VERSION,
    );
    w.field_u8(TAG_CONFIGURATION, 1);
    w.field_u8(TAG_HAT, 0xff);
    w.field_u8(TAG_X, 0x80); // -128 (out of HID logical range), should clamp to -127
    w.field_u8(TAG_RY, 0x80);
    let snap = w.finish();

    let mut pad = UsbHidGamepadHandle::new();
    pad.load_state(&snap).unwrap();

    let resp = pad.handle_control_request(
        SetupPacket {
            bm_request_type: 0xa1,
            b_request: 0x01, // HID_REQUEST_GET_REPORT
            w_value: 0x0100, // Input report, report ID 0
            w_index: 0,
            w_length: 8,
        },
        None,
    );
    let ControlResponse::Data(data) = resp else {
        panic!("expected GET_REPORT to return Data, got {resp:?}");
    };
    assert_eq!(
        data,
        vec![0x00, 0x00, 0x08, (-127i8) as u8, 0x00, 0x00, (-127i8) as u8, 0x00]
    );
}

#[test]
fn hid_composite_gamepad_snapshot_load_clamps_hat_and_axes() {
    // Snapshot tag numbers are part of the stable snapshot format.
    const TAG_CONFIGURATION: u16 = 2;
    const TAG_GAMEPAD_HAT: u16 = 31;
    const TAG_GAMEPAD_X: u16 = 32;
    const TAG_GAMEPAD_RY: u16 = 35;

    let mut w = SnapshotWriter::new(
        <UsbCompositeHidInputHandle as IoSnapshot>::DEVICE_ID,
        <UsbCompositeHidInputHandle as IoSnapshot>::DEVICE_VERSION,
    );
    w.field_u8(TAG_CONFIGURATION, 1);
    w.field_u8(TAG_GAMEPAD_HAT, 0xff);
    w.field_u8(TAG_GAMEPAD_X, 0x80);
    w.field_u8(TAG_GAMEPAD_RY, 0x80);
    let snap = w.finish();

    let mut hid = UsbCompositeHidInputHandle::new();
    hid.load_state(&snap).unwrap();

    let resp = hid.handle_control_request(
        SetupPacket {
            bm_request_type: 0xa1,
            b_request: 0x01, // HID_REQUEST_GET_REPORT
            w_value: 0x0100, // Input report, report ID 0
            w_index: 2,      // gamepad interface
            w_length: 8,
        },
        None,
    );
    let ControlResponse::Data(data) = resp else {
        panic!("expected GET_REPORT to return Data, got {resp:?}");
    };
    assert_eq!(
        data,
        vec![0x00, 0x00, 0x08, (-127i8) as u8, 0x00, 0x00, (-127i8) as u8, 0x00]
    );
}

