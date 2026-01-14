use aero_usb::device::AttachedUsbDevice;
use aero_usb::hid::keyboard::UsbHidKeyboardHandle;
use aero_usb::hid::mouse::UsbHidMouseHandle;
use aero_usb::hub::UsbHubDevice;
use aero_usb::uhci::regs::{
    REG_PORTSC1, REG_USBINTR, REG_USBSTS, USBINTR_RESUME, USBSTS_RESUMEDETECT,
};
use aero_usb::uhci::UhciController;
use aero_usb::usb2_port::Usb2PortMux;
use aero_usb::{SetupPacket, UsbInResult, UsbOutResult};

mod util;

use util::TestMemory;

use std::cell::RefCell;
use std::rc::Rc;

// UHCI root hub PORTSC bits.
const PORTSC_PED: u16 = 1 << 2;
const PORTSC_RD: u16 = 1 << 6;
const PORTSC_SUSP: u16 = 1 << 12;

// Hub-class port features.
const HUB_PORT_FEATURE_RESET: u16 = 4;
const HUB_PORT_FEATURE_POWER: u16 = 8;
const HUB_PORT_FEATURE_SUSPEND: u16 = 2;
const HUB_PORT_FEATURE_C_PORT_SUSPEND: u16 = 18;

fn control_no_data(ctrl: &mut UhciController, addr: u8, setup: SetupPacket) {
    let mut dev = ctrl
        .hub_mut()
        .device_mut_for_address(addr)
        .unwrap_or_else(|| panic!("expected USB device at address {addr}"));
    assert_eq!(dev.handle_setup(setup), UsbOutResult::Ack);
    assert!(
        matches!(dev.handle_in(0, 0), UsbInResult::Data(data) if data.is_empty()),
        "expected ACK for status stage"
    );
}

fn control_no_data_dev(dev: &mut AttachedUsbDevice, setup: SetupPacket) {
    assert_eq!(dev.handle_setup(setup), UsbOutResult::Ack);
    assert!(
        matches!(dev.handle_in(0, 0), UsbInResult::Data(data) if data.is_empty()),
        "expected ACK for status stage"
    );
}

fn control_in(
    ctrl: &mut UhciController,
    addr: u8,
    setup: SetupPacket,
    max_packet: usize,
) -> Vec<u8> {
    let mut dev = ctrl
        .hub_mut()
        .device_mut_for_address(addr)
        .unwrap_or_else(|| panic!("expected USB device at address {addr}"));
    assert_eq!(dev.handle_setup(setup), UsbOutResult::Ack);

    let mut out = Vec::new();
    loop {
        match dev.handle_in(0, max_packet) {
            UsbInResult::Data(chunk) => {
                let n = chunk.len();
                out.extend_from_slice(&chunk);
                if n < max_packet {
                    break;
                }
            }
            UsbInResult::Nak => break,
            UsbInResult::Stall => panic!("expected control IN data"),
            UsbInResult::Timeout => panic!("unexpected TIMEOUT during control IN transfer"),
        }
    }

    // Status stage (OUT ZLP).
    assert_eq!(dev.handle_out(0, &[]), UsbOutResult::Ack);
    out
}

fn control_in_dev(dev: &mut AttachedUsbDevice, setup: SetupPacket, max_packet: usize) -> Vec<u8> {
    assert_eq!(dev.handle_setup(setup), UsbOutResult::Ack);

    let mut out = Vec::new();
    loop {
        match dev.handle_in(0, max_packet) {
            UsbInResult::Data(chunk) => {
                let n = chunk.len();
                out.extend_from_slice(&chunk);
                if n < max_packet {
                    break;
                }
            }
            UsbInResult::Nak => break,
            UsbInResult::Stall => panic!("expected control IN data"),
            UsbInResult::Timeout => panic!("unexpected TIMEOUT during control IN transfer"),
        }
    }

    // Status stage (OUT ZLP).
    assert_eq!(dev.handle_out(0, &[]), UsbOutResult::Ack);
    out
}

#[test]
fn hid_keyboard_remote_wakeup_sets_uhci_resume_detect() {
    let mut ctrl = UhciController::new();
    let keyboard = UsbHidKeyboardHandle::new();
    ctrl.hub_mut().attach(0, Box::new(keyboard.clone()));

    // Force-enable the port (bypassing the 50ms reset timer) so control requests can reach the
    // device and the hub can later poll for remote-wakeup requests while suspended.
    ctrl.hub_mut().force_enable_for_tests(0);

    // Guest enables Resume interrupts so the port-level Resume Detect signal latches in USBSTS and
    // raises IRQ.
    ctrl.io_write(REG_USBINTR, 2, USBINTR_RESUME as u32);

    // Minimal enumeration/configuration:
    // - assign an address
    // - configure the device
    // - enable device remote-wakeup via SET_FEATURE(DEVICE_REMOTE_WAKEUP)
    control_no_data(
        &mut ctrl,
        0,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05, // SET_ADDRESS
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09, // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x03, // SET_FEATURE
            w_value: 0x0001, // DEVICE_REMOTE_WAKEUP
            w_index: 0,
            w_length: 0,
        },
    );
    assert!(keyboard.configured(), "expected keyboard to be configured");

    // Put the port into suspend.
    let cur_portsc = ctrl.io_read(REG_PORTSC1, 2) as u16;
    ctrl.io_write(
        REG_PORTSC1,
        2,
        (cur_portsc | PORTSC_PED | PORTSC_SUSP) as u32,
    );
    let portsc = ctrl.io_read(REG_PORTSC1, 2) as u16;
    assert!(portsc & PORTSC_SUSP != 0, "expected port to be suspended");
    assert_eq!(
        portsc & PORTSC_RD,
        0,
        "resume-detect must not be asserted before remote wake triggers"
    );

    // Inject a key press while the device is suspended. The HID model should request remote wakeup
    // which the root hub turns into a port-level Resume Detect event.
    keyboard.key_event(0x04, true); // HID usage ID for KeyA

    assert!(!ctrl.irq_level(), "no IRQ expected before ticking the hub");
    let mut mem = TestMemory::new(0x1000);
    ctrl.tick_1ms(&mut mem);

    let portsc = ctrl.io_read(REG_PORTSC1, 2) as u16;
    assert!(
        portsc & PORTSC_RD != 0,
        "expected root hub port Resume Detect bit to latch after remote wake"
    );

    let usbsts = ctrl.io_read(REG_USBSTS, 2) as u16;
    assert!(
        usbsts & USBSTS_RESUMEDETECT != 0,
        "expected UHCI USBSTS.RESUMEDETECT to latch from root hub Resume Detect"
    );
    assert!(
        ctrl.irq_level(),
        "expected IRQ level high when USBINTR.RESUME is enabled and USBSTS.RESUMEDETECT is set"
    );
}

#[test]
fn hid_mouse_remote_wakeup_sets_uhci_resume_detect_from_boot_scroll() {
    let mut ctrl = UhciController::new();
    let mouse = UsbHidMouseHandle::new();
    ctrl.hub_mut().attach(0, Box::new(mouse.clone()));
    ctrl.hub_mut().force_enable_for_tests(0);

    // Guest enables Resume interrupts so remote wake latches USBSTS.RESUMEDETECT and raises IRQ.
    ctrl.io_write(REG_USBINTR, 2, USBINTR_RESUME as u32);

    // Minimal enumeration/configuration + enable device remote wakeup.
    control_no_data(
        &mut ctrl,
        0,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05, // SET_ADDRESS
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09, // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x03, // SET_FEATURE
            w_value: 0x0001, // DEVICE_REMOTE_WAKEUP
            w_index: 0,
            w_length: 0,
        },
    );
    // Select HID boot protocol so wheel input is not representable; the mouse should still request
    // remote wakeup on scroll.
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x21, // HostToDevice | Class | Interface
            b_request: 0x0b,       // SET_PROTOCOL
            w_value: 0,            // Boot
            w_index: 0,
            w_length: 0,
        },
    );
    assert!(mouse.configured(), "expected mouse to be configured");

    // Put the port into suspend.
    let cur_portsc = ctrl.io_read(REG_PORTSC1, 2) as u16;
    ctrl.io_write(
        REG_PORTSC1,
        2,
        (cur_portsc | PORTSC_PED | PORTSC_SUSP) as u32,
    );
    let portsc = ctrl.io_read(REG_PORTSC1, 2) as u16;
    assert!(portsc & PORTSC_SUSP != 0, "expected port to be suspended");
    assert_eq!(
        portsc & PORTSC_RD,
        0,
        "resume-detect must not be asserted before remote wake triggers"
    );

    // Inject a scroll while suspended. In boot protocol this should not enqueue an interrupt report,
    // but it should still request remote wakeup.
    mouse.wheel(1);

    assert!(!ctrl.irq_level(), "no IRQ expected before ticking the hub");
    let mut mem = TestMemory::new(0x1000);
    ctrl.tick_1ms(&mut mem);

    let portsc = ctrl.io_read(REG_PORTSC1, 2) as u16;
    assert!(
        portsc & PORTSC_RD != 0,
        "expected root hub port Resume Detect bit to latch after remote wake"
    );

    let usbsts = ctrl.io_read(REG_USBSTS, 2) as u16;
    assert!(
        usbsts & USBSTS_RESUMEDETECT != 0,
        "expected UHCI USBSTS.RESUMEDETECT to latch from root hub Resume Detect"
    );
    assert!(
        ctrl.irq_level(),
        "expected IRQ level high when USBINTR.RESUME is enabled and USBSTS.RESUMEDETECT is set"
    );
}

