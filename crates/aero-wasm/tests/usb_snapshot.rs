#![cfg(target_arch = "wasm32")]

use aero_wasm::{UhciControllerBridge, UhciRuntime, UsbHidPassthroughBridge, WebUsbUhciBridge};
use wasm_bindgen_test::wasm_bindgen_test;

const MIN_REPORT_DESCRIPTOR: &[u8] = &[
    0x06, 0x00, 0xff, // Usage Page (Vendor-defined 0xFF00)
    0x09, 0x01, // Usage (0x01)
    0xa1, 0x01, // Collection (Application)
    0x09, 0x02, //   Usage (0x02)
    0x15, 0x00, //   Logical Minimum (0)
    0x26, 0xff, 0x00, //   Logical Maximum (255)
    0x75, 0x08, //   Report Size (8)
    0x95, 0x01, //   Report Count (1)
    0x81, 0x02, //   Input (Data,Var,Abs)
    0xc0, // End Collection
];

#[wasm_bindgen_test]
fn uhci_controller_bridge_snapshot_is_deterministic_and_roundtrips() {
    let mut guest = vec![0u8; 0x8000];
    let guest_base = guest.as_mut_ptr() as u32;

    let mut bridge = UhciControllerBridge::new(guest_base, guest.len() as u32)
        .expect("new UhciControllerBridge");

    bridge.attach_hub(0, 1).expect("attach_hub ok");

    let dev = UsbHidPassthroughBridge::new(
        0x1234,
        0x5678,
        None,
        Some("Test HID".to_string()),
        None,
        MIN_REPORT_DESCRIPTOR.to_vec(),
        false,
        None,
        None,
    );

    let path = serde_wasm_bindgen::to_value(&vec![0u32, 1u32]).expect("path to_value");
    bridge
        .attach_usb_hid_passthrough_device(path, &dev)
        .expect("attach_usb_hid_passthrough_device ok");

    let snap1 = bridge.snapshot_state().to_vec();
    assert!(
        snap1.len() > 16,
        "expected snapshot to contain at least the header + state fields"
    );

    let snap2 = bridge.snapshot_state().to_vec();
    assert_eq!(snap1, snap2, "snapshot bytes should be deterministic");

    bridge.restore_state(&snap1).expect("restore_state ok");

    let snap3 = bridge.snapshot_state().to_vec();
    assert_eq!(snap1, snap3, "snapshot should roundtrip");
}

#[wasm_bindgen_test]
fn webusb_uhci_bridge_snapshot_is_deterministic_and_roundtrips() {
    let dev = UsbHidPassthroughBridge::new(
        0x1234,
        0x5678,
        None,
        Some("Test HID".to_string()),
        None,
        MIN_REPORT_DESCRIPTOR.to_vec(),
        false,
        None,
        None,
    );

    let mut bridge = WebUsbUhciBridge::new(0);
    bridge.set_connected(true);

    let path = serde_wasm_bindgen::to_value(&vec![0u32, 1u32]).expect("path to_value");
    bridge
        .attach_usb_hid_passthrough_device(path, &dev)
        .expect("attach_usb_hid_passthrough_device ok");

    let snap1 = bridge.snapshot_state().to_vec();
    assert!(snap1.len() > 16, "expected non-empty snapshot bytes");

    let snap2 = bridge.snapshot_state().to_vec();
    assert_eq!(snap1, snap2, "snapshot bytes should be deterministic");

    bridge.restore_state(&snap1).expect("restore_state ok");
    let snap3 = bridge.snapshot_state().to_vec();
    assert_eq!(snap1, snap3, "snapshot should roundtrip");
}

#[wasm_bindgen_test]
fn uhci_runtime_snapshot_is_deterministic_and_roundtrips() {
    let mut guest = vec![0u8; 0x8000];
    let guest_base = guest.as_mut_ptr() as u32;
    let mut runtime = UhciRuntime::new(guest_base, guest.len() as u32).expect("new UhciRuntime");

    runtime.webusb_attach(None).expect("webusb_attach ok");

    let snap1 = runtime.snapshot_state().to_vec();
    assert!(snap1.len() > 16, "expected non-empty snapshot bytes");

    let snap2 = runtime.snapshot_state().to_vec();
    assert_eq!(snap1, snap2, "snapshot bytes should be deterministic");

    runtime.restore_state(&snap1).expect("restore_state ok");
    let snap3 = runtime.snapshot_state().to_vec();
    assert_eq!(snap1, snap3, "snapshot should roundtrip");
}
