#![cfg(target_arch = "wasm32")]

use aero_wasm::XhciControllerBridge;
use wasm_bindgen_test::wasm_bindgen_test;

mod common;

#[wasm_bindgen_test]
fn xhci_controller_bridge_snapshot_is_deterministic_and_roundtrips() {
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x8000);

    let mut bridge =
        XhciControllerBridge::new(guest_base, guest_size).expect("new XhciControllerBridge");

    // Mutate a few registers and advance time so we aren't snapshotting the all-zero default.
    bridge.mmio_write(0x10, 4, 0x1234_5678);
    bridge.mmio_write(0x14, 2, 0xBEEF);
    bridge.tick_1ms();

    let snap1 = bridge.snapshot_state().to_vec();
    assert!(
        snap1.len() > 16,
        "expected snapshot to contain at least the header + state fields"
    );

    let snap2 = bridge.snapshot_state().to_vec();
    assert_eq!(snap1, snap2, "snapshot bytes should be deterministic");

    // Change state again so restore has something to do.
    bridge.mmio_write(0x10, 4, 0xDEAD_BEEF);
    bridge.mmio_write(0x14, 2, 0xABCD);

    bridge.restore_state(&snap1).expect("restore_state ok");

    let snap3 = bridge.snapshot_state().to_vec();
    assert_eq!(snap1, snap3, "snapshot should roundtrip");
}

#[wasm_bindgen_test]
fn xhci_snapshot_restore_rejects_device_id_mismatch() {
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x8000);

    let mut bridge =
        XhciControllerBridge::new(guest_base, guest_size).expect("new XhciControllerBridge");

    let mut snap = bridge.snapshot_state().to_vec();
    snap[8..12].copy_from_slice(b"NOPE");
    assert!(
        bridge.restore_state(&snap).is_err(),
        "expected restore_state to reject device id mismatch"
    );
}

#[wasm_bindgen_test]
fn xhci_snapshot_restore_rejects_oversized_payload() {
    // Must match the size cap in `XhciControllerBridge`.
    const MAX_XHCI_SNAPSHOT_BYTES: usize = 4 * 1024 * 1024;

    let oversized = vec![0u8; MAX_XHCI_SNAPSHOT_BYTES + 1];

    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x8000);
    let mut bridge =
        XhciControllerBridge::new(guest_base, guest_size).expect("new XhciControllerBridge");

    assert!(
        bridge.restore_state(&oversized).is_err(),
        "expected restore_state to reject oversized payload"
    );
}
