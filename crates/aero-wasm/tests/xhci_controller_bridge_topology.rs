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
fn xhci_controller_bridge_topology_attach_and_detach_are_wired() {
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x4000);

    let mut bridge = XhciControllerBridge::new(guest_base, guest_size).expect("new xHCI bridge");

    // Attach an external hub to root port 0 with the maximum port count representable in xHCI
    // route strings (4-bit port numbers).
    bridge.attach_hub(0, 15).expect("attach_hub ok");

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
        .attach_usb_hid_passthrough_device(path.clone(), &dev)
        .expect("attach_usb_hid_passthrough_device ok");

    // Detach should be idempotent.
    bridge
        .detach_at_path(path.clone())
        .expect("detach_at_path ok");
    bridge
        .detach_at_path(path.clone())
        .expect("detach_at_path idempotent");

    // Snapshot/restore should not panic even when topology helpers have been exercised.
    let snapshot = bridge.save_state();
    let mut bridge2 = XhciControllerBridge::new(guest_base, guest_size).expect("new xHCI bridge2");
    bridge2.load_state(&snapshot).expect("load_state ok");
}
