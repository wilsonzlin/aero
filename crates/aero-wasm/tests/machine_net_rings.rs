#![cfg(target_arch = "wasm32")]

use aero_ipc::layout::ring_ctrl;
use aero_wasm::{Machine, SharedRingBuffer};
use js_sys::{Int32Array, SharedArrayBuffer};
use wasm_bindgen_test::wasm_bindgen_test;

fn make_shared_ring(capacity_bytes: u32) -> SharedRingBuffer {
    assert_eq!(capacity_bytes as usize % aero_ipc::layout::RECORD_ALIGN, 0);
    let total_bytes = ring_ctrl::BYTES as u32 + capacity_bytes;
    let sab = SharedArrayBuffer::new(total_bytes);
    let ctrl = Int32Array::new_with_byte_offset_and_length(&sab, 0, ring_ctrl::WORDS as u32);
    // Initialize the header: capacity + zeroed indices.
    ctrl.set_index(ring_ctrl::CAPACITY as u32, capacity_bytes as i32);
    SharedRingBuffer::new(sab, 0).expect("SharedRingBuffer::new")
}

#[wasm_bindgen_test]
fn machine_can_attach_net_rings() {
    let mut m = Machine::new(2 * 1024 * 1024).expect("Machine::new");
    let net_tx = make_shared_ring(64);
    let net_rx = make_shared_ring(64);

    m.attach_l2_tunnel_rings(net_tx, net_rx)
        .expect("attach_l2_tunnel_rings");

    let stats = m.net_stats();
    assert!(
        !stats.is_null(),
        "expected net_stats() to return an object after attaching rings"
    );
}
