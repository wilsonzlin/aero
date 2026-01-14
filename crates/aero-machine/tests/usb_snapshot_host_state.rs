#![cfg(not(target_arch = "wasm32"))]

use aero_machine::{Machine, MachineConfig};
use aero_usb::hid::UsbHidPassthroughHandle;
use aero_usb::hub::UsbHubDevice;
use aero_usb::{
    ControlResponse, SetupPacket, UsbDeviceModel, UsbInResult, UsbWebUsbPassthroughDevice,
};

fn queue_webusb_control_in_action(dev: &UsbWebUsbPassthroughDevice) {
    let setup = SetupPacket {
        bm_request_type: 0x80,
        b_request: 0x06, // GET_DESCRIPTOR
        w_value: 0x0100,
        w_index: 0,
        w_length: 4,
    };

    let mut handle = dev.clone();
    assert_eq!(
        handle.handle_control_request(setup, None),
        ControlResponse::Nak,
        "expected first control request to queue a host action and return NAK"
    );
}

fn queue_webusb_bulk_in_action(dev: &UsbWebUsbPassthroughDevice) {
    let mut handle = dev.clone();
    assert_eq!(
        handle.handle_in_transfer(0x81, 16),
        UsbInResult::Nak,
        "expected first bulk/interrupt IN transfer to queue a host action and return NAK"
    );
}

fn sample_hid_report_descriptor_input_2_bytes() -> Vec<u8> {
    vec![
        0x06, 0x00, 0xff, // Usage Page (Vendor-defined 0xFF00)
        0x09, 0x01, // Usage (0x01)
        0xa1, 0x01, // Collection (Application)
        0x15, 0x00, // Logical Minimum (0)
        0x26, 0xff, 0x00, // Logical Maximum (255)
        0x75, 0x08, // Report Size (8)
        0x95, 0x02, // Report Count (2)
        0x81, 0x02, // Input (Data,Var,Abs)
        0xc0, // End Collection
    ]
}

fn queue_webhid_feature_report_request(dev: &UsbHidPassthroughHandle) {
    // HID class request: GET_REPORT(feature, report_id=3)
    let setup = SetupPacket {
        bm_request_type: 0xA1, // DeviceToHost | Class | Interface
        b_request: 0x01,       // GET_REPORT
        w_value: (3u16 << 8) | 3u16,
        w_index: 0,
        w_length: 64,
    };

    let mut handle = dev.clone();
    assert_eq!(
        handle.handle_control_request(setup, None),
        ControlResponse::Nak,
        "expected GET_REPORT(feature) to queue a host request and return NAK"
    );
}

#[test]
fn snapshot_restore_clears_uhci_webusb_host_state() {
    let mut vm = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_uhci: true,
        // Keep this test minimal/deterministic.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    let webusb = UsbWebUsbPassthroughDevice::new();
    vm.usb_attach_root(1, Box::new(webusb.clone()))
        .expect("attach WebUSB device behind UHCI");

    queue_webusb_control_in_action(&webusb);
    assert_eq!(webusb.pending_summary().queued_actions, 1);

    let snapshot = vm.take_snapshot_full().unwrap();
    vm.restore_snapshot_bytes(&snapshot).unwrap();

    let summary = webusb.pending_summary();
    assert_eq!(summary.queued_actions, 0);
    assert_eq!(summary.inflight_control, None);
}

#[test]
fn snapshot_restore_clears_uhci_webusb_bulk_in_host_state() {
    let mut vm = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_uhci: true,
        // Keep this test minimal/deterministic.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    let webusb = UsbWebUsbPassthroughDevice::new();
    vm.usb_attach_root(1, Box::new(webusb.clone()))
        .expect("attach WebUSB device behind UHCI");

    queue_webusb_bulk_in_action(&webusb);
    let before = webusb.pending_summary();
    assert_eq!(before.queued_actions, 1);
    assert_eq!(before.inflight_endpoints, 1);

    let snapshot = vm.take_snapshot_full().unwrap();
    vm.restore_snapshot_bytes(&snapshot).unwrap();

    let after = webusb.pending_summary();
    assert_eq!(after.queued_actions, 0);
    assert_eq!(after.inflight_endpoints, 0);
    assert_eq!(after.inflight_control, None);
}

