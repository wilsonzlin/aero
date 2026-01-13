#![cfg(target_arch = "wasm32")]

use aero_wasm::{UsbHidPassthroughBridge, XhciControllerBridge};
use wasm_bindgen_test::wasm_bindgen_test;

mod common;

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
fn xhci_topology_snapshot_roundtrips() {
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x8000);

    let mut bridge =
        XhciControllerBridge::new(guest_base, guest_size).expect("new XhciControllerBridge");

    bridge.attach_hub(0, 4).expect("attach_hub ok");

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
    let snap2 = bridge.snapshot_state().to_vec();
    assert_eq!(snap1, snap2, "snapshot bytes should be deterministic");

    bridge.restore_state(&snap1).expect("restore_state ok");
    let snap3 = bridge.snapshot_state().to_vec();
    assert_eq!(snap1, snap3, "snapshot should roundtrip");
}

#[wasm_bindgen_test]
fn xhci_invalid_paths_error() {
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x8000);
    let mut bridge =
        XhciControllerBridge::new(guest_base, guest_size).expect("new XhciControllerBridge");

    // Empty path.
    let empty = serde_wasm_bindgen::to_value(&Vec::<u32>::new()).unwrap();
    assert!(bridge.detach_at_path(empty).is_err());

    // Root port out of range (max_root_port is always <= 255).
    let bad_root = serde_wasm_bindgen::to_value(&vec![256u32]).unwrap();
    assert!(bridge.detach_at_path(bad_root).is_err());

    // Hub port 0 is invalid (hub ports are 1-based).
    let bad_hub_port = serde_wasm_bindgen::to_value(&vec![0u32, 0u32]).unwrap();
    assert!(bridge.detach_at_path(bad_hub_port).is_err());
}
