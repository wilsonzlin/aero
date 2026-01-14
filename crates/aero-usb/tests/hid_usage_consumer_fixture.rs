use aero_usb::hid::usage::keyboard_code_to_consumer_usage;
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
struct FixtureEntry {
    code: String,
    usage: String,
}

fn parse_hex_u16(s: &str) -> u16 {
    let s = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    u16::from_str_radix(s, 16).unwrap_or_else(|e| panic!("invalid u16 hex literal {s:?}: {e}"))
}

#[test]
fn keyboard_code_to_consumer_usage_matches_shared_fixture() {
    let fixture_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/fixtures/hid_usage_consumer.json");
    let text = fs::read_to_string(&fixture_path)
        .unwrap_or_else(|e| panic!("failed to read fixture {fixture_path:?}: {e}"));
    let entries: Vec<FixtureEntry> = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("failed to parse JSON fixture {fixture_path:?}: {e}"));

    assert!(
        !entries.is_empty(),
        "fixture {fixture_path:?} should not be empty"
    );

    let mut expected_by_code = HashMap::<String, u16>::with_capacity(entries.len());
    for entry in entries {
        let expected = parse_hex_u16(&entry.usage);
        assert_eq!(
            expected_by_code.insert(entry.code.clone(), expected),
            None,
            "duplicate fixture entry for KeyboardEvent.code={:?}",
            entry.code
        );
        assert_eq!(
            keyboard_code_to_consumer_usage(&entry.code),
            Some(expected),
            "unexpected Consumer usage for KeyboardEvent.code={:?}",
            entry.code
        );
    }

    assert_eq!(keyboard_code_to_consumer_usage("NoSuchKey"), None);
    // Sanity-check that regular keyboard keys are not mapped via the consumer table.
    assert_eq!(keyboard_code_to_consumer_usage("KeyA"), None);
}
