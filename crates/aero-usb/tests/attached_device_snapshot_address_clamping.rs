use aero_io_snapshot::io::state::{IoSnapshot, SnapshotReader, SnapshotWriter};
use aero_usb::device::AttachedUsbDevice;
use aero_usb::{ControlResponse, SetupPacket, UsbDeviceModel};

#[derive(Default)]
struct DummyUsbDevice;

impl UsbDeviceModel for DummyUsbDevice {
    fn handle_control_request(
        &mut self,
        _setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        ControlResponse::Stall
    }
}

#[test]
fn attached_usb_device_snapshot_load_clamps_address_fields_to_7bit_range() {
    // Snapshot tag numbers are part of the stable snapshot format.
    const TAG_ADDRESS: u16 = 1;
    const TAG_PENDING_ADDRESS: u16 = 2;

    let mut w = SnapshotWriter::new(
        <AttachedUsbDevice as IoSnapshot>::DEVICE_ID,
        <AttachedUsbDevice as IoSnapshot>::DEVICE_VERSION,
    );
    w.field_u8(TAG_ADDRESS, 250);
    w.field_u8(TAG_PENDING_ADDRESS, 250);
    let snap = w.finish();

    let mut dev = AttachedUsbDevice::new(Box::new(DummyUsbDevice));
    dev.load_state(&snap).unwrap();

    assert_eq!(
        dev.address(),
        0,
        "invalid USB addresses should restore as 0 to avoid collisions"
    );

    let snap2 = dev.save_state();
    let r = SnapshotReader::parse(&snap2, <AttachedUsbDevice as IoSnapshot>::DEVICE_ID).unwrap();
    assert_eq!(r.u8(TAG_ADDRESS).unwrap(), Some(0));
    assert_eq!(r.u8(TAG_PENDING_ADDRESS).unwrap(), None);
}
