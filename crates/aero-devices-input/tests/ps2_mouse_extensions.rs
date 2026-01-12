use aero_devices_input::{Ps2Mouse, Ps2MouseButton};

fn send_sample_rate(mouse: &mut Ps2Mouse, rate: u8) {
    // SET_SAMPLE_RATE
    mouse.receive_byte(0xF3);
    assert_eq!(
        mouse.pop_output(),
        Some(0xFA),
        "mouse should ACK SET_SAMPLE_RATE command"
    );

    mouse.receive_byte(rate);
    assert_eq!(
        mouse.pop_output(),
        Some(0xFA),
        "mouse should ACK SET_SAMPLE_RATE data byte"
    );

    assert!(
        !mouse.has_output(),
        "unexpected extra output bytes after SET_SAMPLE_RATE({rate})"
    );
}

fn enable_reporting(mouse: &mut Ps2Mouse) {
    mouse.receive_byte(0xF4);
    assert_eq!(
        mouse.pop_output(),
        Some(0xFA),
        "mouse should ACK ENABLE_DATA_REPORTING"
    );
    assert!(
        !mouse.has_output(),
        "unexpected extra output bytes after ENABLE_DATA_REPORTING"
    );
}

fn take_bytes(mouse: &mut Ps2Mouse, len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    for _ in 0..len {
        out.push(mouse.pop_output().expect("expected mouse output byte"));
    }
    assert!(
        !mouse.has_output(),
        "unexpected extra output bytes after taking packet"
    );
    out
}

#[test]
fn ps2_mouse_does_not_emit_packets_for_wheel_without_intellimouse_extension() {
    let mut mouse = Ps2Mouse::new();

    enable_reporting(&mut mouse);

    // Without the IntelliMouse extension, pure wheel movement should be ignored rather than
    // producing a spurious "zero-motion" PS/2 packet.
    mouse.inject_motion(0, 0, 1);
    assert!(!mouse.has_output());
}

#[test]
fn ps2_mouse_suppresses_side_buttons_until_explorer_extension_enabled() {
    let mut mouse = Ps2Mouse::new();
    enable_reporting(&mut mouse);

    // Side/back button is only representable once the IntelliMouse Explorer extension is active.
    mouse.inject_button(Ps2MouseButton::Side, true);
    assert!(
        !mouse.has_output(),
        "side button should not emit a packet before IntelliMouse Explorer is enabled"
    );

    // Enable IntelliMouse Explorer mode (device ID 0x04).
    send_sample_rate(&mut mouse, 200);
    send_sample_rate(&mut mouse, 200);
    send_sample_rate(&mut mouse, 80);
    assert_eq!(mouse.device_id(), 0x04);

    // Now a movement packet should include the held side button bit (bit 4 of the 4th byte).
    mouse.inject_motion(1, 0, 0);
    assert_eq!(take_bytes(&mut mouse, 4), vec![0x08, 0x01, 0x00, 0x10]);
}

#[test]
fn ps2_mouse_5button_packets_encode_side_extra_and_wheel() {
    let mut mouse = Ps2Mouse::new();

    // Enable IntelliMouse Explorer mode (device ID 0x04).
    send_sample_rate(&mut mouse, 200);
    send_sample_rate(&mut mouse, 200);
    send_sample_rate(&mut mouse, 80);
    assert_eq!(mouse.device_id(), 0x04);

    enable_reporting(&mut mouse);

    // Side/back button should be encoded as bit4 of the 4th byte.
    mouse.inject_button(Ps2MouseButton::Side, true);
    assert_eq!(take_bytes(&mut mouse, 4), vec![0x08, 0x00, 0x00, 0x10]);

    // Extra/forward button should be encoded as bit5 of the 4th byte.
    mouse.inject_button(Ps2MouseButton::Extra, true);
    assert_eq!(take_bytes(&mut mouse, 4), vec![0x08, 0x00, 0x00, 0x30]);

    // Wheel deltas occupy bits 0..3.
    mouse.inject_motion(0, 0, 1);
    assert_eq!(take_bytes(&mut mouse, 4), vec![0x08, 0x00, 0x00, 0x31]);

    // Negative wheel deltas are two's complement in the low nibble.
    mouse.inject_motion(0, 0, -1);
    assert_eq!(take_bytes(&mut mouse, 4), vec![0x08, 0x00, 0x00, 0x3F]);
}
