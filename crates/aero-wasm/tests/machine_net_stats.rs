#![cfg(target_arch = "wasm32")]

use aero_ipc::layout::ring_ctrl;
use aero_wasm::{Machine, SharedRingBuffer};
use js_sys::{BigInt, Int32Array, Reflect, SharedArrayBuffer};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::JsValue;
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

fn get_bigint(obj: &JsValue, key: &str) -> BigInt {
    Reflect::get(obj, &JsValue::from_str(key))
        .expect("Reflect::get")
        .dyn_into::<BigInt>()
        .expect("expected BigInt")
}

#[wasm_bindgen_test]
fn machine_net_stats_smoke() {
    let mut m = Machine::new(2 * 1024 * 1024).expect("Machine::new");

    // No backend attached yet.
    assert!(m.net_stats().is_null());

    let net_tx = make_shared_ring(64);
    let net_rx = make_shared_ring(64);

    m.attach_net_rings(net_tx, net_rx)
        .expect("attach_net_rings");

    let stats = m.net_stats();
    assert!(!stats.is_null());

    // Stats should start at zero.
    for key in [
        "tx_pushed_frames",
        "tx_dropped_oversize",
        "tx_dropped_full",
        "rx_popped_frames",
        "rx_dropped_oversize",
        "rx_corrupt",
    ] {
        let got = get_bigint(&stats, key);
        let s = got
            .to_string(10)
            .expect("BigInt::to_string")
            .as_string()
            .expect("BigInt::to_string returned non-string");
        assert_eq!(s, "0", "{key} should start at 0");
    }

    // Detach via alias and ensure stats are no longer reported.
    m.detach_net_rings();
    assert!(m.net_stats().is_null());

    // Attaching again should restore stats reporting.
    let net_tx = make_shared_ring(64);
    let net_rx = make_shared_ring(64);
    m.attach_net_rings(net_tx, net_rx)
        .expect("attach_net_rings (second)");
    assert!(!m.net_stats().is_null());

    // Snapshot restore always detaches external backends. Ensure net_stats returns null afterwards.
    let snap = m.snapshot_full().expect("snapshot_full");
    m.restore_snapshot(&snap).expect("restore_snapshot");
    assert!(m.net_stats().is_null());
}