#[test]
fn hid_keyboard_remote_wakeup_does_not_trigger_without_device_remote_wakeup() {
    let mut ctrl = UhciController::new();
    let keyboard = UsbHidKeyboardHandle::new();
    ctrl.hub_mut().attach(0, Box::new(keyboard.clone()));

    // Force-enable the port (bypassing the 50ms reset timer) so control requests can reach the
    // device and the hub can later poll for remote-wakeup requests while suspended.
    ctrl.hub_mut().force_enable_for_tests(0);

    // Enable Resume interrupts so a remote wake would latch USBSTS.RESUMEDETECT and raise IRQ.
    ctrl.io_write(REG_USBINTR, 2, USBINTR_RESUME as u32);

    // Minimal enumeration/configuration, but do *not* enable DEVICE_REMOTE_WAKEUP on the device.
    control_no_data(
        &mut ctrl,
        0,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05, // SET_ADDRESS
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09, // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    assert!(keyboard.configured(), "expected keyboard to be configured");

    // Put the port into suspend.
    let cur_portsc = ctrl.io_read(REG_PORTSC1, 2) as u16;
    ctrl.io_write(
        REG_PORTSC1,
        2,
        (cur_portsc | PORTSC_PED | PORTSC_SUSP) as u32,
    );
    let portsc = ctrl.io_read(REG_PORTSC1, 2) as u16;
    assert!(portsc & PORTSC_SUSP != 0, "expected port to be suspended");
    assert_eq!(
        portsc & PORTSC_RD,
        0,
        "resume-detect must not be asserted before remote wake triggers"
    );

    // Inject a key press while suspended. Since DEVICE_REMOTE_WAKEUP is disabled, the HID device
    // must not request remote wakeup and the root hub must not latch Resume Detect.
    keyboard.key_event(0x04, true); // HID usage ID for KeyA

    assert!(!ctrl.irq_level(), "no IRQ expected before ticking the hub");
    let mut mem = TestMemory::new(0x1000);
    for _ in 0..5 {
        ctrl.tick_1ms(&mut mem);
    }

    let portsc = ctrl.io_read(REG_PORTSC1, 2) as u16;
    assert_eq!(
        portsc & PORTSC_RD,
        0,
        "unexpected Resume Detect even though device remote wake is disabled"
    );

    let usbsts = ctrl.io_read(REG_USBSTS, 2) as u16;
    assert_eq!(
        usbsts & USBSTS_RESUMEDETECT,
        0,
        "unexpected UHCI USBSTS.RESUMEDETECT even though device remote wake is disabled"
    );
    assert!(
        !ctrl.irq_level(),
        "unexpected IRQ even though device remote wake is disabled"
    );
}

#[test]
fn hid_keyboard_remote_wakeup_sets_uhci_resume_detect_through_usb2_port_mux() {
    let mut ctrl = UhciController::new();

    let mux = Rc::new(RefCell::new(Usb2PortMux::new(1)));
    ctrl.hub_mut().attach_usb2_port_mux(0, mux.clone(), 0);

    let keyboard = UsbHidKeyboardHandle::new();
    ctrl.hub_mut().attach(0, Box::new(keyboard.clone()));

    // Force-enable the port (bypassing the 50ms reset timer) so control requests can reach the
    // device and the hub can later poll for remote-wakeup requests while suspended.
    ctrl.hub_mut().force_enable_for_tests(0);

    // Guest enables Resume interrupts so the port-level Resume Detect signal latches in USBSTS and
    // raises IRQ.
    ctrl.io_write(REG_USBINTR, 2, USBINTR_RESUME as u32);

    // Minimal enumeration/configuration:
    // - assign an address
    // - configure the device
    // - enable device remote-wakeup via SET_FEATURE(DEVICE_REMOTE_WAKEUP)
    control_no_data(
        &mut ctrl,
        0,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05, // SET_ADDRESS
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09, // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x03, // SET_FEATURE
            w_value: 0x0001, // DEVICE_REMOTE_WAKEUP
            w_index: 0,
            w_length: 0,
        },
    );
    assert!(keyboard.configured(), "expected keyboard to be configured");

    // Put the port into suspend.
    let cur_portsc = ctrl.io_read(REG_PORTSC1, 2) as u16;
    ctrl.io_write(
        REG_PORTSC1,
        2,
        (cur_portsc | PORTSC_PED | PORTSC_SUSP) as u32,
    );
    let portsc = ctrl.io_read(REG_PORTSC1, 2) as u16;
    assert!(portsc & PORTSC_SUSP != 0, "expected port to be suspended");
    assert_eq!(
        portsc & PORTSC_RD,
        0,
        "resume-detect must not be asserted before remote wake triggers"
    );

    // Inject a key press while the device is suspended. The HID model should request remote wakeup
    // which the root hub turns into a port-level Resume Detect event.
    keyboard.key_event(0x04, true); // HID usage ID for KeyA

    assert!(!ctrl.irq_level(), "no IRQ expected before ticking the hub");
    let mut mem = TestMemory::new(0x1000);
    ctrl.tick_1ms(&mut mem);

    let portsc = ctrl.io_read(REG_PORTSC1, 2) as u16;
    assert!(
        portsc & PORTSC_RD != 0,
        "expected root hub port Resume Detect bit to latch after remote wake"
    );

    let usbsts = ctrl.io_read(REG_USBSTS, 2) as u16;
    assert!(
        usbsts & USBSTS_RESUMEDETECT != 0,
        "expected UHCI USBSTS.RESUMEDETECT to latch from root hub Resume Detect"
    );
    assert!(
        ctrl.irq_level(),
        "expected IRQ level high when USBINTR.RESUME is enabled and USBSTS.RESUMEDETECT is set"
    );
}

#[test]
fn hid_keyboard_remote_wakeup_sets_uhci_resume_detect_through_usb2_port_mux_and_external_hub() {
    let mut ctrl = UhciController::new();

    let mux = Rc::new(RefCell::new(Usb2PortMux::new(1)));
    ctrl.hub_mut().attach_usb2_port_mux(0, mux.clone(), 0);

    // Attach an external hub to muxed root port 0.
    ctrl.hub_mut().attach(0, Box::new(UsbHubDevice::new()));
    ctrl.hub_mut().force_enable_for_tests(0);

    // Enable Resume interrupts so remote wake latches USBSTS.RESUMEDETECT and raises IRQ.
    ctrl.io_write(REG_USBINTR, 2, USBINTR_RESUME as u32);

    // Enumerate/configure the hub: address 0 -> address 1, then SET_CONFIGURATION(1).
    control_no_data(
        &mut ctrl,
        0,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05, // SET_ADDRESS
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09, // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    // Enable DEVICE_REMOTE_WAKEUP on the hub itself so it can propagate downstream remote wake
    // requests upstream while suspended.
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x03, // SET_FEATURE
            w_value: 0x0001, // DEVICE_REMOTE_WAKEUP
            w_index: 0,
            w_length: 0,
        },
    );

    // Attach a keyboard behind downstream hub port 1.
    let keyboard = UsbHidKeyboardHandle::new();
    ctrl.hub_mut()
        .attach_at_path(&[0, 1], Box::new(keyboard.clone()))
        .expect("attach keyboard behind hub port 1");

    // Power and reset the hub port so the keyboard becomes reachable.
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x23, // HostToDevice | Class | Other (port)
            b_request: 0x03,       // SET_FEATURE
            w_value: HUB_PORT_FEATURE_POWER,
            w_index: 1,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x23,
            b_request: 0x03, // SET_FEATURE
            w_value: HUB_PORT_FEATURE_RESET,
            w_index: 1,
            w_length: 0,
        },
    );

    let mut mem = TestMemory::new(0x1000);
    for _ in 0..50 {
        ctrl.tick_1ms(&mut mem);
    }

    // Minimal enumeration/configuration for the keyboard + enable remote wakeup.
    control_no_data(
        &mut ctrl,
        0,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05, // SET_ADDRESS
            w_value: 2,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        2,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09, // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        2,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x03, // SET_FEATURE
            w_value: 0x0001, // DEVICE_REMOTE_WAKEUP
            w_index: 0,
            w_length: 0,
        },
    );
    assert!(keyboard.configured(), "expected keyboard to be configured");

    // Put the *root* port into suspend, which should suspend the hub and its downstream devices.
    let cur_portsc = ctrl.io_read(REG_PORTSC1, 2) as u16;
    ctrl.io_write(
        REG_PORTSC1,
        2,
        (cur_portsc | PORTSC_PED | PORTSC_SUSP) as u32,
    );
    let portsc = ctrl.io_read(REG_PORTSC1, 2) as u16;
    assert!(
        portsc & PORTSC_SUSP != 0,
        "expected muxed root port to be suspended"
    );
    assert_eq!(
        portsc & PORTSC_RD,
        0,
        "resume-detect must not be asserted before remote wake triggers"
    );

    // Inject a keypress while suspended. This should request remote wakeup via the hub.
    keyboard.key_event(0x04, true); // HID usage for KeyA.

    assert!(!ctrl.irq_level(), "no IRQ expected before ticking the hub");
    ctrl.tick_1ms(&mut mem);

    let portsc = ctrl.io_read(REG_PORTSC1, 2) as u16;
    assert!(
        portsc & PORTSC_RD != 0,
        "expected Resume Detect after remote wake via muxed external hub"
    );

    let usbsts = ctrl.io_read(REG_USBSTS, 2) as u16;
    assert!(
        usbsts & USBSTS_RESUMEDETECT != 0,
        "expected UHCI USBSTS.RESUMEDETECT to latch from root hub Resume Detect"
    );
    assert!(
        ctrl.irq_level(),
        "expected IRQ level high when USBINTR.RESUME is enabled and USBSTS.RESUMEDETECT is set"
    );
}

