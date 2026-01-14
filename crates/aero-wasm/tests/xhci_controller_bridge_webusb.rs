#![cfg(target_arch = "wasm32")]

use wasm_bindgen::JsValue;
use wasm_bindgen_test::wasm_bindgen_test;

use aero_io_snapshot::io::state::SnapshotReader;
use aero_usb::passthrough::{UsbHostCompletion, UsbHostCompletionIn};
use aero_usb::xhci::{PORTSC_CCS, regs};
use aero_wasm::XhciControllerBridge;

mod common;

const XHCI_BRIDGE_DEVICE_ID: [u8; 4] = *b"XHCB";

// Snapshot tags from `crates/aero-wasm/src/xhci_controller_bridge.rs`.
const TAG_WEBUSB_DEVICE: u16 = 3;

fn webusb_root_port_index(bridge: &mut XhciControllerBridge) -> usize {
    let hcsparams1 = bridge.mmio_read(regs::REG_HCSPARAMS1 as u32, 4);
    let port_count = ((hcsparams1 >> 24) & 0xff) as usize;
    if port_count > 1 { 1 } else { 0 }
}

#[wasm_bindgen_test]
fn xhci_bridge_exports_webusb_passthrough_surface_and_roundtrips_snapshot() {
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x4000);

    let mut bridge =
        XhciControllerBridge::new(guest_base, guest_size).expect("XhciControllerBridge::new ok");
    bridge.set_connected(true);

    let drained = bridge.drain_actions().expect("drain_actions ok");
    assert!(drained.is_null(), "expected no queued actions initially");

    // Ensure the device is attached to the expected root port (PORTSC.CCS=1).
    let port = webusb_root_port_index(&mut bridge);
    let portsc = bridge.mmio_read(regs::port::portsc_offset(port) as u32, 4);
    assert_ne!(
        portsc & PORTSC_CCS,
        0,
        "expected PORTSC.CCS to be set after set_connected(true)"
    );

    // Smoke-test `push_completion` wiring: stale completions should be accepted and ignored.
    let completion = UsbHostCompletion::ControlIn {
        id: 1,
        result: UsbHostCompletionIn::Success {
            data: vec![1, 2, 3],
        },
    };
    let completion_js: JsValue = serde_wasm_bindgen::to_value(&completion).unwrap();
    bridge
        .push_completion(completion_js)
        .expect("push_completion ok");

    // Snapshot/restore must preserve the WebUSB "connected" flag and nested device snapshot.
    let snap_before = bridge.save_state();
    let r = SnapshotReader::parse(&snap_before, XHCI_BRIDGE_DEVICE_ID).expect("parse snapshot");
    assert!(
        r.bytes(TAG_WEBUSB_DEVICE).is_some(),
        "expected WebUSB device field in snapshot when connected"
    );

    let mut restored =
        XhciControllerBridge::new(guest_base, guest_size).expect("XhciControllerBridge::new ok");
    restored
        .load_state(&snap_before)
        .expect("load_state should succeed");

    let drained = restored.drain_actions().expect("drain_actions ok");
    assert!(
        drained.is_null(),
        "expected no queued actions after snapshot restore"
    );

    let port = webusb_root_port_index(&mut restored);
    let portsc = restored.mmio_read(regs::port::portsc_offset(port) as u32, 4);
    assert_ne!(
        portsc & PORTSC_CCS,
        0,
        "expected PORTSC.CCS to remain set after snapshot restore"
    );

    let snap_after = restored.save_state();
    let r2 = SnapshotReader::parse(&snap_after, XHCI_BRIDGE_DEVICE_ID).expect("parse snapshot");
    let webusb_before = r.bytes(TAG_WEBUSB_DEVICE).expect("webusb bytes");
    let webusb_after = r2.bytes(TAG_WEBUSB_DEVICE).expect("webusb bytes");
    assert_eq!(
        webusb_before, webusb_after,
        "expected nested WebUSB device snapshot to roundtrip"
    );
}
