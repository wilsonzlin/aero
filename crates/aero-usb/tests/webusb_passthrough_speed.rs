use aero_io_snapshot::io::state::{IoSnapshot, SnapshotError, SnapshotWriter};
use aero_usb::passthrough::UsbPassthroughDevice;
use aero_usb::{UsbDeviceModel, UsbSpeed, UsbWebUsbPassthroughDevice};

#[test]
fn webusb_passthrough_device_reports_configured_speed_and_snapshots_it() {
    let dev = UsbWebUsbPassthroughDevice::new_with_speed(UsbSpeed::High);
    assert_eq!(UsbDeviceModel::speed(&dev), UsbSpeed::High);

    let snapshot = dev.save_state();
    let mut restored = UsbWebUsbPassthroughDevice::new();
    restored
        .load_state(&snapshot)
        .expect("snapshot restore should succeed");
    assert_eq!(UsbDeviceModel::speed(&restored), UsbSpeed::High);
}

#[test]
fn webusb_passthrough_device_snapshot_rejects_invalid_speed_encoding() {
    // These tags are part of the stable `UsbWebUsbPassthroughDevice` snapshot contract.
    const TAG_SPEED: u16 = 2;
    const TAG_PASSTHROUGH: u16 = 4;

    let passthrough = UsbPassthroughDevice::new();
    let mut w = SnapshotWriter::new(
        <UsbWebUsbPassthroughDevice as IoSnapshot>::DEVICE_ID,
        <UsbWebUsbPassthroughDevice as IoSnapshot>::DEVICE_VERSION,
    );
    w.field_u8(TAG_SPEED, 99);
    w.field_bytes(TAG_PASSTHROUGH, passthrough.save_state());
    let bytes = w.finish();

    let mut restored = UsbWebUsbPassthroughDevice::new();
    let err = restored
        .load_state(&bytes)
        .expect_err("expected invalid speed encoding to fail");
    assert!(
        matches!(err, SnapshotError::InvalidFieldEncoding(_)),
        "expected InvalidFieldEncoding, got {err:?}"
    );
}
