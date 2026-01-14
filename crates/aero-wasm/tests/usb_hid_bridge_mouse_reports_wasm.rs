#![cfg(target_arch = "wasm32")]

use wasm_bindgen::JsCast;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen_test]
fn usb_hid_bridge_mouse_wheel2_produces_single_report() {
    let mut bridge = aero_wasm::UsbHidBridge::new();

    bridge.mouse_wheel2(5, 7);

    let report = bridge.drain_next_mouse_report();
    assert!(
        !report.is_null(),
        "expected a mouse report after wheel2 injection"
    );

    let arr: js_sys::Uint8Array = report
        .dyn_into()
        .expect("expected drain_next_mouse_report to return a Uint8Array");
    let mut bytes = vec![0u8; arr.length() as usize];
    arr.copy_to(&mut bytes);
    assert_eq!(bytes, vec![0, 0, 0, 5, 7]);

    assert!(
        bridge.drain_next_mouse_report().is_null(),
        "wheel2 should produce exactly one report for small deltas"
    );
}
