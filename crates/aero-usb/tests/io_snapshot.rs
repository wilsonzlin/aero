use aero_io_snapshot::io::state::{IoSnapshot, SnapshotError};
use aero_usb::hid::{UsbHidKeyboard, UsbHidMouse};
use aero_usb::hub::UsbHubDevice;
use aero_usb::passthrough::{UsbHostAction, UsbHostCompletion, UsbHostCompletionIn};
use aero_usb::uhci::UhciController;
use aero_usb::usb::{SetupPacket, UsbHandshake};
use aero_usb::GuestMemory;
use aero_usb::UsbWebUsbPassthroughDevice;

mod util;

use util::{
    actlen, install_frame_list, td_ctrl, td_token, write_qh, write_td, Alloc, TestIrq, TestMemory,
    LINK_PTR_T, PORTSC_PR, REG_FRBASEADD, REG_PORTSC1, REG_USBCMD, REG_USBINTR, TD_CTRL_ACTIVE,
    TD_CTRL_NAK, TD_CTRL_STALLED, USBCMD_RUN, USBINTR_IOC,
};

fn control_no_data(ctrl: &mut UhciController, addr: u8, setup: SetupPacket) {
    assert_eq!(
        ctrl.bus_mut().handle_setup(addr, setup),
        UsbHandshake::Ack { bytes: 8 }
    );
    let mut zlp = [0u8; 0];
    assert_eq!(
        ctrl.bus_mut().handle_in(addr, 0, &mut zlp),
        UsbHandshake::Ack { bytes: 0 }
    );
}

fn hub_mut(ctrl: &mut UhciController) -> &mut UsbHubDevice {
    ctrl.bus_mut()
        .port_mut(0)
        .unwrap()
        .device
        .as_mut()
        .unwrap()
        .as_any_mut()
        .downcast_mut::<UsbHubDevice>()
        .unwrap()
}

fn webusb_mut(ctrl: &mut UhciController) -> &mut UsbWebUsbPassthroughDevice {
    ctrl.bus_mut()
        .port_mut(0)
        .unwrap()
        .device
        .as_mut()
        .unwrap()
        .as_any_mut()
        .downcast_mut::<UsbWebUsbPassthroughDevice>()
        .unwrap()
}

fn setup_packet_bytes(setup: SetupPacket) -> [u8; 8] {
    let mut bytes = [0u8; 8];
    bytes[0] = setup.request_type;
    bytes[1] = setup.request;
    bytes[2..4].copy_from_slice(&setup.value.to_le_bytes());
    bytes[4..6].copy_from_slice(&setup.index.to_le_bytes());
    bytes[6..8].copy_from_slice(&setup.length.to_le_bytes());
    bytes
}

