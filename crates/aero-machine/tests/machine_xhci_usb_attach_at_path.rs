#![cfg(not(target_arch = "wasm32"))]

use std::any::Any;

use aero_machine::{Machine, MachineConfig};
use aero_usb::hid::UsbHidKeyboardHandle;
use aero_usb::hub::UsbHubDevice;
use aero_usb::xhci::regs;

#[test]
fn machine_usb_xhci_attach_at_path_attaches_keyboard_behind_nested_hub() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_xhci: true,
        // Keep this test minimal/deterministic.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).unwrap();

    // Attach a USB hub at xHCI root port 0.
    m.usb_xhci_attach_at_path(&[0], Box::new(UsbHubDevice::with_port_count(2)))
        .expect("attach hub at root port 0");

    // Attach a USB HID keyboard behind hub port 1 (path: root port 0 -> hub port 1).
    let keyboard = UsbHidKeyboardHandle::new();
    m.usb_xhci_attach_at_path(&[0, 1], Box::new(keyboard))
        .expect("attach keyboard behind hub");

    let xhci = m.xhci().expect("xhci enabled");
    let mut xhci = xhci.borrow_mut();
    let ctrl = xhci.controller_mut();

    // Root port 0 is encoded as RootHubPortNumber=1 in xHCI slot contexts/topology helpers.
    let portsc0 = ctrl.read_portsc(0);
    assert!(
        (portsc0 & regs::PORTSC_CCS) != 0,
        "port 0 should report a connected device after host attach"
    );
    assert!(
        (portsc0 & regs::PORTSC_CSC) != 0,
        "port 0 should report connect status change after host attach"
    );

    let kb_dev = ctrl
        .find_device_by_topology(1, &[1])
        .expect("keyboard should be reachable at root port 1, route [1]");
    assert!(
        (kb_dev.model() as &dyn Any).is::<UsbHidKeyboardHandle>(),
        "routed device should be the attached keyboard model"
    );

    // Detach the keyboard and ensure it is no longer reachable.
    drop(xhci);
    m.usb_xhci_detach_at_path(&[0, 1])
        .expect("detach keyboard behind hub");

    let xhci = m.xhci().expect("xhci enabled");
    let mut xhci = xhci.borrow_mut();
    let ctrl = xhci.controller_mut();
    assert!(
        ctrl.find_device_by_topology(1, &[1]).is_none(),
        "keyboard should no longer be reachable after detach"
    );
}

#[test]
fn machine_usb_xhci_attach_is_noop_when_xhci_is_disabled() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_xhci: false,
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).unwrap();

    // Should not error even though xHCI is absent.
    m.usb_xhci_attach_root(0, Box::new(UsbHubDevice::with_port_count(1)))
        .expect("no-op attach should succeed when xHCI is disabled");
}

