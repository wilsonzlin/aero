#![cfg(target_arch = "wasm32")]

use aero_io_snapshot::io::state::{IoSnapshot, SnapshotVersion, SnapshotWriter};
use aero_usb::uhci::UhciController;
use aero_wasm::{UhciControllerBridge, UhciRuntime, UsbHidPassthroughBridge, WebUsbUhciBridge};
use wasm_bindgen_test::wasm_bindgen_test;

mod common;

const MIN_REPORT_DESCRIPTOR: &[u8] = &[
    0x06, 0x00, 0xff, // Usage Page (Vendor-defined 0xFF00)
    0x09, 0x01, // Usage (0x01)
    0xa1, 0x01, // Collection (Application)
    0x09, 0x02, //   Usage (0x02)
    0x15, 0x00, //   Logical Minimum (0)
    0x26, 0xff, 0x00, //   Logical Maximum (255)
    0x75, 0x08, //   Report Size (8)
    0x95, 0x01, //   Report Count (1)
    0x81, 0x02, //   Input (Data,Var,Abs)
    0xc0, // End Collection
];

#[wasm_bindgen_test]
fn uhci_controller_bridge_snapshot_is_deterministic_and_roundtrips() {
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x8000);

    let mut bridge =
        UhciControllerBridge::new(guest_base, guest_size).expect("new UhciControllerBridge");

    // Hub ports 1..=3 are reserved for synthetic HID devices, so use port 4+ for this generic
    // passthrough device.
    bridge.attach_hub(0, 4).expect("attach_hub ok");

    let dev = UsbHidPassthroughBridge::new(
        0x1234,
        0x5678,
        None,
        Some("Test HID".to_string()),
        None,
        MIN_REPORT_DESCRIPTOR.to_vec(),
        false,
        None,
        None,
    );

    let path = serde_wasm_bindgen::to_value(&vec![0u32, 4u32]).expect("path to_value");
    bridge
        .attach_usb_hid_passthrough_device(path, &dev)
        .expect("attach_usb_hid_passthrough_device ok");

    let snap1 = bridge.snapshot_state().to_vec();
    assert!(
        snap1.len() > 16,
        "expected snapshot to contain at least the header + state fields"
    );

    let snap2 = bridge.snapshot_state().to_vec();
    assert_eq!(snap1, snap2, "snapshot bytes should be deterministic");

    bridge.restore_state(&snap1).expect("restore_state ok");

    let snap3 = bridge.snapshot_state().to_vec();
    assert_eq!(snap1, snap3, "snapshot should roundtrip");
}

#[wasm_bindgen_test]
fn webusb_uhci_bridge_snapshot_is_deterministic_and_roundtrips() {
    let dev = UsbHidPassthroughBridge::new(
        0x1234,
        0x5678,
        None,
        Some("Test HID".to_string()),
        None,
        MIN_REPORT_DESCRIPTOR.to_vec(),
        false,
        None,
        None,
    );

    let mut bridge = WebUsbUhciBridge::new(0);
    bridge.set_connected(true);

    let path = serde_wasm_bindgen::to_value(&vec![0u32, 4u32]).expect("path to_value");
    bridge
        .attach_usb_hid_passthrough_device(path, &dev)
        .expect("attach_usb_hid_passthrough_device ok");

    let snap1 = bridge.snapshot_state().to_vec();
    assert!(snap1.len() > 16, "expected non-empty snapshot bytes");

    let snap2 = bridge.snapshot_state().to_vec();
    assert_eq!(snap1, snap2, "snapshot bytes should be deterministic");

    bridge.restore_state(&snap1).expect("restore_state ok");
    let snap3 = bridge.snapshot_state().to_vec();
    assert_eq!(snap1, snap3, "snapshot should roundtrip");
}

#[wasm_bindgen_test]
fn uhci_runtime_snapshot_is_deterministic_and_roundtrips() {
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x8000);
    let mut runtime = UhciRuntime::new(guest_base, guest_size).expect("new UhciRuntime");

    runtime.webusb_attach(None).expect("webusb_attach ok");

    let snap1 = runtime.snapshot_state().to_vec();
    assert!(snap1.len() > 16, "expected non-empty snapshot bytes");

    let snap2 = runtime.snapshot_state().to_vec();
    assert_eq!(snap1, snap2, "snapshot bytes should be deterministic");

    runtime.restore_state(&snap1).expect("restore_state ok");
    let snap3 = runtime.snapshot_state().to_vec();
    assert_eq!(snap1, snap3, "snapshot should roundtrip");
}

#[wasm_bindgen_test]
fn usb_snapshot_restore_rejects_device_id_mismatch() {
    // UhciControllerBridge.
    {
        let mut guest = vec![0u8; 0x8000];
        let guest_base = guest.as_mut_ptr() as u32;
        let mut bridge = UhciControllerBridge::new(guest_base, guest.len() as u32)
            .expect("new UhciControllerBridge");

        let mut snap = bridge.snapshot_state().to_vec();
        snap[8..12].copy_from_slice(b"NOPE");
        assert!(
            bridge.restore_state(&snap).is_err(),
            "expected restore_state to reject device id mismatch"
        );
    }

    // WebUsbUhciBridge.
    {
        let mut bridge = WebUsbUhciBridge::new(0);
        let mut snap = bridge.snapshot_state().to_vec();
        snap[8..12].copy_from_slice(b"NOPE");
        assert!(
            bridge.restore_state(&snap).is_err(),
            "expected restore_state to reject device id mismatch"
        );
    }

    // UhciRuntime.
    {
        let mut guest = vec![0u8; 0x8000];
        let guest_base = guest.as_mut_ptr() as u32;
        let mut runtime =
            UhciRuntime::new(guest_base, guest.len() as u32).expect("new UhciRuntime");

        let mut snap = runtime.snapshot_state().to_vec();
        snap[8..12].copy_from_slice(b"NOPE");
        assert!(
            runtime.restore_state(&snap).is_err(),
            "expected restore_state to reject device id mismatch"
        );
    }
}

