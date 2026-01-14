use std::cell::RefCell;
use std::rc::Rc;

use aero_usb::ehci::regs::{
    reg_portsc, CONFIGFLAG_CF, PORTSC_FPR, PORTSC_PO, PORTSC_PP, PORTSC_PR, PORTSC_SUSP,
    REG_CONFIGFLAG,
};
use aero_usb::ehci::EhciController;
use aero_usb::hid::keyboard::UsbHidKeyboardHandle;
use aero_usb::uhci::regs::REG_PORTSC1;
use aero_usb::uhci::UhciController;
use aero_usb::usb2_port::Usb2PortMux;
use aero_usb::{SetupPacket, UsbInResult, UsbOutResult};

mod util;
use util::TestMemory;

// UHCI root hub PORTSC bits.
const UHCI_PORTSC_PED: u16 = 1 << 2;
const UHCI_PORTSC_RD: u16 = 1 << 6;
const UHCI_PORTSC_SUSP: u16 = 1 << 12;

fn control_no_data_uhci(ctrl: &mut UhciController, addr: u8, setup: SetupPacket) {
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

fn control_no_data_ehci(ctrl: &mut EhciController, addr: u8, setup: SetupPacket) {
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
fn usb2_mux_non_owner_uhci_port_writes_do_not_clear_ehci_device_suspend_state() {
    let mux = Rc::new(RefCell::new(Usb2PortMux::new(1)));

    let mut uhci = UhciController::new();
    uhci.hub_mut().attach_usb2_port_mux(0, mux.clone(), 0);

    let mut ehci = EhciController::new_with_port_count(1);
    ehci.hub_mut().attach_usb2_port_mux(0, mux.clone(), 0);

    let keyboard = UsbHidKeyboardHandle::new();
    mux.borrow_mut().attach(0, Box::new(keyboard.clone()));

    // Route the port to EHCI and enable it via the standard reset sequence.
    ehci.mmio_write(REG_CONFIGFLAG, 4, CONFIGFLAG_CF);
    ehci.mmio_write(reg_portsc(0), 4, PORTSC_PP | PORTSC_PR);
    let mut mem = TestMemory::new(0x1000);
    for _ in 0..50 {
        ehci.tick_1ms(&mut mem);
    }

    // Minimal configuration + enable remote wakeup.
    control_no_data_ehci(
        &mut ehci,
        0,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05, // SET_ADDRESS
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data_ehci(
        &mut ehci,
        1,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09, // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data_ehci(
        &mut ehci,
        1,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x03, // SET_FEATURE
            w_value: 0x0001, // DEVICE_REMOTE_WAKEUP
            w_index: 0,
            w_length: 0,
        },
    );

    // Suspend the port.
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    ehci.mmio_write(reg_portsc(0), 4, portsc | PORTSC_SUSP);
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_ne!(portsc & PORTSC_SUSP, 0, "expected EHCI port to be suspended");

    // Non-owner UHCI write while the port is EHCI-owned. This must not affect the shared device's
    // suspended state; otherwise remote wakeup requests will be dropped.
    uhci.io_write(REG_PORTSC1, 2, 0);

    // Inject a keypress after the non-owner write. This should still request remote wakeup.
    keyboard.key_event(0x04, true); // HID usage for KeyA.
    ehci.tick_1ms(&mut mem);

    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_ne!(
        portsc & PORTSC_FPR,
        0,
        "expected EHCI port to enter resume state after remote wakeup"
    );
}

#[test]
fn usb2_mux_non_owner_ehci_port_writes_do_not_clear_uhci_device_suspend_state() {
    let mux = Rc::new(RefCell::new(Usb2PortMux::new(1)));

    let mut uhci = UhciController::new();
    uhci.hub_mut().attach_usb2_port_mux(0, mux.clone(), 0);

    let mut ehci = EhciController::new_with_port_count(1);
    ehci.hub_mut().attach_usb2_port_mux(0, mux.clone(), 0);

    let keyboard = UsbHidKeyboardHandle::new();
    mux.borrow_mut().attach(0, Box::new(keyboard.clone()));

    // Enable the UHCI port so control requests can reach the device.
    uhci.hub_mut().force_enable_for_tests(0);

    // Minimal configuration + enable remote wakeup.
    control_no_data_uhci(
        &mut uhci,
        0,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05, // SET_ADDRESS
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data_uhci(
        &mut uhci,
        1,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09, // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data_uhci(
        &mut uhci,
        1,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x03, // SET_FEATURE
            w_value: 0x0001, // DEVICE_REMOTE_WAKEUP
            w_index: 0,
            w_length: 0,
        },
    );

    // Put the UHCI view into suspend.
    let cur_portsc = uhci.io_read(REG_PORTSC1, 2) as u16;
    uhci.io_write(
        REG_PORTSC1,
        2,
        (cur_portsc | UHCI_PORTSC_PED | UHCI_PORTSC_SUSP) as u32,
    );
    let portsc = uhci.io_read(REG_PORTSC1, 2) as u16;
    assert_ne!(portsc & UHCI_PORTSC_SUSP, 0, "expected UHCI port to be suspended");
    assert_eq!(
        portsc & UHCI_PORTSC_RD,
        0,
        "resume detect must not be asserted before remote wake triggers"
    );

    // Non-owner EHCI write while the port is companion-owned. Preserve PORT_OWNER so the write does
    // not accidentally claim the port for EHCI.
    let ehci_portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_ne!(
        ehci_portsc & PORTSC_PO,
        0,
        "expected EHCI PORT_OWNER set while mux is companion-owned"
    );
    ehci.mmio_write(reg_portsc(0), 4, ehci_portsc);

    // Inject a keypress after the non-owner write. This should still request remote wakeup.
    keyboard.key_event(0x04, true); // HID usage for KeyA.
    let mut mem = TestMemory::new(0x1000);
    uhci.tick_1ms(&mut mem);

    let portsc = uhci.io_read(REG_PORTSC1, 2) as u16;
    assert_ne!(
        portsc & UHCI_PORTSC_RD,
        0,
        "expected UHCI Resume Detect to latch after remote wakeup"
    );
}

