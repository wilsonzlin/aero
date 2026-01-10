//! Translation from browser `KeyboardEvent.code` strings to PS/2 Set-2 scancodes.
//!
//! The browser-facing code uses `KeyboardEvent.code` (physical key position)
//! instead of `key` (layout-dependent) so the guest OS can apply its own layout.
//!
//! The mapping data itself is generated from `tools/gen_scancodes/scancodes.json`
//! to keep Rust/WASM and TypeScript usage consistent.

#[path = "scancodes_generated.rs"]
mod scancodes_generated;

pub use scancodes_generated::Ps2Set2Scancode as Set2Scancode;

/// Converts a browser `KeyboardEvent.code` string into a Set-2 scancode.
///
/// Returns `None` for keys we intentionally ignore (e.g. IME keys) or do not
/// yet support.
pub fn browser_code_to_set2(code: &str) -> Option<Set2Scancode> {
    scancodes_generated::ps2_set2_scancode_for_code(code)
}

/// Convenience helper: returns the full Set-2 byte sequence for a key event.
///
/// This includes `0xE0` for extended keys and `0xF0` break prefixes for key-up.
pub fn browser_code_to_set2_bytes(code: &str, pressed: bool) -> Option<Vec<u8>> {
    scancodes_generated::ps2_set2_bytes_for_key_event(code, pressed)
}

/// Appends a make or break sequence to `out`.
pub fn push_set2_sequence(out: &mut Vec<u8>, scancode: Set2Scancode, pressed: bool) {
    out.extend(scancode.bytes(pressed));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_a_make_break() {
        let sc = browser_code_to_set2("KeyA").unwrap();
        assert_eq!(
            sc,
            Set2Scancode::Simple {
                make: 0x1C,
                extended: false,
            },
        );

        let mut make = Vec::new();
        push_set2_sequence(&mut make, sc, true);
        assert_eq!(make, vec![0x1C]);

        let mut brk = Vec::new();
        push_set2_sequence(&mut brk, sc, false);
        assert_eq!(brk, vec![0xF0, 0x1C]);
    }

    #[test]
    fn arrow_left_is_extended() {
        let sc = browser_code_to_set2("ArrowLeft").unwrap();
        assert_eq!(
            sc,
            Set2Scancode::Simple {
                make: 0x6B,
                extended: true,
            },
        );

        let mut make = Vec::new();
        push_set2_sequence(&mut make, sc, true);
        assert_eq!(make, vec![0xE0, 0x6B]);
    }

    #[test]
    fn print_screen_and_pause_are_sequences() {
        assert_eq!(
            browser_code_to_set2_bytes("PrintScreen", true),
            Some(vec![0xE0, 0x12, 0xE0, 0x7C])
        );
        assert_eq!(
            browser_code_to_set2_bytes("PrintScreen", false),
            Some(vec![0xE0, 0xF0, 0x7C, 0xE0, 0xF0, 0x12])
        );

        assert_eq!(
            browser_code_to_set2_bytes("Pause", true),
            Some(vec![0xE1, 0x14, 0x77, 0xE1, 0xF0, 0x14, 0xF0, 0x77])
        );
        assert_eq!(browser_code_to_set2_bytes("Pause", false), Some(Vec::new()));
    }
}
