#![cfg(not(target_arch = "wasm32"))]

use aero_machine::{Machine, MachineConfig};
use aero_usb::{ControlResponse, SetupPacket, UsbDeviceModel, UsbInResult};
use aero_usb::hid::{UsbHidGamepadHandle, UsbHidKeyboardHandle, UsbHidMouseHandle};

#[test]
fn uhci_synthetic_hid_topology_and_snapshot_restore_handle_stability() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_uhci: true,
        enable_synthetic_usb_hid: true,
        // Keep minimal/deterministic.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).unwrap();

    let uhci = m.uhci().expect("UHCI device should exist");

    // Topology: root port 0 must be occupied by a hub.
    let (kbd, mouse, gamepad) = {
        let mut uhci = uhci.borrow_mut();
        let root = uhci.controller_mut().hub_mut();
        let mut root0 = root
            .port_device_mut(0)
            .expect("root port 0 should have external hub attached");
        assert!(
            root0.as_hub().is_some(),
            "root port 0 should have a USB hub device attached"
        );
        assert_eq!(
            root0.model().hub_port_count(),
            Some(16),
            "external hub should have 16 downstream ports"
        );

        let kbd_dev = root0
            .model_mut()
            .hub_port_device_mut(1)
            .expect("hub port 1 should have synthetic keyboard attached");
        let kbd = (kbd_dev.model() as &dyn std::any::Any)
            .downcast_ref::<UsbHidKeyboardHandle>()
            .expect("hub port 1 should be UsbHidKeyboardHandle")
            .clone();

        let mouse_dev = root0
            .model_mut()
            .hub_port_device_mut(2)
            .expect("hub port 2 should have synthetic mouse attached");
        let mouse = (mouse_dev.model() as &dyn std::any::Any)
            .downcast_ref::<UsbHidMouseHandle>()
            .expect("hub port 2 should be UsbHidMouseHandle")
            .clone();

        let gamepad_dev = root0
            .model_mut()
            .hub_port_device_mut(3)
            .expect("hub port 3 should have synthetic gamepad attached");
        let gamepad = (gamepad_dev.model() as &dyn std::any::Any)
            .downcast_ref::<UsbHidGamepadHandle>()
            .expect("hub port 3 should be UsbHidGamepadHandle")
            .clone();

        (kbd, mouse, gamepad)
    };

    // Smoke-test: configure the keyboard and ensure an injected usage yields an interrupt-IN report.
    assert!(!kbd.configured(), "keyboard should start unconfigured");
    {
        let mut dev = kbd.clone();
        let resp = dev.handle_control_request(
            SetupPacket {
                bm_request_type: 0x00, // HostToDevice | Standard | Device
                b_request: 0x09,       // SET_CONFIGURATION
                w_value: 1,
                w_index: 0,
                w_length: 0,
            },
            None,
        );
        assert!(matches!(resp, ControlResponse::Ack));
    }
    assert!(kbd.configured(), "keyboard should be configured after SET_CONFIGURATION");

    // Inject a key press via the machine API.
    m.inject_usb_hid_keyboard_usage(0x04, true); // 'A'
    {
        let mut dev = kbd.clone();
        let res = dev.handle_interrupt_in(0x81);
        assert!(
            matches!(res, UsbInResult::Data(_)),
            "expected a keyboard interrupt-IN report after injection"
        );
    }

    // Clear any queued reports and return to a stable baseline before snapshotting.
    m.inject_usb_hid_keyboard_usage(0x04, false); // release 'A'
    for _ in 0..8 {
        let mut dev = kbd.clone();
        if matches!(dev.handle_interrupt_in(0x81), UsbInResult::Nak) {
            break;
        }
    }

    // Snapshot/restore roundtrip should preserve topology and keep the same HID handle instances
    // reachable so previously-cloned handles remain valid.
    let snap = m.take_snapshot_full().unwrap();
    m.restore_snapshot_bytes(&snap).unwrap();

    // Re-check topology and obtain a fresh handle clone from the restored tree.
    let kbd_restored = {
        let mut uhci = uhci.borrow_mut();
        let root = uhci.controller_mut().hub_mut();
        let mut root0 = root
            .port_device_mut(0)
            .expect("root port 0 should remain occupied after restore");
        assert!(root0.as_hub().is_some());

        let kbd_dev = root0
            .model_mut()
            .hub_port_device_mut(1)
            .expect("hub port 1 should remain occupied after restore");
        (kbd_dev.model() as &dyn std::any::Any)
            .downcast_ref::<UsbHidKeyboardHandle>()
            .expect("restored hub port 1 should be UsbHidKeyboardHandle")
            .clone()
    };

    // Inject via the pre-restore handle; if restore reconstructed a new keyboard instance, this
    // would not show up in the restored device's interrupt queue.
    kbd.key_event(0x05, true); // 'B'
    {
        let mut dev = kbd_restored.clone();
        let res = dev.handle_interrupt_in(0x81);
        assert!(
            matches!(res, UsbInResult::Data(_)),
            "expected restored keyboard to observe injection via pre-restore handle"
        );
    }

    // Silence unused variable warnings (topology-only in this smoke test).
    let _ = (mouse, gamepad);
}
