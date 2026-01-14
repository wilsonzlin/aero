#![cfg(target_arch = "wasm32")]

use aero_io_snapshot::io::state::SnapshotReader;
use aero_wasm::{XHCI_STEP_FRAMES_MAX_FRAMES, XhciControllerBridge};
use wasm_bindgen_test::wasm_bindgen_test;

mod common;

#[wasm_bindgen_test]
fn xhci_step_frames_clamps_large_values() {
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x1000);
    let mut bridge =
        XhciControllerBridge::new(guest_base, guest_size).expect("new XhciControllerBridge");

    bridge.step_frames(u32::MAX);

    let bytes = bridge.save_state();
    let r = SnapshotReader::parse(&bytes, *b"XHCB").expect("parse snapshot");
    let tick_count = r
        .u64(2)
        .expect("read tick_count")
        .expect("tick_count present");

    assert_eq!(tick_count, u64::from(XHCI_STEP_FRAMES_MAX_FRAMES));
}