#[test]
fn snapshot_roundtrip_is_byte_stable_and_resumes_hub_topology() {
    let io_base = 0x5000;
    let mut ctrl = UhciController::new(io_base, 11);
    ctrl.connect_device(0, Box::new(UsbHubDevice::new_with_ports(4)));

    hub_mut(&mut ctrl).attach(1, Box::new(UsbHidKeyboard::new()));
    hub_mut(&mut ctrl).attach(2, Box::new(UsbHidMouse::new()));

    let mut mem = TestMemory::new(0x20000);
    let mut irq = TestIrq::default();

    // Reset + enable root port 0 so the bus routes packets.
    ctrl.port_write(io_base + REG_PORTSC1, 2, PORTSC_PR as u32, &mut irq);
    for _ in 0..50 {
        ctrl.step_frame(&mut mem, &mut irq);
    }

    // Enumerate hub (addr=1, cfg=1).
    control_no_data(
        &mut ctrl,
        0,
        SetupPacket {
            request_type: 0x00,
            request: 0x05, // SET_ADDRESS
            value: 1,
            index: 0,
            length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            request_type: 0x00,
            request: 0x09, // SET_CONFIGURATION
            value: 1,
            index: 0,
            length: 0,
        },
    );

    // Hub port 1: power + reset, enumerate downstream keyboard to addr=2.
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            request_type: 0x23,
            request: 0x03, // SET_FEATURE
            value: 8,      // PORT_POWER
            index: 1,
            length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            request_type: 0x23,
            request: 0x03, // SET_FEATURE
            value: 4,      // PORT_RESET
            index: 1,
            length: 0,
        },
    );
    for _ in 0..50 {
        ctrl.step_frame(&mut mem, &mut irq);
    }
    control_no_data(
        &mut ctrl,
        0,
        SetupPacket {
            request_type: 0x00,
            request: 0x05,
            value: 2,
            index: 0,
            length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        2,
        SetupPacket {
            request_type: 0x00,
            request: 0x09,
            value: 1,
            index: 0,
            length: 0,
        },
    );

    // Hub port 2: power + reset, enumerate downstream mouse to addr=3.
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            request_type: 0x23,
            request: 0x03,
            value: 8,
            index: 2,
            length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        1,
        SetupPacket {
            request_type: 0x23,
            request: 0x03,
            value: 4,
            index: 2,
            length: 0,
        },
    );
    for _ in 0..50 {
        ctrl.step_frame(&mut mem, &mut irq);
    }
    control_no_data(
        &mut ctrl,
        0,
        SetupPacket {
            request_type: 0x00,
            request: 0x05,
            value: 3,
            index: 0,
            length: 0,
        },
    );
    control_no_data(
        &mut ctrl,
        3,
        SetupPacket {
            request_type: 0x00,
            request: 0x09,
            value: 1,
            index: 0,
            length: 0,
        },
    );

    // Queue one report in each downstream device.
    {
        let kb = ctrl
            .bus_mut()
            .device_mut_for_address(2)
            .unwrap()
            .as_any_mut()
            .downcast_mut::<UsbHidKeyboard>()
            .unwrap();
        kb.key_event(0x04, true); // 'a'

        let mouse = ctrl
            .bus_mut()
            .device_mut_for_address(3)
            .unwrap()
            .as_any_mut()
            .downcast_mut::<UsbHidMouse>()
            .unwrap();
        mouse.movement(1, 1);
    }

    let snap1 = ctrl.save_state();
    let mut restored = UhciController::new(0, 0);
    restored.load_state(&snap1).unwrap();
    let snap2 = restored.save_state();
    assert_eq!(snap1, snap2, "save->load->save must be byte-stable");

    let mut kb_buf = [0u8; 8];
    assert_eq!(
        restored.bus_mut().handle_in(2, 1, &mut kb_buf),
        UsbHandshake::Ack { bytes: 8 }
    );
    assert_eq!(kb_buf[2], 0x04);

    let mut mouse_buf = [0u8; 4];
    assert_eq!(
        restored.bus_mut().handle_in(3, 1, &mut mouse_buf),
        UsbHandshake::Ack { bytes: 4 }
    );
    assert_eq!(mouse_buf[1], 1);
    assert_eq!(mouse_buf[2], 1);
}

#[test]
fn snapshot_restore_rejects_truncated_bytes() {
    let ctrl = UhciController::new(0x6000, 11);
    let snap = ctrl.save_state();

    for len in [0usize, 1, snap.len().saturating_sub(1)] {
        let mut restored = UhciController::new(0, 0);
        let err = restored.load_state(&snap[..len]).unwrap_err();
        assert!(matches!(err, SnapshotError::UnexpectedEof));
    }
}

