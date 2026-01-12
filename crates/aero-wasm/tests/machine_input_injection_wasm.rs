#![cfg(target_arch = "wasm32")]

use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen_test]
fn machine_mouse_injection_exports_forward_without_panicking() {
    // Verify enum discriminants stay stable in the wasm build.
    assert_eq!(aero_wasm::MouseButton::Left as u8, 0);
    assert_eq!(aero_wasm::MouseButton::Middle as u8, 1);
    assert_eq!(aero_wasm::MouseButton::Right as u8, 2);
    assert_eq!(aero_wasm::MouseButton::Back as u8, 3);
    assert_eq!(aero_wasm::MouseButton::Forward as u8, 4);

    assert_eq!(aero_wasm::MouseButtons::Left as u8, 0x01);
    assert_eq!(aero_wasm::MouseButtons::Right as u8, 0x02);
    assert_eq!(aero_wasm::MouseButtons::Middle as u8, 0x04);
    assert_eq!(aero_wasm::MouseButtons::Back as u8, 0x08);
    assert_eq!(aero_wasm::MouseButtons::Forward as u8, 0x10);

    let mut m = aero_wasm::Machine::new(2 * 1024 * 1024).expect("Machine::new should succeed");

    // Keyboard injection should remain usable.
    m.inject_browser_key("KeyA", true);
    m.inject_browser_key("KeyA", false);

    // Mouse motion + wheel.
    m.inject_mouse_motion(10, 5, 1);

    // Button injection via DOM `MouseEvent.button` mapping.
    m.inject_mouse_button(aero_wasm::MouseButton::Left as u8, true);
    m.inject_mouse_button(aero_wasm::MouseButton::Left as u8, false);
    m.inject_mouse_button(aero_wasm::MouseButton::Middle as u8, true);
    m.inject_mouse_button(aero_wasm::MouseButton::Middle as u8, false);
    m.inject_mouse_button(aero_wasm::MouseButton::Right as u8, true);
    m.inject_mouse_button(aero_wasm::MouseButton::Right as u8, false);
    m.inject_mouse_button(aero_wasm::MouseButton::Back as u8, true);
    m.inject_mouse_button(aero_wasm::MouseButton::Back as u8, false);
    m.inject_mouse_button(aero_wasm::MouseButton::Forward as u8, true);
    m.inject_mouse_button(aero_wasm::MouseButton::Forward as u8, false);

    // Unknown button codes should be ignored.
    m.inject_mouse_button(0xFF, true);

    // Mask injection (matches DOM `MouseEvent.buttons`).
    m.inject_mouse_buttons_mask(aero_wasm::MouseButtons::Left as u8);
    m.inject_mouse_buttons_mask(0x00);
    m.inject_mouse_buttons_mask(aero_wasm::MouseButtons::Back as u8);
    m.inject_mouse_buttons_mask(0x00);
    m.inject_mouse_buttons_mask(
        (aero_wasm::MouseButtons::Left as u8)
            | (aero_wasm::MouseButtons::Right as u8)
            | (aero_wasm::MouseButtons::Middle as u8)
            | (aero_wasm::MouseButtons::Back as u8)
            | (aero_wasm::MouseButtons::Forward as u8),
    );
    m.inject_mouse_buttons_mask(0x00);
}
