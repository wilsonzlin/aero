use std::cell::RefCell;
use std::rc::Rc;

use aero_usb::hid::keyboard::UsbHidKeyboardHandle;
use aero_usb::uhci::regs::REG_PORTSC1;
use aero_usb::uhci::UhciController;
use aero_usb::usb2_port::Usb2PortMux;
use aero_usb::{SetupPacket, UsbInResult, UsbOutResult};

mod util;

use util::{
    install_frame_list, td_ctrl, td_token, write_qh, write_td, TestMemory, LINK_PTR_T, PORTSC_PR,
    REG_FRBASEADD, REG_USBCMD, USBCMD_RUN,
};

const PID_IN: u8 = 0x69;
const PID_OUT: u8 = 0xe1;
const PID_SETUP: u8 = 0x2d;

const FRAME_LIST_BASE: u32 = 0x1000;
const QH_ADDR: u32 = 0x2000;
const TD0: u32 = 0x3000;
const TD1: u32 = 0x3020;
const TD2: u32 = 0x3040;

const BUF_SETUP: u32 = 0x4000;
const BUF_DATA: u32 = 0x5000;

// EHCI PORTSC bits we care about.
const EHCI_PORT_RESET: u32 = 1 << 8;
const EHCI_PORT_OWNER: u32 = 1 << 13;

fn uhci_reset_root_port(uhci: &mut UhciController, mem: &mut TestMemory) {
    // Prevent the schedule from running while we're just advancing root hub timers.
    uhci.io_write(REG_USBCMD, 2, 0);
    uhci.io_write(REG_PORTSC1, 2, u32::from(PORTSC_PR));
    for _ in 0..50 {
        uhci.tick_1ms(mem);
    }
}

fn uhci_get_device_descriptor(uhci: &mut UhciController, mem: &mut TestMemory) -> Vec<u8> {
    // Program a trivial one-QH schedule and run a single frame.
    install_frame_list(mem, FRAME_LIST_BASE, QH_ADDR);

    mem.write(BUF_SETUP, &[0x80, 0x06, 0x00, 0x01, 0x00, 0x00, 18, 0x00]);

    write_td(
        mem,
        TD0,
        TD1,
        td_ctrl(true, false),
        td_token(PID_SETUP, 0, 0, false, 8),
        BUF_SETUP,
    );
    write_td(
        mem,
        TD1,
        TD2,
        td_ctrl(true, false),
        td_token(PID_IN, 0, 0, true, 18),
        BUF_DATA,
    );
    write_td(
        mem,
        TD2,
        LINK_PTR_T,
        td_ctrl(true, true),
        td_token(PID_OUT, 0, 0, true, 0),
        0,
    );

    write_qh(mem, QH_ADDR, LINK_PTR_T, TD0);

    uhci.io_write(REG_FRBASEADD, 4, FRAME_LIST_BASE);
    uhci.io_write(REG_USBCMD, 2, u32::from(USBCMD_RUN));

    uhci.tick_1ms(mem);

    // Stop the schedule so subsequent port resets don't process stale TD chains.
    uhci.io_write(REG_USBCMD, 2, 0);

    mem.data[BUF_DATA as usize..BUF_DATA as usize + 18].to_vec()
}

fn ehci_reset_port(mux: &Rc<RefCell<Usb2PortMux>>, port: usize) {
    mux.borrow_mut()
        .ehci_write_portsc_masked(port, EHCI_PORT_RESET, EHCI_PORT_RESET);
    for _ in 0..50 {
        mux.borrow_mut().ehci_tick_1ms(port);
    }
}

fn ehci_get_device_descriptor(mux: &Rc<RefCell<Usb2PortMux>>) -> Option<Vec<u8>> {
    let mut mux_ref = mux.borrow_mut();
    let dev = mux_ref.ehci_device_mut_for_address(0)?;
    let setup = SetupPacket {
        bm_request_type: 0x80,
        b_request: 0x06,
        w_value: 0x0100,
        w_index: 0,
        w_length: 18,
    };
    assert_eq!(dev.handle_setup(setup), UsbOutResult::Ack);
    let data = match dev.handle_in(0, 64) {
        UsbInResult::Data(data) => data,
        other => panic!("expected DATA stage to complete, got {other:?}"),
    };
    assert_eq!(dev.handle_out(0, &[]), UsbOutResult::Ack);
    Some(data)
}

#[test]
fn usb2_companion_routing_swaps_reachability_between_uhci_and_ehci() {
    let mux = Rc::new(RefCell::new(Usb2PortMux::new(1)));

    let mut uhci = UhciController::new();
    uhci.hub_mut().attach_usb2_port_mux(0, mux.clone(), 0);

    // Attach a full-speed device to the shared physical port while the mux is still routing to the
    // companion controller (CONFIGFLAG=0).
    let keyboard = UsbHidKeyboardHandle::new();
    mux.borrow_mut().attach(0, Box::new(keyboard.clone()));

    let mut mem = TestMemory::new(0x20_000);
    uhci_reset_root_port(&mut uhci, &mut mem);

    let desc = uhci_get_device_descriptor(&mut uhci, &mut mem);
    let expected = [
        0x12, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00, 0x40, 0x34, 0x12, 0x01, 0x00, 0x00, 0x01, 0x01,
        0x02, 0x00, 0x01,
    ];
    assert_eq!(desc, expected);

    // EHCI should not be able to route to the device while CONFIGFLAG=0.
    assert!(mux.borrow_mut().ehci_device_mut_for_address(0).is_none());

    // Now let the guest claim the port for EHCI: set CONFIGFLAG and clear PORT_OWNER.
    mux.borrow_mut().set_configflag(true);
    mux.borrow_mut()
        .ehci_write_portsc_masked(0, 0, EHCI_PORT_OWNER);

    // UHCI view should no longer be able to reach the device.
    assert!(uhci.hub_mut().device_mut_for_address(0).is_none());

    // Reset the port through the EHCI view and ensure a control transfer succeeds.
    ehci_reset_port(&mux, 0);
    let ehci_desc = ehci_get_device_descriptor(&mux).expect("device should be reachable via EHCI");
    assert_eq!(ehci_desc, expected);

    // Stress: toggle ownership back and forth a few times and ensure both sides can make forward
    // progress without panicking.
    for i in 0..3 {
        // Hand back to companion.
        mux.borrow_mut()
            .ehci_write_portsc_masked(0, EHCI_PORT_OWNER, EHCI_PORT_OWNER);
        uhci_reset_root_port(&mut uhci, &mut mem);
        let desc = uhci_get_device_descriptor(&mut uhci, &mut mem);
        assert_eq!(desc, expected, "UHCI descriptor mismatch after toggle {i}");

        // And back to EHCI.
        mux.borrow_mut()
            .ehci_write_portsc_masked(0, 0, EHCI_PORT_OWNER);
        ehci_reset_port(&mux, 0);
        let desc = ehci_get_device_descriptor(&mux).expect("EHCI descriptor after toggle");
        assert_eq!(desc, expected, "EHCI descriptor mismatch after toggle {i}");
    }
}
