//! Helpers for mapping browser input events to USB HID usages.
//!
//! The browser host (TypeScript) captures keyboard and mouse events using
//! `KeyboardEvent.code`, `MouseEvent.button`, and `WheelEvent.deltaY`. USB HID
//! input, however, is expressed using *usages* (e.g. 0x04 = "Keyboard a and A")
//! defined by the HID Usage Tables.
//!
//! This module provides pure helpers that can be used by platform/input code to translate browser
//! events into the usage IDs expected by the HID device models in this crate.

/// Maps a `KeyboardEvent.code` string to a USB HID usage ID from the Keyboard/Keypad usage page
/// (0x07).
///
/// This uses `code` (physical key position) rather than `key` (layout-dependent character).
///
/// Keep in sync with:
/// - TypeScript host mapping: `web/src/input/hid_usage.ts::keyboardCodeToHidUsage`
/// - Shared fixture (parity tests): `docs/fixtures/hid_usage_keyboard.json`
pub fn keyboard_code_to_usage(code: &str) -> Option<u8> {
    if let Some(rest) = code.strip_prefix("Key") {
        let b = rest.as_bytes();
        if b.len() == 1 && b[0].is_ascii_uppercase() {
            return Some(0x04 + (b[0] - b'A'));
        }
    }

    if let Some(rest) = code.strip_prefix("Digit") {
        let b = rest.as_bytes();
        if b.len() == 1 && b[0].is_ascii_digit() {
            return Some(match b[0] {
                b'1'..=b'9' => 0x1E + (b[0] - b'1'),
                b'0' => 0x27,
                _ => return None,
            });
        }
    }

    if let Some(rest) = code.strip_prefix('F') {
        if let Ok(n) = rest.parse::<u8>() {
            if (1..=12).contains(&n) {
                return Some(0x3A + (n - 1));
            }
        }
    }

    match code {
        // Modifiers (0xE0..=0xE7).
        "ControlLeft" => Some(0xE0),
        "ShiftLeft" => Some(0xE1),
        "AltLeft" => Some(0xE2),
        "MetaLeft" | "OSLeft" => Some(0xE3),
        "ControlRight" => Some(0xE4),
        "ShiftRight" => Some(0xE5),
        "AltRight" => Some(0xE6),
        "MetaRight" | "OSRight" => Some(0xE7),

        // Basic keys.
        "Enter" => Some(0x28),
        "Escape" => Some(0x29),
        "Backspace" => Some(0x2A),
        "Tab" => Some(0x2B),
        "Space" => Some(0x2C),
        "Minus" => Some(0x2D),
        "Equal" => Some(0x2E),
        "BracketLeft" => Some(0x2F),
        "BracketRight" => Some(0x30),
        "Backslash" => Some(0x31),
        "IntlHash" => Some(0x32),
        "Semicolon" => Some(0x33),
        "Quote" => Some(0x34),
        "Backquote" => Some(0x35),
        "Comma" => Some(0x36),
        "Period" => Some(0x37),
        "Slash" => Some(0x38),
        "CapsLock" => Some(0x39),

        // Navigation / system.
        "PrintScreen" => Some(0x46),
        "ScrollLock" => Some(0x47),
        "Pause" => Some(0x48),
        "Insert" => Some(0x49),
        "Home" => Some(0x4A),
        "PageUp" => Some(0x4B),
        "Delete" => Some(0x4C),
        "End" => Some(0x4D),
        "PageDown" => Some(0x4E),
        "ArrowRight" => Some(0x4F),
        "ArrowLeft" => Some(0x50),
        "ArrowDown" => Some(0x51),
        "ArrowUp" => Some(0x52),

        // Keypad.
        "NumLock" => Some(0x53),
        "NumpadDivide" => Some(0x54),
        "NumpadMultiply" => Some(0x55),
        "NumpadSubtract" => Some(0x56),
        "NumpadAdd" => Some(0x57),
        "NumpadEnter" => Some(0x58),
        "Numpad1" => Some(0x59),
        "Numpad2" => Some(0x5A),
        "Numpad3" => Some(0x5B),
        "Numpad4" => Some(0x5C),
        "Numpad5" => Some(0x5D),
        // Some browsers/keyboard layouts report the numpad 5 position as "NumpadClear".
        "NumpadClear" => Some(0x5D),
        "Numpad6" => Some(0x5E),
        "Numpad7" => Some(0x5F),
        "Numpad8" => Some(0x60),
        "Numpad9" => Some(0x61),
        "Numpad0" => Some(0x62),
        "NumpadDecimal" => Some(0x63),
        "NumpadEqual" => Some(0x67),
        "NumpadComma" => Some(0x85),

        // "Application" key (aka context menu).
        "ContextMenu" => Some(0x65),

        // International keys.
        "IntlBackslash" => Some(0x64),
        "IntlRo" => Some(0x87),
        "IntlYen" => Some(0x89),

        _ => None,
    }
}

