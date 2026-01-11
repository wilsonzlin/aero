use aero_usb::passthrough::{
    SetupPacket as HostSetupPacket, UsbHostAction, UsbHostCompletion, UsbHostCompletionIn,
    UsbWebUsbPassthroughDevice,
};
use aero_usb::uhci::UhciController;
use aero_usb::usb::SetupPacket;
use aero_usb::GuestMemory;

mod util;

use util::{
    actlen, install_frame_list, td_ctrl, td_token, write_qh, write_td, Alloc, TestIrq, TestMemory,
    LINK_PTR_T, PORTSC_PR, REG_FRBASEADD, REG_PORTSC1, REG_USBCMD, REG_USBINTR, TD_CTRL_ACTIVE,
    TD_CTRL_NAK, TD_CTRL_STALLED, USBINTR_IOC, USBCMD_RUN,
};

fn setup_packet_bytes(setup: SetupPacket) -> [u8; 8] {
    let mut bytes = [0u8; 8];
    bytes[0] = setup.request_type;
    bytes[1] = setup.request;
    bytes[2..4].copy_from_slice(&setup.value.to_le_bytes());
    bytes[4..6].copy_from_slice(&setup.index.to_le_bytes());
    bytes[6..8].copy_from_slice(&setup.length.to_le_bytes());
    bytes
}

fn setup_controller(io_base: u16) -> (UhciController, TestMemory, TestIrq, Alloc, u32) {
    let mut ctrl = UhciController::new(io_base, 11);
    ctrl.connect_device(0, Box::new(UsbWebUsbPassthroughDevice::new()));

    let mut mem = TestMemory::new(0x40000);
    let mut irq = TestIrq::default();
    let alloc = Alloc::new(0x2000);

    let fl_base = 0x1000;
    ctrl.port_write(io_base + REG_FRBASEADD, 4, fl_base, &mut irq);
    ctrl.port_write(io_base + REG_USBINTR, 2, USBINTR_IOC as u32, &mut irq);

    // Reset + enable port 1.
    ctrl.port_write(io_base + REG_PORTSC1, 2, PORTSC_PR as u32, &mut irq);
    for _ in 0..50 {
        ctrl.step_frame(&mut mem, &mut irq);
    }

    ctrl.port_write(io_base + REG_USBCMD, 2, USBCMD_RUN as u32, &mut irq);

    (ctrl, mem, irq, alloc, fl_base)
}

