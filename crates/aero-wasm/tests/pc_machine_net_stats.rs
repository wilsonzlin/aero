#![cfg(target_arch = "wasm32")]

use aero_ipc::layout::{
    IPC_MAGIC, IPC_VERSION, io_ipc_queue_kind, ipc_header, queue_desc, ring_ctrl,
};
use aero_wasm::{PcMachine, SharedRingBuffer};
use js_sys::{BigInt, Int32Array, Reflect, SharedArrayBuffer, Uint32Array, Uint8Array};
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

fn make_shared_ring_pair(capacity_bytes: u32) -> (SharedArrayBuffer, SharedRingBuffer, SharedRingBuffer) {
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

fn make_io_ipc_sab(net_capacity_bytes: u32) -> SharedArrayBuffer {
    let align = aero_ipc::layout::RECORD_ALIGN as u32;
    assert_ne!(align, 0);
    assert_eq!(net_capacity_bytes % align, 0);

    let queue_count = 2u32;

    let mut offset_bytes = (ipc_header::BYTES + queue_count as usize * queue_desc::BYTES) as u32;
    offset_bytes = (offset_bytes + (align - 1)) & !(align - 1);

    let net_tx_off = offset_bytes;
    offset_bytes = offset_bytes
        .checked_add(ring_ctrl::BYTES as u32)
        .and_then(|v| v.checked_add(net_capacity_bytes))
        .unwrap();
    offset_bytes = (offset_bytes + (align - 1)) & !(align - 1);

    let net_rx_off = offset_bytes;
    offset_bytes = offset_bytes
        .checked_add(ring_ctrl::BYTES as u32)
        .and_then(|v| v.checked_add(net_capacity_bytes))
        .unwrap();

    let total_bytes = (offset_bytes + (align - 1)) & !(align - 1);
    let sab = SharedArrayBuffer::new(total_bytes);
    let words = Uint32Array::new(&sab);

    words.set_index(ipc_header::MAGIC as u32, IPC_MAGIC);
    words.set_index(ipc_header::VERSION as u32, IPC_VERSION);
    words.set_index(ipc_header::TOTAL_BYTES as u32, total_bytes);
    words.set_index(ipc_header::QUEUE_COUNT as u32, queue_count);

    let desc0 = ipc_header::WORDS as u32;
    words.set_index(desc0 + queue_desc::KIND as u32, io_ipc_queue_kind::NET_TX);
    words.set_index(desc0 + queue_desc::OFFSET_BYTES as u32, net_tx_off);
    words.set_index(
        desc0 + queue_desc::CAPACITY_BYTES as u32,
        net_capacity_bytes,
    );
    words.set_index(desc0 + queue_desc::RESERVED as u32, 0);

    let desc1 = desc0 + queue_desc::WORDS as u32;
    words.set_index(desc1 + queue_desc::KIND as u32, io_ipc_queue_kind::NET_RX);
    words.set_index(desc1 + queue_desc::OFFSET_BYTES as u32, net_rx_off);
    words.set_index(
        desc1 + queue_desc::CAPACITY_BYTES as u32,
        net_capacity_bytes,
    );
    words.set_index(desc1 + queue_desc::RESERVED as u32, 0);

    let ctrl_tx =
        Int32Array::new_with_byte_offset_and_length(&sab, net_tx_off, ring_ctrl::WORDS as u32);
    ctrl_tx.set_index(ring_ctrl::CAPACITY as u32, net_capacity_bytes as i32);
    let ctrl_rx =
        Int32Array::new_with_byte_offset_and_length(&sab, net_rx_off, ring_ctrl::WORDS as u32);
    ctrl_rx.set_index(ring_ctrl::CAPACITY as u32, net_capacity_bytes as i32);

    sab
}

fn get_bigint(obj: &JsValue, key: &str) -> BigInt {
    Reflect::get(obj, &JsValue::from_str(key))
        .expect("Reflect::get")
        .dyn_into::<BigInt>()
        .expect("expected BigInt")
}

#[wasm_bindgen_test]
fn pc_machine_net_stats_smoke() {
    let mut m = PcMachine::new(2 * 1024 * 1024, None).expect("PcMachine::new");

    // No backend attached yet.
    assert!(m.net_stats().is_null());

    let net_tx = make_shared_ring(64);
    let net_rx = make_shared_ring(64);

    // Prefer the legacy alias to ensure it's present and wired correctly.
    m.attach_net_rings(net_tx, net_rx);

    let stats = m.net_stats();
    assert!(!stats.is_null());

    let rx_broken = Reflect::get(&stats, &JsValue::from_str("rx_broken"))
        .expect("Reflect::get(rx_broken)");
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
        let s = got
            .to_string(10)
            .expect("BigInt::to_string")
            .as_string()
            .expect("BigInt::to_string returned non-string");
        assert_eq!(s, "0", "{key} should start at 0");
    }

    let rx_broken = Reflect::get(&stats, &JsValue::from_str("rx_broken")).expect("Reflect::get");
    assert_eq!(rx_broken.as_bool(), Some(false), "rx_broken should start false");

    // Detach via alias and ensure stats are no longer reported.
    m.detach_net_rings();
    assert!(m.net_stats().is_null());
}

#[wasm_bindgen_test]
fn pc_machine_attach_l2_tunnel_from_io_ipc_sab_smoke() {
    let mut m = PcMachine::new(2 * 1024 * 1024, None).expect("PcMachine::new");
    assert!(m.net_stats().is_null());

    let io_ipc = make_io_ipc_sab(64);
    m.attach_l2_tunnel_from_io_ipc_sab(io_ipc)
        .expect("attach_l2_tunnel_from_io_ipc_sab");
    assert!(!m.net_stats().is_null());
}

#[wasm_bindgen_test]
fn pc_machine_net_stats_marks_rx_broken_after_corrupt_record() {
    let mut m = PcMachine::new(2 * 1024 * 1024, None).expect("PcMachine::new");

    let net_tx = make_shared_ring(64);
    let (net_rx_sab, net_rx, net_rx_test) = make_shared_ring_pair(64);
    m.attach_net_rings(net_tx, net_rx);

    // Seed the ring with a valid record so HEAD!=TAIL.
    let frame = vec![0u8; aero_net_e1000::MIN_L2_FRAME_LEN];
    assert!(
        net_rx_test.try_push(&frame),
        "NET_RX try_push should succeed"
    );

    // Corrupt the record length at the current head so the next pop yields PopError::Corrupt,
    // which should permanently flip `rx_broken`.
    let ctrl =
        Int32Array::new_with_byte_offset_and_length(&net_rx_sab, 0, ring_ctrl::WORDS as u32);
    let cap = ctrl.get_index(ring_ctrl::CAPACITY as u32) as u32;
    let head = ctrl.get_index(ring_ctrl::HEAD as u32) as u32;
    let head_index = head % cap;

    let data = Uint8Array::new_with_byte_offset_and_length(&net_rx_sab, ring_ctrl::BYTES as u32, cap);
    let bogus_len = 100u32.to_le_bytes();
    data.subarray(head_index, head_index + 4).copy_from(&bogus_len);

    m.poll_network();

    let stats = m.net_stats();
    assert!(!stats.is_null());

    let rx_broken = Reflect::get(&stats, &JsValue::from_str("rx_broken"))
        .expect("Reflect::get(rx_broken)");
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
