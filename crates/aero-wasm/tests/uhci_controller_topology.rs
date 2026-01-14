#![cfg(target_arch = "wasm32")]

use aero_wasm::{UhciControllerBridge, UsbHidPassthroughBridge, WebHidPassthroughBridge};
use js_sys::JSON;
use wasm_bindgen::JsCast;
use wasm_bindgen_test::wasm_bindgen_test;

mod common;

fn make_dummy_hid() -> UsbHidPassthroughBridge {
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
    WebHidPassthroughBridge::new(
        0x1234,
        0x5678,
        None,
        Some("test".to_string()),
        None,
        collections,
    )
    .expect("WebHidPassthroughBridge::new ok")
}

fn make_controller() -> UhciControllerBridge {
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x4000);
    UhciControllerBridge::new(guest_base, guest_size).expect("UhciControllerBridge::new")
}

#[wasm_bindgen_test]
fn uhci_attach_hub_rejects_reserved_webusb_root_port() {
    let mut uhci = make_controller();

    // Root port 1 is reserved by the UHCI bridge for WebUSB passthrough.
    let err = uhci
        .attach_hub(1, 4)
        .expect_err("expected attach_hub on reserved root port to error");
    assert!(err.is_instance_of::<js_sys::Error>());
}

#[wasm_bindgen_test]
fn uhci_detach_rejects_reserved_webusb_root_port() {
    let mut uhci = make_controller();

    let path = serde_wasm_bindgen::to_value(&vec![1u32]).expect("path");
    let err = uhci
        .detach_at_path(path)
        .expect_err("expected detach_at_path on reserved root port to error");
    assert!(err.is_instance_of::<js_sys::Error>());
}

#[wasm_bindgen_test]
fn uhci_attach_webhid_device_rejects_reserved_webusb_root_port() {
    let mut uhci = make_controller();

    let dev = make_dummy_webhid();
    let path = serde_wasm_bindgen::to_value(&vec![1u32]).expect("path");
    let err = uhci
        .attach_webhid_device(path, &dev)
        .expect_err("expected attach_webhid_device on reserved root port to error");
    assert!(err.is_instance_of::<js_sys::Error>());
}

#[wasm_bindgen_test]
fn uhci_attach_usb_hid_passthrough_device_rejects_reserved_webusb_root_port() {
    let mut uhci = make_controller();

    let dev = make_dummy_hid();
    let path = serde_wasm_bindgen::to_value(&vec![1u32]).expect("path");
    let err = uhci
        .attach_usb_hid_passthrough_device(path, &dev)
        .expect_err("expected attach_usb_hid_passthrough_device on reserved root port to error");
    assert!(err.is_instance_of::<js_sys::Error>());
}
