#![cfg(not(target_arch = "wasm32"))]

#[test]
fn machine_new_enables_synthetic_usb_hid_by_default() {
    let m = aero_wasm::Machine::new(2 * 1024 * 1024).expect("Machine::new should succeed");

    // `Machine::new` uses `MachineConfig::browser_defaults` plus `enable_synthetic_usb_hid=true`.
    // Assert the synthetic HID devices are present (even though the guest has not configured them
    // yet, so `*_configured()` returns false).
    let inner = m.debug_inner();
    assert!(
        inner.usb_hid_keyboard_handle().is_some(),
        "expected synthetic USB HID keyboard device to be attached by default"
    );
    assert!(
        inner.usb_hid_mouse_handle().is_some(),
        "expected synthetic USB HID mouse device to be attached by default"
    );
    assert!(
        inner.usb_hid_gamepad_handle().is_some(),
        "expected synthetic USB HID gamepad device to be attached by default"
    );
    assert!(
        inner.usb_hid_consumer_control_handle().is_some(),
        "expected synthetic USB HID consumer-control device to be attached by default"
    );

    assert!(!m.usb_hid_keyboard_configured());
    assert!(!m.usb_hid_mouse_configured());
    assert!(!m.usb_hid_gamepad_configured());
    assert!(!m.usb_hid_consumer_control_configured());
}

#[test]
fn machine_new_with_input_backends_can_disable_synthetic_usb_hid() {
    let m = aero_wasm::Machine::new_with_input_backends(2 * 1024 * 1024, false, false)
        .expect("Machine::new_with_input_backends should succeed");

    // With synthetic USB HID disabled, no built-in HID devices should be attached behind UHCI.
    let inner = m.debug_inner();
    assert!(
        inner.usb_hid_keyboard_handle().is_none(),
        "expected no synthetic USB HID keyboard device when disabled"
    );
    assert!(
        inner.usb_hid_mouse_handle().is_none(),
        "expected no synthetic USB HID mouse device when disabled"
    );
    assert!(
        inner.usb_hid_gamepad_handle().is_none(),
        "expected no synthetic USB HID gamepad device when disabled"
    );
    assert!(
        inner.usb_hid_consumer_control_handle().is_none(),
        "expected no synthetic USB HID consumer-control device when disabled"
    );

    // Public configured helpers should remain false when the devices are absent.
    assert!(!m.usb_hid_keyboard_configured());
    assert!(!m.usb_hid_mouse_configured());
    assert!(!m.usb_hid_gamepad_configured());
    assert!(!m.usb_hid_consumer_control_configured());
}