#[test]
fn hid_keyboard_remote_wakeup_does_not_propagate_through_usb2_port_mux_and_external_hub_without_hub_remote_wakeup(
) {
    let mut ctrl = UhciController::new();

    let mux = Rc::new(RefCell::new(Usb2PortMux::new(1)));
    ctrl.hub_mut().attach_usb2_port_mux(0, mux.clone(), 0);

    // Attach an external hub to muxed root port 0.
    ctrl.hub_mut().attach(0, Box::new(UsbHubDevice::new()));
    ctrl.hub_mut().force_enable_for_tests(0);

    // Enable Resume interrupts so a remote wake would latch USBSTS.RESUMEDETECT and raise IRQ.
    ctrl.io_write(REG_USBINTR, 2, USBINTR_RESUME as u32);

    // Enumerate/configure the hub: address 0 -> address 1, then SET_CONFIGURATION(1).
    //
    // Note: we intentionally do *not* enable DEVICE_REMOTE_WAKEUP on the hub.
    control_no_data(
        &mut ctrl,
        0,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05, // SET_ADDRESS
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09, // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );

    // Attach a keyboard behind downstream hub port 1.
    let keyboard = UsbHidKeyboardHandle::new();
    ctrl.hub_mut()
        .attach_at_path(&[0, 1], Box::new(keyboard.clone()))
        .expect("attach keyboard behind hub port 1");

    // Power and reset the hub port so the keyboard becomes reachable.
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x23, // HostToDevice | Class | Other (port)
            b_request: 0x03,       // SET_FEATURE
            w_value: HUB_PORT_FEATURE_POWER,
            w_index: 1,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x23,
            b_request: 0x03, // SET_FEATURE
            w_value: HUB_PORT_FEATURE_RESET,
            w_index: 1,
            w_length: 0,
        },
    );

    let mut mem = TestMemory::new(0x1000);
    for _ in 0..50 {
        ctrl.tick_1ms(&mut mem);
    }

    // Minimal enumeration/configuration for the keyboard + enable remote wakeup.
    control_no_data(
        &mut ctrl,
        0,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05, // SET_ADDRESS
            w_value: 2,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        2,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09, // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        2,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x03, // SET_FEATURE
            w_value: 0x0001, // DEVICE_REMOTE_WAKEUP
            w_index: 0,
            w_length: 0,
        },
    );
    assert!(keyboard.configured(), "expected keyboard to be configured");

    // Put the *root* port into suspend, which should suspend the hub and its downstream devices.
    let cur_portsc = ctrl.io_read(REG_PORTSC1, 2) as u16;
    ctrl.io_write(
        REG_PORTSC1,
        2,
        (cur_portsc | PORTSC_PED | PORTSC_SUSP) as u32,
    );
    let portsc = ctrl.io_read(REG_PORTSC1, 2) as u16;
    assert!(
        portsc & PORTSC_SUSP != 0,
        "expected muxed root port to be suspended"
    );
    assert_eq!(
        portsc & PORTSC_RD,
        0,
        "resume-detect must not be asserted before remote wake triggers"
    );

    // Inject a keypress while suspended. Since the hub does not have DEVICE_REMOTE_WAKEUP enabled,
    // it must not propagate the downstream remote wake request upstream.
    keyboard.key_event(0x04, true); // HID usage for KeyA.

    assert!(!ctrl.irq_level(), "no IRQ expected before ticking the hub");
    for _ in 0..5 {
        ctrl.tick_1ms(&mut mem);
    }

    let portsc = ctrl.io_read(REG_PORTSC1, 2) as u16;
    assert_ne!(portsc & PORTSC_SUSP, 0, "port should remain suspended");
    assert_eq!(
        portsc & PORTSC_RD,
        0,
        "unexpected Resume Detect even though hub remote wake is disabled"
    );
    let usbsts = ctrl.io_read(REG_USBSTS, 2) as u16;
    assert_eq!(
        usbsts & USBSTS_RESUMEDETECT,
        0,
        "unexpected UHCI USBSTS.RESUMEDETECT even though hub remote wake is disabled"
    );
    assert!(
        !ctrl.irq_level(),
        "unexpected IRQ even though hub remote wake is disabled"
    );

    // Enable DEVICE_REMOTE_WAKEUP on the hub *after* the downstream device has already requested
    // remote wake. The hub should have drained the wake request even while propagation was
    // disabled, so enabling remote wake later must not "replay" a stale wake event.
    {
        let mut hub_dev = ctrl
            .hub_mut()
            .port_device_mut(0)
            .expect("hub device should be attached");
        control_no_data_dev(
            &mut hub_dev,
            SetupPacket {
                bm_request_type: 0x00,
                b_request: 0x03, // SET_FEATURE
                w_value: 0x0001, // DEVICE_REMOTE_WAKEUP
                w_index: 0,
                w_length: 0,
            },
        );
    }

    for _ in 0..5 {
        ctrl.tick_1ms(&mut mem);
    }

    let portsc = ctrl.io_read(REG_PORTSC1, 2) as u16;
    assert_ne!(portsc & PORTSC_SUSP, 0, "port should remain suspended");
    assert_eq!(
        portsc & PORTSC_RD,
        0,
        "unexpected Resume Detect from a stale wake request after enabling hub remote wake"
    );
    let usbsts = ctrl.io_read(REG_USBSTS, 2) as u16;
    assert_eq!(
        usbsts & USBSTS_RESUMEDETECT,
        0,
        "unexpected UHCI USBSTS.RESUMEDETECT from stale wake request after enabling hub remote wake"
    );
    assert!(
        !ctrl.irq_level(),
        "unexpected IRQ from stale wake request after enabling hub remote wake"
    );

    // A fresh key event should now be able to propagate remote wakeup through the hub.
    keyboard.key_event(0x05, true); // HID usage ID for KeyB
    ctrl.tick_1ms(&mut mem);

    let portsc = ctrl.io_read(REG_PORTSC1, 2) as u16;
    assert!(
        portsc & PORTSC_RD != 0,
        "expected Resume Detect after remote wake once hub remote wake is enabled"
    );

    let usbsts = ctrl.io_read(REG_USBSTS, 2) as u16;
    assert!(
        usbsts & USBSTS_RESUMEDETECT != 0,
        "expected UHCI USBSTS.RESUMEDETECT after remote wake once hub remote wake is enabled"
    );
    assert!(
        ctrl.irq_level(),
        "expected IRQ after remote wake once hub remote wake is enabled"
    );
}

