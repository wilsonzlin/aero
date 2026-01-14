#![cfg(not(target_arch = "wasm32"))]

use aero_io_snapshot::io::state::IoSnapshot as _;
use aero_virtio::devices::input::{VirtioInput, BTN_LEFT, KEY_A};

#[test]
fn machine_can_inject_virtio_input_and_synthetic_usb_hid() {
    // Keep the RAM size small-ish for a fast smoke test while still being large enough for the
    // canonical PC machine configuration.
    let mut m = aero_wasm::Machine::new_with_input_backends(16 * 1024 * 1024, true, true)
        .expect("Machine::new_with_input_backends should succeed");

    // -----------------
    // Synthetic USB HID
    // -----------------
    let usb_kbd = m
        .debug_inner()
        .usb_hid_keyboard_handle()
        .expect("keyboard handle should exist when synthetic USB HID is enabled");
    let before = usb_kbd.save_state();
    m.inject_usb_hid_keyboard_usage(0x04, true); // Keyboard A
    let after = usb_kbd.save_state();
    assert_ne!(before, after, "USB keyboard state should change after injection");

    let usb_mouse = m
        .debug_inner()
        .usb_hid_mouse_handle()
        .expect("mouse handle should exist when synthetic USB HID is enabled");
    let before = usb_mouse.save_state();
    // Buttons are tracked even when unconfigured, so this is observable without guest
    // enumeration.
    m.inject_usb_hid_mouse_buttons(0x01); // left down
    let after = usb_mouse.save_state();
    assert_ne!(before, after, "USB mouse state should change after injection");

    let usb_gamepad = m
        .debug_inner()
        .usb_hid_gamepad_handle()
        .expect("gamepad handle should exist when synthetic USB HID is enabled");
    let before = usb_gamepad.save_state();
    // Set buttons=1, hat=center (8), axes=0.
    // bytes: [01 00 08 00 00 00 00 00]
    m.inject_usb_hid_gamepad_report(0x0008_0001, 0x0000_0000);
    let after = usb_gamepad.save_state();
    assert_ne!(before, after, "USB gamepad state should change after injection");

    // -----------------
    // virtio-input
    // -----------------
    let virtio_kbd = m
        .debug_inner()
        .virtio_input_keyboard()
        .expect("virtio-input keyboard should exist when enabled");
    let before = virtio_kbd
        .borrow_mut()
        .device_mut::<VirtioInput>()
        .expect("virtio-input keyboard downcast")
        .pending_events_len();
    m.inject_virtio_key(KEY_A as u32, true);
    let after = virtio_kbd
        .borrow_mut()
        .device_mut::<VirtioInput>()
        .expect("virtio-input keyboard downcast")
        .pending_events_len();
    assert!(
        after > before,
        "virtio-input keyboard should queue events after injection"
    );

    let virtio_mouse = m
        .debug_inner()
        .virtio_input_mouse()
        .expect("virtio-input mouse should exist when enabled");
    let before = virtio_mouse
        .borrow_mut()
        .device_mut::<VirtioInput>()
        .expect("virtio-input mouse downcast")
        .pending_events_len();
    m.inject_virtio_mouse_rel(10, 5);
    m.inject_virtio_mouse_button(BTN_LEFT as u32, true);
    m.inject_virtio_mouse_wheel(1);
    let after = virtio_mouse
        .borrow_mut()
        .device_mut::<VirtioInput>()
        .expect("virtio-input mouse downcast")
        .pending_events_len();
    assert!(
        after > before,
        "virtio-input mouse should queue events after injection"
    );
}
