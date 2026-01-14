#![cfg(not(target_arch = "wasm32"))]

use aero_machine::{Machine, MachineConfig};
use aero_usb::{ControlResponse, SetupPacket, UsbDeviceModel, UsbInResult};

#[test]
fn inject_input_batch_tracks_usb_mouse_state_before_configuration_when_ps2_is_absent() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_uhci: true,
        enable_synthetic_usb_hid: true,
        // Disable PS/2 so `inject_input_batch` has to fall back to the synthetic USB mouse.
        enable_i8042: false,
        // Keep deterministic and focused.
        enable_serial: false,
        enable_vga: false,
        enable_reset_ctrl: false,
        enable_debugcon: false,
        enable_e1000: false,
        enable_virtio_net: false,
        enable_virtio_input: false,
        enable_ahci: false,
        enable_nvme: false,
        enable_ide: false,
        enable_virtio_blk: false,
        ..Default::default()
    })
    .unwrap();

    let mut mouse = m
        .usb_hid_mouse_handle()
        .expect("synthetic USB mouse should be present");
    assert!(!mouse.configured());
    assert_eq!(mouse.handle_interrupt_in(0x81), UsbInResult::Nak);

    // Press left button (DOM bit0) while unconfigured. The device should track the pressed button
    // but must not emit interrupt-IN reports until configured.
    let words_press: [u32; 6] = [1, 0, 3, 0, 0x01, 0];
    m.inject_input_batch(&words_press);
    assert_eq!(
        mouse.handle_interrupt_in(0x81),
        UsbInResult::Nak,
        "unconfigured USB device must not emit interrupt reports"
    );

    // Configure the mouse and ensure the held button state becomes visible immediately.
    let set_cfg = SetupPacket {
        bm_request_type: 0x00,
        b_request: 0x09, // SET_CONFIGURATION
        w_value: 0x0001,
        w_index: 0,
        w_length: 0,
    };
    assert_eq!(
        mouse.handle_control_request(set_cfg, None),
        ControlResponse::Ack
    );
    assert!(mouse.configured());
    assert_eq!(
        mouse.handle_interrupt_in(0x81),
        UsbInResult::Data(vec![0x01, 0, 0, 0, 0]),
        "expected held button to be visible immediately after SET_CONFIGURATION"
    );

    // Release and ensure the cleared state is reported.
    let words_release: [u32; 6] = [1, 0, 3, 0, 0x00, 0];
    m.inject_input_batch(&words_release);
    assert_eq!(
        mouse.handle_interrupt_in(0x81),
        UsbInResult::Data(vec![0x00, 0, 0, 0, 0])
    );
}

