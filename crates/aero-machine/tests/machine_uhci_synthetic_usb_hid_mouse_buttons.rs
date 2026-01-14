#![cfg(not(target_arch = "wasm32"))]

use aero_machine::{Machine, MachineConfig};
use aero_usb::{ControlResponse, SetupPacket, UsbDeviceModel, UsbInResult};

fn synthetic_usb_hid_cfg() -> MachineConfig {
    MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_uhci: true,
        enable_synthetic_usb_hid: true,
        // Keep the machine minimal/deterministic for device-model tests.
        enable_ahci: false,
        enable_nvme: false,
        enable_ide: false,
        enable_virtio_blk: false,
        enable_virtio_net: false,
        enable_e1000: false,
        enable_vga: false,
        enable_aerogpu: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    }
}

#[test]
fn machine_synthetic_usb_hid_mouse_buttons_mask_injection_produces_reports() {
    let mut m = Machine::new(synthetic_usb_hid_cfg()).unwrap();

    let mut mouse = m
        .usb_hid_mouse_handle()
        .expect("synthetic USB HID mouse handle should be present");

    assert_eq!(
        mouse.handle_control_request(
            SetupPacket {
                bm_request_type: 0x00,
                b_request: 0x09, // SET_CONFIGURATION
                w_value: 1,
                w_index: 0,
                w_length: 0,
            },
            None,
        ),
        ControlResponse::Ack,
        "mouse should accept SET_CONFIGURATION"
    );

    // DOM `MouseEvent.buttons` bit 3 (0x08) maps to the HID back/side button.
    m.inject_usb_hid_mouse_buttons(0x08);
    let report = match mouse.handle_in_transfer(0x81, 5) {
        UsbInResult::Data(data) => data,
        other => panic!("expected mouse report data, got {other:?}"),
    };
    assert_eq!(report, vec![0x08, 0, 0, 0, 0]);

    // Releasing the button should enqueue a follow-up report.
    m.inject_usb_hid_mouse_buttons(0x00);
    let report = match mouse.handle_in_transfer(0x81, 5) {
        UsbInResult::Data(data) => data,
        other => panic!("expected mouse report data, got {other:?}"),
    };
    assert_eq!(report, vec![0x00, 0, 0, 0, 0]);

    // No further reports should remain queued.
    assert!(matches!(
        mouse.handle_in_transfer(0x81, 5),
        UsbInResult::Nak
    ));
}