#[test]
fn snapshot_restore_clears_ehci_webusb_host_state() {
    let mut vm = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_ehci: true,
        // Keep this test minimal/deterministic.
        enable_uhci: false,
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    let webusb = UsbWebUsbPassthroughDevice::new();
    {
        let ehci = vm.ehci().expect("ehci enabled");
        let mut ehci = ehci.borrow_mut();
        ehci.controller_mut()
            .hub_mut()
            .attach(1, Box::new(webusb.clone()));
    }

    queue_webusb_control_in_action(&webusb);
    assert_eq!(webusb.pending_summary().queued_actions, 1);

    let snapshot = vm.take_snapshot_full().unwrap();
    vm.restore_snapshot_bytes(&snapshot).unwrap();

    let summary = webusb.pending_summary();
    assert_eq!(summary.queued_actions, 0);
    assert_eq!(summary.inflight_control, None);
}

#[test]
fn snapshot_restore_clears_muxed_webusb_host_state() {
    let mut vm = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_uhci: true,
        enable_ehci: true,
        // Keep this test minimal/deterministic.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    let webusb = UsbWebUsbPassthroughDevice::new();
    vm.usb_attach_root(1, Box::new(webusb.clone()))
        .expect("attach WebUSB device behind UHCI root port 1");

    // When both UHCI and EHCI are enabled, the first two root ports are backed by a shared USB2
    // mux, so the same physical device should be visible from both controllers.
    {
        let ehci = vm.ehci().expect("ehci enabled");
        assert!(
            ehci.borrow().controller().hub().port_device(1).is_some(),
            "expected EHCI to observe the UHCI-attached device via the shared USB2 mux"
        );
    }

    queue_webusb_control_in_action(&webusb);
    assert_eq!(webusb.pending_summary().queued_actions, 1);

    let snapshot = vm.take_snapshot_full().unwrap();
    vm.restore_snapshot_bytes(&snapshot).unwrap();

    let summary = webusb.pending_summary();
    assert_eq!(summary.queued_actions, 0);
    assert_eq!(summary.inflight_control, None);
}

#[test]
fn snapshot_restore_clears_muxed_webhid_feature_report_host_state() {
    let mut vm = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_uhci: true,
        enable_ehci: true,
        // Keep this test minimal/deterministic.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    let webhid = UsbHidPassthroughHandle::new(
        0x1234,
        0x5678,
        "Vendor".to_string(),
        "Product".to_string(),
        None,
        sample_hid_report_descriptor_input_2_bytes(),
        false,
        None,
        None,
        None,
    );
    vm.usb_attach_root(1, Box::new(webhid.clone()))
        .expect("attach WebHID passthrough device behind UHCI root port 1");

    // When both UHCI and EHCI are enabled, the first two root ports are backed by a shared USB2
    // mux, so the same physical device should be visible from both controllers.
    {
        let ehci = vm.ehci().expect("ehci enabled");
        assert!(
            ehci.borrow().controller().hub().port_device(1).is_some(),
            "expected EHCI to observe the UHCI-attached device via the shared USB2 mux"
        );
    }

    queue_webhid_feature_report_request(&webhid);
    let req = webhid
        .pop_feature_report_request()
        .expect("expected queued feature report request");
    assert_eq!(req.request_id, 1);

    let snapshot = vm.take_snapshot_full().unwrap();
    vm.restore_snapshot_bytes(&snapshot).unwrap();

    assert!(webhid.pop_feature_report_request().is_none());

    // Re-issue the request; IDs should continue from the saved counter.
    queue_webhid_feature_report_request(&webhid);
    let req2 = webhid
        .pop_feature_report_request()
        .expect("expected feature report request after restore");
    assert_eq!(req2.request_id, 2);
}