#[test]
fn hid_keyboard_remote_wakeup_sets_uhci_resume_detect_through_usb2_port_mux_and_nested_hubs() {
    let mut ctrl = UhciController::new();

    let mux = Rc::new(RefCell::new(Usb2PortMux::new(1)));
    ctrl.hub_mut().attach_usb2_port_mux(0, mux.clone(), 0);

    // Attach an external hub to muxed root port 0.
    ctrl.hub_mut().attach(0, Box::new(UsbHubDevice::new()));
    ctrl.hub_mut().force_enable_for_tests(0);

    // Enable Resume interrupts so remote wake latches USBSTS.RESUMEDETECT and raises IRQ.
    ctrl.io_write(REG_USBINTR, 2, USBINTR_RESUME as u32);

    // Enumerate/configure the outer hub: address 0 -> address 1, then SET_CONFIGURATION(1).
    control_no_data(
        &mut ctrl,
        0,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05, // SET_ADDRESS
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09, // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    // Enable DEVICE_REMOTE_WAKEUP on the outer hub so it can propagate downstream remote wakeup
    // requests upstream.
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x03, // SET_FEATURE
            w_value: 0x0001, // DEVICE_REMOTE_WAKEUP
            w_index: 0,
            w_length: 0,
        },
    );

    // Attach an inner hub behind outer-hub port 1.
    ctrl.hub_mut()
        .attach_at_path(&[0, 1], Box::new(UsbHubDevice::new()))
        .expect("attach inner hub behind outer hub port 1");

    // Power and reset the outer hub port so the inner hub becomes reachable.
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x23, // HostToDevice | Class | Other (port)
            b_request: 0x03,       // SET_FEATURE
            w_value: HUB_PORT_FEATURE_POWER,
            w_index: 1,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x23,
            b_request: 0x03, // SET_FEATURE
            w_value: HUB_PORT_FEATURE_RESET,
            w_index: 1,
            w_length: 0,
        },
    );

    let mut mem = TestMemory::new(0x1000);
    for _ in 0..50 {
        ctrl.tick_1ms(&mut mem);
    }

    // Enumerate/configure the inner hub: address 0 -> address 2, then SET_CONFIGURATION(1).
    control_no_data(
        &mut ctrl,
        0,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05, // SET_ADDRESS
            w_value: 2,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        2,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09, // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    // Enable DEVICE_REMOTE_WAKEUP on the inner hub too.
    control_no_data(
        &mut ctrl,
        2,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x03, // SET_FEATURE
            w_value: 0x0001, // DEVICE_REMOTE_WAKEUP
            w_index: 0,
            w_length: 0,
        },
    );

    // Attach a keyboard behind inner-hub port 1.
    let keyboard = UsbHidKeyboardHandle::new();
    ctrl.hub_mut()
        .attach_at_path(&[0, 1, 1], Box::new(keyboard.clone()))
        .expect("attach keyboard behind inner hub port 1");

    // Power and reset the inner hub port so the keyboard becomes reachable.
    control_no_data(
        &mut ctrl,
        2,
        SetupPacket {
            bm_request_type: 0x23, // HostToDevice | Class | Other (port)
            b_request: 0x03,       // SET_FEATURE
            w_value: HUB_PORT_FEATURE_POWER,
            w_index: 1,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        2,
        SetupPacket {
            bm_request_type: 0x23,
            b_request: 0x03, // SET_FEATURE
            w_value: HUB_PORT_FEATURE_RESET,
            w_index: 1,
            w_length: 0,
        },
    );
    for _ in 0..50 {
        ctrl.tick_1ms(&mut mem);
    }

    // Minimal enumeration/configuration for the keyboard + enable remote wakeup.
    control_no_data(
        &mut ctrl,
        0,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05, // SET_ADDRESS
            w_value: 3,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        3,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09, // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        3,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x03, // SET_FEATURE
            w_value: 0x0001, // DEVICE_REMOTE_WAKEUP
            w_index: 0,
            w_length: 0,
        },
    );
    assert!(keyboard.configured(), "expected keyboard to be configured");

    // Sanity: the inner hub should have DEVICE_REMOTE_WAKEUP enabled (bit1 set) after
    // SET_FEATURE(DEVICE_REMOTE_WAKEUP).
    let status = control_in(
        &mut ctrl,
        2,
        SetupPacket {
            bm_request_type: 0x80, // DeviceToHost | Standard | Device
            b_request: 0x00,       // GET_STATUS
            w_value: 0,
            w_index: 0,
            w_length: 2,
        },
        64,
    );
    assert_eq!(status.len(), 2, "expected GET_STATUS to return two bytes");
    let status = u16::from_le_bytes([status[0], status[1]]);
    assert_ne!(
        status & (1 << 1),
        0,
        "inner hub should have DEVICE_REMOTE_WAKEUP enabled"
    );

    // Put the *root* port into suspend, which should suspend the hub chain and downstream devices.
    let cur_portsc = ctrl.io_read(REG_PORTSC1, 2) as u16;
    ctrl.io_write(
        REG_PORTSC1,
        2,
        (cur_portsc | PORTSC_PED | PORTSC_SUSP) as u32,
    );
    let portsc = ctrl.io_read(REG_PORTSC1, 2) as u16;
    assert!(
        portsc & PORTSC_SUSP != 0,
        "expected muxed root port to be suspended"
    );
    assert_eq!(
        portsc & PORTSC_RD,
        0,
        "resume-detect must not be asserted before remote wake triggers"
    );

    // Inject a keypress while suspended. This should request remote wakeup via the hub chain.
    keyboard.key_event(0x04, true); // HID usage for KeyA.

    assert!(!ctrl.irq_level(), "no IRQ expected before ticking the hub");
    ctrl.tick_1ms(&mut mem);

    let portsc = ctrl.io_read(REG_PORTSC1, 2) as u16;
    assert!(
        portsc & PORTSC_RD != 0,
        "expected Resume Detect after remote wake via muxed nested hubs"
    );

    let usbsts = ctrl.io_read(REG_USBSTS, 2) as u16;
    assert!(
        usbsts & USBSTS_RESUMEDETECT != 0,
        "expected UHCI USBSTS.RESUMEDETECT to latch from root hub Resume Detect"
    );
    assert!(
        ctrl.irq_level(),
        "expected IRQ level high when USBINTR.RESUME is enabled and USBSTS.RESUMEDETECT is set"
    );
}

#[test]
fn hid_keyboard_remote_wakeup_does_not_propagate_through_usb2_port_mux_and_nested_hubs_without_inner_hub_remote_wakeup(
) {
    let mut ctrl = UhciController::new();

    let mux = Rc::new(RefCell::new(Usb2PortMux::new(1)));
    ctrl.hub_mut().attach_usb2_port_mux(0, mux.clone(), 0);

    // Attach an external hub to muxed root port 0.
    ctrl.hub_mut().attach(0, Box::new(UsbHubDevice::new()));
    ctrl.hub_mut().force_enable_for_tests(0);

    // Enable Resume interrupts so a remote wake would latch USBSTS.RESUMEDETECT and raise IRQ.
    ctrl.io_write(REG_USBINTR, 2, USBINTR_RESUME as u32);

    // Enumerate/configure the outer hub: address 0 -> address 1, then SET_CONFIGURATION(1).
    control_no_data(
        &mut ctrl,
        0,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05, // SET_ADDRESS
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09, // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    // Enable DEVICE_REMOTE_WAKEUP on the outer hub only.
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x03, // SET_FEATURE
            w_value: 0x0001, // DEVICE_REMOTE_WAKEUP
            w_index: 0,
            w_length: 0,
        },
    );

    // Attach an inner hub behind outer-hub port 1.
    ctrl.hub_mut()
        .attach_at_path(&[0, 1], Box::new(UsbHubDevice::new()))
        .expect("attach inner hub behind outer hub port 1");

    // Power and reset the outer hub port so the inner hub becomes reachable.
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x23, // HostToDevice | Class | Other (port)
            b_request: 0x03,       // SET_FEATURE
            w_value: HUB_PORT_FEATURE_POWER,
            w_index: 1,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x23,
            b_request: 0x03, // SET_FEATURE
            w_value: HUB_PORT_FEATURE_RESET,
            w_index: 1,
            w_length: 0,
        },
    );

    let mut mem = TestMemory::new(0x1000);
    for _ in 0..50 {
        ctrl.tick_1ms(&mut mem);
    }

    // Enumerate/configure the inner hub: address 0 -> address 2, then SET_CONFIGURATION(1).
    //
    // Note: we intentionally do *not* enable DEVICE_REMOTE_WAKEUP on the inner hub.
    control_no_data(
        &mut ctrl,
        0,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05, // SET_ADDRESS
            w_value: 2,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        2,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09, // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    // Sanity: ensure DEVICE_REMOTE_WAKEUP is still disabled on the inner hub.
    let status = control_in(
        &mut ctrl,
        2,
        SetupPacket {
            bm_request_type: 0x80, // DeviceToHost | Standard | Device
            b_request: 0x00,       // GET_STATUS
            w_value: 0,
            w_index: 0,
            w_length: 2,
        },
        64,
    );
    assert_eq!(status.len(), 2, "expected GET_STATUS to return two bytes");
    let status = u16::from_le_bytes([status[0], status[1]]);
    assert_eq!(
        status & (1 << 1),
        0,
        "inner hub should have DEVICE_REMOTE_WAKEUP disabled"
    );

    // Attach a keyboard behind inner-hub port 1.
    let keyboard = UsbHidKeyboardHandle::new();
    ctrl.hub_mut()
        .attach_at_path(&[0, 1, 1], Box::new(keyboard.clone()))
        .expect("attach keyboard behind inner hub port 1");

    // Power and reset the inner hub port so the keyboard becomes reachable.
    control_no_data(
        &mut ctrl,
        2,
        SetupPacket {
            bm_request_type: 0x23, // HostToDevice | Class | Other (port)
            b_request: 0x03,       // SET_FEATURE
            w_value: HUB_PORT_FEATURE_POWER,
            w_index: 1,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        2,
        SetupPacket {
            bm_request_type: 0x23,
            b_request: 0x03, // SET_FEATURE
            w_value: HUB_PORT_FEATURE_RESET,
            w_index: 1,
            w_length: 0,
        },
    );
    for _ in 0..50 {
        ctrl.tick_1ms(&mut mem);
    }

    // Minimal enumeration/configuration for the keyboard + enable device remote wakeup.
    control_no_data(
        &mut ctrl,
        0,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05, // SET_ADDRESS
            w_value: 3,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        3,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09, // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        3,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x03, // SET_FEATURE
            w_value: 0x0001, // DEVICE_REMOTE_WAKEUP
            w_index: 0,
            w_length: 0,
        },
    );
    assert!(keyboard.configured(), "expected keyboard to be configured");

    // Sanity: enabling remote wake on the keyboard must not implicitly enable it on the inner hub.
    let status = control_in(
        &mut ctrl,
        2,
        SetupPacket {
            bm_request_type: 0x80, // DeviceToHost | Standard | Device
            b_request: 0x00,       // GET_STATUS
            w_value: 0,
            w_index: 0,
            w_length: 2,
        },
        64,
    );
    assert_eq!(status.len(), 2, "expected GET_STATUS to return two bytes");
    let status = u16::from_le_bytes([status[0], status[1]]);
    assert_eq!(
        status & (1 << 1),
        0,
        "inner hub should still have DEVICE_REMOTE_WAKEUP disabled"
    );

    // Put the *root* port into suspend, which should suspend the hub chain and downstream devices.
    let cur_portsc = ctrl.io_read(REG_PORTSC1, 2) as u16;
    ctrl.io_write(
        REG_PORTSC1,
        2,
        (cur_portsc | PORTSC_PED | PORTSC_SUSP) as u32,
    );
    let portsc = ctrl.io_read(REG_PORTSC1, 2) as u16;
    assert!(
        portsc & PORTSC_SUSP != 0,
        "expected muxed root port to be suspended"
    );
    assert_eq!(
        portsc & PORTSC_RD,
        0,
        "resume-detect must not be asserted before remote wake triggers"
    );

    // Inject a keypress while suspended. Since the inner hub does not have DEVICE_REMOTE_WAKEUP
    // enabled, it must not propagate the downstream remote wake request upstream.
    keyboard.key_event(0x04, true); // HID usage for KeyA.

    assert!(!ctrl.irq_level(), "no IRQ expected before ticking the hub");
    for _ in 0..5 {
        ctrl.tick_1ms(&mut mem);
    }

    let portsc = ctrl.io_read(REG_PORTSC1, 2) as u16;
    assert_ne!(portsc & PORTSC_SUSP, 0, "port should remain suspended");
    assert_eq!(
        portsc & PORTSC_RD,
        0,
        "unexpected Resume Detect even though inner hub remote wake is disabled"
    );
    let usbsts = ctrl.io_read(REG_USBSTS, 2) as u16;
    assert_eq!(
        usbsts & USBSTS_RESUMEDETECT,
        0,
        "unexpected UHCI USBSTS.RESUMEDETECT even though inner hub remote wake is disabled"
    );
    assert!(
        !ctrl.irq_level(),
        "unexpected IRQ even though inner hub remote wake is disabled"
    );
}

