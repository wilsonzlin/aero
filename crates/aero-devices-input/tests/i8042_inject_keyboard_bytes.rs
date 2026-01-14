use aero_devices_input::I8042Controller;

fn drain_output(c: &mut I8042Controller) -> Vec<u8> {
    let mut out = Vec::new();
    while c.read_port(0x64) & 0x01 != 0 {
        out.push(c.read_port(0x60));
    }
    out
}

#[test]
fn i8042_inject_keyboard_bytes_translates_set2_to_set1_by_default() {
    let mut c = I8042Controller::new();

    // "A" make: Set-2 0x1C -> Set-1 0x1E.
    c.inject_keyboard_bytes(&[0x1C]);
    assert_eq!(drain_output(&mut c), vec![0x1E]);

    // "A" break: Set-2 F0 1C -> Set-1 0x9E.
    c.inject_keyboard_bytes(&[0xF0, 0x1C]);
    assert_eq!(drain_output(&mut c), vec![0x9E]);

    // Left arrow make: Set-2 E0 6B -> Set-1 E0 4B.
    c.inject_keyboard_bytes(&[0xE0, 0x6B]);
    assert_eq!(drain_output(&mut c), vec![0xE0, 0x4B]);
}

#[test]
fn i8042_inject_keyboard_bytes_respects_translation_bit() {
    let mut c = I8042Controller::new();

    // Disable Set-2 -> Set-1 translation (command byte bit 6).
    c.write_port(0x64, 0x60); // write command byte
    c.write_port(0x60, 0x05); // default 0x45 without 0x40

    c.inject_keyboard_bytes(&[0x1C]);
    assert_eq!(drain_output(&mut c), vec![0x1C]);

    c.inject_keyboard_bytes(&[0xF0, 0x1C]);
    assert_eq!(drain_output(&mut c), vec![0xF0, 0x1C]);
}

#[test]
fn i8042_translation_toggle_resets_prefix_state() {
    let mut c = I8042Controller::new();

    // Inject an extended scancode sequence. With translation enabled, the controller will deliver
    // the `0xE0` prefix first and keep internal "saw E0" state until the following byte is
    // consumed.
    c.inject_keyboard_bytes(&[0xE0, 0x1F]);

    // Disable translation before the guest drains the sequence. This causes the second byte to be
    // delivered raw, and (prior to the fix) leaves the translator stuck in the "saw E0" state.
    c.write_port(0x64, 0x60); // write command byte
    c.write_port(0x60, 0x05); // default 0x45 without 0x40 (disable translation)
    assert_eq!(drain_output(&mut c), vec![0xE0, 0x1F]);

    // Re-enable translation and inject a non-extended key. The translation toggle should reset any
    // stale prefix state so the key is translated correctly.
    c.write_port(0x64, 0x60); // write command byte
    c.write_port(0x60, 0x45); // enable translation
    c.inject_keyboard_bytes(&[0x1C]);
    assert_eq!(drain_output(&mut c), vec![0x1E]);
}
