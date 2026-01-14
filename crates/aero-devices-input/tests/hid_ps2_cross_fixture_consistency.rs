use aero_devices_input::browser_code_to_set2_bytes;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

const HID_USAGE_KEYBOARD_JSON: &str =
    include_str!("../../../docs/fixtures/hid_usage_keyboard.json");
const SCANCODES_JSON: &str = include_str!("../../../tools/gen_scancodes/scancodes.json");

// Defensive limits: these JSON files are trusted repo data, but this is still an
// integration test that should never become accidentally expensive.
const MAX_HID_JSON_BYTES: usize = 256 * 1024;
const MAX_HID_ENTRIES: usize = 512;
const MAX_SCANCODES_JSON_BYTES: usize = 256 * 1024;
const MAX_SCANCODES_ENTRIES: usize = 512;
const MAX_SEQ_LEN: usize = 32;

fn parse_hex_u8(s: &str) -> u8 {
    let s = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    if s.is_empty() || s.len() > 2 || !s.as_bytes().iter().all(|b| b.is_ascii_hexdigit()) {
        panic!("Invalid u8 hex literal {s:?} in hid_usage_keyboard.json");
    }
    u8::from_str_radix(s, 16).unwrap_or_else(|e| panic!("Invalid u8 hex literal {s:?}: {e}"))
}

fn load_hid_usage_fixture() -> HashMap<String, u8> {
    if HID_USAGE_KEYBOARD_JSON.len() > MAX_HID_JSON_BYTES {
        panic!(
            "Refusing to parse hid_usage_keyboard.json: size {} exceeds limit {MAX_HID_JSON_BYTES}",
            HID_USAGE_KEYBOARD_JSON.len()
        );
    }

    let root: Value = serde_json::from_str(HID_USAGE_KEYBOARD_JSON)
        .unwrap_or_else(|err| panic!("Failed to parse hid_usage_keyboard.json as JSON: {err}"));
    let arr = root
        .as_array()
        .unwrap_or_else(|| panic!("Expected hid_usage_keyboard.json to contain a top-level array"));
    if arr.len() > MAX_HID_ENTRIES {
        panic!(
            "Refusing to iterate hid_usage_keyboard.json: entry count {} exceeds limit {MAX_HID_ENTRIES}",
            arr.len()
        );
    }

    let mut by_code = HashMap::<String, u8>::with_capacity(arr.len());
    for entry in arr {
        let obj = entry.as_object().unwrap_or_else(|| {
            panic!("Expected each hid_usage_keyboard.json entry to be an object")
        });

        let code = obj.get("code").and_then(|v| v.as_str()).unwrap_or_else(|| {
            panic!("Expected hid_usage_keyboard.json entry field `code` to be a string")
        });
        let usage_str = obj
            .get("usage")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| {
                panic!("Expected hid_usage_keyboard.json entry field `usage` to be a string")
            });
        let usage = parse_hex_u8(usage_str);

        if by_code.insert(code.to_owned(), usage).is_some() {
            panic!("Duplicate hid_usage_keyboard.json entry for KeyboardEvent.code={code:?}");
        }
    }
    by_code
}

fn is_core_code(code: &str) -> bool {
    if let Some(letter) = code.strip_prefix("Key") {
        return letter.len() == 1 && letter.as_bytes()[0].is_ascii_uppercase();
    }
    if let Some(digit) = code.strip_prefix("Digit") {
        return digit.len() == 1 && digit.as_bytes()[0].is_ascii_digit();
    }
    if let Some(num) = code.strip_prefix('F') {
        if let Ok(n) = num.parse::<u8>() {
            return (1..=12).contains(&n);
        }
    }
    matches!(code, "ArrowUp" | "ArrowDown" | "ArrowLeft" | "ArrowRight")
}

#[test]
fn hid_usage_fixture_and_ps2_scancode_mapping_do_not_drift() {
    let hid_usage_by_code = load_hid_usage_fixture();

    // Any DOM code we accept for USB HID should also have a PS/2 Set-2 scancode mapping, since
    // PS/2 is the earliest-boot input backend.
    for code in hid_usage_by_code.keys() {
        let make = browser_code_to_set2_bytes(code, true).unwrap_or_else(|| {
            panic!(
                "hid_usage_keyboard.json contains KeyboardEvent.code={code:?} but the PS/2 scancode table has no make mapping"
            )
        });
        assert!(
            !make.is_empty(),
            "PS/2 make sequence for {code:?} should not be empty"
        );
        assert!(
            make.len() <= MAX_SEQ_LEN,
            "PS/2 make sequence for {code:?} has length {} which exceeds limit {MAX_SEQ_LEN}",
            make.len()
        );

        let brk = browser_code_to_set2_bytes(code, false).unwrap_or_else(|| {
            panic!(
                "hid_usage_keyboard.json contains KeyboardEvent.code={code:?} but the PS/2 scancode table has no break mapping"
            )
        });
        assert!(
            brk.len() <= MAX_SEQ_LEN,
            "PS/2 break sequence for {code:?} has length {} which exceeds limit {MAX_SEQ_LEN}",
            brk.len()
        );
    }

    // And vice versa, for the core key set every keyboard backend should support.
    if SCANCODES_JSON.len() > MAX_SCANCODES_JSON_BYTES {
        panic!(
            "Refusing to parse scancodes.json: size {} exceeds limit {MAX_SCANCODES_JSON_BYTES}",
            SCANCODES_JSON.len()
        );
    }
    let scancodes_root: Value = serde_json::from_str(SCANCODES_JSON)
        .unwrap_or_else(|err| panic!("Failed to parse scancodes.json as JSON: {err}"));
    let ps2_set2 = scancodes_root
        .get("ps2_set2")
        .and_then(|v| v.as_object())
        .unwrap_or_else(|| {
            panic!("Expected scancodes.json to contain a top-level `ps2_set2` object")
        });
    if ps2_set2.len() > MAX_SCANCODES_ENTRIES {
        panic!(
            "Refusing to iterate scancodes.json: entry count {} exceeds limit {MAX_SCANCODES_ENTRIES}",
            ps2_set2.len()
        );
    }

    let hid_codes: HashSet<&str> = hid_usage_by_code.keys().map(|s| s.as_str()).collect();
    let mut missing_hid = Vec::new();
    for code in ps2_set2.keys() {
        if is_core_code(code) && !hid_codes.contains(code.as_str()) {
            missing_hid.push(code.clone());
        }
    }
    missing_hid.sort();
    assert_eq!(
        missing_hid,
        Vec::<String>::new(),
        "Core PS/2 scancode mappings are missing from the shared HID usage fixture (docs/fixtures/hid_usage_keyboard.json): {missing_hid:?}"
    );
}
