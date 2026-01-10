pub mod i8042;
pub mod scancodes;

pub use scancodes::{ps2_set2_bytes_for_key_event, ps2_set2_scancode_for_code, Ps2Set2Scancode};
pub mod ps2_keyboard;
pub mod ps2_mouse;
