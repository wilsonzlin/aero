pub mod i8042;

// The canonical browser-code → PS/2 Set-2 mapping lives in `aero-devices-input`. Re-export it to
// preserve the legacy `emulator::io::input::*` API surface while avoiding duplicated tables.
pub use aero_devices_input::scancode::{
    browser_code_to_set2 as ps2_set2_scancode_for_code,
    browser_code_to_set2_bytes as ps2_set2_bytes_for_key_event, Set2Scancode as Ps2Set2Scancode,
};
