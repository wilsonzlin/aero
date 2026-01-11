use aero_usb::passthrough::{
    SetupPacket as HostSetupPacket, UsbHostAction, UsbHostCompletion, UsbHostCompletionIn,
    UsbHostCompletionOut,
};
use aero_usb::uhci::regs;
use aero_usb::uhci::UhciController;
use aero_usb::{SetupPacket, UsbWebUsbPassthroughDevice};

mod util;

use util::{
    actlen, install_frame_list, td_ctrl, td_token, write_qh, write_td, Alloc, TestMemory,
    LINK_PTR_T, PORTSC_PR, REG_FRBASEADD, REG_PORTSC1, REG_USBCMD, REG_USBINTR, TD_CTRL_ACTIVE,
    TD_CTRL_NAK, TD_CTRL_STALLED, USBCMD_RUN, USBINTR_IOC,
};

const USBINTR_SHORT_PACKET: u16 = 1 << 3;
const TD_CTRL_SPD: u32 = 1 << 29;
const USBSTS_USBERRINT: u16 = 1 << 1;
const TD_CTRL_CRCERR: u32 = 1 << 18;

fn setup_packet_bytes(setup: SetupPacket) -> [u8; 8] {
    [
        setup.bm_request_type,
        setup.b_request,
        setup.w_value.to_le_bytes()[0],
        setup.w_value.to_le_bytes()[1],
        setup.w_index.to_le_bytes()[0],
        setup.w_index.to_le_bytes()[1],
        setup.w_length.to_le_bytes()[0],
        setup.w_length.to_le_bytes()[1],
    ]
}

fn setup_controller() -> (
    UhciController,
    TestMemory,
    Alloc,
    u32,
    UsbWebUsbPassthroughDevice,
) {
    let mut ctrl = UhciController::new();
    let dev = UsbWebUsbPassthroughDevice::new();
    ctrl.hub_mut().attach(0, Box::new(dev.clone()));

    let mut mem = TestMemory::new(0x40000);
    let alloc = Alloc::new(0x2000);

    let fl_base = 0x1000;
    ctrl.io_write(REG_FRBASEADD, 4, fl_base);
    ctrl.io_write(REG_USBINTR, 2, USBINTR_IOC as u32);

    // Reset + enable port 1.
    ctrl.io_write(REG_PORTSC1, 2, PORTSC_PR as u32);
    for _ in 0..50 {
        ctrl.tick_1ms(&mut mem);
    }

    ctrl.io_write(REG_USBCMD, 2, (USBCMD_RUN | regs::USBCMD_MAXP) as u32);

    (ctrl, mem, alloc, fl_base, dev)
}

