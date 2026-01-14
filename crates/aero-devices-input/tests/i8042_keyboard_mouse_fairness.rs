use aero_devices_input::I8042Controller;

#[test]
fn i8042_service_output_alternates_keyboard_and_mouse_when_both_pending() {
    let mut c = I8042Controller::new();

    // Enable mouse reporting so injected motion actually queues bytes.
    // Command sequence:
    // - 0xD4: next data byte goes to the mouse device
    // - 0xF4: enable reporting (mouse should ACK with 0xFA)
    c.write_port(0x64, 0xD4);
    c.write_port(0x60, 0xF4);

    // Drain the ACK.
    let status = c.read_port(0x64);
    assert_ne!(
        status & 0x01,
        0,
        "mouse ACK should be present in output buffer"
    );
    assert_ne!(
        status & 0x20,
        0,
        "mouse ACK should have AUX bit set in status register"
    );
    assert_eq!(c.read_port(0x60), 0xFA);
    assert_eq!(
        c.read_port(0x64) & 0x01,
        0,
        "output buffer should be empty after draining ACK"
    );

    // Ensure keyboard has >1 pending byte (one will land in the controller output buffer and one
    // should remain queued), and that the mouse has a multi-byte packet queued at the same time.
    c.inject_key_scancode_bytes(&[0x1c]);
    c.inject_key_scancode_bytes(&[0x32]);
    c.inject_mouse_motion(1, 0, 0);

    // Drain output while recording the AUX bit (mouse vs keyboard/controller).
    let mut aux_bits = Vec::new();
    let mut bytes = Vec::new();
    for _ in 0..64 {
        let status = c.read_port(0x64);
        if status & 0x01 == 0 {
            break;
        }
        aux_bits.push((status & 0x20) != 0);
        bytes.push(c.read_port(0x60));
    }

    assert!(
        aux_bits.len() >= 4,
        "expected at least 4 bytes of output, got aux_bits={aux_bits:?} bytes={bytes:?}"
    );

    // When both keyboard and mouse have pending data, service_output() should alternate between
    // sources to avoid starving either device.
    assert_eq!(
        &aux_bits[..4],
        &[false, true, false, true],
        "expected keyboard/mouse alternation in the first few bytes, got aux_bits={aux_bits:?} bytes={bytes:?}"
    );
}

#[test]
fn i8042_service_output_does_not_update_prefer_mouse_when_draining_pending_fifo() {
    let mut c = I8042Controller::new();

    // Enable mouse reporting so injected motion actually queues bytes.
    c.write_port(0x64, 0xD4);
    c.write_port(0x60, 0xF4);

    // Drain the ACK.
    let status = c.read_port(0x64);
    assert_ne!(status & 0x01, 0, "mouse ACK should be present in output buffer");
    assert_ne!(
        status & 0x20,
        0,
        "mouse ACK should have AUX bit set in status register"
    );
    assert_eq!(c.read_port(0x60), 0xFA);
    assert_eq!(
        c.read_port(0x64) & 0x01,
        0,
        "output buffer should be empty after draining ACK"
    );

    // Queue multiple keyboard bytes while the output buffer is already full.
    // Each inject call invokes `service_output()`, but once the first byte is loaded into the
    // guest-visible output buffer, additional injected bytes remain pending inside the PS/2 device.
    c.inject_key_scancode_bytes(&[0x1c]);
    c.inject_key_scancode_bytes(&[0x32]);
    c.inject_key_scancode_bytes(&[0x21]);
    c.inject_key_scancode_bytes(&[0x23]);

    // Queue a 3-byte mouse packet (still while the output buffer is full).
    c.inject_mouse_motion(5, 3, 0);

    // Drain output while recording the AUX bit (mouse vs keyboard/controller).
    let mut aux_bits = Vec::new();
    let mut bytes = Vec::new();
    for _ in 0..64 {
        let status = c.read_port(0x64);
        if status & 0x01 == 0 {
            break;
        }
        aux_bits.push((status & 0x20) != 0);
        bytes.push(c.read_port(0x60));
    }

    assert_eq!(
        aux_bits.len(),
        7,
        "expected 4 keyboard bytes + 3 mouse bytes, got aux_bits={aux_bits:?} bytes={bytes:?}"
    );

    // When the guest drains the output buffer, the controller immediately refills it.
    //
    // Note: the canonical i8042 model only updates the `prefer_mouse` toggle when refilling from
    // device queues; draining bytes that were already queued in the pending FIFO does not affect
    // the toggle. This means output sources may repeat even when both keyboard+mouse still have
    // pending bytes.
    assert_eq!(
        &aux_bits[..],
        &[false, true, false, false, true, true, false],
        "expected deterministic AUX-bit pattern, got aux_bits={aux_bits:?} bytes={bytes:?}"
    );
}
