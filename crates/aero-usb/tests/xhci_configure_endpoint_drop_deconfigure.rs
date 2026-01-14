mod util;

use aero_usb::xhci::context::{EndpointContext, SlotContext, CONTEXT_SIZE};
use aero_usb::xhci::trb::{CompletionCode, Trb, TrbType, TRB_LEN};
use aero_usb::xhci::XhciController;
use aero_usb::{ControlResponse, MemoryBus, SetupPacket, UsbDeviceModel, UsbInResult};

use util::{xhci_set_run, Alloc, TestMemory};

#[derive(Clone, Debug)]
struct AlwaysInDevice;

impl UsbDeviceModel for AlwaysInDevice {
    fn handle_control_request(
        &mut self,
        _setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        ControlResponse::Stall
    }

    fn handle_in_transfer(&mut self, ep_addr: u8, max_len: usize) -> UsbInResult {
        if ep_addr != 0x81 {
            return UsbInResult::Stall;
        }
        let mut data = vec![0x11u8, 0x22, 0x33, 0x44];
        data.truncate(max_len);
        UsbInResult::Data(data)
    }
}

fn write_stop_marker(mem: &mut TestMemory, addr: u64) {
    let mut trb = Trb::default();
    trb.set_trb_type(TrbType::NoOpCommand);
    trb.set_cycle(false);
    trb.write_to(mem, addr);
}

fn build_address_device_input_ctx(mem: &mut TestMemory, input_ctx: u64) {
    // Input Control Context (ICC): Drop=0, Add = Slot + EP0.
    MemoryBus::write_u32(mem, input_ctx, 0);
    MemoryBus::write_u32(mem, input_ctx + 0x04, (1 << 0) | (1 << 1));
    // Slot Context Root Hub Port Number = 1.
    MemoryBus::write_u32(mem, input_ctx + 0x20 + 4, 1 << 16);
}

fn build_interrupt_in_input_ctx(
    mem: &mut TestMemory,
    input_ctx: u64,
    endpoint_id: u8,
    tr_ring: u64,
) {
    // Input Control Context (ICC): Add only the endpoint context.
    MemoryBus::write_u32(mem, input_ctx, 0);
    MemoryBus::write_u32(mem, input_ctx + 0x04, 1u32 << endpoint_id);

    // Endpoint context index in Input Context is DCI + 1.
    let ep_off = u64::from(endpoint_id + 1) * (CONTEXT_SIZE as u64);
    let ep_ctx = input_ctx + ep_off;

    // Endpoint type = Interrupt IN (7), MPS = 8.
    MemoryBus::write_u32(mem, ep_ctx + 4, (7u32 << 3) | (8u32 << 16));
    // TR Dequeue Pointer (DCS=1).
    let trdp_raw = (tr_ring & !0x0f) | 1;
    MemoryBus::write_u32(mem, ep_ctx + 8, trdp_raw as u32);
    MemoryBus::write_u32(mem, ep_ctx + 12, (trdp_raw >> 32) as u32);
}

fn build_drop_input_ctx(mem: &mut TestMemory, input_ctx: u64, endpoint_id: u8) {
    // Drop the specified endpoint context, add nothing.
    MemoryBus::write_u32(mem, input_ctx, 1u32 << endpoint_id);
    MemoryBus::write_u32(mem, input_ctx + 0x04, 0);
}

fn build_empty_input_ctx(mem: &mut TestMemory, input_ctx: u64) {
    // ICC Drop=0, Add=0.
    MemoryBus::write_u32(mem, input_ctx, 0);
    MemoryBus::write_u32(mem, input_ctx + 0x04, 0);
}

fn write_interrupt_in_trb(mem: &mut TestMemory, addr: u64, buf: u64, len: u32) {
    let mut trb = Trb::new(buf, len & Trb::STATUS_TRANSFER_LEN_MASK, 0);
    trb.set_trb_type(TrbType::Normal);
    trb.set_cycle(true);
    trb.control |= Trb::CONTROL_IOC_BIT;
    trb.write_to(mem, addr);
}

fn write_cycle_mismatch_marker(mem: &mut TestMemory, addr: u64) {
    let mut trb = Trb::default();
    trb.set_trb_type(TrbType::NoOp);
    trb.set_cycle(false);
    trb.write_to(mem, addr);
}