#[test]
fn control_in_pending_produces_td_nak_until_completion() {
    let (mut ctrl, mut mem, mut alloc, fl_base, dev) = setup_controller();

    let setup = SetupPacket {
        bm_request_type: 0x80,
        b_request: 0x06,
        w_value: 0x0100,
        w_index: 0,
        w_length: 18,
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
    ctrl.tick_1ms(&mut mem);

    assert_eq!(mem.read_u32(setup_td + 4) & TD_CTRL_ACTIVE, 0);

    let data1_ctrl = mem.read_u32(data1_td + 4);
    assert_ne!(data1_ctrl & TD_CTRL_ACTIVE, 0);
    assert_ne!(data1_ctrl & TD_CTRL_NAK, 0);

    // Only one host action should be queued for the in-flight control request.
    let mut actions = dev.drain_actions();
    assert_eq!(actions.len(), 1, "expected exactly one queued host action");
    let action = actions.pop().unwrap();

    let (id, got_setup) = match action {
        UsbHostAction::ControlIn { id, setup } => (id, setup),
        other => panic!("unexpected action: {other:?}"),
    };

    assert_eq!(
        got_setup,
        HostSetupPacket {
            bm_request_type: setup.bm_request_type,
            b_request: setup.b_request,
            w_value: setup.w_value,
            w_index: setup.w_index,
            w_length: setup.w_length,
        }
    );

    // Inject completion with a deterministic payload.
    let payload: Vec<u8> = (0u8..18u8).collect();
    dev.push_completion(UsbHostCompletion::ControlIn {
        id,
        result: UsbHostCompletionIn::Success {
            data: payload.clone(),
        },
    });

    // Frame #2: All DATA TDs complete and buffers are filled.
    ctrl.tick_1ms(&mut mem);

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
    assert!(dev.drain_actions().is_empty());
}

#[test]
fn control_in_short_completion_with_spd_stops_additional_data_tds_in_frame() {
    let (mut ctrl, mut mem, mut alloc, fl_base, dev) = setup_controller();

    // Enable short packet interrupts so the test can observe IRQ assertion.
    ctrl.io_write(
        REG_USBINTR,
        2,
        (USBINTR_IOC | USBINTR_SHORT_PACKET) as u32,
    );

    // Control-IN request with a large wLength. The host completion will provide fewer bytes,
    // resulting in a short packet on the first DATA TD.
    let setup = SetupPacket {
        bm_request_type: 0x80,
        b_request: 0x06,
        w_value: 0x0100,
        w_index: 0,
        w_length: 128,
    };

    let qh_addr = alloc.alloc(0x20, 0x10);
    let setup_buf = alloc.alloc(8, 0x10);
    let setup_td = alloc.alloc(0x20, 0x10);
    let data1_buf = alloc.alloc(64, 0x10);
    let data1_td = alloc.alloc(0x20, 0x10);
    let data2_buf = alloc.alloc(64, 0x10);
    let data2_td = alloc.alloc(0x20, 0x10);
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
    // DATA stage: two 64-byte IN TDs, both with SPD so a short packet stops further TD processing
    // within the same frame.
    write_td(
        &mut mem,
        data1_td,
        data2_td,
        td_ctrl(true, false) | TD_CTRL_SPD,
        td_token(0x69, 0, 0, true, 64),
        data1_buf,
    );
    write_td(
        &mut mem,
        data2_td,
        status_td,
        td_ctrl(true, false) | TD_CTRL_SPD,
        td_token(0x69, 0, 0, false, 64),
        data2_buf,
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

    // Frame #1: SETUP completes and first DATA TD NAKs (pending).
    ctrl.tick_1ms(&mut mem);
    assert_eq!(mem.read_u32(setup_td + 4) & TD_CTRL_ACTIVE, 0);
    assert_ne!(mem.read_u32(data1_td + 4) & TD_CTRL_NAK, 0);

    let mut actions = dev.drain_actions();
    assert_eq!(actions.len(), 1);
    let id = match actions.pop().unwrap() {
        UsbHostAction::ControlIn { id, .. } => id,
        other => panic!("unexpected action: {other:?}"),
    };

    // Provide a short completion (18 bytes).
    let payload: Vec<u8> = (0u8..18u8).collect();
    dev.push_completion(UsbHostCompletion::ControlIn {
        id,
        result: UsbHostCompletionIn::Success {
            data: payload.clone(),
        },
    });

    // Frame #2: first DATA TD completes with a short packet and SPD stops processing within this
    // QH, so the second DATA TD is not NAKed yet.
    ctrl.tick_1ms(&mut mem);

    let data1_ctrl = mem.read_u32(data1_td + 4);
    assert_eq!(data1_ctrl & TD_CTRL_ACTIVE, 0);
    assert_eq!(actlen(data1_ctrl), payload.len());
    assert_eq!(
        &mem.data[data1_buf as usize..data1_buf as usize + payload.len()],
        payload.as_slice()
    );

    let data2_ctrl = mem.read_u32(data2_td + 4);
    assert_ne!(data2_ctrl & TD_CTRL_ACTIVE, 0);
    assert_eq!(data2_ctrl & TD_CTRL_NAK, 0);
    assert!(ctrl.irq_level(), "short packet interrupt should assert IRQ");

    // Simulate a guest driver that handles the short packet interrupt by skipping the remaining
    // DATA TDs and proceeding directly to the STATUS stage.
    ctrl.io_write(regs::REG_USBSTS, 2, regs::USBSTS_USBINT as u32);
    mem.write_u32(qh_addr + 4, status_td);

    // Frame #3: STATUS stage completes (OUT ZLP).
    ctrl.tick_1ms(&mut mem);
    let status_ctrl = mem.read_u32(status_td + 4);
    assert_eq!(status_ctrl & TD_CTRL_ACTIVE, 0);
    assert_eq!(actlen(status_ctrl), 0);
    assert_eq!(mem.read_u32(qh_addr + 4), LINK_PTR_T);
}

#[test]
fn control_out_pending_acks_data_then_naks_status_until_completion() {
    let (mut ctrl, mut mem, mut alloc, fl_base, dev) = setup_controller();

    let setup = SetupPacket {
        bm_request_type: 0x40, // Vendor, host-to-device.
        b_request: 0x01,
        w_value: 0x1234,
        w_index: 0,
        w_length: 3,
    };
    let payload = [0xAAu8, 0xBB, 0xCC];

    let qh_addr = alloc.alloc(0x20, 0x10);
    let setup_buf = alloc.alloc(8, 0x10);
    let setup_td = alloc.alloc(0x20, 0x10);
    let data_buf = alloc.alloc(payload.len() as u32, 0x10);
    let data_td = alloc.alloc(0x20, 0x10);
    let status_td = alloc.alloc(0x20, 0x10);

    mem.write(setup_buf, &setup_packet_bytes(setup));
    mem.write(data_buf, &payload);

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
        td_token(0xE1, 0, 0, true, payload.len()),
        data_buf,
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

    // Frame #1: SETUP and OUT DATA TDs complete, STATUS stage NAKs (pending).
    ctrl.tick_1ms(&mut mem);

    assert_eq!(mem.read_u32(setup_td + 4) & TD_CTRL_ACTIVE, 0);
    assert_eq!(mem.read_u32(data_td + 4) & TD_CTRL_ACTIVE, 0);

    let status_ctrl = mem.read_u32(status_td + 4);
    assert_ne!(status_ctrl & TD_CTRL_ACTIVE, 0);
    assert_ne!(status_ctrl & TD_CTRL_NAK, 0);

    let mut actions = dev.drain_actions();
    assert_eq!(actions.len(), 1, "expected exactly one queued host action");
    let action = actions.pop().unwrap();
    let (id, got_setup, got_data) = match action {
        UsbHostAction::ControlOut { id, setup, data } => (id, setup, data),
        other => panic!("unexpected action: {other:?}"),
    };
    assert_eq!(
        got_setup,
        HostSetupPacket {
            bm_request_type: setup.bm_request_type,
            b_request: setup.b_request,
            w_value: setup.w_value,
            w_index: setup.w_index,
            w_length: setup.w_length,
        }
    );
    assert_eq!(got_data, payload);

    // Frame #2: still pending, should NAK again without emitting a duplicate action.
    ctrl.tick_1ms(&mut mem);
    let status_ctrl = mem.read_u32(status_td + 4);
    assert_ne!(status_ctrl & TD_CTRL_ACTIVE, 0);
    assert_ne!(status_ctrl & TD_CTRL_NAK, 0);
    assert!(
        dev.drain_actions().is_empty(),
        "expected no duplicate host actions while pending"
    );

    // Provide completion and ensure the next frame completes the STATUS stage.
    dev.push_completion(UsbHostCompletion::ControlOut {
        id,
        result: UsbHostCompletionOut::Success {
            bytes_written: payload.len() as u32,
        },
    });

    ctrl.tick_1ms(&mut mem);
    let status_ctrl = mem.read_u32(status_td + 4);
    assert_eq!(status_ctrl & TD_CTRL_ACTIVE, 0);
    assert_eq!(actlen(status_ctrl), 0);

    assert_eq!(mem.read_u32(qh_addr + 4), LINK_PTR_T);
    assert!(dev.drain_actions().is_empty());
}

#[test]
fn control_in_error_completion_maps_to_timeout_and_sets_usberrint() {
    let (mut ctrl, mut mem, mut alloc, fl_base, dev) = setup_controller();

    // Enable error interrupts so we can observe USBSTS_USBERRINT.
    ctrl.io_write(REG_USBINTR, 2, regs::USBINTR_TIMEOUT_CRC as u32);

    let setup = SetupPacket {
        bm_request_type: 0x80,
        b_request: 0x06,
        w_value: 0x0100,
        w_index: 0,
        w_length: 8,
    };

    let qh_addr = alloc.alloc(0x20, 0x10);
    let setup_buf = alloc.alloc(8, 0x10);
    let setup_td = alloc.alloc(0x20, 0x10);
    let data_buf = alloc.alloc(8, 0x10);
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
        td_token(0x69, 0, 0, true, 8),
        data_buf,
    );
    // Status stage: OUT ZLP.
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

    // Frame #1: SETUP completes, DATA TD NAKs while pending.
    ctrl.tick_1ms(&mut mem);

    let mut actions = dev.drain_actions();
    assert_eq!(actions.len(), 1);
    let id = match actions.pop().unwrap() {
        UsbHostAction::ControlIn { id, .. } => id,
        other => panic!("unexpected action: {other:?}"),
    };

    dev.push_completion(UsbHostCompletion::ControlIn {
        id,
        result: UsbHostCompletionIn::Error {
            message: "boom".to_string(),
        },
    });

    // Frame #2: DATA TD completes with TIMEOUT/CRCERR, and controller sets USBERRINT.
    ctrl.tick_1ms(&mut mem);

    let data_ctrl = mem.read_u32(data_td + 4);
    assert_eq!(data_ctrl & TD_CTRL_ACTIVE, 0);
    assert_ne!(data_ctrl & TD_CTRL_CRCERR, 0);
    assert_eq!(actlen(data_ctrl), 0);
    // Controller should stop processing within the QH on error, leaving the status TD active.
    assert_eq!(mem.read_u32(qh_addr + 4), status_td);

    assert_ne!(
        ctrl.io_read(regs::REG_USBSTS, 2) as u16 & USBSTS_USBERRINT,
        0
    );
    assert!(ctrl.irq_level(), "USBERRINT should assert IRQ when enabled");
}

#[test]
fn set_address_is_virtualized_and_applied_after_status_stage() {
    let (mut ctrl, mut mem, mut alloc, fl_base, dev) = setup_controller();

    // Device starts at address 0.
    assert!(ctrl.hub_mut().device_mut_for_address(0).is_some());
    assert!(ctrl.hub_mut().device_mut_for_address(1).is_none());

    let setup = SetupPacket {
        bm_request_type: 0x00,
        b_request: 0x05, // SET_ADDRESS
        w_value: 1,
        w_index: 0,
        w_length: 0,
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

    ctrl.tick_1ms(&mut mem);

    assert!(
        dev.drain_actions().is_empty(),
        "SET_ADDRESS must not be forwarded as a host action"
    );
    assert!(ctrl.hub_mut().device_mut_for_address(1).is_some());
    assert!(ctrl.hub_mut().device_mut_for_address(0).is_none());

    // Negative case: malformed SET_ADDRESS (DeviceToHost bmRequestType) must STALL.
    let (mut ctrl, mut mem, mut alloc, fl_base, dev) = setup_controller();

    let bad_setup = SetupPacket {
        bm_request_type: 0x80,
        b_request: 0x05,
        w_value: 1,
        w_index: 0,
        w_length: 0,
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

    ctrl.tick_1ms(&mut mem);

    let setup_ctrl = mem.read_u32(setup_td + 4);
    assert_eq!(setup_ctrl & TD_CTRL_ACTIVE, 0);
    assert_ne!(setup_ctrl & TD_CTRL_STALLED, 0);

    assert!(
        dev.drain_actions().is_empty(),
        "malformed SET_ADDRESS must not be forwarded"
    );
    assert!(ctrl.hub_mut().device_mut_for_address(0).is_some());
    assert!(ctrl.hub_mut().device_mut_for_address(1).is_none());
}

#[test]
fn vendor_request_with_brequest_05_is_forwarded() {
    let (mut ctrl, mut mem, mut alloc, fl_base, dev) = setup_controller();

    // bRequest=0x05 is SET_ADDRESS only for standard device requests. Vendor requests may legally
    // reuse the same request code and must be forwarded.
    let setup = SetupPacket {
        bm_request_type: 0x40, // Vendor, host-to-device.
        b_request: 0x05,
        w_value: 0x1234,
        w_index: 0,
        w_length: 0,
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

    // Frame #1: SETUP completes but status stage NAKs until the host provides a completion.
    ctrl.tick_1ms(&mut mem);

    assert_eq!(mem.read_u32(setup_td + 4) & TD_CTRL_ACTIVE, 0);

    let status_ctrl = mem.read_u32(status_td + 4);
    assert_ne!(status_ctrl & TD_CTRL_ACTIVE, 0);
    assert_ne!(status_ctrl & TD_CTRL_NAK, 0);
    assert_eq!(status_ctrl & TD_CTRL_STALLED, 0);

    let mut actions = dev.drain_actions();
    assert_eq!(actions.len(), 1, "expected exactly one queued host action");
    let action = actions.pop().unwrap();

    let (id, got_setup, got_data) = match action {
        UsbHostAction::ControlOut { id, setup, data } => (id, setup, data),
        other => panic!("unexpected action: {other:?}"),
    };
    assert_eq!(
        got_setup,
        HostSetupPacket {
            bm_request_type: setup.bm_request_type,
            b_request: setup.b_request,
            w_value: setup.w_value,
            w_index: setup.w_index,
            w_length: setup.w_length,
        }
    );
    assert!(got_data.is_empty());

    // Address must not change (this is not a standard SET_ADDRESS request).
    assert!(ctrl.hub_mut().device_mut_for_address(0).is_some());
    assert!(ctrl.hub_mut().device_mut_for_address(1).is_none());

    // Frame #2: still pending, no duplicate host actions should be emitted.
    ctrl.tick_1ms(&mut mem);
    let status_ctrl = mem.read_u32(status_td + 4);
    assert_ne!(status_ctrl & TD_CTRL_ACTIVE, 0);
    assert_ne!(status_ctrl & TD_CTRL_NAK, 0);
    assert!(
        dev.drain_actions().is_empty(),
        "expected no duplicate host actions while pending"
    );

    dev.push_completion(UsbHostCompletion::ControlOut {
        id,
        result: UsbHostCompletionOut::Success { bytes_written: 0 },
    });

    // Frame #3: status stage completes.
    ctrl.tick_1ms(&mut mem);
    let status_ctrl = mem.read_u32(status_td + 4);
    assert_eq!(status_ctrl & TD_CTRL_ACTIVE, 0);
    assert_eq!(actlen(status_ctrl), 0);
    assert_eq!(mem.read_u32(qh_addr + 4), LINK_PTR_T);

    assert!(dev.drain_actions().is_empty());
    assert!(ctrl.hub_mut().device_mut_for_address(0).is_some());
    assert!(ctrl.hub_mut().device_mut_for_address(1).is_none());
}

#[test]
fn control_in_zero_length_pending_naks_status_until_completion() {
    let (mut ctrl, mut mem, mut alloc, fl_base, dev) = setup_controller();

    // Device-to-host control request with wLength=0: no DATA stage, STATUS is an OUT ZLP.
    let setup = SetupPacket {
        bm_request_type: 0xC0, // Vendor, device-to-host.
        b_request: 0x01,
        w_value: 0x1234,
        w_index: 0,
        w_length: 0,
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

    // Frame #1: SETUP completes; STATUS NAKs while the host action is pending.
    ctrl.tick_1ms(&mut mem);
    assert_eq!(mem.read_u32(setup_td + 4) & TD_CTRL_ACTIVE, 0);

    let status_ctrl = mem.read_u32(status_td + 4);
    assert_ne!(status_ctrl & TD_CTRL_ACTIVE, 0);
    assert_ne!(status_ctrl & TD_CTRL_NAK, 0);
    assert_eq!(status_ctrl & TD_CTRL_STALLED, 0);

    let mut actions = dev.drain_actions();
    assert_eq!(actions.len(), 1, "expected exactly one queued host action");
    let action = actions.pop().unwrap();
    let (id, got_setup) = match action {
        UsbHostAction::ControlIn { id, setup } => (id, setup),
        other => panic!("unexpected action: {other:?}"),
    };
    assert_eq!(
        got_setup,
        HostSetupPacket {
            bm_request_type: setup.bm_request_type,
            b_request: setup.b_request,
            w_value: setup.w_value,
            w_index: setup.w_index,
            w_length: setup.w_length,
        }
    );

    // Frame #2: still pending, no duplicate host actions should be emitted.
    ctrl.tick_1ms(&mut mem);
    let status_ctrl = mem.read_u32(status_td + 4);
    assert_ne!(status_ctrl & TD_CTRL_ACTIVE, 0);
    assert_ne!(status_ctrl & TD_CTRL_NAK, 0);
    assert!(
        dev.drain_actions().is_empty(),
        "expected no duplicate host actions while pending"
    );

    // Complete and ensure the next frame ACKs the status stage (OUT ZLP).
    dev.push_completion(UsbHostCompletion::ControlIn {
        id,
        result: UsbHostCompletionIn::Success {
            // wLength=0; any payload is ignored/truncated.
            data: vec![0xAA, 0xBB],
        },
    });

    ctrl.tick_1ms(&mut mem);

    let status_ctrl = mem.read_u32(status_td + 4);
    assert_eq!(status_ctrl & TD_CTRL_ACTIVE, 0);
    assert_eq!(actlen(status_ctrl), 0);
    assert_eq!(mem.read_u32(qh_addr + 4), LINK_PTR_T);

    assert!(dev.drain_actions().is_empty());
}

#[test]
fn bulk_in_pending_queues_once_and_naks_until_completion() {
    let (mut ctrl, mut mem, mut alloc, fl_base, dev) = setup_controller();

    // First, set device address to 1 (virtualized).
    {
        let setup = SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05,
            w_value: 1,
            w_index: 0,
            w_length: 0,
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

        ctrl.tick_1ms(&mut mem);
        assert!(dev.drain_actions().is_empty());
        assert!(ctrl.hub_mut().device_mut_for_address(1).is_some());
        assert!(ctrl.hub_mut().device_mut_for_address(0).is_none());
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
    ctrl.tick_1ms(&mut mem);
    let ctrl_sts = mem.read_u32(td_addr + 4);
    assert_ne!(ctrl_sts & TD_CTRL_ACTIVE, 0);
    assert_ne!(ctrl_sts & TD_CTRL_NAK, 0);

    let mut actions = dev.drain_actions();
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
    ctrl.tick_1ms(&mut mem);
    let ctrl_sts = mem.read_u32(td_addr + 4);
    assert_ne!(ctrl_sts & TD_CTRL_ACTIVE, 0);
    assert_ne!(ctrl_sts & TD_CTRL_NAK, 0);
    assert!(
        dev.drain_actions().is_empty(),
        "expected no duplicate BulkIn actions while in-flight"
    );

    // Inject completion with more bytes than requested; device should truncate to TD length.
    dev.push_completion(UsbHostCompletion::BulkIn {
        id,
        result: UsbHostCompletionIn::Success {
            data: vec![0xAA, 0xBB, 0xCC, 0xDD, 1, 2, 3, 4, 5, 6, 7],
        },
    });

    // Frame #3: TD completes and buffer contains first 8 bytes.
    ctrl.tick_1ms(&mut mem);
    let ctrl_sts = mem.read_u32(td_addr + 4);
    assert_eq!(ctrl_sts & TD_CTRL_ACTIVE, 0);
    assert_eq!(actlen(ctrl_sts), 8);

    let mut got = [0u8; 8];
    mem.read(buf_addr, &mut got);
    assert_eq!(&got, &[0xAA, 0xBB, 0xCC, 0xDD, 1, 2, 3, 4]);

    assert!(
        dev.drain_actions().is_empty(),
        "expected no extra actions after completion"
    );
    assert_eq!(mem.read_u32(qh_addr + 4), LINK_PTR_T);
}

#[test]
fn bulk_out_pending_queues_once_and_naks_until_completion() {
    let (mut ctrl, mut mem, mut alloc, fl_base, dev) = setup_controller();

    // First, set device address to 1 (virtualized).
    {
        let setup = SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05,
            w_value: 1,
            w_index: 0,
            w_length: 0,
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

        ctrl.tick_1ms(&mut mem);
        assert!(dev.drain_actions().is_empty());
        assert!(ctrl.hub_mut().device_mut_for_address(1).is_some());
        assert!(ctrl.hub_mut().device_mut_for_address(0).is_none());
    }

    let payload = [0xDEu8, 0xAD, 0xBE, 0xEF];

    // Schedule a bulk OUT TD to endpoint 2 (ep addr 0x02), length = payload length.
    let qh_addr = alloc.alloc(0x20, 0x10);
    let td_addr = alloc.alloc(0x20, 0x10);
    let buf_addr = alloc.alloc(payload.len() as u32, 0x10);

    mem.write(buf_addr, &payload);
    write_td(
        &mut mem,
        td_addr,
        LINK_PTR_T,
        td_ctrl(true, true),
        td_token(0xE1, 1, 2, false, payload.len()),
        buf_addr,
    );
    write_qh(&mut mem, qh_addr, LINK_PTR_T, td_addr);
    install_frame_list(&mut mem, fl_base, qh_addr);

    // Frame #1: TD should NAK and emit one BulkOut host action.
    ctrl.tick_1ms(&mut mem);
    let ctrl_sts = mem.read_u32(td_addr + 4);
    assert_ne!(ctrl_sts & TD_CTRL_ACTIVE, 0);
    assert_ne!(ctrl_sts & TD_CTRL_NAK, 0);

    let mut actions = dev.drain_actions();
    assert_eq!(actions.len(), 1, "expected exactly one BulkOut action");
    let action = actions.pop().unwrap();
    let id = match action {
        UsbHostAction::BulkOut { id, endpoint, data } => {
            assert_eq!(endpoint, 0x02);
            assert_eq!(data, payload);
            id
        }
        other => panic!("unexpected action: {other:?}"),
    };

    // Frame #2 without completion: still NAK, and no duplicate action should be queued.
    ctrl.tick_1ms(&mut mem);
    let ctrl_sts = mem.read_u32(td_addr + 4);
    assert_ne!(ctrl_sts & TD_CTRL_ACTIVE, 0);
    assert_ne!(ctrl_sts & TD_CTRL_NAK, 0);
    assert!(
        dev.drain_actions().is_empty(),
        "expected no duplicate BulkOut actions while in-flight"
    );

    dev.push_completion(UsbHostCompletion::BulkOut {
        id,
        result: UsbHostCompletionOut::Success {
            bytes_written: payload.len() as u32,
        },
    });

    // Frame #3: TD completes.
    ctrl.tick_1ms(&mut mem);
    let ctrl_sts = mem.read_u32(td_addr + 4);
    assert_eq!(ctrl_sts & TD_CTRL_ACTIVE, 0);
    assert_eq!(actlen(ctrl_sts), payload.len());
    assert!(
        dev.drain_actions().is_empty(),
        "expected no extra actions after completion"
    );
    assert_eq!(mem.read_u32(qh_addr + 4), LINK_PTR_T);
}