#[wasm_bindgen_test]
fn usb_snapshot_restore_rejects_oversized_payload() {
    // Must match the size caps in the WASM USB bridges.
    const MAX_USB_SNAPSHOT_BYTES: usize = 4 * 1024 * 1024;

    let oversized = vec![0u8; MAX_USB_SNAPSHOT_BYTES + 1];

    // UhciControllerBridge.
    {
        let mut guest = vec![0u8; 0x8000];
        let guest_base = guest.as_mut_ptr() as u32;
        let mut bridge = UhciControllerBridge::new(guest_base, guest.len() as u32)
            .expect("new UhciControllerBridge");
        assert!(
            bridge.restore_state(&oversized).is_err(),
            "expected restore_state to reject oversized payload"
        );
    }

    // WebUsbUhciBridge.
    {
        let mut bridge = WebUsbUhciBridge::new(0);
        assert!(
            bridge.restore_state(&oversized).is_err(),
            "expected restore_state to reject oversized payload"
        );
    }

    // UhciRuntime.
    {
        let mut guest = vec![0u8; 0x8000];
        let guest_base = guest.as_mut_ptr() as u32;
        let mut runtime =
            UhciRuntime::new(guest_base, guest.len() as u32).expect("new UhciRuntime");
        assert!(
            runtime.restore_state(&oversized).is_err(),
            "expected restore_state to reject oversized payload"
        );
    }
}

#[wasm_bindgen_test]
fn usb_snapshot_restore_rejects_truncated_bytes() {
    // UhciControllerBridge.
    {
        let mut guest = vec![0u8; 0x8000];
        let guest_base = guest.as_mut_ptr() as u32;
        let mut bridge = UhciControllerBridge::new(guest_base, guest.len() as u32)
            .expect("new UhciControllerBridge");

        let snap = bridge.snapshot_state().to_vec();
        assert!(
            snap.len() >= 16,
            "expected snapshot to include header bytes"
        );

        for len in [0usize, 1, snap.len().saturating_sub(1)] {
            assert!(
                bridge.restore_state(&snap[..len]).is_err(),
                "expected restore_state to reject truncated bytes"
            );
        }
    }

    // WebUsbUhciBridge.
    {
        let mut bridge = WebUsbUhciBridge::new(0);
        let snap = bridge.snapshot_state().to_vec();
        assert!(
            snap.len() >= 16,
            "expected snapshot to include header bytes"
        );
        for len in [0usize, 1, snap.len().saturating_sub(1)] {
            assert!(
                bridge.restore_state(&snap[..len]).is_err(),
                "expected restore_state to reject truncated bytes"
            );
        }
    }

    // UhciRuntime.
    {
        let mut guest = vec![0u8; 0x8000];
        let guest_base = guest.as_mut_ptr() as u32;
        let mut runtime =
            UhciRuntime::new(guest_base, guest.len() as u32).expect("new UhciRuntime");
        let snap = runtime.snapshot_state().to_vec();
        assert!(
            snap.len() >= 16,
            "expected snapshot to include header bytes"
        );
        for len in [0usize, 1, snap.len().saturating_sub(1)] {
            assert!(
                runtime.restore_state(&snap[..len]).is_err(),
                "expected restore_state to reject truncated bytes"
            );
        }
    }
}

#[wasm_bindgen_test]
fn uhci_runtime_snapshot_restore_rejects_too_many_webhid_devices() {
    // Must match the runtime snapshot decoder limit.
    const MAX_WEBHID_SNAPSHOT_DEVICES: u32 = 1024;

    let mut guest = vec![0u8; 0x8000];
    let guest_base = guest.as_mut_ptr() as u32;
    let mut runtime = UhciRuntime::new(guest_base, guest.len() as u32).expect("new UhciRuntime");

    // Build a minimal snapshot with a valid controller payload but an invalid WebHID list count.
    let ctrl = UhciController::new();
    let ctrl_snapshot = ctrl.save_state();

    let mut webhid_list = Vec::new();
    webhid_list.extend_from_slice(&(MAX_WEBHID_SNAPSHOT_DEVICES + 1).to_le_bytes());

    let mut w = SnapshotWriter::new(*b"UHRT", SnapshotVersion::new(1, 0));
    w.field_bytes(1, ctrl_snapshot); // TAG_CONTROLLER
    w.field_bytes(6, webhid_list); // TAG_WEBHID_DEVICES
    let snapshot = w.finish();

    assert!(
        runtime.restore_state(&snapshot).is_err(),
        "expected restore_state to reject snapshots with too many WebHID devices"
    );
}
