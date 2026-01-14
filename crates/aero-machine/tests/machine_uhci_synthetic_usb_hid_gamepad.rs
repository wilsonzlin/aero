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
fn machine_synthetic_usb_hid_gamepad_report_injection_produces_report() {
    let mut m = Machine::new(synthetic_usb_hid_cfg()).unwrap();

    let mut gamepad = m
        .usb_hid_gamepad_handle()
        .expect("synthetic USB HID gamepad handle should be present");

    assert_eq!(
        gamepad.handle_control_request(
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
        "gamepad should accept SET_CONFIGURATION"
    );

    // Bytes: [01 00 08 00 00 00 00 00]
    // - buttons=1
    // - hat=8 (null/center)
    // - axes=0
    m.inject_usb_hid_gamepad_report(0x0008_0001, 0x0000_0000);

    let report = match gamepad.handle_in_transfer(0x81, 8) {
        UsbInResult::Data(data) => data,
        other => panic!("expected gamepad report data, got {other:?}"),
    };
    assert_eq!(report, vec![0x01, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00]);
}