#[test]
fn snapshot_restore_clears_webusb_host_state_behind_hub() {
    let mut vm = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_uhci: true,
        // Keep this test minimal/deterministic.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    // Attach a hub at UHCI root port 0, then attach a WebUSB passthrough device behind that hub to
    // ensure `AttachedUsbDevice::reset_host_state_for_restore()` recurses through nested hubs.
    vm.usb_attach_root(0, Box::new(UsbHubDevice::with_port_count(4)))
        .expect("attach hub behind UHCI root port 0");

    let webusb = UsbWebUsbPassthroughDevice::new();
    vm.usb_attach_at_path(&[0, 1], Box::new(webusb.clone()))
        .expect("attach WebUSB device behind hub port 1");

    queue_webusb_control_in_action(&webusb);
    assert_eq!(webusb.pending_summary().queued_actions, 1);

    let snapshot = vm.take_snapshot_full().unwrap();
    vm.restore_snapshot_bytes(&snapshot).unwrap();

    let summary = webusb.pending_summary();
    assert_eq!(summary.queued_actions, 0);
    assert_eq!(summary.inflight_control, None);
}

#[test]
fn snapshot_restore_clears_ehci_webusb_host_state_behind_hub() {
    let mut vm = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_ehci: true,
        // Keep this test minimal/deterministic.
        enable_uhci: false,
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    vm.usb_ehci_attach_root(0, Box::new(UsbHubDevice::with_port_count(4)))
        .expect("attach hub behind EHCI root port 0");

    let webusb = UsbWebUsbPassthroughDevice::new();
    vm.usb_ehci_attach_at_path(&[0, 1], Box::new(webusb.clone()))
        .expect("attach WebUSB device behind hub port 1");

    queue_webusb_control_in_action(&webusb);
    assert_eq!(webusb.pending_summary().queued_actions, 1);

    let snapshot = vm.take_snapshot_full().unwrap();
    vm.restore_snapshot_bytes(&snapshot).unwrap();

    let summary = webusb.pending_summary();
    assert_eq!(summary.queued_actions, 0);
    assert_eq!(summary.inflight_control, None);
}

#[test]
fn snapshot_restore_clears_uhci_webhid_feature_report_host_state() {
    let mut vm = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_uhci: true,
        // Keep this test minimal/deterministic.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    let webhid = UsbHidPassthroughHandle::new(
        0x1234,
        0x5678,
        "Vendor".to_string(),
        "Product".to_string(),
        None,
        sample_hid_report_descriptor_input_2_bytes(),
        false,
        None,
        None,
        None,
    );

    vm.usb_attach_root(0, Box::new(webhid.clone()))
        .expect("attach WebHID passthrough device behind UHCI");

    // Queue a host-side feature report request and simulate the host popping it before snapshot.
    queue_webhid_feature_report_request(&webhid);
    let req = webhid
        .pop_feature_report_request()
        .expect("expected queued feature report request");
    assert_eq!(req.request_id, 1);

    let snapshot = vm.take_snapshot_full().unwrap();
    vm.restore_snapshot_bytes(&snapshot).unwrap();

    // Host-side feature report requests are backed by asynchronous WebHID operations; after restore
    // they must be cleared so the guest can re-issue a fresh request.
    assert!(webhid.pop_feature_report_request().is_none());

    // Re-issue the request; IDs should continue from the saved counter.
    queue_webhid_feature_report_request(&webhid);
    let req2 = webhid
        .pop_feature_report_request()
        .expect("expected feature report request after restore");
    assert_eq!(req2.request_id, 2);
}

