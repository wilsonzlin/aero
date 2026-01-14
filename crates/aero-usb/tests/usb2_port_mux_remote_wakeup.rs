use std::cell::RefCell;
use std::rc::Rc;

use aero_usb::ehci::regs::{
    reg_portsc, CONFIGFLAG_CF, PORTSC_FPR, PORTSC_LS_MASK, PORTSC_PP, PORTSC_PR, PORTSC_SUSP,
    REG_CONFIGFLAG,
};
use aero_usb::ehci::EhciController;
use aero_usb::hid::keyboard::UsbHidKeyboardHandle;
use aero_usb::usb2_port::Usb2PortMux;
use aero_usb::{SetupPacket, UsbInResult, UsbOutResult};

mod util;

use util::TestMemory;

fn control_no_data(ehci: &mut EhciController, addr: u8, setup: SetupPacket) {
    let mut dev = ehci
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
fn usb2_port_mux_ehci_remote_wakeup_enters_resume_state() {
    let mux = Rc::new(RefCell::new(Usb2PortMux::new(1)));
    let mut ehci = EhciController::new_with_port_count(1);
    ehci.hub_mut().attach_usb2_port_mux(0, mux.clone(), 0);

    let keyboard = UsbHidKeyboardHandle::new();
    mux.borrow_mut().attach(0, Box::new(keyboard.clone()));

    // Claim ports for EHCI (clears PORT_OWNER).
    ehci.mmio_write(REG_CONFIGFLAG, 4, CONFIGFLAG_CF);

    // Reset the port to enable it.
    ehci.mmio_write(reg_portsc(0), 4, PORTSC_PP | PORTSC_PR);
    let mut mem = TestMemory::new(0x1000);
    for _ in 0..50 {
        ehci.tick_1ms(&mut mem);
    }

    // Minimal configuration + enable remote wakeup.
    control_no_data(
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
    control_no_data(
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
    control_no_data(
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
    assert!(keyboard.configured(), "expected keyboard to be configured");

    // Suspend the port.
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    ehci.mmio_write(reg_portsc(0), 4, portsc | PORTSC_SUSP);
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_ne!(portsc & PORTSC_SUSP, 0, "expected port to be suspended");

    // Inject a keypress while suspended. This should request remote wakeup.
    keyboard.key_event(0x04, true); // HID usage for KeyA.

    // Tick once to allow the root hub to observe the remote wakeup request.
    ehci.tick_1ms(&mut mem);

    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_ne!(
        portsc & PORTSC_FPR,
        0,
        "expected muxed EHCI port to enter resume state after remote wakeup"
    );
    assert_eq!(
        portsc & PORTSC_LS_MASK,
        0b01 << 10,
        "expected K-state while resuming"
    );

    // After the resume timer expires, the port should exit suspend/resume and return to J state.
    for _ in 0..20 {
        ehci.tick_1ms(&mut mem);
    }
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_eq!(portsc & (PORTSC_SUSP | PORTSC_FPR), 0);
    assert_eq!(portsc & PORTSC_LS_MASK, 0b10 << 10);

    // The device should be reachable again after resume.
    assert!(ehci.hub_mut().device_mut_for_address(1).is_some());
}

