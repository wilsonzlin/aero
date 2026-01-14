#![cfg(target_arch = "wasm32")]

use aero_wasm::{UsbHidPassthroughBridge, XhciControllerBridge};
use wasm_bindgen::JsCast;
use wasm_bindgen_test::wasm_bindgen_test;

mod common;

fn make_controller() -> XhciControllerBridge {
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x4000);
    XhciControllerBridge::new(guest_base, guest_size).expect("XhciControllerBridge::new")
}

fn make_dummy_hid() -> UsbHidPassthroughBridge {
    // A minimal (possibly empty) HID report descriptor is sufficient for topology tests; we are not
    // exercising guest enumeration here.
    UsbHidPassthroughBridge::new(
        0x1234,
        0x5678,
        None,
        Some("test".to_string()),
        None,
        Vec::new(),
        false,
        None,
        None,
    )
}

#[wasm_bindgen_test]
fn xhci_attach_hub_and_device_at_valid_path_succeeds() {
    let mut xhci = make_controller();

    xhci.attach_hub(0, 4).expect("attach hub ok");

    let dev = make_dummy_hid();
    let path = serde_wasm_bindgen::to_value(&vec![0u32, 1u32]).expect("path");
    xhci.attach_usb_hid_passthrough_device(path, &dev)
        .expect("attach device ok");
}

#[wasm_bindgen_test]
fn xhci_attach_rejects_downstream_port_over_15() {
    let mut xhci = make_controller();
    xhci.attach_hub(0, 4).expect("attach hub ok");

    let dev = make_dummy_hid();
    let path = serde_wasm_bindgen::to_value(&vec![0u32, 16u32]).expect("path");
    let err = xhci
        .attach_usb_hid_passthrough_device(path, &dev)
        .expect_err("expected invalid hub port to error");
    assert!(err.is_instance_of::<js_sys::Error>());
}

#[wasm_bindgen_test]
fn xhci_attach_rejects_paths_deeper_than_5_hub_tiers() {
    let mut xhci = make_controller();
    xhci.attach_hub(0, 4).expect("attach hub ok");

    let dev = make_dummy_hid();
    // Root port + 6 downstream hubs (Route String only supports 5).
    let path = serde_wasm_bindgen::to_value(&vec![0u32, 1u32, 1u32, 1u32, 1u32, 1u32, 1u32]).expect("path");
    let err = xhci
        .attach_usb_hid_passthrough_device(path, &dev)
        .expect_err("expected too-deep path to error");
    assert!(err.is_instance_of::<js_sys::Error>());
}

#[wasm_bindgen_test]
fn xhci_detach_is_idempotent() {
    let mut xhci = make_controller();
    xhci.attach_hub(0, 4).expect("attach hub ok");

    let dev = make_dummy_hid();
    let path = serde_wasm_bindgen::to_value(&vec![0u32, 1u32]).expect("path");
    xhci.attach_usb_hid_passthrough_device(path.clone(), &dev)
        .expect("attach device ok");

    xhci.detach_at_path(path.clone()).expect("first detach ok");
    xhci.detach_at_path(path).expect("second detach ok");
}
