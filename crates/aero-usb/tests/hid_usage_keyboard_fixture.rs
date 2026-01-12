use aero_usb::hid::usage::keyboard_code_to_usage;
use serde::Deserialize;
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

    for entry in entries {
        let expected = parse_hex_u8(&entry.usage);
        assert_eq!(
            keyboard_code_to_usage(&entry.code),
            Some(expected),
            "unexpected HID usage for KeyboardEvent.code={:?}",
            entry.code
        );
    }

    assert_eq!(keyboard_code_to_usage("NoSuchKey"), None);
}
