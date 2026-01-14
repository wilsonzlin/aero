use aero_devices_input::scancode::{
    browser_code_to_set2, browser_code_to_set2_bytes, Set2Scancode,
};

#[test]
fn common_alphanumerics() {
    let key_a = browser_code_to_set2("KeyA").expect("KeyA should map");
    assert_eq!(
        key_a,
        Set2Scancode::Simple {
            make: 0x1C,
            extended: false
        }
    );
    assert_eq!(browser_code_to_set2_bytes("KeyA", true), Some(vec![0x1C]));
    assert_eq!(
        browser_code_to_set2_bytes("KeyA", false),
        Some(vec![0xF0, 0x1C])
    );

    assert_eq!(browser_code_to_set2_bytes("Digit1", true), Some(vec![0x16]));
    assert_eq!(
        browser_code_to_set2_bytes("Digit1", false),
        Some(vec![0xF0, 0x16])
    );

    assert_eq!(browser_code_to_set2_bytes("Enter", true), Some(vec![0x5A]));
    assert_eq!(
        browser_code_to_set2_bytes("Enter", false),
        Some(vec![0xF0, 0x5A])
    );
}

#[test]
fn function_keys() {
    assert_eq!(browser_code_to_set2_bytes("F1", true), Some(vec![0x05]));
    assert_eq!(
        browser_code_to_set2_bytes("F1", false),
        Some(vec![0xF0, 0x05])
    );

    assert_eq!(browser_code_to_set2_bytes("F12", true), Some(vec![0x07]));
    assert_eq!(
        browser_code_to_set2_bytes("F12", false),
        Some(vec![0xF0, 0x07])
    );
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
        let sc = browser_code_to_set2(code).unwrap_or_else(|| panic!("{code} should map"));
        assert_eq!(
            sc,
            Set2Scancode::Simple {
                make,
                extended: true
            },
            "{code} should be an E0-extended key"
        );
        assert_eq!(
            browser_code_to_set2_bytes(code, true),
            Some(vec![0xE0, make]),
            "{code} make bytes"
        );
        assert_eq!(
            browser_code_to_set2_bytes(code, false),
            Some(vec![0xE0, 0xF0, make]),
            "{code} break bytes"
        );
    }
}

#[test]
fn right_modifiers_and_numpad_extended() {
    let rctrl = browser_code_to_set2("ControlRight").expect("ControlRight should map");
    assert_eq!(
        rctrl,
        Set2Scancode::Simple {
            make: 0x14,
            extended: true
        }
    );
    assert_eq!(
        browser_code_to_set2_bytes("ControlRight", true),
        Some(vec![0xE0, 0x14])
    );
    assert_eq!(
        browser_code_to_set2_bytes("ControlRight", false),
        Some(vec![0xE0, 0xF0, 0x14])
    );

    let ralt = browser_code_to_set2("AltRight").expect("AltRight should map");
    assert_eq!(
        ralt,
        Set2Scancode::Simple {
            make: 0x11,
            extended: true
        }
    );
    assert_eq!(
        browser_code_to_set2_bytes("AltRight", true),
        Some(vec![0xE0, 0x11])
    );
    assert_eq!(
        browser_code_to_set2_bytes("AltRight", false),
        Some(vec![0xE0, 0xF0, 0x11])
    );

    assert_eq!(
        browser_code_to_set2_bytes("NumpadEnter", true),
        Some(vec![0xE0, 0x5A])
    );
    assert_eq!(
        browser_code_to_set2_bytes("NumpadEnter", false),
        Some(vec![0xE0, 0xF0, 0x5A])
    );

    assert_eq!(
        browser_code_to_set2_bytes("NumpadDivide", true),
        Some(vec![0xE0, 0x4A])
    );
    assert_eq!(
        browser_code_to_set2_bytes("NumpadDivide", false),
        Some(vec![0xE0, 0xF0, 0x4A])
    );
}

#[test]
fn special_multi_byte_sequences() {
    let print_screen = browser_code_to_set2("PrintScreen").expect("PrintScreen should map");
    assert_eq!(
        print_screen,
        Set2Scancode::Sequence {
            make: &[0xE0, 0x12, 0xE0, 0x7C],
            break_seq: &[0xE0, 0xF0, 0x7C, 0xE0, 0xF0, 0x12]
        }
    );
    assert_eq!(
        browser_code_to_set2_bytes("PrintScreen", true),
        Some(vec![0xE0, 0x12, 0xE0, 0x7C])
    );
    assert_eq!(
        browser_code_to_set2_bytes("PrintScreen", false),
        Some(vec![0xE0, 0xF0, 0x7C, 0xE0, 0xF0, 0x12])
    );

    let pause = browser_code_to_set2("Pause").expect("Pause should map");
    assert_eq!(
        pause,
        Set2Scancode::Sequence {
            make: &[0xE1, 0x14, 0x77, 0xE1, 0xF0, 0x14, 0xF0, 0x77],
            break_seq: &[]
        }
    );
    assert_eq!(
        browser_code_to_set2_bytes("Pause", true),
        Some(vec![0xE1, 0x14, 0x77, 0xE1, 0xF0, 0x14, 0xF0, 0x77])
    );
    assert_eq!(browser_code_to_set2_bytes("Pause", false), Some(Vec::new()));
}

#[test]
fn non_us_layout_keys_best_effort() {
    // Japanese Yen key is best-effort mapped to the same scancode as the ANSI backslash key.
    assert_eq!(
        browser_code_to_set2_bytes("IntlYen", true),
        Some(vec![0x5D])
    );
    assert_eq!(
        browser_code_to_set2_bytes("IntlYen", false),
        Some(vec![0xF0, 0x5D])
    );

    // Japanese Ro key is best-effort mapped to the ISO 102-key extra backslash key.
    assert_eq!(browser_code_to_set2_bytes("IntlRo", true), Some(vec![0x61]));
    assert_eq!(
        browser_code_to_set2_bytes("IntlRo", false),
        Some(vec![0xF0, 0x61])
    );
}
