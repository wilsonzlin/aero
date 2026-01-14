#![cfg(target_arch = "wasm32")]

use aero_wasm::UhciControllerBridge;
use wasm_bindgen::JsCast;
use wasm_bindgen_test::wasm_bindgen_test;

mod common;

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

