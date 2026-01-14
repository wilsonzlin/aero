use aero_io_snapshot::io::state::{IoSnapshot, SnapshotWriter};
use aero_usb::hid::{
    UsbCompositeHidInputHandle, UsbHidConsumerControlHandle, UsbHidGamepadHandle, UsbHidKeyboardHandle,
    UsbHidMouseHandle,
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

#[test]
fn hid_snapshot_load_clamps_configuration_field() {
    assert_snapshot_load_clamps_configuration::<UsbHidKeyboardHandle>();
    assert_snapshot_load_clamps_configuration::<UsbHidMouseHandle>();
    assert_snapshot_load_clamps_configuration::<UsbHidGamepadHandle>();
    assert_snapshot_load_clamps_configuration::<UsbHidConsumerControlHandle>();
    assert_snapshot_load_clamps_configuration::<UsbCompositeHidInputHandle>();
}

