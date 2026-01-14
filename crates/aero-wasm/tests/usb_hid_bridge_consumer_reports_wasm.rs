#![cfg(target_arch = "wasm32")]

use wasm_bindgen::JsCast;
use wasm_bindgen_test::wasm_bindgen_test;

fn js_u8_array_to_vec(value: wasm_bindgen::JsValue) -> Vec<u8> {
    let arr: js_sys::Uint8Array = value
        .dyn_into()
        .expect("expected a Uint8Array from UsbHidBridge drain");
    let mut bytes = vec![0u8; arr.length() as usize];
    arr.copy_to(&mut bytes);
    bytes
}

#[wasm_bindgen_test]
fn usb_hid_bridge_consumer_event_produces_press_and_release_reports() {
    let mut bridge = aero_wasm::UsbHidBridge::new();

    // Volume Up (Consumer usage page 0x0C, usage ID 0x00E9).
    bridge.consumer_event(0x00e9, true);
    bridge.consumer_event(0x00e9, false);

    let first = bridge.drain_next_consumer_report();
    assert!(
        !first.is_null(),
        "expected a consumer-control report after key press"
    );
    assert_eq!(js_u8_array_to_vec(first), vec![0xe9, 0x00]);

    let second = bridge.drain_next_consumer_report();
    assert!(
        !second.is_null(),
        "expected a consumer-control report after key release"
    );
    assert_eq!(
        js_u8_array_to_vec(second),
        vec![0x00, 0x00],
        "release should clear the usage to 0"
    );

    assert!(
        bridge.drain_next_consumer_report().is_null(),
        "expected no further consumer-control reports"
    );
}
