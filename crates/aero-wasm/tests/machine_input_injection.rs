#[test]
fn machine_mouse_injection_exports_forward_without_panicking() {
    // Verify the exported enum mappings stay stable.
    assert_eq!(aero_wasm::MouseButton::Left as u8, 0);
    assert_eq!(aero_wasm::MouseButton::Middle as u8, 1);
    assert_eq!(aero_wasm::MouseButton::Right as u8, 2);

    assert_eq!(aero_wasm::MouseButtons::Left as u8, 0x01);
    assert_eq!(aero_wasm::MouseButtons::Right as u8, 0x02);
    assert_eq!(aero_wasm::MouseButtons::Middle as u8, 0x04);

    // Keep the RAM size small-ish for a fast smoke test while still being large enough for the
    // canonical PC machine configuration.
    let mut m = aero_wasm::Machine::new(16 * 1024 * 1024).expect("Machine::new should succeed");

    // Keyboard injection is already exposed; ensure it still works.
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
    m.inject_mouse_buttons_mask(aero_wasm::MouseButtons::Left as u8);
    m.inject_mouse_buttons_mask(0x00);
    m.inject_mouse_buttons_mask(
        (aero_wasm::MouseButtons::Left as u8)
            | (aero_wasm::MouseButtons::Right as u8)
            | (aero_wasm::MouseButtons::Middle as u8),
    );
    m.inject_mouse_buttons_mask(0x00);

    // Explicit helpers should also remain callable.
    m.inject_mouse_left(true);
    m.inject_mouse_left(false);
    m.inject_mouse_right(true);
    m.inject_mouse_right(false);
    m.inject_mouse_middle(true);
    m.inject_mouse_middle(false);
}