#[test]
fn configure_endpoint_drop_flags_disables_endpoint_and_blocks_transfers() {
    let mut mem = TestMemory::new(0x40_000);
    let mut alloc = Alloc::new(0x1000);

    let dcbaa = alloc.alloc(0x200, 0x40) as u64;
    let dev_ctx = alloc.alloc(0x800, 0x40) as u64;
    let cmd_ring = alloc.alloc(0x100, 0x10) as u64;

    let input_ctx_addr = alloc.alloc(0x200, 0x40) as u64;
    let input_ctx_cfg = alloc.alloc(0x200, 0x40) as u64;
    let input_ctx_drop = alloc.alloc(0x200, 0x40) as u64;

    let transfer_ring = alloc.alloc(3 * (TRB_LEN as u32), 0x10) as u64;
    let buf1 = alloc.alloc(8, 0x10) as u64;
    let buf2 = alloc.alloc(8, 0x10) as u64;

    let mut xhci = XhciController::with_port_count(1);
    xhci.set_dcbaap(dcbaa);
    xhci.set_command_ring(cmd_ring, true);
    xhci.attach_device(0, Box::new(AlwaysInDevice));
    while xhci.pop_pending_event().is_some() {}
    xhci_set_run(&mut xhci);
    // Enable Slot (TRB0).
    {
        let mut trb = Trb::default();
        trb.set_trb_type(TrbType::EnableSlotCommand);
        trb.set_cycle(true);
        trb.write_to(&mut mem, cmd_ring);
    }
    write_stop_marker(&mut mem, cmd_ring + TRB_LEN as u64);

    xhci.process_command_ring(&mut mem, 8);
    let cc0 = xhci
        .pop_pending_event()
        .expect("expected Enable Slot completion");
    assert_eq!(cc0.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(cc0.completion_code_raw(), CompletionCode::Success.as_u8());
    let slot_id = cc0.slot_id();
    assert_ne!(slot_id, 0);

    // Install Device Context pointer.
    mem.write_u64(dcbaa + u64::from(slot_id) * 8, dev_ctx);

    // Address Device (TRB1).
    build_address_device_input_ctx(&mut mem, input_ctx_addr);
    {
        let mut trb = Trb::new(input_ctx_addr, 0, 0);
        trb.set_trb_type(TrbType::AddressDeviceCommand);
        trb.set_slot_id(slot_id);
        trb.set_cycle(true);
        trb.write_to(&mut mem, cmd_ring + TRB_LEN as u64);
    }
    write_stop_marker(&mut mem, cmd_ring + 2 * TRB_LEN as u64);

    xhci.process_command_ring(&mut mem, 8);
    let cc1 = xhci
        .pop_pending_event()
        .expect("expected Address Device completion");
    assert_eq!(cc1.completion_code_raw(), CompletionCode::Success.as_u8());

    // Configure Endpoint (add EP1 IN, DCI=3) (TRB2).
    let endpoint_id = 3u8;
    build_interrupt_in_input_ctx(&mut mem, input_ctx_cfg, endpoint_id, transfer_ring);
    {
        let mut trb = Trb::new(input_ctx_cfg, 0, 0);
        trb.set_trb_type(TrbType::ConfigureEndpointCommand);
        trb.set_slot_id(slot_id);
        trb.set_cycle(true);
        trb.write_to(&mut mem, cmd_ring + 2 * TRB_LEN as u64);
    }
    write_stop_marker(&mut mem, cmd_ring + 3 * TRB_LEN as u64);

    xhci.process_command_ring(&mut mem, 8);
    let cc2 = xhci
        .pop_pending_event()
        .expect("expected Configure Endpoint completion");
    assert_eq!(cc2.completion_code_raw(), CompletionCode::Success.as_u8());

    // Transfer ring: two ready Normal TRBs, then cycle-mismatch marker.
    write_interrupt_in_trb(&mut mem, transfer_ring, buf1, 4);
    write_interrupt_in_trb(&mut mem, transfer_ring + TRB_LEN as u64, buf2, 4);
    write_cycle_mismatch_marker(&mut mem, transfer_ring + 2 * TRB_LEN as u64);

    // Ring doorbell once and execute first transfer.
    xhci.ring_doorbell(slot_id, endpoint_id);
    xhci.tick_1ms(&mut mem);

    assert_eq!(
        &mem.data[buf1 as usize..buf1 as usize + 4],
        &[0x11, 0x22, 0x33, 0x44]
    );

    let tev0 = xhci.pop_pending_event().expect("expected transfer event");
    assert_eq!(tev0.trb_type(), TrbType::TransferEvent);
    assert_eq!(tev0.slot_id(), slot_id);
    assert_eq!(tev0.endpoint_id(), endpoint_id);
    assert_eq!(tev0.completion_code_raw(), CompletionCode::Success.as_u8());

    // Configure Endpoint (drop EP1 IN) (TRB3).
    build_drop_input_ctx(&mut mem, input_ctx_drop, endpoint_id);
    {
        let mut trb = Trb::new(input_ctx_drop, 0, 0);
        trb.set_trb_type(TrbType::ConfigureEndpointCommand);
        trb.set_slot_id(slot_id);
        trb.set_cycle(true);
        trb.write_to(&mut mem, cmd_ring + 3 * TRB_LEN as u64);
    }
    write_stop_marker(&mut mem, cmd_ring + 4 * TRB_LEN as u64);

    xhci.process_command_ring(&mut mem, 8);
    let cc3 = xhci
        .pop_pending_event()
        .expect("expected drop Configure Endpoint completion");
    assert_eq!(cc3.completion_code_raw(), CompletionCode::Success.as_u8());

    // Endpoint context should now be cleared/disabled in guest memory and controller state.
    let ep_ctx_paddr = dev_ctx + u64::from(endpoint_id) * (CONTEXT_SIZE as u64);
    let ep_ctx = EndpointContext::read_from(&mut mem, ep_ctx_paddr);
    assert_eq!(ep_ctx.endpoint_state(), 0);
    assert_eq!(ep_ctx.tr_dequeue_pointer(), 0);
    assert!(
        xhci.slot_state(slot_id)
            .and_then(|s| s.transfer_ring(endpoint_id))
            .is_none(),
        "controller-local ring cursor should be cleared for dropped endpoints"
    );

    // Ring doorbell again: endpoint is disabled so the second TRB must not execute.
    xhci.ring_doorbell(slot_id, endpoint_id);
    xhci.tick_1ms(&mut mem);
    assert_eq!(&mem.data[buf2 as usize..buf2 as usize + 4], &[0, 0, 0, 0]);
    assert!(
        xhci.pop_pending_event().is_none(),
        "dropped endpoints must not emit transfer events"
    );
}

#[test]
fn configure_endpoint_deconfigure_disables_all_non_ep0_endpoints() {
    let mut mem = TestMemory::new(0x40_000);
    let mut alloc = Alloc::new(0x1000);

    let dcbaa = alloc.alloc(0x200, 0x40) as u64;
    let dev_ctx = alloc.alloc(0x800, 0x40) as u64;
    let cmd_ring = alloc.alloc(0x100, 0x10) as u64;

    let input_ctx_addr = alloc.alloc(0x200, 0x40) as u64;
    let input_ctx_cfg = alloc.alloc(0x200, 0x40) as u64;
    let input_ctx_deconfig = alloc.alloc(0x200, 0x40) as u64;

    let transfer_ring = alloc.alloc(3 * (TRB_LEN as u32), 0x10) as u64;
    let buf1 = alloc.alloc(8, 0x10) as u64;
    let buf2 = alloc.alloc(8, 0x10) as u64;

    let mut xhci = XhciController::with_port_count(1);
    xhci.set_dcbaap(dcbaa);
    xhci.set_command_ring(cmd_ring, true);
    xhci.attach_device(0, Box::new(AlwaysInDevice));
    while xhci.pop_pending_event().is_some() {}
    xhci_set_run(&mut xhci);
    // Enable Slot.
    {
        let mut trb = Trb::default();
        trb.set_trb_type(TrbType::EnableSlotCommand);
        trb.set_cycle(true);
        trb.write_to(&mut mem, cmd_ring);
    }
    write_stop_marker(&mut mem, cmd_ring + TRB_LEN as u64);
    xhci.process_command_ring(&mut mem, 8);
    let cc0 = xhci
        .pop_pending_event()
        .expect("expected Enable Slot completion");
    assert_eq!(cc0.completion_code_raw(), CompletionCode::Success.as_u8());
    let slot_id = cc0.slot_id();
    assert_ne!(slot_id, 0);

    mem.write_u64(dcbaa + u64::from(slot_id) * 8, dev_ctx);

    // Address Device.
    build_address_device_input_ctx(&mut mem, input_ctx_addr);
    {
        let mut trb = Trb::new(input_ctx_addr, 0, 0);
        trb.set_trb_type(TrbType::AddressDeviceCommand);
        trb.set_slot_id(slot_id);
        trb.set_cycle(true);
        trb.write_to(&mut mem, cmd_ring + TRB_LEN as u64);
    }
    write_stop_marker(&mut mem, cmd_ring + 2 * TRB_LEN as u64);
    xhci.process_command_ring(&mut mem, 8);
    let cc1 = xhci
        .pop_pending_event()
        .expect("expected Address Device completion");
    assert_eq!(cc1.completion_code_raw(), CompletionCode::Success.as_u8());

    // Configure Endpoint (add EP1 IN, DCI=3).
    let endpoint_id = 3u8;
    build_interrupt_in_input_ctx(&mut mem, input_ctx_cfg, endpoint_id, transfer_ring);
    {
        let mut trb = Trb::new(input_ctx_cfg, 0, 0);
        trb.set_trb_type(TrbType::ConfigureEndpointCommand);
        trb.set_slot_id(slot_id);
        trb.set_cycle(true);
        trb.write_to(&mut mem, cmd_ring + 2 * TRB_LEN as u64);
    }
    write_stop_marker(&mut mem, cmd_ring + 3 * TRB_LEN as u64);
    xhci.process_command_ring(&mut mem, 8);
    let cc2 = xhci
        .pop_pending_event()
        .expect("expected Configure Endpoint completion");
    assert_eq!(cc2.completion_code_raw(), CompletionCode::Success.as_u8());

    write_interrupt_in_trb(&mut mem, transfer_ring, buf1, 4);
    write_interrupt_in_trb(&mut mem, transfer_ring + TRB_LEN as u64, buf2, 4);
    write_cycle_mismatch_marker(&mut mem, transfer_ring + 2 * TRB_LEN as u64);

    xhci.ring_doorbell(slot_id, endpoint_id);
    xhci.tick_1ms(&mut mem);

    assert_eq!(
        &mem.data[buf1 as usize..buf1 as usize + 4],
        &[0x11, 0x22, 0x33, 0x44]
    );
    let tev0 = xhci.pop_pending_event().expect("expected transfer event");
    assert_eq!(tev0.trb_type(), TrbType::TransferEvent);

    // Deconfigure via Configure Endpoint with the Deconfigure flag set.
    build_empty_input_ctx(&mut mem, input_ctx_deconfig);
    {
        let mut trb = Trb::new(input_ctx_deconfig, 0, 0);
        trb.set_trb_type(TrbType::ConfigureEndpointCommand);
        trb.set_slot_id(slot_id);
        trb.set_cycle(true);
        trb.set_configure_endpoint_deconfigure(true);
        trb.write_to(&mut mem, cmd_ring + 3 * TRB_LEN as u64);
    }
    write_stop_marker(&mut mem, cmd_ring + 4 * TRB_LEN as u64);
    xhci.process_command_ring(&mut mem, 8);
    let cc3 = xhci
        .pop_pending_event()
        .expect("expected deconfigure completion");
    assert_eq!(cc3.completion_code_raw(), CompletionCode::Success.as_u8());

    // Endpoint context should be cleared/disabled and Slot Context Context Entries should be 1.
    let ep_ctx_paddr = dev_ctx + u64::from(endpoint_id) * (CONTEXT_SIZE as u64);
    let ep_ctx = EndpointContext::read_from(&mut mem, ep_ctx_paddr);
    assert_eq!(ep_ctx.endpoint_state(), 0);
    assert_eq!(ep_ctx.tr_dequeue_pointer(), 0);

    let slot_ctx = SlotContext::read_from(&mut mem, dev_ctx);
    assert_eq!(slot_ctx.context_entries(), 1);
    assert_eq!(
        xhci.slot_state(slot_id)
            .expect("slot should still be enabled")
            .slot_context()
            .context_entries(),
        1
    );

    // Even though the next TRB is ready, deconfigured endpoints must not execute.
    xhci.ring_doorbell(slot_id, endpoint_id);
    xhci.tick_1ms(&mut mem);
    assert_eq!(&mem.data[buf2 as usize..buf2 as usize + 4], &[0, 0, 0, 0]);
    assert!(
        xhci.pop_pending_event().is_none(),
        "deconfigured endpoints must not emit transfer events"
    );
}
