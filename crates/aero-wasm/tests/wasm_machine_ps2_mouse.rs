#![cfg(target_arch = "wasm32")]

use aero_wasm::Machine;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen_test]
fn machine_accepts_ps2_mouse_injection_calls() {
    let mut m = Machine::new(2 * 1024 * 1024).expect("Machine::new ok");

    // Smoke test: ensure the wasm-bindgen wrapper exports the PS/2 mouse injection APIs and they
    // can be invoked against a live in-memory machine instance.
    m.inject_ps2_mouse_buttons(aero_wasm::MouseButtons::Left as u8); // left down
    m.inject_ps2_mouse_motion(10, 5, 1); // right + up + wheel up
    m.inject_ps2_mouse_buttons(0x00); // release
}
