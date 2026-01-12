#[test]
fn machine_mouse_injection_exports_forward_without_panicking() {
    // Verify the exported enum mappings stay stable.
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

    // Keep the RAM size small-ish for a fast smoke test while still being large enough for the
    // canonical PC machine configuration.
    let mut m = aero_wasm::Machine::new(16 * 1024 * 1024).expect("Machine::new should succeed");

    // Keyboard injection is already exposed; ensure it still works.
    m.inject_browser_key("KeyA", true);
    m.inject_browser_key("KeyA", false);
    // Raw Set-2 scancode byte injection (matches `InputEventType.KeyScancode`).
    m.inject_key_scancode_bytes(0x1C, 1); // make
    m.inject_key_scancode_bytes(0x0000_1CF0, 2); // break: F0 1C (packed LE)
    m.inject_keyboard_bytes(&[0x1C]);

    // Mouse motion + wheel.
    m.inject_mouse_motion(10, 5, 1);
    // PS/2 coordinate variant (+Y up).
    m.inject_ps2_mouse_motion(10, 5, 1);

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
    // PS/2 alias (same bit mapping).
    m.inject_ps2_mouse_buttons(aero_wasm::MouseButtons::Left as u8);
    m.inject_ps2_mouse_buttons(0x00);

    // Explicit helpers should also remain callable.
    m.inject_mouse_left(true);
    m.inject_mouse_left(false);
    m.inject_mouse_right(true);
    m.inject_mouse_right(false);
    m.inject_mouse_middle(true);
    m.inject_mouse_middle(false);
    m.inject_mouse_back(true);
    m.inject_mouse_back(false);
    m.inject_mouse_forward(true);
    m.inject_mouse_forward(false);
}
