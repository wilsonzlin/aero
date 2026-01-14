use aero_io_snapshot::io::state::IoSnapshot;
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
