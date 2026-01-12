#![cfg(target_arch = "wasm32")]

use wasm_bindgen::prelude::JsValue;
use wasm_bindgen_test::wasm_bindgen_test;

use aero_ipc::layout::{io_ipc_queue_kind, ipc_header, queue_desc, ring_ctrl, IPC_MAGIC, IPC_VERSION};
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

fn build_minimal_io_ipc_sab(net_tx_capacity: u32, net_rx_capacity: u32) -> js_sys::SharedArrayBuffer {
    // Layout:
    // - ipc_header (16B)
    // - 2 * queue_desc (2 * 16B)
    // - NET_TX ring header (16B) + data
    // - NET_RX ring header (16B) + data
    let queue_count = 2u32;
    let desc_bytes = (ipc_header::BYTES + (queue_count as usize) * queue_desc::BYTES) as u32;

    let net_tx_offset = desc_bytes;
    let net_rx_offset = net_tx_offset + ring_ctrl::BYTES as u32 + net_tx_capacity;

    let total_bytes = net_rx_offset + ring_ctrl::BYTES as u32 + net_rx_capacity;

    let sab = js_sys::SharedArrayBuffer::new(total_bytes);
    let words = js_sys::Uint32Array::new(&sab);

    // Top-level IPC header.
    words.set_index(ipc_header::MAGIC as u32, IPC_MAGIC);
    words.set_index(ipc_header::VERSION as u32, IPC_VERSION);
    words.set_index(ipc_header::TOTAL_BYTES as u32, total_bytes);
    words.set_index(ipc_header::QUEUE_COUNT as u32, queue_count);

    // Queue descriptors.
    let write_desc = |idx: u32, kind: u32, offset_bytes: u32, capacity_bytes: u32| {
        let base = ipc_header::WORDS as u32 + idx * queue_desc::WORDS as u32;
        words.set_index(base + queue_desc::KIND as u32, kind);
        words.set_index(base + queue_desc::OFFSET_BYTES as u32, offset_bytes);
        words.set_index(base + queue_desc::CAPACITY_BYTES as u32, capacity_bytes);
        words.set_index(base + queue_desc::RESERVED as u32, 0);

        // Initialize the ring header control words.
        let ctrl = js_sys::Int32Array::new_with_byte_offset_and_length(
            &sab,
            offset_bytes,
            ring_ctrl::WORDS as u32,
        );
        ctrl.set_index(ring_ctrl::HEAD as u32, 0);
        ctrl.set_index(ring_ctrl::TAIL_RESERVE as u32, 0);
        ctrl.set_index(ring_ctrl::TAIL_COMMIT as u32, 0);
        ctrl.set_index(ring_ctrl::CAPACITY as u32, capacity_bytes as i32);
    };

    write_desc(0, io_ipc_queue_kind::NET_TX, net_tx_offset, net_tx_capacity);
    write_desc(1, io_ipc_queue_kind::NET_RX, net_rx_offset, net_rx_capacity);

    sab
}

#[wasm_bindgen_test]
fn machine_attach_l2_tunnel_from_io_ipc_sab_smoke() {
    let mut m = Machine::new(2 * 1024 * 1024).expect("Machine::new");
    // Small but non-trivial capacities (must be 4-byte aligned).
    let sab = build_minimal_io_ipc_sab(64, 64);

    m.attach_l2_tunnel_from_io_ipc_sab(sab)
        .expect("attach_l2_tunnel_from_io_ipc_sab");
    m.detach_network();
}
