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

fn make_test_usb_hid_device() -> UsbHidPassthroughBridge {
    UsbHidPassthroughBridge::new(
        0x1234,
        0x5678,
        None,
        Some("Test HID".to_string()),
        None,
        MIN_REPORT_DESCRIPTOR.to_vec(),
        false,
        None,
        None,
    )
}

#[wasm_bindgen_test]
fn xhci_controller_bridge_topology_attach_and_detach_are_wired() {
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x4000);

    let mut bridge = XhciControllerBridge::new(guest_base, guest_size).expect("new xHCI bridge");

    // Attach an external hub to root port 0 with the maximum port count representable in xHCI
    // route strings (4-bit port numbers).
    bridge.attach_hub(0, 15).expect("attach_hub ok");

    let dev = make_test_usb_hid_device();

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

#[wasm_bindgen_test]
fn xhci_controller_bridge_topology_rejects_reserved_webusb_root_port() {
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x4000);
    let mut bridge = XhciControllerBridge::new(guest_base, guest_size).expect("new xHCI bridge");

    // Default xHCI port count is >1, so root port 1 is reserved for WebUSB passthrough.
    assert!(
        bridge.attach_hub(1, 4).is_err(),
        "attach_hub on reserved root port should error"
    );

    let path = serde_wasm_bindgen::to_value(&vec![1u32]).expect("path to_value");
    assert!(
        bridge.detach_at_path(path).is_err(),
        "detach_at_path on reserved root port should error"
    );
}

#[wasm_bindgen_test]
fn xhci_controller_bridge_topology_rejects_invalid_ports_in_path() {
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x4000);
    let mut bridge = XhciControllerBridge::new(guest_base, guest_size).expect("new xHCI bridge");
    let dev = make_test_usb_hid_device();

    // Downstream hub ports are encoded in 4-bit route-string nibbles; 16 is not representable.
    let path = serde_wasm_bindgen::to_value(&vec![0u32, 16u32]).expect("path to_value");
    assert!(
        bridge
            .attach_usb_hid_passthrough_device(path.clone(), &dev)
            .is_err(),
        "attach_usb_hid_passthrough_device should reject hub port > 15"
    );
    assert!(
        bridge.detach_at_path(path).is_err(),
        "detach_at_path should reject hub port > 15"
    );
}

#[wasm_bindgen_test]
fn xhci_controller_bridge_topology_rejects_paths_deeper_than_route_string_limit() {
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x4000);
    let mut bridge = XhciControllerBridge::new(guest_base, guest_size).expect("new xHCI bridge");
    let dev = make_test_usb_hid_device();

    // Route String is 20 bits (5 nibbles), so xHCI can only represent 5 downstream hub tiers.
    // Path includes root port + 6 downstream hops (too deep).
    let path = serde_wasm_bindgen::to_value(&vec![0u32, 1u32, 1u32, 1u32, 1u32, 1u32, 1u32])
        .expect("path to_value");
    assert!(
        bridge
            .attach_usb_hid_passthrough_device(path.clone(), &dev)
            .is_err(),
        "attach_usb_hid_passthrough_device should reject paths deeper than route-string limit"
    );
    assert!(
        bridge.detach_at_path(path).is_err(),
        "detach_at_path should reject paths deeper than route-string limit"
    );
}
