#![cfg(target_arch = "wasm32")]

use aero_wasm::{UsbHidPassthroughBridge, WebHidPassthroughBridge, XhciControllerBridge};
use js_sys::JSON;
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

fn make_dummy_webhid() -> WebHidPassthroughBridge {
    let fixture = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/hid/webhid_normalized_mouse.json"
    ));
    let collections = JSON::parse(fixture).expect("parse webhid_normalized_mouse.json fixture");
    WebHidPassthroughBridge::new(0x1234, 0x5678, None, Some("test".to_string()), None, collections)
        .expect("WebHidPassthroughBridge::new ok")
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
    let path = serde_wasm_bindgen::to_value(&vec![0u32, 1u32, 1u32, 1u32, 1u32, 1u32, 1u32])
        .expect("path");
    let err = xhci
        .attach_usb_hid_passthrough_device(path, &dev)
        .expect_err("expected too-deep path to error");
    assert!(err.is_instance_of::<js_sys::Error>());
}

#[wasm_bindgen_test]
fn xhci_attach_rejects_reserved_webusb_root_port() {
    let mut xhci = make_controller();

    // Root port 1 is reserved by the xHCI bridge for WebUSB passthrough.
    let err = xhci
        .attach_hub(1, 4)
        .expect_err("expected attach_hub on reserved root port to error");
    assert!(err.is_instance_of::<js_sys::Error>());
}

#[wasm_bindgen_test]
fn xhci_attach_webhid_device_rejects_reserved_webusb_root_port() {
    let mut xhci = make_controller();

    let dev = make_dummy_webhid();
    let path = serde_wasm_bindgen::to_value(&vec![1u32]).expect("path");
    let err = xhci
        .attach_webhid_device(path, &dev)
        .expect_err("expected attach_webhid_device on reserved root port to error");
    assert!(err.is_instance_of::<js_sys::Error>());
}

#[wasm_bindgen_test]
fn xhci_attach_usb_hid_passthrough_device_rejects_reserved_webusb_root_port() {
    let mut xhci = make_controller();

    let dev = make_dummy_hid();
    let path = serde_wasm_bindgen::to_value(&vec![1u32]).expect("path");
    let err = xhci
        .attach_usb_hid_passthrough_device(path, &dev)
        .expect_err("expected attach_usb_hid_passthrough_device on reserved root port to error");
    assert!(err.is_instance_of::<js_sys::Error>());
}

#[wasm_bindgen_test]
fn xhci_detach_rejects_reserved_webusb_root_port() {
    let mut xhci = make_controller();

    let path = serde_wasm_bindgen::to_value(&vec![1u32]).expect("path");
    let err = xhci
        .detach_at_path(path)
        .expect_err("expected detach_at_path on reserved root port to error");
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
