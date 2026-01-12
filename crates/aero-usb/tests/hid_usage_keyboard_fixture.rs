use aero_usb::hid::usage::keyboard_code_to_usage;
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
struct FixtureEntry {
    code: String,
    usage: String,
}

fn parse_hex_u8(s: &str) -> u8 {
    let s = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    u8::from_str_radix(s, 16).unwrap_or_else(|e| panic!("invalid u8 hex literal {s:?}: {e}"))
}

#[test]
fn keyboard_code_to_usage_matches_shared_fixture() {
    let fixture_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/fixtures/hid_usage_keyboard.json");
    let text = fs::read_to_string(&fixture_path)
        .unwrap_or_else(|e| panic!("failed to read fixture {fixture_path:?}: {e}"));
    let entries: Vec<FixtureEntry> = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("failed to parse JSON fixture {fixture_path:?}: {e}"));

    assert!(
        !entries.is_empty(),
        "fixture {fixture_path:?} should not be empty"
    );

    let mut expected_by_code = HashMap::<String, u8>::with_capacity(entries.len());
    for entry in entries {
        let expected = parse_hex_u8(&entry.usage);
        assert_eq!(
            expected_by_code.insert(entry.code.clone(), expected),
            None,
            "duplicate fixture entry for KeyboardEvent.code={:?}",
            entry.code
        );
        assert_eq!(
            keyboard_code_to_usage(&entry.code),
            Some(expected),
            "unexpected HID usage for KeyboardEvent.code={:?}",
            entry.code
        );
    }

    // Ensure the Rust-side mapping does not "accidentally" support additional codes without the
    // shared fixture being updated. Use the PS/2 scancode list as a stable, project-wide superset
    // of common `KeyboardEvent.code` values.
    let scancodes_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tools/gen_scancodes/scancodes.json");
    let scancodes_text = fs::read_to_string(&scancodes_path)
        .unwrap_or_else(|e| panic!("failed to read scancodes {scancodes_path:?}: {e}"));
    let scancodes: serde_json::Value = serde_json::from_str(&scancodes_text)
        .unwrap_or_else(|e| panic!("failed to parse scancodes JSON {scancodes_path:?}: {e}"));
    let ps2_set2 = scancodes
        .get("ps2_set2")
        .and_then(|v| v.as_object())
        .expect("scancodes.json must contain an object key ps2_set2");
    for code in ps2_set2.keys() {
        assert_eq!(
            keyboard_code_to_usage(code),
            expected_by_code.get(code).copied(),
            "fixture drift for KeyboardEvent.code={code:?}"
        );
    }

    assert_eq!(keyboard_code_to_usage("NoSuchKey"), None);
}
