#![cfg(target_arch = "wasm32")]

use aero_wasm::UhciRuntime;
use wasm_bindgen_test::wasm_bindgen_test;

mod common;

fn make_runtime() -> UhciRuntime {
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x4000);
    UhciRuntime::new(guest_base, guest_size).expect("UhciRuntime::new")
}

#[wasm_bindgen_test]
fn uhci_runtime_webusb_drain_actions_returns_null_when_not_attached() {
    let mut rt = make_runtime();
    let drained = rt.webusb_drain_actions().expect("webusb_drain_actions ok");
    assert!(
        drained.is_null(),
        "expected webusb_drain_actions to return null when WebUSB is not attached"
    );
}

#[wasm_bindgen_test]
fn uhci_runtime_webusb_drain_actions_returns_null_when_attached_but_idle() {
    let mut rt = make_runtime();
    rt.webusb_attach(Some(1)).expect("webusb_attach ok");
    let drained = rt.webusb_drain_actions().expect("webusb_drain_actions ok");
    assert!(
        drained.is_null(),
        "expected webusb_drain_actions to return null when there are no queued actions"
    );
}

