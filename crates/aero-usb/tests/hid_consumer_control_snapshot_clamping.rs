use aero_io_snapshot::io::state::codec::Encoder;
use aero_io_snapshot::io::state::{IoSnapshot, SnapshotWriter};
use aero_usb::hid::UsbHidConsumerControlHandle;
use aero_usb::{ControlResponse, SetupPacket, UsbDeviceModel, UsbInResult};

#[test]
fn hid_consumer_control_snapshot_load_filters_out_of_range_pressed_usages() {
    // Snapshot tag numbers are part of the stable snapshot format.
    const TAG_CONFIGURATION: u16 = 2;
    const TAG_PRESSED_USAGES: u16 = 9;

    // Pressed usages encode as: u32 count + `count` u16 values (little-endian).
    let pressed = Encoder::new()
        .u32(2)
        .u16(0x00e9) // AudioVolumeUp (valid)
        .u16(0xffff) // invalid (outside logical max 0x03ff)
        .finish();

    let mut w = SnapshotWriter::new(
        <UsbHidConsumerControlHandle as IoSnapshot>::DEVICE_ID,
        <UsbHidConsumerControlHandle as IoSnapshot>::DEVICE_VERSION,
    );
    w.field_u8(TAG_CONFIGURATION, 1);
    w.field_bytes(TAG_PRESSED_USAGES, pressed);
    let snap = w.finish();

    let mut dev = UsbHidConsumerControlHandle::new();
    dev.load_state(&snap).unwrap();

    let resp = dev.handle_control_request(
        SetupPacket {
            bm_request_type: 0xa1, // DeviceToHost | Class | Interface
            b_request: 0x01,       // HID_REQUEST_GET_REPORT
            w_value: 1u16 << 8,    // Input report, ID 0
            w_index: 0,
            w_length: 2,
        },
        None,
    );
    assert_eq!(resp, ControlResponse::Data(vec![0xe9, 0x00]));
}

#[test]
fn hid_consumer_control_snapshot_load_sanitizes_pending_reports() {
    // Snapshot tag numbers are part of the stable snapshot format.
    const TAG_CONFIGURATION: u16 = 2;
    const TAG_PENDING_REPORTS: u16 = 11;

    let pending = vec![vec![0xff, 0xff]]; // invalid usage

    let mut w = SnapshotWriter::new(
        <UsbHidConsumerControlHandle as IoSnapshot>::DEVICE_ID,
        <UsbHidConsumerControlHandle as IoSnapshot>::DEVICE_VERSION,
    );
    w.field_u8(TAG_CONFIGURATION, 1);
    w.field_bytes(TAG_PENDING_REPORTS, Encoder::new().vec_bytes(&pending).finish());
    let snap = w.finish();

    let mut dev = UsbHidConsumerControlHandle::new();
    dev.load_state(&snap).unwrap();

    let report = match dev.handle_in_transfer(0x81, 2) {
        UsbInResult::Data(data) => data,
        other => panic!("expected restored consumer report data, got {other:?}"),
    };
    assert_eq!(report, vec![0x00, 0x00]);
    assert!(matches!(dev.handle_in_transfer(0x81, 2), UsbInResult::Nak));
}