#[test]
fn hid_keyboard_remote_wakeup_sets_uhci_resume_detect_through_external_hub() {
    let mut ctrl = UhciController::new();

    // Attach an external hub to root port 0 (the browser runtime's synthetic HID topology).
    ctrl.hub_mut().attach(0, Box::new(UsbHubDevice::new()));
    ctrl.hub_mut().force_enable_for_tests(0);

    // Enable Resume interrupts so remote wake latches USBSTS.RESUMEDETECT and raises IRQ.
    ctrl.io_write(REG_USBINTR, 2, USBINTR_RESUME as u32);

    // Enumerate/configure the hub: address 0 -> address 1, then SET_CONFIGURATION(1).
    control_no_data(
        &mut ctrl,
        0,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05, // SET_ADDRESS
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09, // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    // Enable DEVICE_REMOTE_WAKEUP on the hub itself so it can propagate downstream remote wake
    // requests upstream while suspended.
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x03, // SET_FEATURE
            w_value: 0x0001, // DEVICE_REMOTE_WAKEUP
            w_index: 0,
            w_length: 0,
        },
    );

    // Attach a keyboard behind downstream hub port 1.
    let keyboard = UsbHidKeyboardHandle::new();
    ctrl.hub_mut()
        .attach_at_path(&[0, 1], Box::new(keyboard.clone()))
        .expect("attach keyboard behind hub port 1");

    // Power and reset the hub port so the keyboard becomes reachable.
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x23, // HostToDevice | Class | Other (port)
            b_request: 0x03,       // SET_FEATURE
            w_value: HUB_PORT_FEATURE_POWER,
            w_index: 1,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x23,
            b_request: 0x03, // SET_FEATURE
            w_value: HUB_PORT_FEATURE_RESET,
            w_index: 1,
            w_length: 0,
        },
    );

    let mut mem = TestMemory::new(0x1000);
    for _ in 0..50 {
        ctrl.tick_1ms(&mut mem);
    }

    // Minimal enumeration/configuration for the keyboard + enable remote wakeup.
    control_no_data(
        &mut ctrl,
        0,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05, // SET_ADDRESS
            w_value: 2,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        2,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09, // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        2,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x03, // SET_FEATURE
            w_value: 0x0001, // DEVICE_REMOTE_WAKEUP
            w_index: 0,
            w_length: 0,
        },
    );
    assert!(keyboard.configured(), "expected keyboard to be configured");

    // Put the *root* port into suspend, which should suspend the hub and its downstream devices.
    let cur_portsc = ctrl.io_read(REG_PORTSC1, 2) as u16;
    ctrl.io_write(
        REG_PORTSC1,
        2,
        (cur_portsc | PORTSC_PED | PORTSC_SUSP) as u32,
    );
    let portsc = ctrl.io_read(REG_PORTSC1, 2) as u16;
    assert!(
        portsc & PORTSC_SUSP != 0,
        "expected root port to be suspended"
    );
    assert_eq!(
        portsc & PORTSC_RD,
        0,
        "resume-detect must not be asserted before remote wake triggers"
    );

    // Inject a keypress while suspended. This should request remote wakeup via the hub.
    keyboard.key_event(0x04, true); // HID usage for KeyA.

    assert!(!ctrl.irq_level(), "no IRQ expected before ticking the hub");
    ctrl.tick_1ms(&mut mem);

    let portsc = ctrl.io_read(REG_PORTSC1, 2) as u16;
    assert!(
        portsc & PORTSC_RD != 0,
        "expected Resume Detect after remote wake via external hub"
    );

    let usbsts = ctrl.io_read(REG_USBSTS, 2) as u16;
    assert!(
        usbsts & USBSTS_RESUMEDETECT != 0,
        "expected UHCI USBSTS.RESUMEDETECT to latch from root hub Resume Detect"
    );
    assert!(
        ctrl.irq_level(),
        "expected IRQ level high when USBINTR.RESUME is enabled and USBSTS.RESUMEDETECT is set"
    );
}

