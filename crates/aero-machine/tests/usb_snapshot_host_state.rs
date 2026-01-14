#![cfg(not(target_arch = "wasm32"))]

use aero_machine::{Machine, MachineConfig};
use aero_usb::hub::UsbHubDevice;
use aero_usb::{ControlResponse, SetupPacket, UsbDeviceModel, UsbWebUsbPassthroughDevice};

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

    // When both UHCI and EHCI are enabled, root port 0 is backed by a shared USB2 mux, so the same
    // physical device should be visible from both controllers.
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
