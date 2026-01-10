use aero_devices_input::I8042Controller;

fn drain_bytes(i8042: &mut I8042Controller, count: usize) -> Vec<u8> {
    (0..count).map(|_| i8042.read_port(0x60)).collect()
}

#[test]
fn numpad_key_translates_without_e0_prefix() {
    let mut i8042 = I8042Controller::new();

    i8042.inject_browser_key("Numpad7", true);
    assert_eq!(drain_bytes(&mut i8042, 1), vec![0x47]);

    i8042.inject_browser_key("Numpad7", false);
    assert_eq!(drain_bytes(&mut i8042, 1), vec![0xC7]);
}

#[test]
fn meta_left_translates_to_set1_extended_windows_key() {
    let mut i8042 = I8042Controller::new();

    i8042.inject_browser_key("MetaLeft", true);
    assert_eq!(drain_bytes(&mut i8042, 2), vec![0xE0, 0x5B]);

    i8042.inject_browser_key("MetaLeft", false);
    assert_eq!(drain_bytes(&mut i8042, 2), vec![0xE0, 0xDB]);
}

#[test]
fn print_screen_translates_to_set1_multi_byte_sequence() {
    let mut i8042 = I8042Controller::new();

    i8042.inject_browser_key("PrintScreen", true);
    assert_eq!(drain_bytes(&mut i8042, 4), vec![0xE0, 0x2A, 0xE0, 0x37]);

    i8042.inject_browser_key("PrintScreen", false);
    assert_eq!(drain_bytes(&mut i8042, 4), vec![0xE0, 0xB7, 0xE0, 0xAA]);
}

#[test]
fn pause_translates_to_set1_make_sequence_only() {
    let mut i8042 = I8042Controller::new();

    i8042.inject_browser_key("Pause", true);
    assert_eq!(
        drain_bytes(&mut i8042, 6),
        vec![0xE1, 0x1D, 0x45, 0xE1, 0x9D, 0xC5]
    );
}