#[test]
fn hid_keyboard_remote_wakeup_does_not_propagate_through_external_hub_without_hub_remote_wakeup() {
    let mut ctrl = UhciController::new();

    // Attach an external hub to root port 0 (the browser runtime's synthetic HID topology).
    ctrl.hub_mut().attach(0, Box::new(UsbHubDevice::new()));
    ctrl.hub_mut().force_enable_for_tests(0);

    // Enable Resume interrupts so remote wake latches USBSTS.RESUMEDETECT and raises IRQ.
    ctrl.io_write(REG_USBINTR, 2, USBINTR_RESUME as u32);

    // Enumerate/configure the hub: address 0 -> address 1, then SET_CONFIGURATION(1).
    //
    // Note: we intentionally do *not* enable DEVICE_REMOTE_WAKEUP on the hub.
    control_no_data(
        &mut ctrl,
        0,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05, // SET_ADDRESS
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09, // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );

    // Attach a keyboard behind downstream hub port 1.
    let keyboard = UsbHidKeyboardHandle::new();
    ctrl.hub_mut()
        .attach_at_path(&[0, 1], Box::new(keyboard.clone()))
        .expect("attach keyboard behind hub port 1");

    // Power and reset the hub port so the keyboard becomes reachable.
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x23, // HostToDevice | Class | Other (port)
            b_request: 0x03,       // SET_FEATURE
            w_value: HUB_PORT_FEATURE_POWER,
            w_index: 1,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x23,
            b_request: 0x03, // SET_FEATURE
            w_value: HUB_PORT_FEATURE_RESET,
            w_index: 1,
            w_length: 0,
        },
    );

    let mut mem = TestMemory::new(0x1000);
    for _ in 0..50 {
        ctrl.tick_1ms(&mut mem);
    }

    // Minimal enumeration/configuration for the keyboard + enable device remote wakeup.
    control_no_data(
        &mut ctrl,
        0,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05, // SET_ADDRESS
            w_value: 2,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        2,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09, // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        2,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x03, // SET_FEATURE
            w_value: 0x0001, // DEVICE_REMOTE_WAKEUP
            w_index: 0,
            w_length: 0,
        },
    );
    assert!(keyboard.configured(), "expected keyboard to be configured");

    // Put the *root* port into suspend, which should suspend the hub and its downstream devices.
    let cur_portsc = ctrl.io_read(REG_PORTSC1, 2) as u16;
    ctrl.io_write(
        REG_PORTSC1,
        2,
        (cur_portsc | PORTSC_PED | PORTSC_SUSP) as u32,
    );
    let portsc = ctrl.io_read(REG_PORTSC1, 2) as u16;
    assert!(
        portsc & PORTSC_SUSP != 0,
        "expected root port to be suspended"
    );
    assert_eq!(
        portsc & PORTSC_RD,
        0,
        "resume-detect must not be asserted before remote wake triggers"
    );

    // Inject a keypress while suspended. Since the hub does not have DEVICE_REMOTE_WAKEUP enabled,
    // it must not propagate the downstream remote wake request upstream.
    keyboard.key_event(0x04, true); // HID usage for KeyA.

    assert!(!ctrl.irq_level(), "no IRQ expected before ticking the hub");
    for _ in 0..5 {
        ctrl.tick_1ms(&mut mem);
    }

    let portsc = ctrl.io_read(REG_PORTSC1, 2) as u16;
    assert_ne!(portsc & PORTSC_SUSP, 0, "port should remain suspended");
    assert_eq!(
        portsc & PORTSC_RD,
        0,
        "unexpected Resume Detect even though hub remote wake is disabled"
    );
    let usbsts = ctrl.io_read(REG_USBSTS, 2) as u16;
    assert_eq!(
        usbsts & USBSTS_RESUMEDETECT,
        0,
        "unexpected UHCI USBSTS.RESUMEDETECT even though hub remote wake is disabled"
    );
    assert!(
        !ctrl.irq_level(),
        "unexpected IRQ even though hub remote wake is disabled"
    );

    // Enable DEVICE_REMOTE_WAKEUP on the hub *after* the downstream device has already requested
    // remote wake. The hub should have drained the wake request even while propagation was
    // disabled, so enabling remote wake later must not "replay" a stale wake event.
    {
        let mut hub_dev = ctrl
            .hub_mut()
            .port_device_mut(0)
            .expect("hub device should be attached");
        control_no_data_dev(
            &mut hub_dev,
            SetupPacket {
                bm_request_type: 0x00,
                b_request: 0x03, // SET_FEATURE
                w_value: 0x0001, // DEVICE_REMOTE_WAKEUP
                w_index: 0,
                w_length: 0,
            },
        );
    }

    for _ in 0..5 {
        ctrl.tick_1ms(&mut mem);
    }

    let portsc = ctrl.io_read(REG_PORTSC1, 2) as u16;
    assert_ne!(portsc & PORTSC_SUSP, 0, "port should remain suspended");
    assert_eq!(
        portsc & PORTSC_RD,
        0,
        "unexpected Resume Detect from a stale wake request after enabling hub remote wake"
    );
    let usbsts = ctrl.io_read(REG_USBSTS, 2) as u16;
    assert_eq!(
        usbsts & USBSTS_RESUMEDETECT,
        0,
        "unexpected UHCI USBSTS.RESUMEDETECT from stale wake request after enabling hub remote wake"
    );
    assert!(
        !ctrl.irq_level(),
        "unexpected IRQ from stale wake request after enabling hub remote wake"
    );

    // A fresh key event should now propagate remote wakeup through the hub.
    keyboard.key_event(0x05, true); // HID usage ID for KeyB
    ctrl.tick_1ms(&mut mem);

    let portsc = ctrl.io_read(REG_PORTSC1, 2) as u16;
    assert!(
        portsc & PORTSC_RD != 0,
        "expected Resume Detect after remote wake once hub remote wake is enabled"
    );

    let usbsts = ctrl.io_read(REG_USBSTS, 2) as u16;
    assert!(
        usbsts & USBSTS_RESUMEDETECT != 0,
        "expected UHCI USBSTS.RESUMEDETECT after remote wake once hub remote wake is enabled"
    );
    assert!(
        ctrl.irq_level(),
        "expected IRQ after remote wake once hub remote wake is enabled"
    );
}

#[test]
fn hid_keyboard_remote_wakeup_sets_uhci_resume_detect_through_nested_hubs() {
    let mut ctrl = UhciController::new();

    // Attach an external hub to root port 0.
    ctrl.hub_mut().attach(0, Box::new(UsbHubDevice::new()));
    ctrl.hub_mut().force_enable_for_tests(0);

    // Enable Resume interrupts so remote wake latches USBSTS.RESUMEDETECT and raises IRQ.
    ctrl.io_write(REG_USBINTR, 2, USBINTR_RESUME as u32);

    // Enumerate/configure the outer hub: address 0 -> address 1, then SET_CONFIGURATION(1).
    control_no_data(
        &mut ctrl,
        0,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05, // SET_ADDRESS
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09, // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    // Enable DEVICE_REMOTE_WAKEUP on the outer hub so it can propagate downstream remote wake
    // requests upstream while suspended.
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x03, // SET_FEATURE
            w_value: 0x0001, // DEVICE_REMOTE_WAKEUP
            w_index: 0,
            w_length: 0,
        },
    );

    // Attach an inner hub behind outer-hub port 1.
    ctrl.hub_mut()
        .attach_at_path(&[0, 1], Box::new(UsbHubDevice::new()))
        .expect("attach inner hub behind outer hub port 1");

    // Power and reset the outer hub port so the inner hub becomes reachable.
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x23, // HostToDevice | Class | Other (port)
            b_request: 0x03,       // SET_FEATURE
            w_value: HUB_PORT_FEATURE_POWER,
            w_index: 1,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x23,
            b_request: 0x03, // SET_FEATURE
            w_value: HUB_PORT_FEATURE_RESET,
            w_index: 1,
            w_length: 0,
        },
    );

    let mut mem = TestMemory::new(0x1000);
    for _ in 0..50 {
        ctrl.tick_1ms(&mut mem);
    }

    // Enumerate/configure the inner hub: address 0 -> address 2, then SET_CONFIGURATION(1).
    control_no_data(
        &mut ctrl,
        0,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05, // SET_ADDRESS
            w_value: 2,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        2,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09, // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    // Enable DEVICE_REMOTE_WAKEUP on the inner hub too so it can propagate downstream remote wake
    // requests upstream while suspended.
    control_no_data(
        &mut ctrl,
        2,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x03, // SET_FEATURE
            w_value: 0x0001, // DEVICE_REMOTE_WAKEUP
            w_index: 0,
            w_length: 0,
        },
    );

    // Attach a keyboard behind inner-hub port 1.
    let keyboard = UsbHidKeyboardHandle::new();
    ctrl.hub_mut()
        .attach_at_path(&[0, 1, 1], Box::new(keyboard.clone()))
        .expect("attach keyboard behind inner hub port 1");

    // Power and reset the inner hub port so the keyboard becomes reachable.
    control_no_data(
        &mut ctrl,
        2,
        SetupPacket {
            bm_request_type: 0x23, // HostToDevice | Class | Other (port)
            b_request: 0x03,       // SET_FEATURE
            w_value: HUB_PORT_FEATURE_POWER,
            w_index: 1,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        2,
        SetupPacket {
            bm_request_type: 0x23,
            b_request: 0x03, // SET_FEATURE
            w_value: HUB_PORT_FEATURE_RESET,
            w_index: 1,
            w_length: 0,
        },
    );
    for _ in 0..50 {
        ctrl.tick_1ms(&mut mem);
    }

    // Minimal enumeration/configuration for the keyboard + enable remote wakeup.
    control_no_data(
        &mut ctrl,
        0,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05, // SET_ADDRESS
            w_value: 3,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        3,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09, // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        3,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x03, // SET_FEATURE
            w_value: 0x0001, // DEVICE_REMOTE_WAKEUP
            w_index: 0,
            w_length: 0,
        },
    );
    assert!(keyboard.configured(), "expected keyboard to be configured");

    // Put the *root* port into suspend, which should suspend the hub chain and downstream devices.
    let cur_portsc = ctrl.io_read(REG_PORTSC1, 2) as u16;
    ctrl.io_write(
        REG_PORTSC1,
        2,
        (cur_portsc | PORTSC_PED | PORTSC_SUSP) as u32,
    );
    let portsc = ctrl.io_read(REG_PORTSC1, 2) as u16;
    assert!(
        portsc & PORTSC_SUSP != 0,
        "expected root port to be suspended"
    );
    assert_eq!(
        portsc & PORTSC_RD,
        0,
        "resume-detect must not be asserted before remote wake triggers"
    );

    // Inject a keypress while suspended. This should request remote wakeup via the hub chain.
    keyboard.key_event(0x04, true); // HID usage for KeyA.

    assert!(!ctrl.irq_level(), "no IRQ expected before ticking the hub");
    ctrl.tick_1ms(&mut mem);

    let portsc = ctrl.io_read(REG_PORTSC1, 2) as u16;
    assert!(
        portsc & PORTSC_RD != 0,
        "expected Resume Detect after remote wake via nested hubs"
    );

    let usbsts = ctrl.io_read(REG_USBSTS, 2) as u16;
    assert!(
        usbsts & USBSTS_RESUMEDETECT != 0,
        "expected UHCI USBSTS.RESUMEDETECT to latch from root hub Resume Detect"
    );
    assert!(
        ctrl.irq_level(),
        "expected IRQ level high when USBINTR.RESUME is enabled and USBSTS.RESUMEDETECT is set"
    );
}

