use emulator::io::input::{ps2_set2_scancode_for_code, Ps2Set2Scancode};

#[test]
fn common_alphanumerics() {
    let key_a = ps2_set2_scancode_for_code("KeyA").expect("KeyA should map");
    assert_eq!(
        key_a,
        Ps2Set2Scancode::Simple {
            make: 0x1C,
            extended: false
        }
    );
    assert_eq!(key_a.bytes(true), vec![0x1C]);
    assert_eq!(key_a.bytes(false), vec![0xF0, 0x1C]);

    let digit_1 = ps2_set2_scancode_for_code("Digit1").expect("Digit1 should map");
    assert_eq!(digit_1.bytes(true), vec![0x16]);
    assert_eq!(digit_1.bytes(false), vec![0xF0, 0x16]);

    let enter = ps2_set2_scancode_for_code("Enter").expect("Enter should map");
    assert_eq!(enter.bytes(true), vec![0x5A]);
    assert_eq!(enter.bytes(false), vec![0xF0, 0x5A]);
}

#[test]
fn function_keys() {
    let f1 = ps2_set2_scancode_for_code("F1").expect("F1 should map");
    assert_eq!(f1.bytes(true), vec![0x05]);
    assert_eq!(f1.bytes(false), vec![0xF0, 0x05]);

    let f12 = ps2_set2_scancode_for_code("F12").expect("F12 should map");
    assert_eq!(f12.bytes(true), vec![0x07]);
    assert_eq!(f12.bytes(false), vec![0xF0, 0x07]);
}

#[test]
fn extended_navigation_cluster() {
    for (code, make) in [
        ("Insert", 0x70),
        ("Delete", 0x71),
        ("Home", 0x6C),
        ("End", 0x69),
        ("PageUp", 0x7D),
        ("PageDown", 0x7A),
        ("ArrowUp", 0x75),
        ("ArrowDown", 0x72),
        ("ArrowLeft", 0x6B),
        ("ArrowRight", 0x74),
    ] {
        let sc = ps2_set2_scancode_for_code(code).unwrap_or_else(|| panic!("{code} should map"));
        assert_eq!(
            sc,
            Ps2Set2Scancode::Simple {
                make,
                extended: true
            },
            "{code} should be an E0-extended key"
        );
        assert_eq!(sc.bytes(true), vec![0xE0, make], "{code} make bytes");
        assert_eq!(
            sc.bytes(false),
            vec![0xE0, 0xF0, make],
            "{code} break bytes"
        );
    }
}

#[test]
fn right_modifiers_and_numpad_extended() {
    let rctrl = ps2_set2_scancode_for_code("ControlRight").expect("ControlRight should map");
    assert_eq!(
        rctrl,
        Ps2Set2Scancode::Simple {
            make: 0x14,
            extended: true
        }
    );
    assert_eq!(rctrl.bytes(true), vec![0xE0, 0x14]);
    assert_eq!(rctrl.bytes(false), vec![0xE0, 0xF0, 0x14]);

    let ralt = ps2_set2_scancode_for_code("AltRight").expect("AltRight should map");
    assert_eq!(
        ralt,
        Ps2Set2Scancode::Simple {
            make: 0x11,
            extended: true
        }
    );
    assert_eq!(ralt.bytes(true), vec![0xE0, 0x11]);
    assert_eq!(ralt.bytes(false), vec![0xE0, 0xF0, 0x11]);

    let np_enter = ps2_set2_scancode_for_code("NumpadEnter").expect("NumpadEnter should map");
    assert_eq!(np_enter.bytes(true), vec![0xE0, 0x5A]);
    assert_eq!(np_enter.bytes(false), vec![0xE0, 0xF0, 0x5A]);

    let np_div = ps2_set2_scancode_for_code("NumpadDivide").expect("NumpadDivide should map");
    assert_eq!(np_div.bytes(true), vec![0xE0, 0x4A]);
    assert_eq!(np_div.bytes(false), vec![0xE0, 0xF0, 0x4A]);
}

#[test]
fn special_multi_byte_sequences() {
    let print_screen = ps2_set2_scancode_for_code("PrintScreen").expect("PrintScreen should map");
    assert_eq!(
        print_screen,
        Ps2Set2Scancode::Sequence {
            make: &[0xE0, 0x12, 0xE0, 0x7C],
            break_seq: &[0xE0, 0xF0, 0x7C, 0xE0, 0xF0, 0x12]
        }
    );

    let pause = ps2_set2_scancode_for_code("Pause").expect("Pause should map");
    assert_eq!(
        pause,
        Ps2Set2Scancode::Sequence {
            make: &[0xE1, 0x14, 0x77, 0xE1, 0xF0, 0x14, 0xF0, 0x77],
            break_seq: &[]
        }
    );
}

#[test]
fn non_us_layout_keys_best_effort() {
    // Japanese Yen key is best-effort mapped to the same scancode as the ANSI backslash key.
    let yen = ps2_set2_scancode_for_code("IntlYen").expect("IntlYen should map");
    assert_eq!(yen.bytes(true), vec![0x5D]);
    assert_eq!(yen.bytes(false), vec![0xF0, 0x5D]);

    // Japanese Ro key is best-effort mapped to the ISO 102-key extra backslash key.
    let ro = ps2_set2_scancode_for_code("IntlRo").expect("IntlRo should map");
    assert_eq!(ro.bytes(true), vec![0x61]);
    assert_eq!(ro.bytes(false), vec![0xF0, 0x61]);
}
