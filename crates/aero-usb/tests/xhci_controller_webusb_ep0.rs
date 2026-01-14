use aero_usb::passthrough::{
    SetupPacket as HostSetupPacket, UsbHostAction, UsbHostCompletion, UsbHostCompletionIn,
};
use aero_usb::xhci::context::SlotContext;
use aero_usb::xhci::context::{EndpointContext, CONTEXT_SIZE};
use aero_usb::xhci::interrupter::IMAN_IE;
use aero_usb::xhci::trb::{CompletionCode, Trb, TrbType, TRB_LEN};
use aero_usb::xhci::{regs, CommandCompletionCode, XhciController};
use aero_usb::{MemoryBus, SetupPacket, UsbWebUsbPassthroughDevice};

mod util;

use util::{Alloc, TestMemory};

fn write_erst_entry(mem: &mut TestMemory, erstba: u64, seg_base: u64, seg_size_trbs: u32) {
    MemoryBus::write_u64(mem, erstba, seg_base);
    MemoryBus::write_u32(mem, erstba + 8, seg_size_trbs);
    MemoryBus::write_u32(mem, erstba + 12, 0);
}

fn setup_packet_bytes(setup: SetupPacket) -> [u8; 8] {
    [
        setup.bm_request_type,
        setup.b_request,
        (setup.w_value & 0x00ff) as u8,
        (setup.w_value >> 8) as u8,
        (setup.w_index & 0x00ff) as u8,
        (setup.w_index >> 8) as u8,
        (setup.w_length & 0x00ff) as u8,
        (setup.w_length >> 8) as u8,
    ]
}

