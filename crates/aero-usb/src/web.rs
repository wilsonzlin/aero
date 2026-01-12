//! Browser input glue.
//!
//! The emulator runs in a browser and receives `KeyboardEvent.code` strings and pointer events.
//! This module maps those inputs onto USB HID usage values suitable for the HID keyboard/mouse
//! devices in this crate.

pub const MOUSE_BUTTON_LEFT: u8 = 0x01;
pub const MOUSE_BUTTON_RIGHT: u8 = 0x02;
pub const MOUSE_BUTTON_MIDDLE: u8 = 0x04;
pub const MOUSE_BUTTON_SIDE: u8 = 0x08;
pub const MOUSE_BUTTON_EXTRA: u8 = 0x10;

/// Map a JavaScript `KeyboardEvent.code` string to a USB HID keyboard usage (Usage Page 0x07).
pub fn keyboard_code_to_hid_usage(code: &str) -> Option<u8> {
    crate::hid::usage::keyboard_code_to_usage(code)
}

/// Convert browser mouse button indices (as used by DOM `MouseEvent.button`) to a HID mask.
pub fn mouse_button_to_hid_mask(button: i16) -> Option<u8> {
    crate::hid::usage::mouse_button_index_to_bit(button)
}

/// Convert browser `MouseEvent.buttons` bitfield to the HID button bitfield used by mouse reports.
pub fn mouse_buttons_bitfield_to_hid_mask(buttons: u16) -> u8 {
    crate::hid::usage::mouse_buttons_bitfield_to_bits(buttons)
}

/// Convert browser `WheelEvent.deltaY` into a conventional HID wheel step.
pub fn wheel_delta_y_to_step(delta_y: f64) -> i32 {
    crate::hid::usage::wheel_delta_y_to_step(delta_y)
}