/// Converts DOM `MouseEvent.button` indices to the HID button bit used by `UsbHidMouse::button_event`.
///
/// The mapping follows the DOM `MouseEvent.buttons` bitfield:
/// - 0 => bit0 (`0x01`) left
/// - 1 => bit2 (`0x04`) middle
/// - 2 => bit1 (`0x02`) right
/// - 3 => bit3 (`0x08`) back / side
/// - 4 => bit4 (`0x10`) forward / extra
pub fn mouse_button_index_to_bit(button: i16) -> Option<u8> {
    match button {
        0 => Some(0x01),
        2 => Some(0x02),
        1 => Some(0x04),
        3 => Some(0x08),
        4 => Some(0x10),
        _ => None,
    }
}

/// Converts `MouseEvent.buttons` bitfield to the HID button bitfield used by `MouseReport.buttons`.
pub fn mouse_buttons_bitfield_to_bits(buttons: u16) -> u8 {
    (buttons & 0x1f) as u8
}

/// Converts `WheelEvent.deltaY` into a conventional HID wheel step.
///
/// The returned value is intended to be fed into `UsbHidMouse::wheel()`.
pub fn wheel_delta_y_to_step(delta_y: f64) -> i32 {
    if delta_y > 0.0 {
        -1
    } else if delta_y < 0.0 {
        1
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_letter_keys() {
        assert_eq!(keyboard_code_to_usage("KeyA"), Some(0x04));
        assert_eq!(keyboard_code_to_usage("KeyZ"), Some(0x1D));
        assert_eq!(keyboard_code_to_usage("Keya"), None);
    }

    #[test]
    fn maps_digit_keys() {
        assert_eq!(keyboard_code_to_usage("Digit1"), Some(0x1E));
        assert_eq!(keyboard_code_to_usage("Digit9"), Some(0x26));
        assert_eq!(keyboard_code_to_usage("Digit0"), Some(0x27));
    }

    #[test]
    fn maps_function_keys() {
        assert_eq!(keyboard_code_to_usage("F1"), Some(0x3A));
        assert_eq!(keyboard_code_to_usage("F12"), Some(0x45));
        assert_eq!(keyboard_code_to_usage("F13"), None);
    }

    #[test]
    fn maps_modifiers_and_arrows() {
        assert_eq!(keyboard_code_to_usage("ControlLeft"), Some(0xE0));
        assert_eq!(keyboard_code_to_usage("MetaRight"), Some(0xE7));
        assert_eq!(keyboard_code_to_usage("ArrowUp"), Some(0x52));
        assert_eq!(keyboard_code_to_usage("ArrowLeft"), Some(0x50));
    }

    #[test]
    fn maps_mouse_buttons_and_wheel() {
        assert_eq!(mouse_button_index_to_bit(0), Some(0x01));
        assert_eq!(mouse_button_index_to_bit(2), Some(0x02));
        assert_eq!(mouse_button_index_to_bit(1), Some(0x04));
        assert_eq!(mouse_button_index_to_bit(3), Some(0x08));
        assert_eq!(mouse_button_index_to_bit(4), Some(0x10));
        assert_eq!(mouse_button_index_to_bit(5), None);

        assert_eq!(mouse_buttons_bitfield_to_bits(0b1_1111), 0b1_1111);
        assert_eq!(mouse_buttons_bitfield_to_bits(0b10_0000), 0);

        assert_eq!(wheel_delta_y_to_step(10.0), -1);
        assert_eq!(wheel_delta_y_to_step(-5.0), 1);
        assert_eq!(wheel_delta_y_to_step(0.0), 0);
    }
}
