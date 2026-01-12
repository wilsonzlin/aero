use aero_devices_input::browser_code_to_set2_bytes;

use serde_json::Value;

const SCANCODES_JSON: &str = include_str!("../../../tools/gen_scancodes/scancodes.json");

const REGEN_HINT: &str = "Regenerate with `npm run gen:scancodes`.";

// Defensive limits: the JSON is a trusted repo file, but this is still an
// integration test that should never become accidentally expensive.
const MAX_JSON_BYTES: usize = 256 * 1024;
const MAX_CODE_ENTRIES: usize = 512;
const MAX_SEQ_LEN: usize = 32;

fn parse_hex_byte(s: &str) -> u8 {
    if s.len() != 2 || !s.as_bytes().iter().all(|b| b.is_ascii_hexdigit()) {
        panic!("Invalid scancode byte {s:?} in scancodes.json. {REGEN_HINT}");
    }
    u8::from_str_radix(s, 16)
        .unwrap_or_else(|_| panic!("Invalid scancode byte {s:?} in scancodes.json. {REGEN_HINT}"))
}

fn parse_byte_array(v: &Value, code: &str, field: &str) -> Vec<u8> {
    let arr = v.as_array().unwrap_or_else(|| {
        panic!(
            "Expected scancodes.json entry {code:?} field {field:?} to be an array. {REGEN_HINT}"
        )
    });
    if arr.len() > MAX_SEQ_LEN {
        panic!(
            "Refusing to parse scancodes.json entry {code:?} field {field:?}: sequence length {} exceeds limit {MAX_SEQ_LEN}. {REGEN_HINT}",
            arr.len()
        );
    }
    arr.iter()
        .map(|v| {
            v.as_str().unwrap_or_else(|| {
                panic!(
                    "Expected scancodes.json entry {code:?} field {field:?} to contain only strings. {REGEN_HINT}"
                )
            })
        })
        .map(parse_hex_byte)
        .collect()
}

fn default_break_from_make(code: &str, make: &[u8]) -> Vec<u8> {
    match *make {
        [b] => vec![0xF0, b],
        [0xE0, b] => vec![0xE0, 0xF0, b],
        _ => panic!(
            "scancodes.json entry {code:?} omits `break` but has non-simple make sequence {make:?}. {REGEN_HINT}"
        ),
    }
}

#[test]
fn generated_scancodes_match_source_json() {
    if SCANCODES_JSON.len() > MAX_JSON_BYTES {
        panic!(
            "Refusing to parse scancodes.json: size {} exceeds limit {MAX_JSON_BYTES}. {REGEN_HINT}",
            SCANCODES_JSON.len()
        );
    }

    let root: Value = serde_json::from_str(SCANCODES_JSON).unwrap_or_else(|err| {
        panic!("Failed to parse scancodes.json as JSON: {err}. {REGEN_HINT}")
    });

    let ps2_set2 = root
        .get("ps2_set2")
        .and_then(|v| v.as_object())
        .unwrap_or_else(|| {
            panic!("Expected scancodes.json to contain a top-level `ps2_set2` object. {REGEN_HINT}")
        });

    if ps2_set2.len() > MAX_CODE_ENTRIES {
        panic!(
            "Refusing to iterate scancodes.json: entry count {} exceeds limit {MAX_CODE_ENTRIES}. {REGEN_HINT}",
            ps2_set2.len()
        );
    }

    for (code, entry) in ps2_set2.iter() {
        let entry_obj = entry.as_object().unwrap_or_else(|| {
            panic!("Expected scancodes.json entry {code:?} to be an object. {REGEN_HINT}")
        });

        let make_val = entry_obj.get("make").unwrap_or_else(|| {
            panic!("Expected scancodes.json entry {code:?} to have a `make` field. {REGEN_HINT}")
        });
        let expected_make = parse_byte_array(make_val, code, "make");

        let expected_break = match entry_obj.get("break") {
            Some(v) => parse_byte_array(v, code, "break"),
            None => default_break_from_make(code, &expected_make),
        };

        let actual_make = browser_code_to_set2_bytes(code, true).unwrap_or_else(|| {
            panic!("Missing generated scancode mapping for browser code {code:?}. {REGEN_HINT}")
        });
        assert_eq!(
            actual_make,
            expected_make,
            "PS/2 Set-2 make bytes for {code:?} are out of sync with tools/gen_scancodes/scancodes.json. {REGEN_HINT}"
        );

        let actual_break = browser_code_to_set2_bytes(code, false).unwrap_or_else(|| {
            panic!("Missing generated scancode mapping for browser code {code:?}. {REGEN_HINT}")
        });
        assert_eq!(
            actual_break,
            expected_break,
            "PS/2 Set-2 break bytes for {code:?} are out of sync with tools/gen_scancodes/scancodes.json. {REGEN_HINT}"
        );
    }
}