#[test]
fn xhci_controller_ep0_control_in_webusb_nak_keeps_td_pending_and_dequeue_pinned() {
    let mut mem = TestMemory::new(0x20_000);
    let mut alloc = Alloc::new(0x1000);

    // Guest structures.
    let dcbaa = alloc.alloc(0x100, 0x40) as u64;
    let dev_ctx = alloc.alloc(0x400, 0x40) as u64;
    let erstba = alloc.alloc(16, 0x10) as u64;
    let event_ring_base = alloc.alloc((TRB_LEN as u32) * 8, 0x10) as u64;
    let transfer_ring_base = alloc.alloc((TRB_LEN as u32) * 3, 0x10) as u64;
    let data_buf = alloc.alloc(64, 0x10) as u64;

    write_erst_entry(&mut mem, erstba, event_ring_base, 8);

    // Controller + device.
    let dev = UsbWebUsbPassthroughDevice::new();
    let mut xhci = XhciController::new();
    xhci.set_dcbaap(dcbaa);
    xhci.attach_device(0, Box::new(dev.clone()));

    // Drain the initial Port Status Change Event so the event ring contains only our transfer event.
    while xhci.pop_pending_event().is_some() {}

    let completion = xhci.enable_slot(&mut mem);
    assert_eq!(completion.completion_code, CommandCompletionCode::Success);
    let slot_id = completion.slot_id;
    assert_ne!(slot_id, 0);

    // Provide a Device Context so the controller can update the Endpoint Context TR Dequeue Pointer
    // field as it processes the control TD.
    MemoryBus::write_u64(&mut mem, dcbaa + u64::from(slot_id) * 8, dev_ctx);
    let ep0_ctx_paddr = dev_ctx + CONTEXT_SIZE as u64;
    let mut ep0_ctx = EndpointContext::default();
    ep0_ctx.set_endpoint_state(1); // Running
    ep0_ctx.set_tr_dequeue_pointer(transfer_ring_base, true);
    ep0_ctx.write_to(&mut mem, ep0_ctx_paddr);

    let mut slot_ctx = SlotContext::default();
    slot_ctx.set_root_hub_port_number(1);
    let completion = xhci.address_device(slot_id, slot_ctx);
    assert_eq!(completion.completion_code, CommandCompletionCode::Success);

    // Configure interrupter 0 to deliver events into our guest event ring.
    xhci.mmio_write(regs::REG_INTR0_ERSTSZ, 4, 1);
    xhci.mmio_write(regs::REG_INTR0_ERSTBA_LO, 4, erstba);
    xhci.mmio_write(regs::REG_INTR0_ERSTBA_HI, 4, erstba >> 32);
    xhci.mmio_write(regs::REG_INTR0_ERDP_LO, 4, event_ring_base);
    xhci.mmio_write(regs::REG_INTR0_ERDP_HI, 4, event_ring_base >> 32);
    xhci.mmio_write(regs::REG_INTR0_IMAN, 4, u64::from(IMAN_IE));
    xhci.mmio_write(regs::REG_USBCMD, 4, u64::from(regs::USBCMD_RUN));

    // EP0 control-IN GET_DESCRIPTOR via Setup/Data/Status stage TRBs.
    let setup = SetupPacket {
        bm_request_type: 0x80, // DeviceToHost | Standard | Device
        b_request: 0x06,       // GET_DESCRIPTOR
        w_value: 0x0100,       // DEVICE descriptor, index 0
        w_index: 0,
        w_length: 18,
    };

    let mut setup_trb = Trb {
        parameter: u64::from_le_bytes(setup_packet_bytes(setup)),
        status: 8,
        ..Default::default()
    };
    setup_trb.set_cycle(true);
    setup_trb.set_trb_type(TrbType::SetupStage);
    setup_trb.write_to(&mut mem, transfer_ring_base);

    let mut data_trb = Trb {
        parameter: data_buf,
        status: setup.w_length as u32,
        ..Default::default()
    };
    data_trb.set_cycle(true);
    data_trb.set_trb_type(TrbType::DataStage);
    data_trb.set_dir_in(true);
    data_trb.write_to(&mut mem, transfer_ring_base + TRB_LEN as u64);

    let mut status_trb = Trb {
        control: Trb::CONTROL_IOC, // request Transfer Event
        ..Default::default()
    };
    status_trb.set_cycle(true);
    status_trb.set_trb_type(TrbType::StatusStage);
    // DIR=0 (Status OUT) for a control read.
    status_trb.write_to(&mut mem, transfer_ring_base + 2 * TRB_LEN as u64);

    // Initialise guest DMA buffer to a sentinel.
    let sentinel = vec![0xa5u8; setup.w_length as usize];
    mem.write_physical(data_buf, &sentinel);

    // Endpoint 0 uses DCI=1.
    xhci.set_endpoint_ring(slot_id, 1, transfer_ring_base, true);

    // Ring doorbell via MMIO and tick once: SETUP is consumed, DATA stage NAKs, no event yet.
    let doorbell_offset = u64::from(regs::DBOFF_VALUE)
        + u64::from(slot_id) * u64::from(regs::doorbell::DOORBELL_STRIDE);
    xhci.mmio_write(doorbell_offset, 4, 1);
    xhci.tick(&mut mem);

    // xHCI should not advance the architectural dequeue pointer while a control TD is pending.
    let ring = xhci
        .slot_state(slot_id)
        .unwrap()
        .transfer_ring(1)
        .expect("ep0 transfer ring should exist");
    assert_eq!(ring.dequeue_ptr(), transfer_ring_base);
    assert!(ring.cycle_state());
    let ep0_ctx = EndpointContext::read_from(&mut mem, ep0_ctx_paddr);
    assert_eq!(
        ep0_ctx.tr_dequeue_pointer(),
        transfer_ring_base,
        "endpoint context TRDP must remain pinned while a TD is pending"
    );
    assert!(ep0_ctx.dcs());

    // Passthrough model should have queued a single host action.
    let mut actions = dev.drain_actions();
    assert_eq!(actions.len(), 1);
    let action = actions.pop().unwrap();
    let (id, got_setup) = match action {
        UsbHostAction::ControlIn { id, setup } => (id, setup),
        other => panic!("unexpected host action: {other:?}"),
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

    // While NAKed, the controller must not DMA into guest memory or emit a transfer event.
    let mut got = vec![0u8; setup.w_length as usize];
    mem.read_physical(data_buf, &mut got);
    assert_eq!(got, sentinel);
    assert_eq!(xhci.pending_event_count(), 0);
    assert!(!xhci.irq_level());

    // Tick again without a completion: still pending and must not queue a duplicate host action.
    xhci.tick(&mut mem);
    assert_eq!(
        xhci.slot_state(slot_id)
            .unwrap()
            .transfer_ring(1)
            .unwrap()
            .dequeue_ptr(),
        transfer_ring_base
    );
    assert!(dev.drain_actions().is_empty());
    assert_eq!(xhci.pending_event_count(), 0);

    // Provide a deterministic completion payload.
    let payload: Vec<u8> = (0u8..18u8).collect();
    dev.push_completion(UsbHostCompletion::ControlIn {
        id,
        result: UsbHostCompletionIn::Success {
            data: payload.clone(),
        },
    });

    // Tick until the DATA + STATUS stages complete; then service the event ring.
    xhci.tick(&mut mem);
    assert_eq!(
        xhci.pending_event_count(),
        1,
        "transfer event should be queued"
    );
    xhci.service_event_ring(&mut mem);

    // DMA should have written the returned bytes into guest memory.
    mem.read_physical(data_buf, &mut got);
    assert_eq!(got, payload);

    // Architectural dequeue pointer is committed only after the TD completes (StatusStage).
    let ring = xhci
        .slot_state(slot_id)
        .unwrap()
        .transfer_ring(1)
        .expect("ep0 transfer ring should exist");
    assert_eq!(ring.dequeue_ptr(), transfer_ring_base + 3 * TRB_LEN as u64);
    let ep0_ctx = EndpointContext::read_from(&mut mem, ep0_ctx_paddr);
    assert_eq!(
        ep0_ctx.tr_dequeue_pointer(),
        transfer_ring_base + 3 * TRB_LEN as u64,
        "endpoint context TRDP should commit once the TD completes"
    );
    assert!(ep0_ctx.dcs());

    // Verify we got a Transfer Event for the status stage.
    let ev = Trb::read_from(&mut mem, event_ring_base);
    assert_eq!(ev.trb_type(), TrbType::TransferEvent);
    assert_eq!(ev.slot_id(), slot_id);
    assert_eq!(ev.endpoint_id(), 1);
    assert_eq!(
        ev.parameter & !0x0f,
        transfer_ring_base + 2 * TRB_LEN as u64
    );
    assert_eq!(ev.completion_code_raw(), CompletionCode::Success.raw());
    assert_eq!(ev.status & 0x00ff_ffff, 0);

    assert!(xhci.interrupter0().interrupt_pending());
    assert!(xhci.irq_level());
}
