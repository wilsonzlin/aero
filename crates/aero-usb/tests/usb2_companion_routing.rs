use std::cell::RefCell;
use std::rc::Rc;

use aero_io_snapshot::io::state::IoSnapshot;
use aero_usb::ehci::regs::{
    reg_portsc, CONFIGFLAG_CF, PORTSC_HSP, PORTSC_PED, PORTSC_PO, PORTSC_PR as EHCI_PORTSC_PR,
    PORTSC_SUSP, REG_CONFIGFLAG,
};
use aero_usb::ehci::EhciController;
use aero_usb::hid::keyboard::UsbHidKeyboardHandle;
use aero_usb::uhci::regs::REG_PORTSC1;
use aero_usb::uhci::UhciController;
use aero_usb::usb2_port::Usb2PortMux;
use aero_usb::{ControlResponse, SetupPacket, UsbDeviceModel, UsbInResult, UsbOutResult, UsbSpeed};

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

fn ehci_reset_port(ehci: &mut EhciController, mem: &mut TestMemory, port: usize) {
    ehci.mmio_write(reg_portsc(port), 4, EHCI_PORTSC_PR);
    for _ in 0..50 {
        ehci.tick_1ms(mem);
    }
}

fn ehci_get_device_descriptor(ehci: &mut EhciController) -> Option<Vec<u8>> {
    let mut dev = ehci.hub_mut().device_mut_for_address(0)?;
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

    let mut ehci = EhciController::new_with_port_count(1);
    ehci.hub_mut().attach_usb2_port_mux(0, mux.clone(), 0);

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
    assert!(ehci.hub_mut().device_mut_for_address(0).is_none());

    // Now let the guest claim the port for EHCI via CONFIGFLAG. The controller models the
    // CONFIGFLAG 0->1 transition by clearing PORTSC.PORT_OWNER on all ports.
    ehci.mmio_write(REG_CONFIGFLAG, 4, CONFIGFLAG_CF);
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_eq!(portsc & PORTSC_PO, 0, "CONFIGFLAG should clear PORT_OWNER");

    // UHCI view should no longer be able to reach the device.
    assert!(uhci.hub_mut().device_mut_for_address(0).is_none());

    // Reset the port through the EHCI view and ensure a control transfer succeeds.
    ehci_reset_port(&mut ehci, &mut mem, 0);
    let ehci_desc =
        ehci_get_device_descriptor(&mut ehci).expect("device should be reachable via EHCI");
    assert_eq!(ehci_desc, expected);

    // Stress: toggle ownership back and forth a few times and ensure both sides can make forward
    // progress without panicking.
    for i in 0..3 {
        // Hand back to companion. EHCI requires the port to be disabled before PORT_OWNER writes.
        let portsc = ehci.mmio_read(reg_portsc(0), 4);
        ehci.mmio_write(reg_portsc(0), 4, portsc & !PORTSC_PED);
        let portsc = ehci.mmio_read(reg_portsc(0), 4);
        assert_eq!(
            portsc & PORTSC_PED,
            0,
            "port must be disabled before handoff"
        );
        ehci.mmio_write(reg_portsc(0), 4, portsc | PORTSC_PO);
        uhci_reset_root_port(&mut uhci, &mut mem);
        let desc = uhci_get_device_descriptor(&mut uhci, &mut mem);
        assert_eq!(desc, expected, "UHCI descriptor mismatch after toggle {i}");

        // And back to EHCI.
        let portsc = ehci.mmio_read(reg_portsc(0), 4);
        ehci.mmio_write(reg_portsc(0), 4, portsc & !PORTSC_PO);
        ehci_reset_port(&mut ehci, &mut mem, 0);
        let desc = ehci_get_device_descriptor(&mut ehci).expect("EHCI descriptor after toggle");
        assert_eq!(desc, expected, "EHCI descriptor mismatch after toggle {i}");
    }
}

