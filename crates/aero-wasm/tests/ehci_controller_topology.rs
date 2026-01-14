#![cfg(target_arch = "wasm32")]

use aero_wasm::EhciControllerBridge;
use wasm_bindgen::JsCast;
use wasm_bindgen_test::wasm_bindgen_test;

mod common;

fn make_controller() -> EhciControllerBridge {
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x4000);
    EhciControllerBridge::new(guest_base, guest_size).expect("EhciControllerBridge::new")
}

#[wasm_bindgen_test]
fn ehci_attach_hub_rejects_reserved_webusb_root_port() {
    let mut ehci = make_controller();

    // Root port 0 is reserved by the EHCI bridge for WebUSB passthrough.
    let err = ehci
        .attach_hub(0, 4)
        .expect_err("expected attach_hub on reserved root port to error");
    assert!(err.is_instance_of::<js_sys::Error>());
}

#[wasm_bindgen_test]
fn ehci_detach_rejects_reserved_webusb_root_port() {
    let mut ehci = make_controller();

    let path = serde_wasm_bindgen::to_value(&vec![0u32]).expect("path");
    let err = ehci
        .detach_at_path(path)
        .expect_err("expected detach_at_path on reserved root port to error");
    assert!(err.is_instance_of::<js_sys::Error>());
}
