#![cfg(not(target_arch = "wasm32"))]

use aero_machine::{Machine, MachineConfig};
use aero_usb::{ControlResponse, SetupPacket, UsbDeviceModel, UsbInResult};

#[test]
fn inject_input_batch_tracks_usb_gamepad_state_before_configuration() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_uhci: true,
        enable_synthetic_usb_hid: true,
        // Keep deterministic and focused.
        enable_serial: false,
        enable_vga: false,
        enable_reset_ctrl: false,
        enable_debugcon: false,
        enable_virtio_input: false,
        enable_e1000: false,
        enable_virtio_net: false,
        enable_ahci: false,
        enable_nvme: false,
        enable_ide: false,
        enable_virtio_blk: false,
        ..Default::default()
    })
    .unwrap();

    let mut gamepad = m
        .usb_hid_gamepad_handle()
        .expect("synthetic USB gamepad should be present");
    assert!(!gamepad.configured());
    assert_eq!(gamepad.handle_interrupt_in(0x81), UsbInResult::Nak);

    // Send a non-default gamepad report while unconfigured. The device model should track the
    // current state but must not emit interrupt-IN reports until configured.
    //
    // bytes: [01 00 08 00 00 00 00 00]
    // - buttons=1, hat=8 (center), axes=0.
    let words_report: [u32; 6] = [1, 0, 5, 0, 0x0008_0001, 0x0000_0000];
    m.inject_input_batch(&words_report);
    assert_eq!(
        gamepad.handle_interrupt_in(0x81),
        UsbInResult::Nak,
        "unconfigured USB device must not emit interrupt reports"
    );

    // Configure the gamepad and ensure the held state becomes visible immediately.
    let set_cfg = SetupPacket {
        bm_request_type: 0x00,
        b_request: 0x09, // SET_CONFIGURATION
        w_value: 0x0001,
        w_index: 0,
        w_length: 0,
    };
    assert_eq!(
        gamepad.handle_control_request(set_cfg, None),
        ControlResponse::Ack
    );
    assert!(gamepad.configured());
    assert_eq!(
        gamepad.handle_interrupt_in(0x81),
        UsbInResult::Data(vec![0x01, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00]),
        "expected held gamepad report to be visible immediately after SET_CONFIGURATION"
    );
}

