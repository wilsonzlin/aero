#![cfg(not(target_arch = "wasm32"))]

use aero_devices::i8042::{I8042_DATA_PORT, I8042_STATUS_PORT};
use aero_machine::{Machine, MachineConfig};
use aero_usb::{ControlResponse, SetupPacket, UsbDeviceModel, UsbInResult};

fn new_test_machine() -> Machine {
    Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_uhci: true,
        enable_synthetic_usb_hid: true,
        enable_i8042: true,
        // Keep the machine minimal/deterministic for these backend-selection tests.
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
    .unwrap()
}

fn drain_i8042_output(m: &mut Machine) -> Vec<u8> {
    let mut out = Vec::new();
    // Bound the drain to avoid infinite loops if a buggy device leaves the status bit stuck.
    for _ in 0..64 {
        let status = m.io_read(I8042_STATUS_PORT, 1) as u8;
        if (status & 0x01) == 0 {
            break;
        }
        out.push(m.io_read(I8042_DATA_PORT, 1) as u8);
    }
    out
}

fn enable_ps2_mouse_reporting(m: &mut Machine) {
    // Write mouse command: 0xD4 tells the controller the next data byte is for the mouse.
    m.io_write(I8042_STATUS_PORT, 1, 0xD4);
    // 0xF4: Enable data reporting (stream mode).
    m.io_write(I8042_DATA_PORT, 1, 0xF4);

    // Expect ACK (0xFA).
    let mut out = drain_i8042_output(m);
    // Some firmware flows may have left additional bytes in the output buffer; search for the ACK.
    if !out.contains(&0xFA) {
        for _ in 0..16 {
            out.extend(drain_i8042_output(m));
            if out.contains(&0xFA) {
                break;
            }
        }
    }
    assert!(
        out.contains(&0xFA),
        "expected PS/2 mouse ACK (0xFA) after enabling reporting (got {out:02x?})"
    );
}

#[test]
fn inject_input_batch_keyboard_backend_switches_from_ps2_to_usb_only_after_key_release_when_usb_becomes_configured(
) {
    let mut m = new_test_machine();

    let mut kbd = m
        .usb_hid_keyboard_handle()
        .expect("synthetic USB keyboard should be present");
    assert!(
        !kbd.configured(),
        "synthetic USB keyboard should start unconfigured"
    );

    // Ensure the i8042 output buffer starts empty.
    let _ = drain_i8042_output(&mut m);

    // Press 'A' while the USB keyboard is unconfigured; `inject_input_batch` should route to the
    // PS/2 backend and emit a scancode byte.
    let words_press_a: [u32; 10] = [
        2, 0, // header
        6, 0, 0x0104, 0, // KeyHidUsage press (usage=0x04)
        1, 0, 0x1c, 1, // KeyScancode make (Set-2 0x1C)
    ];
    m.inject_input_batch(&words_press_a);

    let out = drain_i8042_output(&mut m);
    assert!(
        out == vec![0x1e] || out == vec![0x1c],
        "expected PS/2 key-down to produce a translated Set-1 byte (0x1e) or raw Set-2 byte (0x1c), got {out:02x?}"
    );

    // Configure the USB keyboard *while the key is still held*. Backend selection should remain on
    // PS/2 until the key is released.
    let set_cfg = SetupPacket {
        bm_request_type: 0x00, // HostToDevice | Standard | Device
        b_request: 0x09,       // SET_CONFIGURATION
        w_value: 0x0001,
        w_index: 0,
        w_length: 0,
    };
    assert_eq!(
        kbd.handle_control_request(set_cfg, None),
        ControlResponse::Ack
    );
    assert!(kbd.configured());

    // Clear any buffered i8042 bytes before releasing.
    let _ = drain_i8042_output(&mut m);

    // Release 'A' after the USB keyboard becomes configured. The release should still be routed to
    // the PS/2 backend (to keep the press+release pair consistent).
    let words_release_a: [u32; 10] = [
        2, 0, // header
        6, 0, 0x0004, 0, // KeyHidUsage release
        1, 0, 0x1cf0, 2, // KeyScancode break (Set-2 0xF0 0x1C)
    ];
    m.inject_input_batch(&words_release_a);

    let out = drain_i8042_output(&mut m);
    assert!(
        out == vec![0x9e] || out == vec![0xf0, 0x1c],
        "expected PS/2 key-up to produce a translated Set-1 byte (0x9e) or raw Set-2 bytes (0xf0 0x1c), got {out:02x?}"
    );

    // With no keys held and the USB keyboard now configured, subsequent events should route to the
    // USB backend (and scancode events should be ignored).
    let _ = drain_i8042_output(&mut m);
    let words_press_b: [u32; 10] = [
        2, 0, // header
        6, 0, 0x0105, 0, // KeyHidUsage press (usage=0x05)
        1, 0, 0x32, 1, // KeyScancode make for 'B' (Set-2 0x32)
    ];
    m.inject_input_batch(&words_press_b);

    let out = drain_i8042_output(&mut m);
    assert!(
        out.is_empty(),
        "expected no i8042 output after backend switches to USB (got {out:02x?})"
    );
    assert_eq!(
        kbd.handle_interrupt_in(0x81),
        UsbInResult::Data(vec![0, 0, 0x05, 0, 0, 0, 0, 0]),
        "expected USB keyboard report for 'B' after switching away from PS/2"
    );
}

