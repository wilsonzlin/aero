use aero_io_snapshot::io::state::{IoSnapshot, SnapshotWriter};
use aero_usb::hub::UsbHubDevice;
use aero_usb::{ControlResponse, SetupPacket, UsbDeviceModel};

#[test]
fn usb_hub_snapshot_load_clamps_configuration_field_to_1() {
    // Snapshot tag numbers are part of the stable snapshot format.
    const TAG_CONFIGURATION: u16 = 1;

    let mut w = SnapshotWriter::new(
        <UsbHubDevice as IoSnapshot>::DEVICE_ID,
        <UsbHubDevice as IoSnapshot>::DEVICE_VERSION,
    );
    w.field_u8(TAG_CONFIGURATION, 7);
    let snap = w.finish();

    let mut hub = UsbHubDevice::new_with_ports(4);
    hub.load_state(&snap).unwrap();

    let resp = hub.handle_control_request(
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
}

