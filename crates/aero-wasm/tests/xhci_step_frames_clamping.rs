#![cfg(target_arch = "wasm32")]

use aero_io_snapshot::io::state::SnapshotReader;
use aero_wasm::XhciControllerBridge;
use wasm_bindgen_test::wasm_bindgen_test;

// Mirrors `crates/aero-wasm/src/xhci_controller_bridge.rs`.
const XHCI_BRIDGE_DEVICE_ID: [u8; 4] = *b"XHCB";
const TAG_TICK_COUNT: u16 = 2;
const MAX_STEP_FRAMES_PER_CALL: u64 = 10_000;

#[wasm_bindgen_test]
fn xhci_step_frames_clamps_huge_values() {
    // Use a small but non-zero guest base. `guest_size=0` means "rest of linear memory".
    let mut bridge = XhciControllerBridge::new(0x1000, 0).expect("bridge constructor");

    let before = bridge.save_state();
    bridge.step_frames(u32::MAX);
    let after = bridge.save_state();

    let before =
        SnapshotReader::parse(&before, XHCI_BRIDGE_DEVICE_ID).expect("parse before snapshot");
    let after = SnapshotReader::parse(&after, XHCI_BRIDGE_DEVICE_ID).expect("parse after snapshot");

    let before_tick = before
        .u64(TAG_TICK_COUNT)
        .expect("tick_count field")
        .unwrap_or(0);
    let after_tick = after
        .u64(TAG_TICK_COUNT)
        .expect("tick_count field")
        .unwrap_or(0);

    assert_eq!(
        after_tick.wrapping_sub(before_tick),
        MAX_STEP_FRAMES_PER_CALL,
        "tick_count should advance by the deterministic per-call clamp"
    );
}
