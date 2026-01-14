#![cfg(target_arch = "wasm32")]

use aero_ipc::layout::ring_ctrl;
use aero_wasm::{Machine, SharedRingBuffer};
use js_sys::{BigInt, Int32Array, Reflect, SharedArrayBuffer, Uint8Array};
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

fn make_shared_ring_pair(
    capacity_bytes: u32,
) -> (SharedArrayBuffer, SharedRingBuffer, SharedRingBuffer) {
    assert_eq!(capacity_bytes as usize % aero_ipc::layout::RECORD_ALIGN, 0);
    let total_bytes = ring_ctrl::BYTES as u32 + capacity_bytes;
    let sab = SharedArrayBuffer::new(total_bytes);
    let ctrl = Int32Array::new_with_byte_offset_and_length(&sab, 0, ring_ctrl::WORDS as u32);
    // Initialize the header: capacity + zeroed indices.
    ctrl.set_index(ring_ctrl::CAPACITY as u32, capacity_bytes as i32);

    let a = SharedRingBuffer::new(sab.clone(), 0).expect("SharedRingBuffer::new (a)");
    let b = SharedRingBuffer::new(sab.clone(), 0).expect("SharedRingBuffer::new (b)");
    (sab, a, b)
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
    let (_net_rx_sab, net_rx, net_rx_test) = make_shared_ring_pair(64);

    m.attach_net_rings(net_tx, net_rx)
        .expect("attach_net_rings");

    let stats = m.net_stats();
    assert!(!stats.is_null());

    let rx_broken =
        Reflect::get(&stats, &JsValue::from_str("rx_broken")).expect("Reflect::get(rx_broken)");
    assert_eq!(
        rx_broken.as_bool(),
        Some(false),
        "rx_broken should start false"
    );

    // Stats should start at zero.
    for key in [
        "tx_pushed_frames",
        "tx_pushed_bytes",
        "tx_dropped_oversize",
        "tx_dropped_oversize_bytes",
        "tx_dropped_full",
        "tx_dropped_full_bytes",
        "rx_popped_frames",
        "rx_popped_bytes",
        "rx_dropped_oversize",
        "rx_dropped_oversize_bytes",
        "rx_corrupt",
    ] {
        let got = get_bigint(&stats, key);
        assert_eq!(got, 0u64, "{key} should start at 0");
    }

    let rx_broken = Reflect::get(&stats, &JsValue::from_str("rx_broken")).expect("Reflect::get");
    assert_eq!(
        rx_broken.as_bool(),
        Some(false),
        "rx_broken should start false"
    );

    // Push one host->guest frame into NET_RX and run the network pump once.
    // This should cause the ring backend to pop the frame and increment its RX counters.
    let frame = vec![0u8; aero_net_e1000::MIN_L2_FRAME_LEN];
    assert!(
        net_rx_test.try_push(&frame),
        "NET_RX try_push should succeed"
    );
    m.poll_network();

    let stats = m.net_stats();
    assert_eq!(
        get_bigint(&stats, "rx_popped_frames"),
        1u64,
        "rx_popped_frames should increment after polling"
    );
    assert_eq!(
        get_bigint(&stats, "rx_popped_bytes"),
        frame.len() as u64,
        "rx_popped_bytes should track delivered bytes"
    );

    let got = get_bigint(&stats, "rx_popped_bytes");
    let s = got
        .to_string(10)
        .expect("BigInt::to_string")
        .as_string()
        .expect("BigInt::to_string returned non-string");
    assert_eq!(
        s,
        frame.len().to_string(),
        "rx_popped_bytes should increment by frame.len() after polling"
    );

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

#[wasm_bindgen_test]
fn machine_net_stats_marks_rx_broken_after_corrupt_record() {
    let mut m = Machine::new(2 * 1024 * 1024).expect("Machine::new");

    let net_tx = make_shared_ring(64);
    let (net_rx_sab, net_rx, net_rx_test) = make_shared_ring_pair(64);
    m.attach_net_rings(net_tx, net_rx)
        .expect("attach_net_rings");

    // Seed the ring with a valid record so HEAD!=TAIL.
    let frame = vec![0u8; aero_net_e1000::MIN_L2_FRAME_LEN];
    assert!(
        net_rx_test.try_push(&frame),
        "NET_RX try_push should succeed"
    );

    // Corrupt the record length at the current head so the next pop yields PopError::Corrupt,
    // which should permanently flip `rx_broken`.
    let ctrl = Int32Array::new_with_byte_offset_and_length(&net_rx_sab, 0, ring_ctrl::WORDS as u32);
    let cap = ctrl.get_index(ring_ctrl::CAPACITY as u32) as u32;
    let head = ctrl.get_index(ring_ctrl::HEAD as u32) as u32;
    let head_index = head % cap;

    let data =
        Uint8Array::new_with_byte_offset_and_length(&net_rx_sab, ring_ctrl::BYTES as u32, cap);
    let bogus_len = 100u32.to_le_bytes();
    data.subarray(head_index, head_index + 4)
        .copy_from(&bogus_len);

    m.poll_network();

    let stats = m.net_stats();
    assert!(!stats.is_null());

    let rx_broken =
        Reflect::get(&stats, &JsValue::from_str("rx_broken")).expect("Reflect::get(rx_broken)");
    assert_eq!(
        rx_broken.as_bool(),
        Some(true),
        "rx_broken should become true after ring corruption"
    );

    let got = get_bigint(&stats, "rx_corrupt");
    let s = got
        .to_string(10)
        .expect("BigInt::to_string")
        .as_string()
        .expect("BigInt::to_string returned non-string");
    assert_eq!(s, "1", "rx_corrupt should increment after corruption");
}
