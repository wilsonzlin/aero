#![cfg(not(target_arch = "wasm32"))]

use aero_machine::{Machine, MachineConfig};
use aero_usb::hid::UsbHidKeyboardHandle;
use aero_usb::hub::UsbHubDevice;
use aero_usb::UsbHubAttachError;

#[test]
fn machine_usb_attach_detach_topology_helpers() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_uhci: true,
        // Keep the machine minimal/deterministic for this host-side topology test.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).unwrap();

    // Root hub has only 2 ports (0 and 1); port 2 should fail.
    assert_eq!(
        m.usb_attach_root(2, Box::new(UsbHubDevice::new()))
            .unwrap_err(),
        UsbHubAttachError::InvalidPort
    );

    // Attach a hub to root port 0.
    m.usb_attach_root(0, Box::new(UsbHubDevice::with_port_count(4)))
        .unwrap();

    // Root port 0 is now occupied.
    assert_eq!(
        m.usb_attach_root(0, Box::new(UsbHubDevice::new()))
            .unwrap_err(),
        UsbHubAttachError::PortOccupied
    );

    // Attach a keyboard behind the hub (hub ports are 1-based).
    m.usb_attach_path(&[0, 1], Box::new(UsbHidKeyboardHandle::new()))
        .unwrap();

    // Hub port 0 is invalid (USB hub ports are 1-based).
    assert_eq!(
        m.usb_attach_path(&[0, 0], Box::new(UsbHidKeyboardHandle::new()))
            .unwrap_err(),
        UsbHubAttachError::InvalidPort
    );

    // Detach the keyboard.
    m.usb_detach_path(&[0, 1]).unwrap();

    // Detaching again should fail.
    assert_eq!(
        m.usb_detach_path(&[0, 1]).unwrap_err(),
        UsbHubAttachError::NoDevice
    );

    // Attaching behind a non-hub device should fail with NotAHub.
    m.usb_attach_root(1, Box::new(UsbHidKeyboardHandle::new()))
        .unwrap();
    assert_eq!(
        m.usb_attach_path(&[1, 1], Box::new(UsbHidKeyboardHandle::new()))
            .unwrap_err(),
        UsbHubAttachError::NotAHub
    );

    // Empty paths are invalid when UHCI is enabled.
    assert_eq!(
        m.usb_detach_path(&[]).unwrap_err(),
        UsbHubAttachError::InvalidPort
    );
}

#[test]
fn machine_usb_attach_detach_are_noops_when_uhci_is_disabled() {
    // No UHCI controller is present when `enable_uhci` is false.
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_uhci: false,
        // Keep the machine minimal/deterministic for this host-side API behaviour test.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).unwrap();

    // Attaching/detaching should be a no-op (and should not panic) even with invalid ports/paths.
    m.usb_attach_root(0, Box::new(UsbHubDevice::new()))
        .expect("usb_attach_root should be a no-op when UHCI is disabled");
    m.usb_detach_root(0)
        .expect("usb_detach_root should be a no-op when UHCI is disabled");

    m.usb_attach_path(&[2, 0, 99], Box::new(UsbHidKeyboardHandle::new()))
        .expect("usb_attach_path should be a no-op when UHCI is disabled");
    m.usb_detach_path(&[])
        .expect("usb_detach_path should be a no-op when UHCI is disabled");
}
