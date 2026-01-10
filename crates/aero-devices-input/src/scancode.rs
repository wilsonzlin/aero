//! Translation from browser `KeyboardEvent.code` strings to PS/2 Set-2 scancodes.
//!
//! The browser-facing code uses `KeyboardEvent.code` (physical key position)
//! instead of `key` (layout-dependent) so the guest OS can apply its own layout.

/// A single Set-2 scancode, optionally prefixed by `0xE0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Set2Scancode {
    pub code: u8,
    pub extended: bool,
}

/// Converts a browser `KeyboardEvent.code` string into a Set-2 scancode.
///
/// Returns `None` for keys we intentionally ignore (e.g. IME keys) or do not
/// yet support.
pub fn browser_code_to_set2(code: &str) -> Option<Set2Scancode> {
    let (code, extended) = match code {
        "Escape" => (0x76, false),
        "F1" => (0x05, false),
        "F2" => (0x06, false),
        "F3" => (0x04, false),
        "F4" => (0x0C, false),
        "F5" => (0x03, false),
        "F6" => (0x0B, false),
        "F7" => (0x83, false),
        "F8" => (0x0A, false),
        "F9" => (0x01, false),
        "F10" => (0x09, false),
        "F11" => (0x78, false),
        "F12" => (0x07, false),
        "Backquote" => (0x0E, false),
        "Digit1" => (0x16, false),
        "Digit2" => (0x1E, false),
        "Digit3" => (0x26, false),
        "Digit4" => (0x25, false),
        "Digit5" => (0x2E, false),
        "Digit6" => (0x36, false),
        "Digit7" => (0x3D, false),
        "Digit8" => (0x3E, false),
        "Digit9" => (0x46, false),
        "Digit0" => (0x45, false),
        "Minus" => (0x4E, false),
        "Equal" => (0x55, false),
        "Backspace" => (0x66, false),
        "Tab" => (0x0D, false),
        "KeyQ" => (0x15, false),
        "KeyW" => (0x1D, false),
        "KeyE" => (0x24, false),
        "KeyR" => (0x2D, false),
        "KeyT" => (0x2C, false),
        "KeyY" => (0x35, false),
        "KeyU" => (0x3C, false),
        "KeyI" => (0x43, false),
        "KeyO" => (0x44, false),
        "KeyP" => (0x4D, false),
        "BracketLeft" => (0x54, false),
        "BracketRight" => (0x5B, false),
        "Backslash" => (0x5D, false),
        "CapsLock" => (0x58, false),
        "KeyA" => (0x1C, false),
        "KeyS" => (0x1B, false),
        "KeyD" => (0x23, false),
        "KeyF" => (0x2B, false),
        "KeyG" => (0x34, false),
        "KeyH" => (0x33, false),
        "KeyJ" => (0x3B, false),
        "KeyK" => (0x42, false),
        "KeyL" => (0x4B, false),
        "Semicolon" => (0x4C, false),
        "Quote" => (0x52, false),
        "Enter" => (0x5A, false),
        "ShiftLeft" => (0x12, false),
        "KeyZ" => (0x1A, false),
        "KeyX" => (0x22, false),
        "KeyC" => (0x21, false),
        "KeyV" => (0x2A, false),
        "KeyB" => (0x32, false),
        "KeyN" => (0x31, false),
        "KeyM" => (0x3A, false),
        "Comma" => (0x41, false),
        "Period" => (0x49, false),
        "Slash" => (0x4A, false),
        "ShiftRight" => (0x59, false),
        "ControlLeft" => (0x14, false),
        "AltLeft" => (0x11, false),
        "Space" => (0x29, false),
        // Extended keys (0xE0-prefixed)
        "ControlRight" => (0x14, true),
        "AltRight" => (0x11, true),
        "ArrowUp" => (0x75, true),
        "ArrowDown" => (0x72, true),
        "ArrowLeft" => (0x6B, true),
        "ArrowRight" => (0x74, true),
        "Home" => (0x6C, true),
        "End" => (0x69, true),
        "PageUp" => (0x7D, true),
        "PageDown" => (0x7A, true),
        "Insert" => (0x70, true),
        "Delete" => (0x71, true),
        "NumpadEnter" => (0x5A, true),
        "NumpadDivide" => (0x4A, true),
        _ => return None,
    };

    Some(Set2Scancode { code, extended })
}

/// Appends a make or break sequence to `out`.
pub fn push_set2_sequence(out: &mut Vec<u8>, scancode: Set2Scancode, pressed: bool) {
    if pressed {
        if scancode.extended {
            out.push(0xE0);
        }
        out.push(scancode.code);
        return;
    }

    if scancode.extended {
        out.push(0xE0);
    }
    out.push(0xF0);
    out.push(scancode.code);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_a_make_break() {
        let sc = browser_code_to_set2("KeyA").unwrap();
        assert_eq!(
            sc,
            Set2Scancode {
                code: 0x1C,
                extended: false
            }
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
            Set2Scancode {
                code: 0x6B,
                extended: true
            }
        );

        let mut make = Vec::new();
        push_set2_sequence(&mut make, sc, true);
        assert_eq!(make, vec![0xE0, 0x6B]);
    }
}
