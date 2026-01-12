#![cfg(target_arch = "wasm32")]

use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen_test]
fn machine_mouse_injection_exports_forward_without_panicking() {
    let mut m = aero_wasm::Machine::new(2 * 1024 * 1024).expect("Machine::new should succeed");

    // Keyboard injection should remain usable.
    m.inject_browser_key("KeyA", true);
    m.inject_browser_key("KeyA", false);

    // Mouse motion + wheel.
    m.inject_mouse_motion(10, 5, 1);

    // Button injection via DOM `MouseEvent.button` mapping.
    m.inject_mouse_button(0, true);
    m.inject_mouse_button(0, false);
    m.inject_mouse_button(1, true);
    m.inject_mouse_button(1, false);
    m.inject_mouse_button(2, true);
    m.inject_mouse_button(2, false);

    // Unknown button codes should be ignored.
    m.inject_mouse_button(0xFF, true);

    // Mask injection (bit0=left, bit1=right, bit2=middle).
    m.inject_mouse_buttons_mask(0x01);
    m.inject_mouse_buttons_mask(0x00);
    m.inject_mouse_buttons_mask(0x07);
    m.inject_mouse_buttons_mask(0x00);
}

