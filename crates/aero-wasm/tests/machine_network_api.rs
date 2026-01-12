#![cfg(target_arch = "wasm32")]

use wasm_bindgen::prelude::JsValue;
use wasm_bindgen_test::wasm_bindgen_test;

use aero_wasm::{Machine, SharedRingBuffer};

/// Compile-time smoke test ensuring the canonical `Machine` exposes the expected networking API to
/// wasm-bindgen consumers.
#[wasm_bindgen_test]
fn machine_exposes_l2_tunnel_ring_api() {
    fn assert_attach(_: fn(&mut Machine, SharedRingBuffer, SharedRingBuffer) -> Result<(), JsValue>) {}
    fn assert_attach_sab(
        _: fn(&mut Machine, js_sys::SharedArrayBuffer) -> Result<(), JsValue>,
    ) {
    }
    fn assert_detach(_: fn(&mut Machine)) {}

    assert_attach(Machine::attach_l2_tunnel_rings);
    assert_attach_sab(Machine::attach_l2_tunnel_from_io_ipc_sab);
    assert_detach(Machine::detach_network);
}