#[test]
fn usb2_companion_routing_mux_reports_ehci_high_speed_indicator() {
    struct HighSpeedDevice;

    impl UsbDeviceModel for HighSpeedDevice {
        fn speed(&self) -> UsbSpeed {
            UsbSpeed::High
        }

        fn handle_control_request(
            &mut self,
            _setup: SetupPacket,
            _data_stage: Option<&[u8]>,
        ) -> ControlResponse {
            ControlResponse::Stall
        }
    }

    let mux = Rc::new(RefCell::new(Usb2PortMux::new(1)));
    let mut ehci = EhciController::new_with_port_count(1);
    ehci.hub_mut().attach_usb2_port_mux(0, mux.clone(), 0);

    mux.borrow_mut().attach(0, Box::new(HighSpeedDevice));

    // Route ports to EHCI and claim ownership (clears PORT_OWNER).
    ehci.mmio_write(REG_CONFIGFLAG, 4, CONFIGFLAG_CF);

    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_ne!(
        portsc & PORTSC_HSP,
        0,
        "expected EHCI PORTSC.HSP set when a high-speed device is attached"
    );
}

#[test]
fn usb2_companion_routing_snapshot_roundtrip_is_order_independent() {
    let mux = Rc::new(RefCell::new(Usb2PortMux::new(1)));

    let mut uhci = UhciController::new();
    uhci.hub_mut().attach_usb2_port_mux(0, mux.clone(), 0);

    let mut ehci = EhciController::new_with_port_count(1);
    ehci.hub_mut().attach_usb2_port_mux(0, mux.clone(), 0);

    let keyboard = UsbHidKeyboardHandle::new();
    mux.borrow_mut().attach(0, Box::new(keyboard));

    let mut mem = TestMemory::new(0x20_000);
    uhci_reset_root_port(&mut uhci, &mut mem);

    // Claim the port for EHCI and complete the reset sequence so the device is routable.
    ehci.mmio_write(REG_CONFIGFLAG, 4, CONFIGFLAG_CF);
    ehci_reset_port(&mut ehci, &mut mem, 0);

    // Mutate the shared device state (address assignment) so snapshot/restore has something
    // meaningful to preserve.
    {
        let mut dev = ehci
            .hub_mut()
            .device_mut_for_address(0)
            .expect("device should be reachable after EHCI reset");
        let setup = SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05, // SET_ADDRESS
            w_value: 1,
            w_index: 0,
            w_length: 0,
        };
        assert_eq!(dev.handle_setup(setup), UsbOutResult::Ack);
        assert!(
            matches!(dev.handle_in(0, 0), UsbInResult::Data(data) if data.is_empty()),
            "expected ACK for status stage"
        );
    }

    assert!(ehci.hub_mut().device_mut_for_address(1).is_some());
    assert!(uhci.hub_mut().device_mut_for_address(1).is_none());

    let ehci_snapshot = ehci.save_state();
    let uhci_snapshot = uhci.save_state();

    // Restore order: EHCI first, then UHCI.
    {
        let mux = Rc::new(RefCell::new(Usb2PortMux::new(1)));
        let mut uhci = UhciController::new();
        uhci.hub_mut().attach_usb2_port_mux(0, mux.clone(), 0);
        let mut ehci = EhciController::new_with_port_count(1);
        ehci.hub_mut().attach_usb2_port_mux(0, mux.clone(), 0);

        ehci.load_state(&ehci_snapshot)
            .expect("EHCI snapshot restore should succeed");
        uhci.load_state(&uhci_snapshot)
            .expect("UHCI snapshot restore should succeed");

        assert!(
            mux.borrow().configflag(),
            "mux CONFIGFLAG should be restored"
        );
        assert_eq!(mux.borrow().port_device(0).unwrap().address(), 1);
        assert!(ehci.hub_mut().device_mut_for_address(1).is_some());
        assert!(uhci.hub_mut().device_mut_for_address(1).is_none());
    }

    // Restore order: UHCI first, then EHCI.
    {
        let mux = Rc::new(RefCell::new(Usb2PortMux::new(1)));
        let mut uhci = UhciController::new();
        uhci.hub_mut().attach_usb2_port_mux(0, mux.clone(), 0);
        let mut ehci = EhciController::new_with_port_count(1);
        ehci.hub_mut().attach_usb2_port_mux(0, mux.clone(), 0);

        uhci.load_state(&uhci_snapshot)
            .expect("UHCI snapshot restore should succeed");
        ehci.load_state(&ehci_snapshot)
            .expect("EHCI snapshot restore should succeed");

        assert!(
            mux.borrow().configflag(),
            "mux CONFIGFLAG should be restored"
        );
        assert_eq!(mux.borrow().port_device(0).unwrap().address(), 1);
        assert!(ehci.hub_mut().device_mut_for_address(1).is_some());
        assert!(uhci.hub_mut().device_mut_for_address(1).is_none());
    }
}

