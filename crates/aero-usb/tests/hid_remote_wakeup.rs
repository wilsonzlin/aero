use aero_usb::hid::keyboard::UsbHidKeyboardHandle;
use aero_usb::usb2_port::Usb2PortMux;
use aero_usb::uhci::regs::{
    REG_PORTSC1, REG_USBINTR, REG_USBSTS, USBINTR_RESUME, USBSTS_RESUMEDETECT,
};
use aero_usb::uhci::UhciController;
use aero_usb::{SetupPacket, UsbInResult, UsbOutResult};

mod util;

use util::TestMemory;

use std::cell::RefCell;
use std::rc::Rc;

// UHCI root hub PORTSC bits.
const PORTSC_PED: u16 = 1 << 2;
const PORTSC_RD: u16 = 1 << 6;
const PORTSC_SUSP: u16 = 1 << 12;

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