#[test]
fn snapshot_restore_clears_ehci_webhid_feature_report_host_state() {
    let mut vm = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_ehci: true,
        enable_uhci: false,
        // Keep this test minimal/deterministic.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    let webhid = UsbHidPassthroughHandle::new(
        0x1234,
        0x5678,
        "Vendor".to_string(),
        "Product".to_string(),
        None,
        sample_hid_report_descriptor_input_2_bytes(),
        false,
        None,
        None,
        None,
    );

    vm.usb_ehci_attach_root(0, Box::new(webhid.clone()))
        .expect("attach WebHID passthrough device behind EHCI");

    // Queue a host-side feature report request and simulate the host popping it before snapshot.
    queue_webhid_feature_report_request(&webhid);
    let req = webhid
        .pop_feature_report_request()
        .expect("expected queued feature report request");
    assert_eq!(req.request_id, 1);

    let snapshot = vm.take_snapshot_full().unwrap();
    vm.restore_snapshot_bytes(&snapshot).unwrap();

    // Host-side feature report requests are backed by asynchronous WebHID operations; after restore
    // they must be cleared so the guest can re-issue a fresh request.
    assert!(webhid.pop_feature_report_request().is_none());

    // Re-issue the request; IDs should continue from the saved counter.
    queue_webhid_feature_report_request(&webhid);
    let req2 = webhid
        .pop_feature_report_request()
        .expect("expected feature report request after restore");
    assert_eq!(req2.request_id, 2);
}

#[test]
fn snapshot_restore_clears_webhid_feature_report_host_state_behind_hub() {
    let mut vm = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_uhci: true,
        // Keep this test minimal/deterministic.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    // Attach a hub at UHCI root port 0, then attach a WebHID passthrough device behind that hub to
    // ensure `AttachedUsbDevice::reset_host_state_for_restore()` recurses through nested hubs.
    vm.usb_attach_root(0, Box::new(UsbHubDevice::with_port_count(4)))
        .expect("attach hub behind UHCI root port 0");

    let webhid = UsbHidPassthroughHandle::new(
        0x1234,
        0x5678,
        "Vendor".to_string(),
        "Product".to_string(),
        None,
        sample_hid_report_descriptor_input_2_bytes(),
        false,
        None,
        None,
        None,
    );
    vm.usb_attach_at_path(&[0, 1], Box::new(webhid.clone()))
        .expect("attach WebHID passthrough device behind hub port 1");

    queue_webhid_feature_report_request(&webhid);
    let req = webhid
        .pop_feature_report_request()
        .expect("expected queued feature report request");
    assert_eq!(req.request_id, 1);

    let snapshot = vm.take_snapshot_full().unwrap();
    vm.restore_snapshot_bytes(&snapshot).unwrap();

    // After restore, in-flight host-side feature report operations cannot be resumed and must be
    // cleared so the guest's retries will re-emit a fresh request.
    assert!(webhid.pop_feature_report_request().is_none());

    queue_webhid_feature_report_request(&webhid);
    let req2 = webhid
        .pop_feature_report_request()
        .expect("expected feature report request after restore");
    assert_eq!(req2.request_id, 2);
}

#[test]
fn snapshot_restore_clears_ehci_webhid_feature_report_host_state_behind_hub() {
    let mut vm = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_ehci: true,
        enable_uhci: false,
        // Keep this test minimal/deterministic.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    vm.usb_ehci_attach_root(0, Box::new(UsbHubDevice::with_port_count(4)))
        .expect("attach hub behind EHCI root port 0");

    let webhid = UsbHidPassthroughHandle::new(
        0x1234,
        0x5678,
        "Vendor".to_string(),
        "Product".to_string(),
        None,
        sample_hid_report_descriptor_input_2_bytes(),
        false,
        None,
        None,
        None,
    );
    vm.usb_ehci_attach_at_path(&[0, 1], Box::new(webhid.clone()))
        .expect("attach WebHID passthrough device behind hub port 1");

    queue_webhid_feature_report_request(&webhid);
    let req = webhid
        .pop_feature_report_request()
        .expect("expected queued feature report request");
    assert_eq!(req.request_id, 1);

    let snapshot = vm.take_snapshot_full().unwrap();
    vm.restore_snapshot_bytes(&snapshot).unwrap();

    assert!(webhid.pop_feature_report_request().is_none());

    // Re-issue the request; IDs should continue from the saved counter.
    queue_webhid_feature_report_request(&webhid);
    let req2 = webhid
        .pop_feature_report_request()
        .expect("expected feature report request after restore");
    assert_eq!(req2.request_id, 2);
}
