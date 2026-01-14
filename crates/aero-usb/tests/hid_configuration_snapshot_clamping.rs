use aero_io_snapshot::io::state::{IoSnapshot, SnapshotReader, SnapshotWriter};
use aero_usb::hid::{
    UsbCompositeHidInputHandle, UsbHidConsumerControlHandle, UsbHidGamepadHandle,
    UsbHidKeyboardHandle, UsbHidMouseHandle, UsbHidPassthroughHandle,
};
use aero_usb::{ControlResponse, SetupPacket, UsbDeviceModel};

fn assert_snapshot_load_clamps_configuration<D: IoSnapshot + UsbDeviceModel + Default>() {
    // Snapshot tag numbers are part of the stable snapshot format.
    const TAG_CONFIGURATION: u16 = 2;

    let mut w = SnapshotWriter::new(D::DEVICE_ID, D::DEVICE_VERSION);
    w.field_u8(TAG_CONFIGURATION, 7);
    let snap = w.finish();

    let mut dev = D::default();
    dev.load_state(&snap).unwrap();

    let resp = dev.handle_control_request(
        SetupPacket {
            bm_request_type: 0x80, // DeviceToHost | Standard | Device
            b_request: 0x08,       // GET_CONFIGURATION
            w_value: 0,
            w_index: 0,
            w_length: 1,
        },
        None,
    );
    assert_eq!(resp, ControlResponse::Data(vec![1]));
}

fn assert_snapshot_load_clamps_address<D: IoSnapshot + UsbDeviceModel + Default>() {
    // Snapshot tag numbers are part of the stable snapshot format.
    const TAG_ADDRESS: u16 = 1;

    let mut w = SnapshotWriter::new(D::DEVICE_ID, D::DEVICE_VERSION);
    w.field_u8(TAG_ADDRESS, 250);
    let snap = w.finish();

    let mut dev = D::default();
    dev.load_state(&snap).unwrap();

    let saved = dev.save_state();
    let r = SnapshotReader::parse(&saved, D::DEVICE_ID).expect("parse saved snapshot");
    assert_eq!(
        r.u8(TAG_ADDRESS).expect("read address tag"),
        Some(0),
        "invalid snapshot addresses should restore as 0"
    );
}

#[test]
fn hid_snapshot_load_clamps_configuration_field() {
    assert_snapshot_load_clamps_address::<UsbHidKeyboardHandle>();
    assert_snapshot_load_clamps_address::<UsbHidMouseHandle>();
    assert_snapshot_load_clamps_address::<UsbHidGamepadHandle>();
    assert_snapshot_load_clamps_address::<UsbHidConsumerControlHandle>();
    assert_snapshot_load_clamps_address::<UsbCompositeHidInputHandle>();

    assert_snapshot_load_clamps_configuration::<UsbHidKeyboardHandle>();
    assert_snapshot_load_clamps_configuration::<UsbHidMouseHandle>();
    assert_snapshot_load_clamps_configuration::<UsbHidGamepadHandle>();
    assert_snapshot_load_clamps_configuration::<UsbHidConsumerControlHandle>();
    assert_snapshot_load_clamps_configuration::<UsbCompositeHidInputHandle>();
}

#[test]
fn hid_passthrough_snapshot_load_clamps_configuration_field() {
    const TAG_CONFIGURATION: u16 = 2;
    const TAG_ADDRESS: u16 = 1;

    let report_desc = vec![
        0x06, 0x00, 0xff, // Usage Page (Vendor-defined 0xFF00)
        0x09, 0x01, // Usage (0x01)
        0xa1, 0x01, // Collection (Application)
        0x15, 0x00, // Logical Minimum (0)
        0x26, 0xff, 0x00, // Logical Maximum (255)
        0x75, 0x08, // Report Size (8)
        0x95, 0x02, // Report Count (2)
        0x81, 0x02, // Input (Data,Var,Abs)
        0xc0, // End Collection
    ];

    let mut w = SnapshotWriter::new(
        <UsbHidPassthroughHandle as IoSnapshot>::DEVICE_ID,
        <UsbHidPassthroughHandle as IoSnapshot>::DEVICE_VERSION,
    );
    w.field_u8(TAG_ADDRESS, 250);
    w.field_u8(TAG_CONFIGURATION, 7);
    let snap = w.finish();

    let mut dev = UsbHidPassthroughHandle::new(
        0x1234,
        0x5678,
        "Vendor".to_string(),
        "Product".to_string(),
        None,
        report_desc,
        false, // no interrupt OUT
        None,
        None,
        None,
    );
    dev.load_state(&snap).unwrap();

    let resp = dev.handle_control_request(
        SetupPacket {
            bm_request_type: 0x80,
            b_request: 0x08, // GET_CONFIGURATION
            w_value: 0,
            w_index: 0,
            w_length: 1,
        },
        None,
    );
    assert_eq!(resp, ControlResponse::Data(vec![1]));

    let saved = dev.save_state();
    let r = SnapshotReader::parse(&saved, <UsbHidPassthroughHandle as IoSnapshot>::DEVICE_ID)
        .expect("parse saved snapshot");
    assert_eq!(
        r.u8(TAG_ADDRESS).expect("read address tag"),
        Some(0),
        "invalid snapshot addresses should restore as 0"
    );
}
