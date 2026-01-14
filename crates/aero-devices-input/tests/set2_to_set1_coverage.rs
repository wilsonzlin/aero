use std::collections::BTreeSet;

use serde_json::Value;

const SCANCODES_JSON: &str = include_str!("../../../tools/gen_scancodes/scancodes.json");
const I8042_RS: &str = include_str!("../src/i8042.rs");

const REGEN_HINT: &str = "Regenerate with `npm run gen:scancodes`.";

// Defensive limits: this is trusted repo data, but the test should never become
// accidentally expensive.
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

fn collect_emitted_code_pairs(bytes: &[u8], out: &mut BTreeSet<(u8, bool)>) {
    // Mirrors the `Set2ToSet1` prefix handling in `src/i8042.rs`: `E0` prefixes
    // extend the following scancode byte (even across `F0`), and `E1` resets the
    // prefix state (Pause/Break sequence).
    let mut saw_e0 = false;

    for &b in bytes {
        match b {
            0xE0 => saw_e0 = true,
            0xE1 => saw_e0 = false,
            0xF0 => {}
            code => {
                out.insert((code, saw_e0));
                saw_e0 = false;
            }
        }
    }

    if saw_e0 {
        panic!("Found dangling 0xE0 prefix in Set-2 scancode sequence {bytes:02x?}. {REGEN_HINT}");
    }
}

fn parse_explicit_set2_to_set1_pairs(src: &str) -> BTreeSet<(u8, bool)> {
    fn is_hex(b: u8) -> bool {
        b.is_ascii_hexdigit()
    }

    fn hex_val(b: u8) -> u8 {
        match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'f' => 10 + (b - b'a'),
            b'A'..=b'F' => 10 + (b - b'A'),
            _ => 0,
        }
    }

    let bytes = src.as_bytes();
    let mut out = BTreeSet::new();
    let mut i = 0usize;

    while i + 3 < bytes.len() {
        if bytes[i] != b'(' || bytes[i + 1] != b'0' || bytes[i + 2] != b'x' {
            i += 1;
            continue;
        }

        let mut j = i + 3;
        let mut val: u8 = 0;
        let mut digits = 0u8;
        while j < bytes.len() && is_hex(bytes[j]) && digits < 2 {
            val = val.wrapping_mul(16).wrapping_add(hex_val(bytes[j]));
            j += 1;
            digits += 1;
        }
        if digits == 0 {
            i += 1;
            continue;
        }

        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if j >= bytes.len() || bytes[j] != b',' {
            i += 1;
            continue;
        }
        j += 1;

        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }

        let extended = if src[j..].starts_with("true") {
            j += 4;
            true
        } else if src[j..].starts_with("false") {
            j += 5;
            false
        } else {
            i += 1;
            continue;
        };

        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if j >= bytes.len() || bytes[j] != b')' {
            i += 1;
            continue;
        }
        j += 1;

        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if j + 1 >= bytes.len() || bytes[j] != b'=' || bytes[j + 1] != b'>' {
            i += 1;
            continue;
        }

        out.insert((val, extended));
        i = j;
    }

    out
}

#[test]
fn set2_to_set1_translation_table_covers_all_emitted_scancodes() {
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

    let mut emitted_pairs = BTreeSet::new();

    for (code, entry) in ps2_set2.iter() {
        let entry_obj = entry.as_object().unwrap_or_else(|| {
            panic!("Expected scancodes.json entry {code:?} to be an object. {REGEN_HINT}")
        });

        let make_val = entry_obj.get("make").unwrap_or_else(|| {
            panic!("Expected scancodes.json entry {code:?} to have a `make` field. {REGEN_HINT}")
        });
        let make = parse_byte_array(make_val, code, "make");

        let brk = match entry_obj.get("break") {
            Some(v) => parse_byte_array(v, code, "break"),
            None => default_break_from_make(code, &make),
        };

        collect_emitted_code_pairs(&make, &mut emitted_pairs);
        collect_emitted_code_pairs(&brk, &mut emitted_pairs);
    }

    let explicit_pairs = parse_explicit_set2_to_set1_pairs(I8042_RS);

    let missing: Vec<(u8, bool)> = emitted_pairs.difference(&explicit_pairs).copied().collect();

    assert!(
        missing.is_empty(),
        "i8042 Set-2 -> Set-1 translation table (`src/i8042.rs::set2_to_set1`) is missing explicit mappings for these emitted (byte, extended) pairs: {missing:02x?}."
    );
}