#[test]
fn snapshot_resume_webusb_inflight_action_naks_until_completion() {
    let io_base = 0x5200;
    let mut ctrl = UhciController::new(io_base, 11);
    ctrl.connect_device(0, Box::new(UsbWebUsbPassthroughDevice::new()));

    let mut mem = TestMemory::new(0x40000);
    let mut irq = TestIrq::default();
    let mut alloc = Alloc::new(0x2000);
    let fl_base = 0x1000;

    ctrl.port_write(io_base + REG_FRBASEADD, 4, fl_base, &mut irq);
    ctrl.port_write(io_base + REG_USBINTR, 2, USBINTR_IOC as u32, &mut irq);

    // Reset + enable root port 0.
    ctrl.port_write(io_base + REG_PORTSC1, 2, PORTSC_PR as u32, &mut irq);
    for _ in 0..50 {
        ctrl.step_frame(&mut mem, &mut irq);
    }
    ctrl.port_write(io_base + REG_USBCMD, 2, USBCMD_RUN as u32, &mut irq);

    // Control-IN request that will be forwarded as a host action.
    let setup = SetupPacket {
        request_type: 0x80,
        request: 0x06,
        value: 0x0100,
        index: 0,
        length: 18,
    };

    let qh_addr = alloc.alloc(0x20, 0x10);
    let setup_buf = alloc.alloc(8, 0x10);
    let setup_td = alloc.alloc(0x20, 0x10);
    let data_buf = alloc.alloc(18, 0x10);
    let data_td = alloc.alloc(0x20, 0x10);
    let status_td = alloc.alloc(0x20, 0x10);

    mem.write(setup_buf, &setup_packet_bytes(setup));

    write_td(
        &mut mem,
        setup_td,
        data_td,
        td_ctrl(true, false),
        td_token(0x2D, 0, 0, false, 8),
        setup_buf,
    );
    write_td(
        &mut mem,
        data_td,
        status_td,
        td_ctrl(true, false),
        td_token(0x69, 0, 0, true, 18),
        data_buf,
    );
    write_td(
        &mut mem,
        status_td,
        LINK_PTR_T,
        td_ctrl(true, true),
        td_token(0xE1, 0, 0, true, 0),
        0,
    );

    write_qh(&mut mem, qh_addr, LINK_PTR_T, setup_td);
    install_frame_list(&mut mem, fl_base, qh_addr);

    // Frame #1: SETUP completes; DATA TD NAKs and queues exactly one host action.
    ctrl.step_frame(&mut mem, &mut irq);
    assert_eq!(mem.read_u32(setup_td + 4) & TD_CTRL_ACTIVE, 0);
    let data_ctrl = mem.read_u32(data_td + 4);
    assert_ne!(data_ctrl & TD_CTRL_ACTIVE, 0);
    assert_ne!(data_ctrl & TD_CTRL_NAK, 0);
    assert_eq!(data_ctrl & TD_CTRL_STALLED, 0);

    let action = webusb_mut(&mut ctrl).drain_actions().pop().unwrap();
    let id = match action {
        UsbHostAction::ControlIn { id, .. } => id,
        other => panic!("unexpected action: {other:?}"),
    };

    let snap = ctrl.save_state();
    let mem_snapshot = mem.data.clone();

    let mut restored = UhciController::new(0, 0);
    restored.load_state(&snap).unwrap();
    let mut mem2 = TestMemory::new(mem_snapshot.len());
    mem2.data = mem_snapshot;
    let mut irq2 = TestIrq::default();

    // Frame #2: still NAK, and no duplicate host action should be queued.
    restored.step_frame(&mut mem2, &mut irq2);
    let data_ctrl2 = mem2.read_u32(data_td + 4);
    assert_ne!(data_ctrl2 & TD_CTRL_ACTIVE, 0);
    assert_ne!(data_ctrl2 & TD_CTRL_NAK, 0);
    assert_eq!(data_ctrl2 & TD_CTRL_STALLED, 0);
    assert!(
        webusb_mut(&mut restored).drain_actions().is_empty(),
        "expected no new actions while inflight after restore"
    );

    // Completion with a deterministic payload should unblock the pending DATA TD.
    let payload: Vec<u8> = (0u8..18u8).collect();
    webusb_mut(&mut restored).push_completion(UsbHostCompletion::ControlIn {
        id,
        result: UsbHostCompletionIn::Success {
            data: payload.clone(),
        },
    });

    restored.step_frame(&mut mem2, &mut irq2);

    let data_ctrl3 = mem2.read_u32(data_td + 4);
    assert_eq!(data_ctrl3 & TD_CTRL_ACTIVE, 0);
    assert_eq!(actlen(data_ctrl3), 18);

    let mut got = vec![0u8; 18];
    mem2.read(data_buf, &mut got);
    assert_eq!(got, payload);
}
