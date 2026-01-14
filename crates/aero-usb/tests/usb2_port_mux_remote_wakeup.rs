use std::cell::RefCell;
use std::rc::Rc;

use aero_usb::device::AttachedUsbDevice;
use aero_usb::ehci::regs::{
    reg_portsc, CONFIGFLAG_CF, PORTSC_FPR, PORTSC_LS_MASK, PORTSC_PP, PORTSC_PR, PORTSC_SUSP,
    REG_CONFIGFLAG,
};
use aero_usb::ehci::EhciController;
use aero_usb::hid::keyboard::UsbHidKeyboardHandle;
use aero_usb::hub::UsbHubDevice;
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

fn control_no_data_dev(dev: &mut AttachedUsbDevice, setup: SetupPacket) {
    assert_eq!(dev.handle_setup(setup), UsbOutResult::Ack);
    assert!(
        matches!(dev.handle_in(0, 0), UsbInResult::Data(data) if data.is_empty()),
        "expected ACK for status stage"
    );
}

fn control_in(
    ehci: &mut EhciController,
    addr: u8,
    setup: SetupPacket,
    max_packet: usize,
) -> Vec<u8> {
    let mut dev = ehci
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

// Hub-class port features (USB 2.0 spec 11.24.2.7).
const HUB_PORT_FEATURE_SUSPEND: u16 = 2;
const HUB_PORT_FEATURE_RESET: u16 = 4;
const HUB_PORT_FEATURE_POWER: u16 = 8;
const HUB_PORT_FEATURE_C_PORT_SUSPEND: u16 = 18;

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

#[test]
fn usb2_port_mux_ehci_remote_wakeup_enters_resume_state_through_external_hub() {
    let mux = Rc::new(RefCell::new(Usb2PortMux::new(1)));
    let mut ehci = EhciController::new_with_port_count(1);
    ehci.hub_mut().attach_usb2_port_mux(0, mux.clone(), 0);
    ehci.hub_mut().attach(0, Box::new(UsbHubDevice::new()));

    // Claim ports for EHCI (clears PORT_OWNER).
    ehci.mmio_write(REG_CONFIGFLAG, 4, CONFIGFLAG_CF);

    // Reset the port to enable it.
    ehci.mmio_write(reg_portsc(0), 4, PORTSC_PP | PORTSC_PR);
    let mut mem = TestMemory::new(0x1000);
    for _ in 0..50 {
        ehci.tick_1ms(&mut mem);
    }

    // Enumerate/configure the hub itself: address 0 -> address 1, then SET_CONFIGURATION(1).
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
    // Enable DEVICE_REMOTE_WAKEUP on the hub itself so it can propagate downstream remote wake
    // requests upstream while suspended.
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

    // Hotplug a keyboard behind hub port 1.
    let keyboard = UsbHidKeyboardHandle::new();
    ehci.hub_mut()
        .attach_at_path(&[0, 1], Box::new(keyboard.clone()))
        .expect("attach keyboard behind hub port 1");

    // Power+reset the hub port so the keyboard becomes reachable.
    control_no_data(
        &mut ehci,
        1,
        SetupPacket {
            bm_request_type: 0x23, // HostToDevice | Class | Other
            b_request: 0x03,       // SET_FEATURE
            w_value: HUB_PORT_FEATURE_POWER,
            w_index: 1,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ehci,
        1,
        SetupPacket {
            bm_request_type: 0x23, // HostToDevice | Class | Other
            b_request: 0x03,       // SET_FEATURE
            w_value: HUB_PORT_FEATURE_RESET,
            w_index: 1,
            w_length: 0,
        },
    );
    for _ in 0..50 {
        ehci.tick_1ms(&mut mem);
    }

    // Minimal configuration + enable remote wakeup on the downstream keyboard.
    control_no_data(
        &mut ehci,
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
        &mut ehci,
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
        &mut ehci,
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

    // Suspend the root port (this should also suspend the hub + keyboard).
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    ehci.mmio_write(reg_portsc(0), 4, portsc | PORTSC_SUSP);
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_ne!(
        portsc & PORTSC_SUSP,
        0,
        "expected muxed EHCI port to be suspended"
    );

    // Inject a keypress while suspended. This should request remote wakeup.
    keyboard.key_event(0x04, true); // HID usage for KeyA.

    // Tick once to allow the root hub to observe the remote wakeup request.
    ehci.tick_1ms(&mut mem);

    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_ne!(
        portsc & PORTSC_FPR,
        0,
        "expected muxed EHCI port to enter resume state after remote wakeup through hub"
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
    assert!(ehci.hub_mut().device_mut_for_address(2).is_some());
}

#[test]
fn usb2_port_mux_ehci_remote_wakeup_does_not_propagate_through_external_hub_without_hub_remote_wakeup(
) {
    let mux = Rc::new(RefCell::new(Usb2PortMux::new(1)));
    let mut ehci = EhciController::new_with_port_count(1);
    ehci.hub_mut().attach_usb2_port_mux(0, mux.clone(), 0);
    ehci.hub_mut().attach(0, Box::new(UsbHubDevice::new()));

    // Claim ports for EHCI (clears PORT_OWNER).
    ehci.mmio_write(REG_CONFIGFLAG, 4, CONFIGFLAG_CF);

    // Reset the port to enable it.
    ehci.mmio_write(reg_portsc(0), 4, PORTSC_PP | PORTSC_PR);
    let mut mem = TestMemory::new(0x1000);
    for _ in 0..50 {
        ehci.tick_1ms(&mut mem);
    }

    // Enumerate/configure the hub itself: address 0 -> address 1, then SET_CONFIGURATION(1).
    //
    // Note: we intentionally do *not* enable DEVICE_REMOTE_WAKEUP on the hub.
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

    // Hotplug a keyboard behind hub port 1.
    let keyboard = UsbHidKeyboardHandle::new();
    ehci.hub_mut()
        .attach_at_path(&[0, 1], Box::new(keyboard.clone()))
        .expect("attach keyboard behind hub port 1");

    // Power+reset the hub port so the keyboard becomes reachable.
    control_no_data(
        &mut ehci,
        1,
        SetupPacket {
            bm_request_type: 0x23, // HostToDevice | Class | Other
            b_request: 0x03,       // SET_FEATURE
            w_value: HUB_PORT_FEATURE_POWER,
            w_index: 1,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ehci,
        1,
        SetupPacket {
            bm_request_type: 0x23, // HostToDevice | Class | Other
            b_request: 0x03,       // SET_FEATURE
            w_value: HUB_PORT_FEATURE_RESET,
            w_index: 1,
            w_length: 0,
        },
    );
    for _ in 0..50 {
        ehci.tick_1ms(&mut mem);
    }

    // Minimal configuration + enable remote wakeup on the downstream keyboard.
    control_no_data(
        &mut ehci,
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
        &mut ehci,
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
        &mut ehci,
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

    // Suspend the root port (this should also suspend the external hub + keyboard).
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    ehci.mmio_write(reg_portsc(0), 4, portsc | PORTSC_SUSP);
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_ne!(
        portsc & PORTSC_SUSP,
        0,
        "expected muxed EHCI port to be suspended"
    );

    // Inject a keypress while suspended. Since the hub has not enabled DEVICE_REMOTE_WAKEUP, it
    // must not propagate the downstream remote wake request upstream.
    keyboard.key_event(0x04, true); // HID usage for KeyA.

    for _ in 0..5 {
        ehci.tick_1ms(&mut mem);
    }

    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_eq!(
        portsc & PORTSC_FPR,
        0,
        "unexpected EHCI resume state even though hub remote wake is disabled"
    );
    assert_ne!(portsc & PORTSC_SUSP, 0, "port should remain suspended");
    assert_eq!(portsc & PORTSC_LS_MASK, 0b10 << 10, "expected J-state");
    assert!(
        ehci.hub_mut().device_mut_for_address(2).is_none(),
        "device should not be reachable while the root port remains suspended"
    );

    // Enable DEVICE_REMOTE_WAKEUP on the hub *after* the downstream device has already requested
    // remote wake. The hub should have drained the wake request even while propagation was
    // disabled, so enabling remote wake later must not "replay" a stale wake event.
    {
        let mut hub_dev = ehci
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
        ehci.tick_1ms(&mut mem);
    }

    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_eq!(
        portsc & PORTSC_FPR,
        0,
        "unexpected EHCI resume state from a stale wake request after enabling hub remote wake"
    );
    assert_ne!(portsc & PORTSC_SUSP, 0, "port should remain suspended");
    assert_eq!(portsc & PORTSC_LS_MASK, 0b10 << 10, "expected J-state");
    assert!(
        ehci.hub_mut().device_mut_for_address(2).is_none(),
        "device should remain unreachable while the root port remains suspended"
    );

    // A fresh key event should now propagate remote wakeup through the hub.
    keyboard.key_event(0x05, true); // HID usage for KeyB.
    ehci.tick_1ms(&mut mem);

    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_ne!(
        portsc & PORTSC_FPR,
        0,
        "expected muxed EHCI port to enter resume state after remote wake once hub remote wake is enabled"
    );
    assert_eq!(portsc & PORTSC_LS_MASK, 0b01 << 10, "expected K-state");

    // Let the resume timer expire; the port should exit suspend/resume and return to J state.
    for _ in 0..20 {
        ehci.tick_1ms(&mut mem);
    }
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_eq!(portsc & (PORTSC_SUSP | PORTSC_FPR), 0);
    assert_eq!(portsc & PORTSC_LS_MASK, 0b10 << 10);
    assert!(
        ehci.hub_mut().device_mut_for_address(2).is_some(),
        "device should be reachable after remote wake resumes the port"
    );
}

#[test]
fn usb2_port_mux_ehci_remote_wakeup_enters_resume_state_through_nested_hubs() {
    let mux = Rc::new(RefCell::new(Usb2PortMux::new(1)));
    let mut ehci = EhciController::new_with_port_count(1);
    ehci.hub_mut().attach_usb2_port_mux(0, mux.clone(), 0);
    ehci.hub_mut().attach(0, Box::new(UsbHubDevice::new()));

    // Claim ports for EHCI (clears PORT_OWNER).
    ehci.mmio_write(REG_CONFIGFLAG, 4, CONFIGFLAG_CF);

    // Reset the root port to enable it.
    ehci.mmio_write(reg_portsc(0), 4, PORTSC_PP | PORTSC_PR);
    let mut mem = TestMemory::new(0x1000);
    for _ in 0..50 {
        ehci.tick_1ms(&mut mem);
    }

    // Enumerate/configure the outer hub: address 0 -> address 1, then SET_CONFIGURATION(1).
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
    // Enable DEVICE_REMOTE_WAKEUP on the outer hub so it can propagate downstream remote wakeup
    // requests upstream.
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

    // Attach an inner hub behind outer-hub port 1.
    ehci.hub_mut()
        .attach_at_path(&[0, 1], Box::new(UsbHubDevice::new()))
        .expect("attach inner hub behind outer hub port 1");

    // Power+reset the outer hub port so the inner hub becomes reachable.
    control_no_data(
        &mut ehci,
        1,
        SetupPacket {
            bm_request_type: 0x23, // HostToDevice | Class | Other
            b_request: 0x03,       // SET_FEATURE
            w_value: HUB_PORT_FEATURE_POWER,
            w_index: 1,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ehci,
        1,
        SetupPacket {
            bm_request_type: 0x23, // HostToDevice | Class | Other
            b_request: 0x03,       // SET_FEATURE
            w_value: HUB_PORT_FEATURE_RESET,
            w_index: 1,
            w_length: 0,
        },
    );
    for _ in 0..50 {
        ehci.tick_1ms(&mut mem);
    }

    // Enumerate/configure the inner hub: address 0 -> address 2, then SET_CONFIGURATION(1).
    control_no_data(
        &mut ehci,
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
        &mut ehci,
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
        &mut ehci,
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
    ehci.hub_mut()
        .attach_at_path(&[0, 1, 1], Box::new(keyboard.clone()))
        .expect("attach keyboard behind inner hub port 1");

    // Power+reset the inner hub port so the keyboard becomes reachable.
    control_no_data(
        &mut ehci,
        2,
        SetupPacket {
            bm_request_type: 0x23, // HostToDevice | Class | Other
            b_request: 0x03,       // SET_FEATURE
            w_value: HUB_PORT_FEATURE_POWER,
            w_index: 1,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ehci,
        2,
        SetupPacket {
            bm_request_type: 0x23, // HostToDevice | Class | Other
            b_request: 0x03,       // SET_FEATURE
            w_value: HUB_PORT_FEATURE_RESET,
            w_index: 1,
            w_length: 0,
        },
    );
    for _ in 0..50 {
        ehci.tick_1ms(&mut mem);
    }

    // Minimal configuration + enable remote wakeup on the downstream keyboard.
    control_no_data(
        &mut ehci,
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
        &mut ehci,
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
        &mut ehci,
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

    // Suspend the root port (this should also suspend the hub chain + keyboard).
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    ehci.mmio_write(reg_portsc(0), 4, portsc | PORTSC_SUSP);
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_ne!(
        portsc & PORTSC_SUSP,
        0,
        "expected root port to be suspended"
    );

    // Inject a keypress while suspended. This should request remote wakeup via the hub chain.
    keyboard.key_event(0x04, true); // HID usage for KeyA.

    // Tick once to allow the root hub to observe the remote wakeup request.
    ehci.tick_1ms(&mut mem);

    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_ne!(
        portsc & PORTSC_FPR,
        0,
        "expected muxed EHCI port to enter resume state after remote wakeup through nested hubs"
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
    assert!(ehci.hub_mut().device_mut_for_address(3).is_some());
}

#[test]
fn usb2_port_mux_ehci_remote_wakeup_does_not_propagate_through_nested_hubs_without_inner_hub_remote_wakeup(
) {
    let mux = Rc::new(RefCell::new(Usb2PortMux::new(1)));
    let mut ehci = EhciController::new_with_port_count(1);
    ehci.hub_mut().attach_usb2_port_mux(0, mux.clone(), 0);
    ehci.hub_mut().attach(0, Box::new(UsbHubDevice::new()));

    // Claim ports for EHCI (clears PORT_OWNER).
    ehci.mmio_write(REG_CONFIGFLAG, 4, CONFIGFLAG_CF);

    // Reset the root port to enable it.
    ehci.mmio_write(reg_portsc(0), 4, PORTSC_PP | PORTSC_PR);
    let mut mem = TestMemory::new(0x1000);
    for _ in 0..50 {
        ehci.tick_1ms(&mut mem);
    }

    // Enumerate/configure the outer hub: address 0 -> address 1, then SET_CONFIGURATION(1).
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
    // Enable DEVICE_REMOTE_WAKEUP on the outer hub only (the inner hub remains at the default
    // disabled state in this test).
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

    // Attach an inner hub behind outer-hub port 1.
    ehci.hub_mut()
        .attach_at_path(&[0, 1], Box::new(UsbHubDevice::new()))
        .expect("attach inner hub behind outer hub port 1");

    // Power+reset the outer hub port so the inner hub becomes reachable.
    control_no_data(
        &mut ehci,
        1,
        SetupPacket {
            bm_request_type: 0x23, // HostToDevice | Class | Other
            b_request: 0x03,       // SET_FEATURE
            w_value: HUB_PORT_FEATURE_POWER,
            w_index: 1,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ehci,
        1,
        SetupPacket {
            bm_request_type: 0x23, // HostToDevice | Class | Other
            b_request: 0x03,       // SET_FEATURE
            w_value: HUB_PORT_FEATURE_RESET,
            w_index: 1,
            w_length: 0,
        },
    );
    for _ in 0..50 {
        ehci.tick_1ms(&mut mem);
    }

    // Enumerate/configure the inner hub: address 0 -> address 2, then SET_CONFIGURATION(1).
    //
    // Note: we intentionally do *not* enable DEVICE_REMOTE_WAKEUP on the inner hub.
    control_no_data(
        &mut ehci,
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
        &mut ehci,
        2,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09, // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    // Sanity: the inner hub should still report DEVICE_REMOTE_WAKEUP disabled (bit1 clear).
    let status = control_in(
        &mut ehci,
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
    ehci.hub_mut()
        .attach_at_path(&[0, 1, 1], Box::new(keyboard.clone()))
        .expect("attach keyboard behind inner hub port 1");

    // Power+reset the inner hub port so the keyboard becomes reachable.
    control_no_data(
        &mut ehci,
        2,
        SetupPacket {
            bm_request_type: 0x23, // HostToDevice | Class | Other
            b_request: 0x03,       // SET_FEATURE
            w_value: HUB_PORT_FEATURE_POWER,
            w_index: 1,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ehci,
        2,
        SetupPacket {
            bm_request_type: 0x23, // HostToDevice | Class | Other
            b_request: 0x03,       // SET_FEATURE
            w_value: HUB_PORT_FEATURE_RESET,
            w_index: 1,
            w_length: 0,
        },
    );
    for _ in 0..50 {
        ehci.tick_1ms(&mut mem);
    }

    // Minimal configuration + enable remote wakeup on the downstream keyboard.
    control_no_data(
        &mut ehci,
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
        &mut ehci,
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
        &mut ehci,
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

    // Suspend the root port (this should also suspend the hub chain + keyboard).
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    ehci.mmio_write(reg_portsc(0), 4, portsc | PORTSC_SUSP);
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_ne!(
        portsc & PORTSC_SUSP,
        0,
        "expected root port to be suspended"
    );

    // Inject a keypress while suspended. Since the inner hub has not enabled DEVICE_REMOTE_WAKEUP,
    // it must not propagate the downstream remote wake request upstream.
    keyboard.key_event(0x04, true); // HID usage for KeyA.

    for _ in 0..5 {
        ehci.tick_1ms(&mut mem);
    }

    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_eq!(
        portsc & PORTSC_FPR,
        0,
        "unexpected resume state even though inner hub remote wake is disabled"
    );
    assert_ne!(portsc & PORTSC_SUSP, 0, "port should remain suspended");
    assert_eq!(portsc & PORTSC_LS_MASK, 0b10 << 10, "expected J-state");
    assert!(ehci.hub_mut().device_mut_for_address(3).is_none());

    // Enable DEVICE_REMOTE_WAKEUP on the inner hub *after* the downstream device has already
    // requested remote wake. The inner hub should have drained the wake request even while
    // propagation was disabled, so enabling remote wake later must not "replay" a stale wake event.
    {
        let mut outer_hub_dev = ehci
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
        ehci.tick_1ms(&mut mem);
    }

    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_eq!(
        portsc & PORTSC_FPR,
        0,
        "unexpected resume state from a stale wake request after enabling inner hub remote wake"
    );
    assert_ne!(portsc & PORTSC_SUSP, 0, "port should remain suspended");
    assert_eq!(portsc & PORTSC_LS_MASK, 0b10 << 10, "expected J-state");
    assert!(
        ehci.hub_mut().device_mut_for_address(3).is_none(),
        "device should remain unreachable while the root port remains suspended"
    );

    // A fresh key event should now propagate remote wakeup through the hub chain.
    keyboard.key_event(0x05, true); // HID usage for KeyB.
    ehci.tick_1ms(&mut mem);

    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_ne!(
        portsc & PORTSC_FPR,
        0,
        "expected resume state after remote wake once inner hub remote wake is enabled"
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
    assert!(
        ehci.hub_mut().device_mut_for_address(3).is_some(),
        "device should be reachable after remote wake resumes the port"
    );
}

#[test]
fn ehci_remote_wakeup_enters_resume_state_through_external_hub() {
    let mut ehci = EhciController::new_with_port_count(1);
    ehci.hub_mut().attach(0, Box::new(UsbHubDevice::new()));

    // Claim ports for EHCI (clears PORT_OWNER).
    ehci.mmio_write(REG_CONFIGFLAG, 4, CONFIGFLAG_CF);

    // Reset the root port to enable it.
    ehci.mmio_write(reg_portsc(0), 4, PORTSC_PP | PORTSC_PR);
    let mut mem = TestMemory::new(0x1000);
    for _ in 0..50 {
        ehci.tick_1ms(&mut mem);
    }

    // Enumerate/configure the hub itself: address 0 -> address 1, then SET_CONFIGURATION(1).
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
    // Enable DEVICE_REMOTE_WAKEUP on the hub itself so it can propagate downstream remote wake
    // requests upstream while suspended.
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

    // Hotplug a keyboard behind hub port 1.
    let keyboard = UsbHidKeyboardHandle::new();
    ehci.hub_mut()
        .attach_at_path(&[0, 1], Box::new(keyboard.clone()))
        .expect("attach keyboard behind hub port 1");

    // Power+reset the hub port so the keyboard becomes reachable.
    control_no_data(
        &mut ehci,
        1,
        SetupPacket {
            bm_request_type: 0x23, // HostToDevice | Class | Other
            b_request: 0x03,       // SET_FEATURE
            w_value: HUB_PORT_FEATURE_POWER,
            w_index: 1,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ehci,
        1,
        SetupPacket {
            bm_request_type: 0x23, // HostToDevice | Class | Other
            b_request: 0x03,       // SET_FEATURE
            w_value: HUB_PORT_FEATURE_RESET,
            w_index: 1,
            w_length: 0,
        },
    );
    for _ in 0..50 {
        ehci.tick_1ms(&mut mem);
    }

    // Minimal configuration + enable remote wakeup on the downstream keyboard.
    control_no_data(
        &mut ehci,
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
        &mut ehci,
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
        &mut ehci,
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

    // Suspend the root port (this should also suspend the external hub + keyboard).
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    ehci.mmio_write(reg_portsc(0), 4, portsc | PORTSC_SUSP);
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_ne!(
        portsc & PORTSC_SUSP,
        0,
        "expected root port to be suspended"
    );

    // Inject a keypress while suspended. This should request remote wakeup.
    keyboard.key_event(0x04, true); // HID usage for KeyA.

    // Tick once to allow the root hub to observe the remote wakeup request.
    ehci.tick_1ms(&mut mem);

    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_ne!(
        portsc & PORTSC_FPR,
        0,
        "expected EHCI root port to enter resume state after remote wakeup through hub"
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
    assert!(ehci.hub_mut().device_mut_for_address(2).is_some());
}

#[test]
fn ehci_remote_wakeup_clears_external_hub_port_suspend_when_waking_upstream() {
    const HUB_PORT_STATUS_SUSPEND: u16 = 1 << 2;
    const HUB_PORT_CHANGE_SUSPEND: u16 = 1 << 2;

    let mut ehci = EhciController::new_with_port_count(1);
    ehci.hub_mut().attach(0, Box::new(UsbHubDevice::new()));

    // Claim ports for EHCI (clears PORT_OWNER) and reset the root port to enable it.
    ehci.mmio_write(REG_CONFIGFLAG, 4, CONFIGFLAG_CF);
    ehci.mmio_write(reg_portsc(0), 4, PORTSC_PP | PORTSC_PR);
    let mut mem = TestMemory::new(0x1000);
    for _ in 0..50 {
        ehci.tick_1ms(&mut mem);
    }

    // Enumerate/configure the hub: address 0 -> address 1, SET_CONFIGURATION(1), and enable hub
    // remote wake.
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

    // Hotplug a keyboard behind hub port 1.
    let keyboard = UsbHidKeyboardHandle::new();
    ehci.hub_mut()
        .attach_at_path(&[0, 1], Box::new(keyboard.clone()))
        .expect("attach keyboard behind hub port 1");

    // Power+reset the hub port so the keyboard becomes reachable.
    control_no_data(
        &mut ehci,
        1,
        SetupPacket {
            bm_request_type: 0x23, // HostToDevice | Class | Other
            b_request: 0x03,       // SET_FEATURE
            w_value: HUB_PORT_FEATURE_POWER,
            w_index: 1,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ehci,
        1,
        SetupPacket {
            bm_request_type: 0x23, // HostToDevice | Class | Other
            b_request: 0x03,       // SET_FEATURE
            w_value: HUB_PORT_FEATURE_RESET,
            w_index: 1,
            w_length: 0,
        },
    );
    for _ in 0..50 {
        ehci.tick_1ms(&mut mem);
    }

    // Minimal configuration + enable remote wakeup on the downstream keyboard.
    control_no_data(
        &mut ehci,
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
        &mut ehci,
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
        &mut ehci,
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

    // Selectively suspend hub port 1, then clear suspend-change so we can observe the wake edge.
    control_no_data(
        &mut ehci,
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
        &mut ehci,
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
    assert_eq!(st.len(), 4);
    let status = u16::from_le_bytes([st[0], st[1]]);
    assert_ne!(
        status & HUB_PORT_STATUS_SUSPEND,
        0,
        "expected hub port to be selectively suspended before remote wake"
    );

    control_no_data(
        &mut ehci,
        1,
        SetupPacket {
            bm_request_type: 0x23,
            b_request: 0x01, // CLEAR_FEATURE
            w_value: HUB_PORT_FEATURE_C_PORT_SUSPEND,
            w_index: 1,
            w_length: 0,
        },
    );

    // Suspend the root port into upstream suspend.
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    ehci.mmio_write(reg_portsc(0), 4, portsc | PORTSC_SUSP);

    // Trigger remote wake via the keyboard.
    keyboard.key_event(0x04, true); // HID usage for KeyA.
    ehci.tick_1ms(&mut mem);

    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_ne!(
        portsc & PORTSC_FPR,
        0,
        "expected EHCI root port to enter resume state after remote wake"
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

    // The hub should also clear the downstream port's selective suspend state so the device will be
    // active once the upstream link resumes.
    let st = control_in(
        &mut ehci,
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

    // The device should be reachable again after resume.
    assert!(ehci.hub_mut().device_mut_for_address(2).is_some());
}

#[test]
fn ehci_remote_wakeup_does_not_propagate_through_external_hub_without_hub_remote_wakeup() {
    let mut ehci = EhciController::new_with_port_count(1);
    ehci.hub_mut().attach(0, Box::new(UsbHubDevice::new()));

    // Claim ports for EHCI (clears PORT_OWNER).
    ehci.mmio_write(REG_CONFIGFLAG, 4, CONFIGFLAG_CF);

    // Reset the root port to enable it.
    ehci.mmio_write(reg_portsc(0), 4, PORTSC_PP | PORTSC_PR);
    let mut mem = TestMemory::new(0x1000);
    for _ in 0..50 {
        ehci.tick_1ms(&mut mem);
    }

    // Enumerate/configure the hub itself: address 0 -> address 1, then SET_CONFIGURATION(1).
    //
    // Note: we intentionally do *not* enable DEVICE_REMOTE_WAKEUP on the hub.
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

    // Hotplug a keyboard behind hub port 1.
    let keyboard = UsbHidKeyboardHandle::new();
    ehci.hub_mut()
        .attach_at_path(&[0, 1], Box::new(keyboard.clone()))
        .expect("attach keyboard behind hub port 1");

    // Power+reset the hub port so the keyboard becomes reachable.
    control_no_data(
        &mut ehci,
        1,
        SetupPacket {
            bm_request_type: 0x23, // HostToDevice | Class | Other
            b_request: 0x03,       // SET_FEATURE
            w_value: HUB_PORT_FEATURE_POWER,
            w_index: 1,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ehci,
        1,
        SetupPacket {
            bm_request_type: 0x23, // HostToDevice | Class | Other
            b_request: 0x03,       // SET_FEATURE
            w_value: HUB_PORT_FEATURE_RESET,
            w_index: 1,
            w_length: 0,
        },
    );
    for _ in 0..50 {
        ehci.tick_1ms(&mut mem);
    }

    // Minimal configuration + enable remote wakeup on the downstream keyboard.
    control_no_data(
        &mut ehci,
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
        &mut ehci,
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
        &mut ehci,
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

    // Suspend the root port (this should also suspend the external hub + keyboard).
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    ehci.mmio_write(reg_portsc(0), 4, portsc | PORTSC_SUSP);
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_ne!(
        portsc & PORTSC_SUSP,
        0,
        "expected root port to be suspended"
    );

    // Inject a keypress while suspended. Since the hub has not enabled DEVICE_REMOTE_WAKEUP, it
    // must not propagate the downstream remote wake request upstream.
    keyboard.key_event(0x04, true); // HID usage for KeyA.

    for _ in 0..5 {
        ehci.tick_1ms(&mut mem);
    }

    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_eq!(
        portsc & PORTSC_FPR,
        0,
        "unexpected resume state even though hub remote wake is disabled"
    );
    assert_ne!(portsc & PORTSC_SUSP, 0, "port should remain suspended");
    assert_eq!(portsc & PORTSC_LS_MASK, 0b10 << 10, "expected J-state");
    assert!(ehci.hub_mut().device_mut_for_address(2).is_none());
}

#[test]
fn ehci_remote_wakeup_enters_resume_state_through_nested_hubs() {
    let mut ehci = EhciController::new_with_port_count(1);
    ehci.hub_mut().attach(0, Box::new(UsbHubDevice::new()));

    // Claim ports for EHCI (clears PORT_OWNER).
    ehci.mmio_write(REG_CONFIGFLAG, 4, CONFIGFLAG_CF);

    // Reset the root port to enable it.
    ehci.mmio_write(reg_portsc(0), 4, PORTSC_PP | PORTSC_PR);
    let mut mem = TestMemory::new(0x1000);
    for _ in 0..50 {
        ehci.tick_1ms(&mut mem);
    }

    // Enumerate/configure the outer hub: address 0 -> address 1, then SET_CONFIGURATION(1).
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
    // Enable hub remote wake on the outer hub (exercises hub feature plumbing).
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

    // Attach an inner hub behind outer-hub port 1.
    ehci.hub_mut()
        .attach_at_path(&[0, 1], Box::new(UsbHubDevice::new()))
        .expect("attach inner hub behind outer hub port 1");

    // Power+reset the outer hub port so the inner hub becomes reachable.
    control_no_data(
        &mut ehci,
        1,
        SetupPacket {
            bm_request_type: 0x23, // HostToDevice | Class | Other
            b_request: 0x03,       // SET_FEATURE
            w_value: HUB_PORT_FEATURE_POWER,
            w_index: 1,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ehci,
        1,
        SetupPacket {
            bm_request_type: 0x23, // HostToDevice | Class | Other
            b_request: 0x03,       // SET_FEATURE
            w_value: HUB_PORT_FEATURE_RESET,
            w_index: 1,
            w_length: 0,
        },
    );
    for _ in 0..50 {
        ehci.tick_1ms(&mut mem);
    }

    // Enumerate/configure the inner hub: address 0 -> address 2, then SET_CONFIGURATION(1).
    control_no_data(
        &mut ehci,
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
        &mut ehci,
        2,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09, // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    // Enable hub remote wake on the inner hub (exercises hub feature plumbing).
    control_no_data(
        &mut ehci,
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
    ehci.hub_mut()
        .attach_at_path(&[0, 1, 1], Box::new(keyboard.clone()))
        .expect("attach keyboard behind inner hub port 1");

    // Power+reset the inner hub port so the keyboard becomes reachable.
    control_no_data(
        &mut ehci,
        2,
        SetupPacket {
            bm_request_type: 0x23, // HostToDevice | Class | Other
            b_request: 0x03,       // SET_FEATURE
            w_value: HUB_PORT_FEATURE_POWER,
            w_index: 1,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ehci,
        2,
        SetupPacket {
            bm_request_type: 0x23, // HostToDevice | Class | Other
            b_request: 0x03,       // SET_FEATURE
            w_value: HUB_PORT_FEATURE_RESET,
            w_index: 1,
            w_length: 0,
        },
    );
    for _ in 0..50 {
        ehci.tick_1ms(&mut mem);
    }

    // Minimal configuration + enable remote wakeup on the downstream keyboard.
    control_no_data(
        &mut ehci,
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
        &mut ehci,
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
        &mut ehci,
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

    // Suspend the root port (this should also suspend the hub chain + keyboard).
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    ehci.mmio_write(reg_portsc(0), 4, portsc | PORTSC_SUSP);
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_ne!(
        portsc & PORTSC_SUSP,
        0,
        "expected root port to be suspended"
    );

    // Inject a keypress while suspended. This should request remote wakeup via the hub chain.
    keyboard.key_event(0x04, true); // HID usage for KeyA.

    // Tick once to allow the root hub to observe the remote wakeup request.
    ehci.tick_1ms(&mut mem);

    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_ne!(
        portsc & PORTSC_FPR,
        0,
        "expected EHCI root port to enter resume state after remote wakeup through nested hubs"
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
    assert!(ehci.hub_mut().device_mut_for_address(3).is_some());
}

#[test]
fn ehci_remote_wakeup_does_not_propagate_through_nested_hubs_without_inner_hub_remote_wakeup() {
    let mut ehci = EhciController::new_with_port_count(1);
    ehci.hub_mut().attach(0, Box::new(UsbHubDevice::new()));

    // Claim ports for EHCI (clears PORT_OWNER).
    ehci.mmio_write(REG_CONFIGFLAG, 4, CONFIGFLAG_CF);

    // Reset the root port to enable it.
    ehci.mmio_write(reg_portsc(0), 4, PORTSC_PP | PORTSC_PR);
    let mut mem = TestMemory::new(0x1000);
    for _ in 0..50 {
        ehci.tick_1ms(&mut mem);
    }

    // Enumerate/configure the outer hub: address 0 -> address 1, then SET_CONFIGURATION(1).
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
    // Enable hub remote wake on the outer hub only (inner hub remains disabled).
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

    // Attach an inner hub behind outer-hub port 1.
    ehci.hub_mut()
        .attach_at_path(&[0, 1], Box::new(UsbHubDevice::new()))
        .expect("attach inner hub behind outer hub port 1");

    // Power+reset the outer hub port so the inner hub becomes reachable.
    control_no_data(
        &mut ehci,
        1,
        SetupPacket {
            bm_request_type: 0x23, // HostToDevice | Class | Other
            b_request: 0x03,       // SET_FEATURE
            w_value: HUB_PORT_FEATURE_POWER,
            w_index: 1,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ehci,
        1,
        SetupPacket {
            bm_request_type: 0x23, // HostToDevice | Class | Other
            b_request: 0x03,       // SET_FEATURE
            w_value: HUB_PORT_FEATURE_RESET,
            w_index: 1,
            w_length: 0,
        },
    );
    for _ in 0..50 {
        ehci.tick_1ms(&mut mem);
    }

    // Enumerate/configure the inner hub: address 0 -> address 2, then SET_CONFIGURATION(1).
    //
    // Note: we intentionally do *not* enable DEVICE_REMOTE_WAKEUP on the inner hub.
    control_no_data(
        &mut ehci,
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
        &mut ehci,
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
    ehci.hub_mut()
        .attach_at_path(&[0, 1, 1], Box::new(keyboard.clone()))
        .expect("attach keyboard behind inner hub port 1");

    // Power+reset the inner hub port so the keyboard becomes reachable.
    control_no_data(
        &mut ehci,
        2,
        SetupPacket {
            bm_request_type: 0x23, // HostToDevice | Class | Other
            b_request: 0x03,       // SET_FEATURE
            w_value: HUB_PORT_FEATURE_POWER,
            w_index: 1,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ehci,
        2,
        SetupPacket {
            bm_request_type: 0x23, // HostToDevice | Class | Other
            b_request: 0x03,       // SET_FEATURE
            w_value: HUB_PORT_FEATURE_RESET,
            w_index: 1,
            w_length: 0,
        },
    );
    for _ in 0..50 {
        ehci.tick_1ms(&mut mem);
    }

    // Minimal configuration + enable remote wakeup on the downstream keyboard.
    control_no_data(
        &mut ehci,
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
        &mut ehci,
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
        &mut ehci,
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

    // Suspend the root port (this should also suspend the hub chain + keyboard).
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    ehci.mmio_write(reg_portsc(0), 4, portsc | PORTSC_SUSP);
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_ne!(
        portsc & PORTSC_SUSP,
        0,
        "expected root port to be suspended"
    );

    // Inject a keypress while suspended. Since the inner hub has not enabled DEVICE_REMOTE_WAKEUP,
    // it must not propagate the downstream remote wake request upstream.
    keyboard.key_event(0x04, true); // HID usage for KeyA.

    for _ in 0..5 {
        ehci.tick_1ms(&mut mem);
    }

    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_eq!(
        portsc & PORTSC_FPR,
        0,
        "unexpected resume state even though inner hub remote wake is disabled"
    );
    assert_ne!(portsc & PORTSC_SUSP, 0, "port should remain suspended");
    assert_eq!(portsc & PORTSC_LS_MASK, 0b10 << 10, "expected J-state");
    assert!(ehci.hub_mut().device_mut_for_address(3).is_none());
}
