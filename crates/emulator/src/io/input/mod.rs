pub mod i8042;

pub use aero_devices_input::scancode::{
    browser_code_to_set2 as ps2_set2_scancode_for_code,
    browser_code_to_set2_bytes as ps2_set2_bytes_for_key_event, Set2Scancode as Ps2Set2Scancode,
};
