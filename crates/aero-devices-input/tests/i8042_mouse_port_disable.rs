use aero_devices_input::{I8042Controller, Ps2MouseButton};

#[test]
fn i8042_drops_injected_mouse_motion_while_mouse_port_disabled() {
    let mut i8042 = I8042Controller::new();

    // Enable mouse reporting (ACK should be queued).
    i8042.write_port(0x64, 0xD4);
    i8042.write_port(0x60, 0xF4);
    assert_eq!(i8042.read_port(0x60), 0xFA);

    // Disable the mouse (aux) port.
    i8042.write_port(0x64, 0xA7);
    assert_eq!(
        i8042.read_port(0x64) & 0x01,
        0,
        "output buffer should be empty before injection"
    );

    // Host injects motion while the port is disabled; it should be dropped (not buffered).
    i8042.inject_mouse_motion(10, 0, 0);
    assert_eq!(
        i8042.read_port(0x64) & 0x01,
        0,
        "mouse motion should not be buffered while port is disabled"
    );

    // Re-enable the mouse port; the previously injected motion must not appear.
    i8042.write_port(0x64, 0xA8);
    assert_eq!(
        i8042.read_port(0x64) & 0x01,
        0,
        "re-enabling the mouse port should not release buffered motion"
    );

    // Fresh motion after enabling should still work.
    i8042.inject_mouse_motion(5, 0, 0);
    let status = i8042.read_port(0x64);
    assert_ne!(status & 0x01, 0, "output buffer should contain a packet");
    assert_ne!(status & 0x20, 0, "AUX bit should be set for mouse data");

    assert_eq!(i8042.read_port(0x60), 0x08);
    assert_eq!(i8042.read_port(0x60), 5);
    assert_eq!(i8042.read_port(0x60), 0);
}

#[test]
fn i8042_tracks_host_button_state_while_mouse_port_disabled() {
    let mut i8042 = I8042Controller::new();

    // Enable mouse reporting (ACK should be queued).
    i8042.write_port(0x64, 0xD4);
    i8042.write_port(0x60, 0xF4);
    assert_eq!(i8042.read_port(0x60), 0xFA);

    // Disable the mouse (aux) port.
    i8042.write_port(0x64, 0xA7);

    // Host presses left button while port disabled. No packet should be queued.
    i8042.inject_mouse_button(Ps2MouseButton::Left, true);
    assert_eq!(
        i8042.read_port(0x64) & 0x01,
        0,
        "mouse button changes should not be buffered while port is disabled"
    );

    // Re-enable the port and inject motion: the packet should reflect the held button.
    i8042.write_port(0x64, 0xA8);
    i8042.inject_mouse_motion(1, 0, 0);

    let status = i8042.read_port(0x64);
    assert_ne!(status & 0x01, 0, "output buffer should contain a packet");
    assert_ne!(status & 0x20, 0, "AUX bit should be set for mouse data");

    let b0 = i8042.read_port(0x60);
    assert_ne!(b0 & 0x01, 0, "left button bit should be set in status byte");
    assert_eq!(i8042.read_port(0x60), 1);
    assert_eq!(i8042.read_port(0x60), 0);
}