#[test]
fn usb2_companion_routing_snapshot_restore_preserves_device_suspend_state() {
    #[derive(Clone)]
    struct SuspendedSpy(Rc<RefCell<bool>>);

    impl SuspendedSpy {
        fn new() -> Self {
            Self(Rc::new(RefCell::new(false)))
        }

        fn suspended(&self) -> bool {
            *self.0.borrow()
        }
    }

    impl UsbDeviceModel for SuspendedSpy {
        fn reset(&mut self) {
            *self.0.borrow_mut() = false;
        }

        fn handle_control_request(
            &mut self,
            _setup: SetupPacket,
            _data_stage: Option<&[u8]>,
        ) -> ControlResponse {
            ControlResponse::Ack
        }

        fn set_suspended(&mut self, suspended: bool) {
            *self.0.borrow_mut() = suspended;
        }
    }

    let mux = Rc::new(RefCell::new(Usb2PortMux::new(1)));

    let mut uhci = UhciController::new();
    uhci.hub_mut().attach_usb2_port_mux(0, mux.clone(), 0);

    let mut ehci = EhciController::new_with_port_count(1);
    ehci.hub_mut().attach_usb2_port_mux(0, mux.clone(), 0);

    let spy = SuspendedSpy::new();
    mux.borrow_mut().attach(0, Box::new(spy.clone()));

    let mut mem = TestMemory::new(0x20_000);
    uhci_reset_root_port(&mut uhci, &mut mem);

    // Claim the port for EHCI so the EHCI view becomes the effective owner.
    ehci.mmio_write(REG_CONFIGFLAG, 4, CONFIGFLAG_CF);
    ehci_reset_port(&mut ehci, &mut mem, 0);

    // Suspend the device through the owning (EHCI) view.
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    ehci.mmio_write(reg_portsc(0), 4, portsc | PORTSC_SUSP);
    assert!(
        spy.suspended(),
        "device model should observe suspend while owned by EHCI"
    );

    let ehci_snapshot = ehci.save_state();
    let uhci_snapshot = uhci.save_state();

    // Restore order: EHCI first, then UHCI. This previously left the device in the *UHCI* suspend
    // state (false) because the mux applied the last-loaded view record unconditionally.
    {
        let mux = Rc::new(RefCell::new(Usb2PortMux::new(1)));
        let mut uhci = UhciController::new();
        uhci.hub_mut().attach_usb2_port_mux(0, mux.clone(), 0);
        let mut ehci = EhciController::new_with_port_count(1);
        ehci.hub_mut().attach_usb2_port_mux(0, mux.clone(), 0);

        let spy = SuspendedSpy::new();
        mux.borrow_mut().attach(0, Box::new(spy.clone()));

        ehci.load_state(&ehci_snapshot)
            .expect("EHCI snapshot restore should succeed");
        uhci.load_state(&uhci_snapshot)
            .expect("UHCI snapshot restore should succeed");

        assert!(spy.suspended(), "device should remain suspended after restore");
    }

    // Restore order: UHCI first, then EHCI.
    {
        let mux = Rc::new(RefCell::new(Usb2PortMux::new(1)));
        let mut uhci = UhciController::new();
        uhci.hub_mut().attach_usb2_port_mux(0, mux.clone(), 0);
        let mut ehci = EhciController::new_with_port_count(1);
        ehci.hub_mut().attach_usb2_port_mux(0, mux.clone(), 0);

        let spy = SuspendedSpy::new();
        mux.borrow_mut().attach(0, Box::new(spy.clone()));

        uhci.load_state(&uhci_snapshot)
            .expect("UHCI snapshot restore should succeed");
        ehci.load_state(&ehci_snapshot)
            .expect("EHCI snapshot restore should succeed");

        assert!(spy.suspended(), "device should remain suspended after restore");
    }
}
