#![cfg(not(target_arch = "wasm32"))]

use aero_io_snapshot::io::state::IoSnapshot as _;
use aero_devices::i8042::I8042_DATA_PORT;
use aero_usb::hid::KEYBOARD_LED_MASK;
use aero_usb::{ControlResponse, SetupPacket, UsbDeviceModel};
use aero_virtio::devices::input::{BTN_LEFT, KEY_A, VirtioInput};

#[test]
fn machine_can_inject_virtio_input_and_synthetic_usb_hid() {
    // Keep the RAM size small-ish for a fast smoke test while still being large enough for the
    // canonical PC machine configuration.
    let mut m = aero_wasm::Machine::new_with_input_backends(16 * 1024 * 1024, true, true)
        .expect("Machine::new_with_input_backends should succeed");

    // Synthetic USB HID devices are attached at reset, but are not "configured" until the guest
    // completes USB enumeration (`SET_CONFIGURATION`).
    assert!(!m.usb_hid_keyboard_configured());
    assert!(!m.usb_hid_mouse_configured());
    assert!(!m.usb_hid_gamepad_configured());
    assert!(!m.usb_hid_consumer_control_configured());

    // -----------------
    // Synthetic USB HID
    // -----------------
    let mut usb_kbd = m
        .debug_inner()
        .usb_hid_keyboard_handle()
        .expect("keyboard handle should exist when synthetic USB HID is enabled");
    let before = usb_kbd.save_state();
    m.inject_usb_hid_keyboard_usage(0x04, true); // Keyboard A
    let after = usb_kbd.save_state();
    assert_ne!(
        before, after,
        "USB keyboard state should change after injection"
    );

    // Mark the device configured and validate the public configured helper.
    let setup = SetupPacket {
        bm_request_type: 0x00, // HostToDevice | Standard | Device
        b_request: 0x09,       // SET_CONFIGURATION
        w_value: 1,
        w_index: 0,
        w_length: 0,
    };
    let resp = usb_kbd.handle_control_request(setup, None);
    assert!(matches!(resp, ControlResponse::Ack));
    assert!(m.usb_hid_keyboard_configured());

    assert_eq!(m.usb_hid_keyboard_leds(), 0);
    let leds = [0xffu8];
    let resp = usb_kbd.handle_control_request(
        SetupPacket {
            bm_request_type: 0x21, // HostToDevice | Class | Interface
            b_request: 0x09,       // SET_REPORT
            w_value: 2u16 << 8,    // Output report, ID 0
            w_index: 0,
            w_length: 1,
        },
        Some(&leds),
    );
    assert!(matches!(resp, ControlResponse::Ack));
    assert_eq!(m.usb_hid_keyboard_leds(), u32::from(KEYBOARD_LED_MASK));

    // -----------------
    // PS/2 (i8042) LEDs
    // -----------------
    //
    // When PS/2 is present, expose LEDs as a HID-style bitmask (matching the USB/virtio helpers):
    // - bit0: NumLock
    // - bit1: CapsLock
    // - bit2: ScrollLock
    assert_eq!(m.ps2_keyboard_leds(), 0);
    {
        let inner = m.debug_inner_mut();
        inner.io_write(I8042_DATA_PORT, 1, 0xED); // Set LEDs
        inner.io_write(I8042_DATA_PORT, 1, 0x04); // CapsLock (PS/2 bit2)
    }
    assert_eq!(m.ps2_keyboard_leds(), 0x02);

    let mut usb_mouse = m
        .debug_inner()
        .usb_hid_mouse_handle()
        .expect("mouse handle should exist when synthetic USB HID is enabled");
    let before = usb_mouse.save_state();
    // Buttons are tracked even when unconfigured, so this is observable without guest
    // enumeration.
    m.inject_usb_hid_mouse_buttons(0x01); // left down
    let after = usb_mouse.save_state();
    assert_ne!(
        before, after,
        "USB mouse state should change after injection"
    );

    // Mark the mouse configured and validate the public configured helper.
    let setup = SetupPacket {
        bm_request_type: 0x00, // HostToDevice | Standard | Device
        b_request: 0x09,       // SET_CONFIGURATION
        w_value: 1,
        w_index: 0,
        w_length: 0,
    };
    let resp = usb_mouse.handle_control_request(setup, None);
    assert!(matches!(resp, ControlResponse::Ack));
    assert!(m.usb_hid_mouse_configured());

    let mut usb_gamepad = m
        .debug_inner()
        .usb_hid_gamepad_handle()
        .expect("gamepad handle should exist when synthetic USB HID is enabled");
    let before = usb_gamepad.save_state();
    // Set buttons=1, hat=center (8), axes=0.
    // bytes: [01 00 08 00 00 00 00 00]
    m.inject_usb_hid_gamepad_report(0x0008_0001, 0x0000_0000);
    let after = usb_gamepad.save_state();
    assert_ne!(
        before, after,
        "USB gamepad state should change after injection"
    );

    // Mark the gamepad configured and validate the public configured helper.
    let setup = SetupPacket {
        bm_request_type: 0x00, // HostToDevice | Standard | Device
        b_request: 0x09,       // SET_CONFIGURATION
        w_value: 1,
        w_index: 0,
        w_length: 0,
    };
    let resp = usb_gamepad.handle_control_request(setup, None);
    assert!(matches!(resp, ControlResponse::Ack));
    assert!(m.usb_hid_gamepad_configured());

    let mut usb_consumer = m
        .debug_inner()
        .usb_hid_consumer_control_handle()
        .expect("consumer-control handle should exist when synthetic USB HID is enabled");
    let before = usb_consumer.save_state();
    m.inject_usb_hid_consumer_usage(0x00b5, true); // Scan Next Track
    let after = usb_consumer.save_state();
    assert_ne!(
        before, after,
        "USB consumer-control state should change after injection"
    );

    let setup = SetupPacket {
        bm_request_type: 0x00, // HostToDevice | Standard | Device
        b_request: 0x09,       // SET_CONFIGURATION
        w_value: 1,
        w_index: 0,
        w_length: 0,
    };
    let resp = usb_consumer.handle_control_request(setup, None);
    assert!(matches!(resp, ControlResponse::Ack));
    assert!(m.usb_hid_consumer_control_configured());

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
    let leds = virtio_kbd
        .borrow_mut()
        .device_mut::<VirtioInput>()
        .expect("virtio-input keyboard downcast")
        .leds_mask();
    assert_eq!(
        m.virtio_input_keyboard_leds(),
        u32::from(leds),
        "Machine::virtio_input_keyboard_leds should reflect device state"
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