#[test]
fn inject_input_batch_mouse_backend_switches_from_ps2_to_usb_only_after_buttons_released_when_usb_becomes_configured(
) {
    let mut m = new_test_machine();

    let mut mouse = m
        .usb_hid_mouse_handle()
        .expect("synthetic USB mouse should be present");
    assert!(
        !mouse.configured(),
        "synthetic USB mouse should start unconfigured"
    );

    enable_ps2_mouse_reporting(&mut m);
    // Drain the ACK and any other buffered output before starting assertions.
    let _ = drain_i8042_output(&mut m);

    // Press left button while the USB mouse is unconfigured; this should route via PS/2 and emit a
    // PS/2 mouse packet.
    let words_press: [u32; 6] = [1, 0, 3, 0, 0x01, 0];
    m.inject_input_batch(&words_press);
    let out = drain_i8042_output(&mut m);
    assert!(
        !out.is_empty(),
        "expected PS/2 mouse button press to produce i8042 output (got empty)"
    );

    // Configure the USB mouse *while the button is still held*. Backend selection should remain on
    // PS/2 until the button is released.
    let set_cfg = SetupPacket {
        bm_request_type: 0x00, // HostToDevice | Standard | Device
        b_request: 0x09,       // SET_CONFIGURATION
        w_value: 0x0001,
        w_index: 0,
        w_length: 0,
    };
    assert_eq!(
        mouse.handle_control_request(set_cfg, None),
        ControlResponse::Ack
    );
    assert!(mouse.configured());

    // Clear any buffered i8042 bytes before releasing.
    let _ = drain_i8042_output(&mut m);

    // Release all buttons after the USB mouse becomes configured. The release should still be
    // routed to the PS/2 backend (to keep the press+release pair consistent).
    let words_release: [u32; 6] = [1, 0, 3, 0, 0x00, 0];
    m.inject_input_batch(&words_release);
    let out = drain_i8042_output(&mut m);
    assert!(
        !out.is_empty(),
        "expected PS/2 mouse button release to produce i8042 output (got empty)"
    );

    // With no buttons held and the USB mouse now configured, subsequent motion should route to the
    // USB backend (and PS/2 packets should stop).
    let _ = drain_i8042_output(&mut m);
    let words_move: [u32; 6] = [1, 0, 2, 0, 5, 0]; // MouseMove dx=5, dy=0
    m.inject_input_batch(&words_move);

    let out = drain_i8042_output(&mut m);
    assert!(
        out.is_empty(),
        "expected no PS/2 mouse packet after backend switches to USB (got {out:02x?})"
    );
    assert_eq!(
        mouse.handle_interrupt_in(0x81),
        UsbInResult::Data(vec![0x00, 5, 0, 0, 0]),
        "expected USB mouse report after switching away from PS/2"
    );
}
