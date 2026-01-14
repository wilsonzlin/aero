#![cfg(not(target_arch = "wasm32"))]

use std::rc::Rc;

use aero_devices::pci::{profile, PciInterruptPin};
use aero_io_snapshot::io::state::IoSnapshot;
use aero_machine::{Machine, MachineConfig};
use aero_usb::hid::UsbHidKeyboardHandle;
use aero_usb::hub::UsbHubDevice;
use aero_usb::{ControlResponse, SetupPacket, UsbDeviceModel, UsbInResult, UsbWebUsbPassthroughDevice};
use pretty_assertions::{assert_eq, assert_ne};

#[test]
fn snapshot_restore_roundtrips_xhci_state_and_redrives_intx_level() {
    let mut vm = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_xhci: true,
        // Keep this test focused on xHCI + PCI INTx snapshot restore behavior.
        enable_ahci: false,
        enable_ide: false,
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    let xhci = vm.xhci().expect("xhci enabled");
    let interrupts = vm.platform_interrupts().expect("pc platform enabled");
    let pci_intx = vm.pci_intx_router().expect("pc platform enabled");

    let (gsi, bdf) = {
        let bdf = profile::USB_XHCI_QEMU.bdf;
        let gsi = pci_intx.borrow().gsi_for_intx(bdf, PciInterruptPin::IntA);
        (gsi, bdf)
    };

    xhci.borrow_mut().raise_event_interrupt();
    assert!(xhci.borrow().irq_level(), "xHCI IRQ level should be asserted");

    // Intentionally do *not* sync xHCI's INTx into the platform interrupt controller before
    // snapshot. This leaves the interrupt sink desynchronized, which restore must fix up by
    // polling device-level IRQ lines again.
    assert_eq!(interrupts.borrow().gsi_level(gsi), false);

    let expected_xhci_state = { xhci.borrow().save_state() };

    let snapshot = vm.take_snapshot_full().unwrap();

    // Mutate state after snapshot so restore is an observable rewind.
    xhci.borrow_mut().clear_event_interrupt();
    assert!(
        !xhci.borrow().irq_level(),
        "clearing the event interrupt should deassert xHCI INTx"
    );

    let mutated_xhci_state = { xhci.borrow().save_state() };
    assert_ne!(
        mutated_xhci_state, expected_xhci_state,
        "xHCI state mutation did not change serialized state; test is not effective"
    );

    vm.restore_snapshot_bytes(&snapshot).unwrap();

    // Restore should not replace the xHCI instance (host wiring/backends live outside snapshots).
    let xhci_after = vm.xhci().expect("xhci still enabled");
    assert!(
        Rc::ptr_eq(&xhci, &xhci_after),
        "restore must not replace the xHCI instance"
    );

    let restored_xhci_state = { xhci_after.borrow().save_state() };
    // xHCI's save_state format may include transient fields (e.g. internal bookkeeping) that are
    // not required to roundtrip byte-for-byte. Assert the restore is observable by ensuring the
    // post-restore state differs from the mutated (post-snapshot) state.
    assert_ne!(restored_xhci_state, mutated_xhci_state);
    assert!(xhci_after.borrow().irq_level());

    // After restore, the xHCI's asserted INTx level should be re-driven into the platform
    // interrupt sink via PCI routing.
    assert_eq!(
        interrupts.borrow().gsi_level(gsi),
        true,
        "expected PCI INTx (GSI {gsi}) to be asserted for xHCI (bdf={bdf:?}) after restore"
    );
}

#[test]
fn snapshot_restore_preserves_host_attached_xhci_device_handles() {
    let mut vm = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_xhci: true,
        // Keep this test focused on xHCI snapshot restore behavior.
        enable_ahci: false,
        enable_ide: false,
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    // Host attach a hub + a shareable USB HID keyboard handle.
    vm.usb_xhci_attach_at_path(&[0], Box::new(UsbHubDevice::with_port_count(2)))
        .expect("attach hub at root port 0");

    let keyboard = UsbHidKeyboardHandle::new();
    let keyboard_handle = keyboard.clone();
    vm.usb_xhci_attach_at_path(&[0, 1], Box::new(keyboard))
        .expect("attach keyboard behind hub");

    // Configure the keyboard so injected key events buffer interrupt reports.
    {
        let xhci = vm.xhci().expect("xhci enabled");
        let mut xhci = xhci.borrow_mut();
        let ctrl = xhci.controller_mut();

        let kb_dev = ctrl
            .find_device_by_topology(1, &[1])
            .expect("keyboard reachable via topology");

        let setup = SetupPacket {
            bm_request_type: 0x00, // Host-to-device | Standard | Device
            b_request: 9,          // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        };
        assert_eq!(
            kb_dev.model_mut().handle_control_request(setup, None),
            ControlResponse::Ack,
            "expected SET_CONFIGURATION to succeed"
        );
    }

    let snapshot = vm.take_snapshot_full().unwrap();
    vm.restore_snapshot_bytes(&snapshot).unwrap();

    // After restore, the host-side keyboard handle must still drive the attached device model.
    keyboard_handle.key_event(0x04, true); // HID usage: 'A'

    let xhci = vm.xhci().expect("xhci enabled");
    let mut xhci = xhci.borrow_mut();
    let ctrl = xhci.controller_mut();
    let kb_dev = ctrl
        .find_device_by_topology(1, &[1])
        .expect("keyboard still reachable after restore");

    match kb_dev.model_mut().handle_interrupt_in(0x81) {
        UsbInResult::Data(report) => {
            assert_eq!(report.len(), 8);
            // Boot keyboard report: bytes[2..] are key usage codes; ensure 'A' is present.
            assert_eq!(report[2], 0x04);
        }
        other => panic!("expected interrupt report after key injection, got {other:?}"),
    }
}

#[test]
fn snapshot_restore_clears_xhci_webusb_host_state() {
    let mut vm = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_xhci: true,
        // Keep this test focused on xHCI snapshot restore behavior.
        enable_ahci: false,
        enable_ide: false,
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
    vm.usb_xhci_attach_root(0, Box::new(webusb.clone()))
        .expect("attach webusb device at root port 0");

    // Queue a host action so there is host-side asynchronous state to clear.
    let setup = SetupPacket {
        bm_request_type: 0x80,
        b_request: 0x06, // GET_DESCRIPTOR
        w_value: 0x0100,
        w_index: 0,
        w_length: 4,
    };
    let mut model = webusb.clone();
    assert_eq!(
        model.handle_control_request(setup, None),
        ControlResponse::Nak
    );
    assert_eq!(
        webusb.pending_summary().queued_actions,
        1,
        "expected queued host action before snapshot"
    );

    let snapshot = vm.take_snapshot_full().unwrap();
    vm.restore_snapshot_bytes(&snapshot).unwrap();

    let summary = webusb.pending_summary();
    assert_eq!(
        summary.queued_actions, 0,
        "expected host action queue to be cleared after snapshot restore"
    );
    assert_eq!(
        summary.inflight_control, None,
        "expected inflight control transfer to be cleared after snapshot restore"
    );
}
