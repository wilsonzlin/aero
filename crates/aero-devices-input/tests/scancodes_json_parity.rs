#![cfg(not(target_arch = "wasm32"))]

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use aero_devices_input::browser_code_to_set2_bytes;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct RawMapping {
    ps2_set2: BTreeMap<String, RawEntry>,
}

#[derive(Debug, Deserialize)]
struct RawEntry {
    make: Vec<String>,
    #[serde(rename = "break")]
    break_seq: Option<Vec<String>>,
}

fn parse_hex_byte(hex: &str) -> u8 {
    if hex.len() != 2 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        panic!("Invalid hex byte string: {hex:?}");
    }
    u8::from_str_radix(hex, 16).expect("validated hexdigits")
}

fn parse_hex_bytes(bytes: &[String]) -> Vec<u8> {
    bytes.iter().map(|b| parse_hex_byte(b)).collect()
}

fn expected_bytes_for_entry(entry: &RawEntry, pressed: bool) -> Vec<u8> {
    let make = parse_hex_bytes(&entry.make);

    if let Some(brk) = entry.break_seq.as_ref() {
        let brk = parse_hex_bytes(brk);
        return if pressed { make } else { brk };
    }

    match make.as_slice() {
        [b] => {
            if pressed {
                vec![*b]
            } else {
                vec![0xF0, *b]
            }
        }
        [0xE0, b] => {
            if pressed {
                vec![0xE0, *b]
            } else {
                vec![0xE0, 0xF0, *b]
            }
        }
        _ => panic!(
            "Non-simple key mapping for make={:?} missing explicit break sequence",
            entry.make
        ),
    }
}

#[test]
fn scancodes_generated_matches_scancodes_json() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mapping_path = manifest_dir.join("../../tools/gen_scancodes/scancodes.json");

    let raw: RawMapping =
        serde_json::from_str(&fs::read_to_string(mapping_path).expect("read scancodes.json"))
            .expect("parse scancodes.json");

    for (code, entry) in raw.ps2_set2 {
        for pressed in [true, false] {
            let expected = expected_bytes_for_entry(&entry, pressed);
            let actual = browser_code_to_set2_bytes(&code, pressed)
                .unwrap_or_else(|| panic!("missing generated mapping for {code:?}"));
            assert_eq!(actual, expected, "code={code} pressed={pressed}");
        }
    }
}