fn passthrough_device_mut(ctrl: &mut UhciController) -> &mut UsbWebUsbPassthroughDevice {
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

#[test]
fn control_in_pending_produces_td_nak_until_completion() {
    let io_base = 0x5200;
    let (mut ctrl, mut mem, mut irq, mut alloc, fl_base) = setup_controller(io_base);

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

    // Split the data stage into 8+8+2 to exercise multi-TD sequencing.
    let data1_buf = alloc.alloc(8, 0x10);
    let data1_td = alloc.alloc(0x20, 0x10);
    let data2_buf = alloc.alloc(8, 0x10);
    let data2_td = alloc.alloc(0x20, 0x10);
    let data3_buf = alloc.alloc(2, 0x10);
    let data3_td = alloc.alloc(0x20, 0x10);

    let status_td = alloc.alloc(0x20, 0x10);

    mem.write(setup_buf, &setup_packet_bytes(setup));

    write_td(
        &mut mem,
        setup_td,
        data1_td,
        td_ctrl(true, false),
        td_token(0x2D, 0, 0, false, 8),
        setup_buf,
    );
    write_td(
        &mut mem,
        data1_td,
        data2_td,
        td_ctrl(true, false),
        td_token(0x69, 0, 0, true, 8),
        data1_buf,
    );
    write_td(
        &mut mem,
        data2_td,
        data3_td,
        td_ctrl(true, false),
        td_token(0x69, 0, 0, false, 8),
        data2_buf,
    );
    write_td(
        &mut mem,
        data3_td,
        status_td,
        td_ctrl(true, false),
        td_token(0x69, 0, 0, true, 2),
        data3_buf,
    );
    // Status stage: OUT zero-length, DATA1.
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

    // Frame #1: SETUP completes but first IN DATA TD NAKs (pending).
    ctrl.step_frame(&mut mem, &mut irq);

    assert_eq!(mem.read_u32(setup_td + 4) & TD_CTRL_ACTIVE, 0);

    let data1_ctrl = mem.read_u32(data1_td + 4);
    assert_ne!(data1_ctrl & TD_CTRL_ACTIVE, 0);
    assert_ne!(data1_ctrl & TD_CTRL_NAK, 0);

    // Only one host action should be queued for the in-flight control request.
    let mut actions = passthrough_device_mut(&mut ctrl).drain_actions();
    assert_eq!(actions.len(), 1, "expected exactly one queued host action");
    let action = actions.pop().unwrap();

    let (id, got_setup) = match action {
        UsbHostAction::ControlIn { id, setup } => (id, setup),
        other => panic!("unexpected action: {other:?}"),
    };

    assert_eq!(
        got_setup,
        HostSetupPacket {
            bm_request_type: setup.request_type,
            b_request: setup.request,
            w_value: setup.value,
            w_index: setup.index,
            w_length: setup.length,
        }
    );

    // Inject completion with a deterministic payload.
    let payload: Vec<u8> = (0u8..18u8).collect();
    passthrough_device_mut(&mut ctrl).push_completion(UsbHostCompletion::ControlIn {
        id,
        result: UsbHostCompletionIn::Success {
            data: payload.clone(),
        },
    });

    // Frame #2: All DATA TDs complete and buffers are filled.
    ctrl.step_frame(&mut mem, &mut irq);

    for (td, expected_len) in [(data1_td, 8usize), (data2_td, 8), (data3_td, 2)] {
        let ctrl_sts = mem.read_u32(td + 4);
        assert_eq!(ctrl_sts & TD_CTRL_ACTIVE, 0);
        assert_eq!(actlen(ctrl_sts), expected_len);
    }

    let mut got = Vec::new();
    let mut tmp = [0u8; 8];
    mem.read(data1_buf, &mut tmp);
    got.extend_from_slice(&tmp);
    mem.read(data2_buf, &mut tmp);
    got.extend_from_slice(&tmp);
    let mut tmp2 = [0u8; 2];
    mem.read(data3_buf, &mut tmp2);
    got.extend_from_slice(&tmp2);
    assert_eq!(got, payload);

    // QH element pointer should have advanced past the TD chain.
    assert_eq!(mem.read_u32(qh_addr + 4), LINK_PTR_T);

    // No duplicate host actions should be emitted across retries.
    assert!(
        passthrough_device_mut(&mut ctrl).drain_actions().is_empty(),
        "expected no new host actions after completion"
    );
}

#[test]
fn set_address_is_virtualized_and_applied_after_status_stage() {
    let io_base = 0x5300;
    let (mut ctrl, mut mem, mut irq, mut alloc, fl_base) = setup_controller(io_base);

    let setup = SetupPacket {
        request_type: 0x00,
        request: 0x05, // SET_ADDRESS
        value: 1,
        index: 0,
        length: 0,
    };

    let qh_addr = alloc.alloc(0x20, 0x10);
    let setup_buf = alloc.alloc(8, 0x10);
    let setup_td = alloc.alloc(0x20, 0x10);
    let status_td = alloc.alloc(0x20, 0x10);

    mem.write(setup_buf, &setup_packet_bytes(setup));
    write_td(
        &mut mem,
        setup_td,
        status_td,
        td_ctrl(true, false),
        td_token(0x2D, 0, 0, false, 8),
        setup_buf,
    );
    // Status stage: IN zero-length, DATA1.
    write_td(
        &mut mem,
        status_td,
        LINK_PTR_T,
        td_ctrl(true, true),
        td_token(0x69, 0, 0, true, 0),
        0,
    );
    write_qh(&mut mem, qh_addr, LINK_PTR_T, setup_td);
    install_frame_list(&mut mem, fl_base, qh_addr);

    ctrl.step_frame(&mut mem, &mut irq);

    assert!(
        passthrough_device_mut(&mut ctrl).drain_actions().is_empty(),
        "SET_ADDRESS must not be forwarded as a host action"
    );
    assert_eq!(passthrough_device_mut(&mut ctrl).address(), 1);

    // Negative case: malformed SET_ADDRESS (DeviceToHost bmRequestType) must STALL.
    let io_base = 0x5310;
    let (mut ctrl, mut mem, mut irq, mut alloc, fl_base) = setup_controller(io_base);

    let bad_setup = SetupPacket {
        request_type: 0x80,
        request: 0x05,
        value: 1,
        index: 0,
        length: 0,
    };

    let qh_addr = alloc.alloc(0x20, 0x10);
    let setup_buf = alloc.alloc(8, 0x10);
    let setup_td = alloc.alloc(0x20, 0x10);
    let status_td = alloc.alloc(0x20, 0x10);

    mem.write(setup_buf, &setup_packet_bytes(bad_setup));
    write_td(
        &mut mem,
        setup_td,
        status_td,
        td_ctrl(true, false),
        td_token(0x2D, 0, 0, false, 8),
        setup_buf,
    );
    // For bmRequestType=IN and wLength=0, the STATUS stage is OUT.
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

    ctrl.step_frame(&mut mem, &mut irq);

    let status_ctrl = mem.read_u32(status_td + 4);
    assert_eq!(status_ctrl & TD_CTRL_ACTIVE, 0);
    assert_ne!(status_ctrl & TD_CTRL_STALLED, 0);

    assert!(
        passthrough_device_mut(&mut ctrl).drain_actions().is_empty(),
        "malformed SET_ADDRESS must not be forwarded"
    );
}

#[test]
fn bulk_in_pending_queues_once_and_naks_until_completion() {
    let io_base = 0x5400;
    let (mut ctrl, mut mem, mut irq, mut alloc, fl_base) = setup_controller(io_base);

    // First, set device address to 1 (virtualized).
    {
        let setup = SetupPacket {
            request_type: 0x00,
            request: 0x05,
            value: 1,
            index: 0,
            length: 0,
        };

        let qh_addr = alloc.alloc(0x20, 0x10);
        let setup_buf = alloc.alloc(8, 0x10);
        let setup_td = alloc.alloc(0x20, 0x10);
        let status_td = alloc.alloc(0x20, 0x10);

        mem.write(setup_buf, &setup_packet_bytes(setup));
        write_td(
            &mut mem,
            setup_td,
            status_td,
            td_ctrl(true, false),
            td_token(0x2D, 0, 0, false, 8),
            setup_buf,
        );
        write_td(
            &mut mem,
            status_td,
            LINK_PTR_T,
            td_ctrl(true, true),
            td_token(0x69, 0, 0, true, 0),
            0,
        );
        write_qh(&mut mem, qh_addr, LINK_PTR_T, setup_td);
        install_frame_list(&mut mem, fl_base, qh_addr);

        ctrl.step_frame(&mut mem, &mut irq);
        assert_eq!(passthrough_device_mut(&mut ctrl).address(), 1);
        assert!(passthrough_device_mut(&mut ctrl).drain_actions().is_empty());
    }

    // Schedule a bulk IN TD to endpoint 1 (ep addr 0x81), length 8.
    let qh_addr = alloc.alloc(0x20, 0x10);
    let td_addr = alloc.alloc(0x20, 0x10);
    let buf_addr = alloc.alloc(8, 0x10);

    write_td(
        &mut mem,
        td_addr,
        LINK_PTR_T,
        td_ctrl(true, true),
        td_token(0x69, 1, 1, false, 8),
        buf_addr,
    );
    write_qh(&mut mem, qh_addr, LINK_PTR_T, td_addr);
    install_frame_list(&mut mem, fl_base, qh_addr);

    // Frame #1: TD should NAK and emit one BulkIn host action.
    ctrl.step_frame(&mut mem, &mut irq);
    let ctrl_sts = mem.read_u32(td_addr + 4);
    assert_ne!(ctrl_sts & TD_CTRL_ACTIVE, 0);
    assert_ne!(ctrl_sts & TD_CTRL_NAK, 0);

    let mut actions = passthrough_device_mut(&mut ctrl).drain_actions();
    assert_eq!(actions.len(), 1, "expected exactly one BulkIn action");
    let action = actions.pop().unwrap();
    let id = match action {
        UsbHostAction::BulkIn {
            id,
            endpoint,
            length,
        } => {
            assert_eq!(endpoint, 0x81);
            assert_eq!(length, 8);
            id
        }
        other => panic!("unexpected action: {other:?}"),
    };

    // Frame #2 without completion: still NAK, and no duplicate action should be queued.
    ctrl.step_frame(&mut mem, &mut irq);
    let ctrl_sts = mem.read_u32(td_addr + 4);
    assert_ne!(ctrl_sts & TD_CTRL_ACTIVE, 0);
    assert_ne!(ctrl_sts & TD_CTRL_NAK, 0);
    assert!(
        passthrough_device_mut(&mut ctrl).drain_actions().is_empty(),
        "expected no duplicate BulkIn actions while in-flight"
    );

    // Inject completion with more bytes than requested; device should truncate to TD length.
    passthrough_device_mut(&mut ctrl).push_completion(UsbHostCompletion::BulkIn {
        id,
        result: UsbHostCompletionIn::Success {
            data: vec![0xAA, 0xBB, 0xCC, 0xDD, 1, 2, 3, 4, 5, 6, 7],
        },
    });

    // Frame #3: TD completes and buffer contains first 8 bytes.
    ctrl.step_frame(&mut mem, &mut irq);
    let ctrl_sts = mem.read_u32(td_addr + 4);
    assert_eq!(ctrl_sts & TD_CTRL_ACTIVE, 0);
    assert_eq!(actlen(ctrl_sts), 8);

    assert_eq!(
        &mem.data[buf_addr as usize..buf_addr as usize + 8],
        &[0xAA, 0xBB, 0xCC, 0xDD, 1, 2, 3, 4]
    );

    assert!(
        passthrough_device_mut(&mut ctrl).drain_actions().is_empty(),
        "expected no extra actions after completion"
    );
    assert_eq!(mem.read_u32(qh_addr + 4), LINK_PTR_T);
}