#[test]
fn hid_keyboard_remote_wakeup_does_not_propagate_through_nested_hubs_without_inner_hub_remote_wakeup(
) {
    let mut ctrl = UhciController::new();

    // Attach an external hub to root port 0.
    ctrl.hub_mut().attach(0, Box::new(UsbHubDevice::new()));
    ctrl.hub_mut().force_enable_for_tests(0);

    // Enable Resume interrupts so remote wake latches USBSTS.RESUMEDETECT and raises IRQ.
    ctrl.io_write(REG_USBINTR, 2, USBINTR_RESUME as u32);

    // Enumerate/configure the outer hub: address 0 -> address 1, then SET_CONFIGURATION(1).
    control_no_data(
        &mut ctrl,
        0,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05, // SET_ADDRESS
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09, // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    // Enable DEVICE_REMOTE_WAKEUP on the outer hub only. The inner hub still has remote wake
    // disabled, so it will block downstream wake requests from reaching the root.
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x03, // SET_FEATURE
            w_value: 0x0001, // DEVICE_REMOTE_WAKEUP
            w_index: 0,
            w_length: 0,
        },
    );

    // Attach an inner hub behind outer-hub port 1.
    ctrl.hub_mut()
        .attach_at_path(&[0, 1], Box::new(UsbHubDevice::new()))
        .expect("attach inner hub behind outer hub port 1");

    // Power and reset the outer hub port so the inner hub becomes reachable.
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x23, // HostToDevice | Class | Other (port)
            b_request: 0x03,       // SET_FEATURE
            w_value: HUB_PORT_FEATURE_POWER,
            w_index: 1,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x23,
            b_request: 0x03, // SET_FEATURE
            w_value: HUB_PORT_FEATURE_RESET,
            w_index: 1,
            w_length: 0,
        },
    );

    let mut mem = TestMemory::new(0x1000);
    for _ in 0..50 {
        ctrl.tick_1ms(&mut mem);
    }

    // Enumerate/configure the inner hub: address 0 -> address 2, then SET_CONFIGURATION(1).
    //
    // Note: we intentionally do *not* enable DEVICE_REMOTE_WAKEUP on the inner hub.
    control_no_data(
        &mut ctrl,
        0,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05, // SET_ADDRESS
            w_value: 2,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        2,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09, // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );

    // Attach a keyboard behind inner-hub port 1.
    let keyboard = UsbHidKeyboardHandle::new();
    ctrl.hub_mut()
        .attach_at_path(&[0, 1, 1], Box::new(keyboard.clone()))
        .expect("attach keyboard behind inner hub port 1");

    // Power and reset the inner hub port so the keyboard becomes reachable.
    control_no_data(
        &mut ctrl,
        2,
        SetupPacket {
            bm_request_type: 0x23, // HostToDevice | Class | Other (port)
            b_request: 0x03,       // SET_FEATURE
            w_value: HUB_PORT_FEATURE_POWER,
            w_index: 1,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        2,
        SetupPacket {
            bm_request_type: 0x23,
            b_request: 0x03, // SET_FEATURE
            w_value: HUB_PORT_FEATURE_RESET,
            w_index: 1,
            w_length: 0,
        },
    );
    for _ in 0..50 {
        ctrl.tick_1ms(&mut mem);
    }

    // Minimal enumeration/configuration for the keyboard + enable device remote wakeup.
    control_no_data(
        &mut ctrl,
        0,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05, // SET_ADDRESS
            w_value: 3,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        3,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09, // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        3,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x03, // SET_FEATURE
            w_value: 0x0001, // DEVICE_REMOTE_WAKEUP
            w_index: 0,
            w_length: 0,
        },
    );
    assert!(keyboard.configured(), "expected keyboard to be configured");

    // Put the *root* port into suspend, which should suspend the hub chain and downstream devices.
    let cur_portsc = ctrl.io_read(REG_PORTSC1, 2) as u16;
    ctrl.io_write(
        REG_PORTSC1,
        2,
        (cur_portsc | PORTSC_PED | PORTSC_SUSP) as u32,
    );
    let portsc = ctrl.io_read(REG_PORTSC1, 2) as u16;
    assert!(
        portsc & PORTSC_SUSP != 0,
        "expected root port to be suspended"
    );
    assert_eq!(
        portsc & PORTSC_RD,
        0,
        "resume-detect must not be asserted before remote wake triggers"
    );

    // Inject a keypress while suspended. Since the inner hub does not have DEVICE_REMOTE_WAKEUP
    // enabled, it must not propagate the downstream remote wake request upstream.
    keyboard.key_event(0x04, true); // HID usage for KeyA.

    assert!(!ctrl.irq_level(), "no IRQ expected before ticking the hub");
    for _ in 0..5 {
        ctrl.tick_1ms(&mut mem);
    }

    let portsc = ctrl.io_read(REG_PORTSC1, 2) as u16;
    assert_ne!(portsc & PORTSC_SUSP, 0, "port should remain suspended");
    assert_eq!(
        portsc & PORTSC_RD,
        0,
        "unexpected Resume Detect even though inner hub remote wake is disabled"
    );
    let usbsts = ctrl.io_read(REG_USBSTS, 2) as u16;
    assert_eq!(
        usbsts & USBSTS_RESUMEDETECT,
        0,
        "unexpected UHCI USBSTS.RESUMEDETECT even though inner hub remote wake is disabled"
    );
    assert!(
        !ctrl.irq_level(),
        "unexpected IRQ even though inner hub remote wake is disabled"
    );

    // Enable DEVICE_REMOTE_WAKEUP on the inner hub *after* the downstream device has already
    // requested remote wake. The inner hub should have drained the wake request even while
    // propagation was disabled, so enabling remote wake later must not "replay" a stale wake event.
    {
        let mut outer_hub_dev = ctrl
            .hub_mut()
            .port_device_mut(0)
            .expect("outer hub device should be attached");
        let inner_hub_dev = outer_hub_dev
            .model_mut()
            .hub_port_device_mut(1)
            .expect("inner hub device should be attached");
        control_no_data_dev(
            inner_hub_dev,
            SetupPacket {
                bm_request_type: 0x00,
                b_request: 0x03, // SET_FEATURE
                w_value: 0x0001, // DEVICE_REMOTE_WAKEUP
                w_index: 0,
                w_length: 0,
            },
        );
    }

    for _ in 0..5 {
        ctrl.tick_1ms(&mut mem);
    }

    let portsc = ctrl.io_read(REG_PORTSC1, 2) as u16;
    assert_ne!(portsc & PORTSC_SUSP, 0, "port should remain suspended");
    assert_eq!(
        portsc & PORTSC_RD,
        0,
        "unexpected Resume Detect from a stale wake request after enabling inner hub remote wake"
    );
    let usbsts = ctrl.io_read(REG_USBSTS, 2) as u16;
    assert_eq!(
        usbsts & USBSTS_RESUMEDETECT,
        0,
        "unexpected UHCI USBSTS.RESUMEDETECT from stale wake request after enabling inner hub remote wake"
    );
    assert!(
        !ctrl.irq_level(),
        "unexpected IRQ from stale wake request after enabling inner hub remote wake"
    );

    // A fresh key event should now propagate remote wakeup through the hub chain.
    keyboard.key_event(0x05, true); // HID usage for KeyB.
    ctrl.tick_1ms(&mut mem);

    let portsc = ctrl.io_read(REG_PORTSC1, 2) as u16;
    assert!(
        portsc & PORTSC_RD != 0,
        "expected Resume Detect after remote wake once inner hub remote wake is enabled"
    );

    let usbsts = ctrl.io_read(REG_USBSTS, 2) as u16;
    assert!(
        usbsts & USBSTS_RESUMEDETECT != 0,
        "expected UHCI USBSTS.RESUMEDETECT after remote wake once inner hub remote wake is enabled"
    );
    assert!(
        ctrl.irq_level(),
        "expected IRQ after remote wake once inner hub remote wake is enabled"
    );
}

