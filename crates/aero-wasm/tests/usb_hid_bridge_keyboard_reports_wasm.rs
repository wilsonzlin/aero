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
fn usb_hid_bridge_keyboard_press_and_release_produce_reports() {
    let mut bridge = aero_wasm::UsbHidBridge::new();

    // Hold LeftShift + 'A', then release in reverse order. This exercises both modifier handling
    // and the regular 6-key array in the boot keyboard report.
    bridge.keyboard_event(0xe1, true); // LeftShift
    bridge.keyboard_event(0x04, true); // 'A'
    bridge.keyboard_event(0x04, false);
    bridge.keyboard_event(0xe1, false);

    let shift_down = bridge.drain_next_keyboard_report();
    assert!(
        !shift_down.is_null(),
        "expected a keyboard report after modifier press"
    );
    assert_eq!(
        js_u8_array_to_vec(shift_down),
        vec![0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
        "expected LeftShift to set modifier bit 1"
    );

    let a_down = bridge.drain_next_keyboard_report();
    assert!(!a_down.is_null(), "expected a keyboard report after key press");
    assert_eq!(
        js_u8_array_to_vec(a_down),
        vec![0x02, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00],
        "expected 'A' keycode (0x04) in the first slot while shift is held"
    );

    let a_up = bridge.drain_next_keyboard_report();
    assert!(
        !a_up.is_null(),
        "expected a keyboard report after key release"
    );
    assert_eq!(
        js_u8_array_to_vec(a_up),
        vec![0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
        "releasing 'A' should clear the key array but keep the modifier"
    );

    let shift_up = bridge.drain_next_keyboard_report();
    assert!(
        !shift_up.is_null(),
        "expected a keyboard report after modifier release"
    );
    assert_eq!(
        js_u8_array_to_vec(shift_up),
        vec![0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
        "releasing LeftShift should clear the modifier byte"
    );

    assert!(
        bridge.drain_next_keyboard_report().is_null(),
        "expected no further keyboard reports"
    );
}