#[test]
fn hid_keyboard_remote_wakeup_resumes_selectively_suspended_external_hub_port() {
    const HUB_PORT_STATUS_SUSPEND: u16 = 1 << 2;
    const HUB_PORT_CHANGE_SUSPEND: u16 = 1 << 2;

    let mut ctrl = UhciController::new();

    // Attach an external hub to root port 0.
    ctrl.hub_mut().attach(0, Box::new(UsbHubDevice::new()));
    ctrl.hub_mut().force_enable_for_tests(0);

    // Enumerate/configure the hub: address 0 -> address 1, then SET_CONFIGURATION(1).
    control_no_data(
        &mut ctrl,
        0,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05, // SET_ADDRESS
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09, // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );

    // Attach a keyboard behind downstream hub port 1.
    let keyboard = UsbHidKeyboardHandle::new();
    ctrl.hub_mut()
        .attach_at_path(&[0, 1], Box::new(keyboard.clone()))
        .expect("attach keyboard behind hub port 1");

    // Power and reset the hub port so the keyboard becomes reachable.
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x23, // HostToDevice | Class | Other (port)
            b_request: 0x03,       // SET_FEATURE
            w_value: HUB_PORT_FEATURE_POWER,
            w_index: 1,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x23,
            b_request: 0x03, // SET_FEATURE
            w_value: HUB_PORT_FEATURE_RESET,
            w_index: 1,
            w_length: 0,
        },
    );

    let mut mem = TestMemory::new(0x1000);
    for _ in 0..50 {
        ctrl.tick_1ms(&mut mem);
    }

    // Minimal enumeration/configuration for the keyboard + enable remote wakeup.
    control_no_data(
        &mut ctrl,
        0,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05, // SET_ADDRESS
            w_value: 2,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        2,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09, // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        2,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x03, // SET_FEATURE
            w_value: 0x0001, // DEVICE_REMOTE_WAKEUP
            w_index: 0,
            w_length: 0,
        },
    );
    assert!(keyboard.configured(), "expected keyboard to be configured");

    // Selectively suspend downstream hub port 1.
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x23,
            b_request: 0x03, // SET_FEATURE
            w_value: HUB_PORT_FEATURE_SUSPEND,
            w_index: 1,
            w_length: 0,
        },
    );
    let st = control_in(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0xa3, // DeviceToHost | Class | Other (port)
            b_request: 0x00,       // GET_STATUS
            w_value: 0,
            w_index: 1,
            w_length: 4,
        },
        64,
    );
    let status = u16::from_le_bytes([st[0], st[1]]);
    assert_ne!(
        status & HUB_PORT_STATUS_SUSPEND,
        0,
        "expected port to be suspended"
    );

    // Clear suspend-change so we can observe a fresh edge from remote wake.
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x23,
            b_request: 0x01, // CLEAR_FEATURE
            w_value: HUB_PORT_FEATURE_C_PORT_SUSPEND,
            w_index: 1,
            w_length: 0,
        },
    );

    // Inject a keypress while the downstream port is suspended; the hub should observe the remote
    // wake event and resume the port.
    keyboard.key_event(0x04, true); // HID usage for KeyA.
    ctrl.tick_1ms(&mut mem);

    let st = control_in(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0xa3,
            b_request: 0x00, // GET_STATUS
            w_value: 0,
            w_index: 1,
            w_length: 4,
        },
        64,
    );
    let status = u16::from_le_bytes([st[0], st[1]]);
    let change = u16::from_le_bytes([st[2], st[3]]);
    assert_eq!(
        status & HUB_PORT_STATUS_SUSPEND,
        0,
        "expected port to resume after remote wake"
    );
    assert_ne!(
        change & HUB_PORT_CHANGE_SUSPEND,
        0,
        "expected suspend-change bit to latch after remote wake resume"
    );
}

#[test]
fn hid_keyboard_remote_wakeup_clears_external_hub_port_suspend_when_waking_upstream() {
    const HUB_PORT_STATUS_SUSPEND: u16 = 1 << 2;
    const HUB_PORT_CHANGE_SUSPEND: u16 = 1 << 2;

    let mut ctrl = UhciController::new();

    // Attach an external hub to root port 0.
    ctrl.hub_mut().attach(0, Box::new(UsbHubDevice::new()));
    ctrl.hub_mut().force_enable_for_tests(0);

    // Enable resume interrupts so a Resume Detect event raises IRQ.
    ctrl.io_write(REG_USBINTR, 2, USBINTR_RESUME as u32);

    // Enumerate/configure the hub: address 0 -> address 1, SET_CONFIGURATION(1), and enable hub
    // remote wake.
    control_no_data(
        &mut ctrl,
        0,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05, // SET_ADDRESS
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09, // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x03, // SET_FEATURE
            w_value: 0x0001, // DEVICE_REMOTE_WAKEUP
            w_index: 0,
            w_length: 0,
        },
    );

    // Attach a keyboard behind downstream hub port 1.
    let keyboard = UsbHidKeyboardHandle::new();
    ctrl.hub_mut()
        .attach_at_path(&[0, 1], Box::new(keyboard.clone()))
        .expect("attach keyboard behind hub port 1");

    // Power and reset the hub port so the keyboard becomes reachable.
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x23, // HostToDevice | Class | Other (port)
            b_request: 0x03,       // SET_FEATURE
            w_value: HUB_PORT_FEATURE_POWER,
            w_index: 1,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x23,
            b_request: 0x03, // SET_FEATURE
            w_value: HUB_PORT_FEATURE_RESET,
            w_index: 1,
            w_length: 0,
        },
    );

    let mut mem = TestMemory::new(0x1000);
    for _ in 0..50 {
        ctrl.tick_1ms(&mut mem);
    }

    // Minimal enumeration/configuration for the keyboard + enable remote wakeup.
    control_no_data(
        &mut ctrl,
        0,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05, // SET_ADDRESS
            w_value: 2,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        2,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09, // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        2,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x03, // SET_FEATURE
            w_value: 0x0001, // DEVICE_REMOTE_WAKEUP
            w_index: 0,
            w_length: 0,
        },
    );
    assert!(keyboard.configured(), "expected keyboard to be configured");

    // Selectively suspend downstream hub port 1, then clear suspend-change so we can observe the
    // resume edge driven by remote wake.
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x23,
            b_request: 0x03, // SET_FEATURE
            w_value: HUB_PORT_FEATURE_SUSPEND,
            w_index: 1,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            bm_request_type: 0x23,
            b_request: 0x01, // CLEAR_FEATURE
            w_value: HUB_PORT_FEATURE_C_PORT_SUSPEND,
            w_index: 1,
            w_length: 0,
        },
    );

    // Suspend the *root* port into upstream suspend.
    let cur_portsc = ctrl.io_read(REG_PORTSC1, 2) as u16;
    ctrl.io_write(
        REG_PORTSC1,
        2,
        (cur_portsc | PORTSC_PED | PORTSC_SUSP) as u32,
    );

    // Trigger remote wake via the keyboard.
    keyboard.key_event(0x04, true); // HID usage for KeyA.
    ctrl.tick_1ms(&mut mem);

    let portsc = ctrl.io_read(REG_PORTSC1, 2) as u16;
    assert_ne!(
        portsc & PORTSC_RD,
        0,
        "expected Resume Detect after remote wake"
    );

    let usbsts = ctrl.io_read(REG_USBSTS, 2) as u16;
    assert_ne!(
        usbsts & USBSTS_RESUMEDETECT,
        0,
        "expected USBSTS.RESUMEDETECT to latch from Resume Detect"
    );

    // The hub should also clear the downstream port's selective suspend state so the device will be
    // active once the upstream link resumes.
    {
        let mut hub_dev = ctrl
            .hub_mut()
            .port_device_mut(0)
            .expect("hub device should be attached");
        let st = control_in_dev(
            &mut hub_dev,
            SetupPacket {
                bm_request_type: 0xa3, // DeviceToHost | Class | Other (port)
                b_request: 0x00,       // GET_STATUS
                w_value: 0,
                w_index: 1,
                w_length: 4,
            },
            64,
        );
        assert_eq!(st.len(), 4);
        let status = u16::from_le_bytes([st[0], st[1]]);
        let change = u16::from_le_bytes([st[2], st[3]]);
        assert_eq!(
            status & HUB_PORT_STATUS_SUSPEND,
            0,
            "expected hub port suspend bit to clear on remote wake"
        );
        assert_ne!(
            change & HUB_PORT_CHANGE_SUSPEND,
            0,
            "expected hub C_PORT_SUSPEND to latch when remote wake resumes the port"
        );
    }
}
