mod util;

use aero_usb::xhci::context::SlotContext;
use aero_usb::xhci::context::{EndpointContext, CONTEXT_SIZE};
use aero_usb::xhci::trb::{CompletionCode, Trb, TrbType, TRB_LEN};
use aero_usb::xhci::{CommandCompletionCode, XhciController};
use aero_usb::MemoryBus;

use util::{xhci_set_run, Alloc, TestMemory};

use aero_usb::{ControlResponse, SetupPacket, UsbDeviceModel, UsbInResult};

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

#[test]
fn endpoint_commands_update_context_and_transfer_ring() {
    let mut mem = TestMemory::new(0x20_000);
    let mut alloc = Alloc::new(0x1000);

    let dcbaa = alloc.alloc(0x200, 0x40) as u64;
    let dev_ctx = alloc.alloc(0x400, 0x40) as u64;
    let cmd_ring = alloc.alloc(0x100, 0x40) as u64;
    let old_trdp = alloc.alloc(0x100, 0x10) as u64;
    let new_trdp = alloc.alloc(0x100, 0x10) as u64;

    let endpoint_id = 2u8; // EP1 OUT (Device Context index 2).

    let mut xhci = XhciController::new();
    xhci.set_dcbaap(dcbaa);
    xhci.set_command_ring(cmd_ring, true);
    xhci_set_run(&mut xhci);
    // Enable Slot.
    {
        let mut trb = Trb::default();
        trb.set_cycle(true);
        trb.set_trb_type(TrbType::EnableSlotCommand);
        trb.write_to(&mut mem, cmd_ring);
    }
    // Stop marker (cycle mismatch).
    {
        let mut trb = Trb::default();
        trb.set_cycle(false);
        trb.set_trb_type(TrbType::NoOpCommand);
        trb.write_to(&mut mem, cmd_ring + TRB_LEN as u64);
    }

    xhci.process_command_ring(&mut mem, 8);
    let evt0 = xhci.pop_pending_event().expect("enable-slot completion");
    assert_eq!(evt0.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(evt0.completion_code_raw(), CompletionCode::Success.as_u8());
    let slot_id = evt0.slot_id();
    assert_ne!(slot_id, 0);

    // Install Device Context pointer (simulates guest setup between Enable Slot and endpoint commands).
    mem.write_u64(dcbaa + (slot_id as u64) * 8, dev_ctx);

    // Seed endpoint context state + initial dequeue pointer.
    let mut ep_ctx = EndpointContext::default();
    ep_ctx.set_endpoint_state(1); // Running
    ep_ctx.set_tr_dequeue_pointer(old_trdp, true);
    ep_ctx.write_to(
        &mut mem,
        dev_ctx + (endpoint_id as u64) * (CONTEXT_SIZE as u64),
    );

    // Command ring:
    //  - Stop Endpoint
    //  - Set TR Dequeue Pointer
    //  - Reset Endpoint
    {
        let mut trb = Trb::default();
        trb.set_cycle(true);
        trb.set_trb_type(TrbType::StopEndpointCommand);
        trb.set_slot_id(slot_id);
        trb.set_endpoint_id(endpoint_id);
        trb.write_to(&mut mem, cmd_ring + TRB_LEN as u64);
    }
    {
        let mut trb = Trb::new(new_trdp, 0, 0); // DCS=0
        trb.set_cycle(true);
        trb.set_trb_type(TrbType::SetTrDequeuePointerCommand);
        trb.set_slot_id(slot_id);
        trb.set_endpoint_id(endpoint_id);
        trb.write_to(&mut mem, cmd_ring + 2 * TRB_LEN as u64);
    }
    {
        let mut trb = Trb::default();
        trb.set_cycle(true);
        trb.set_trb_type(TrbType::ResetEndpointCommand);
        trb.set_slot_id(slot_id);
        trb.set_endpoint_id(endpoint_id);
        trb.write_to(&mut mem, cmd_ring + 3 * TRB_LEN as u64);
    }
    {
        let mut trb = Trb::default();
        trb.set_cycle(false);
        trb.set_trb_type(TrbType::NoOpCommand);
        trb.write_to(&mut mem, cmd_ring + 4 * TRB_LEN as u64);
    }

    // Process Stop Endpoint + Set TR Dequeue Pointer.
    xhci.process_command_ring(&mut mem, 2);
    let evt1 = xhci.pop_pending_event().expect("stop-endpoint completion");
    let evt2 = xhci
        .pop_pending_event()
        .expect("set-tr-dequeue-pointer completion");

    assert_eq!(evt1.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(evt1.completion_code_raw(), CompletionCode::Success.as_u8());
    assert_eq!(evt1.slot_id(), slot_id);
    assert_eq!(evt1.parameter & !0x0f, cmd_ring + TRB_LEN as u64);

    assert_eq!(evt2.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(evt2.completion_code_raw(), CompletionCode::Success.as_u8());
    assert_eq!(evt2.slot_id(), slot_id);
    assert_eq!(evt2.parameter & !0x0f, cmd_ring + 2 * TRB_LEN as u64);

    // Stop Endpoint should set endpoint state to Stopped (3), and Set TRDP should update dequeue ptr + DCS.
    let ep_ctx_out = EndpointContext::read_from(
        &mut mem,
        dev_ctx + (endpoint_id as u64) * (CONTEXT_SIZE as u64),
    );
    assert_eq!(ep_ctx_out.endpoint_state(), 3);
    assert_eq!(ep_ctx_out.tr_dequeue_pointer(), new_trdp);
    assert!(!ep_ctx_out.dcs());

    // Controller-local transfer ring cursor should be updated by Set TR Dequeue Pointer.
    let ring = xhci
        .slot_state(slot_id)
        .and_then(|s| s.transfer_ring(endpoint_id))
        .expect("endpoint ring cursor should be installed");
    assert_eq!(ring.dequeue_ptr(), new_trdp);
    assert!(!ring.cycle_state());

    // Simulate a halt, then process Reset Endpoint.
    let mut halted = ep_ctx_out;
    halted.set_endpoint_state(2);
    halted.write_to(
        &mut mem,
        dev_ctx + (endpoint_id as u64) * (CONTEXT_SIZE as u64),
    );

    xhci.process_command_ring(&mut mem, 1);
    let evt3 = xhci.pop_pending_event().expect("reset-endpoint completion");
    assert_eq!(evt3.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(evt3.completion_code_raw(), CompletionCode::Success.as_u8());
    assert_eq!(evt3.slot_id(), slot_id);
    assert_eq!(evt3.parameter & !0x0f, cmd_ring + 3 * TRB_LEN as u64);

    let ep_ctx_reset = EndpointContext::read_from(
        &mut mem,
        dev_ctx + (endpoint_id as u64) * (CONTEXT_SIZE as u64),
    );
    assert_eq!(ep_ctx_reset.endpoint_state(), 1);
    assert_eq!(ep_ctx_reset.tr_dequeue_pointer(), new_trdp);
}

#[test]
fn stop_endpoint_disabled_endpoint_returns_endpoint_not_enabled_error() {
    let mut mem = TestMemory::new(0x20_000);
    let mut alloc = Alloc::new(0x1000);

    let dcbaa = alloc.alloc(0x200, 0x40) as u64;
    let dev_ctx = alloc.alloc(0x400, 0x40) as u64;
    let cmd_ring = alloc.alloc(0x100, 0x40) as u64;

    let endpoint_id = 2u8;

    let mut xhci = XhciController::new();
    xhci.set_dcbaap(dcbaa);
    xhci.set_command_ring(cmd_ring, true);
    xhci_set_run(&mut xhci);
    // Enable Slot.
    {
        let mut trb = Trb::default();
        trb.set_cycle(true);
        trb.set_trb_type(TrbType::EnableSlotCommand);
        trb.write_to(&mut mem, cmd_ring);
    }
    {
        let mut trb = Trb::default();
        trb.set_cycle(false);
        trb.set_trb_type(TrbType::NoOpCommand);
        trb.write_to(&mut mem, cmd_ring + TRB_LEN as u64);
    }

    xhci.process_command_ring(&mut mem, 8);
    let evt0 = xhci.pop_pending_event().expect("enable-slot completion");
    let slot_id = evt0.slot_id();
    assert_ne!(slot_id, 0);

    mem.write_u64(dcbaa + (slot_id as u64) * 8, dev_ctx);

    // Endpoint context is left Disabled (0).
    let ep_ctx_paddr = dev_ctx + (endpoint_id as u64) * (CONTEXT_SIZE as u64);
    let ep_ctx = EndpointContext::default();
    ep_ctx.write_to(&mut mem, ep_ctx_paddr);

    // Stop Endpoint command.
    {
        let mut trb = Trb::default();
        trb.set_cycle(true);
        trb.set_trb_type(TrbType::StopEndpointCommand);
        trb.set_slot_id(slot_id);
        trb.set_endpoint_id(endpoint_id);
        trb.write_to(&mut mem, cmd_ring + TRB_LEN as u64);
    }

    // Stop marker.
    {
        let mut trb = Trb::default();
        trb.set_cycle(false);
        trb.set_trb_type(TrbType::NoOpCommand);
        trb.write_to(&mut mem, cmd_ring + 2 * TRB_LEN as u64);
    }

    xhci.process_command_ring(&mut mem, 8);
    let evt1 = xhci.pop_pending_event().expect("stop-endpoint completion");
    assert_eq!(evt1.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(
        evt1.completion_code_raw(),
        CompletionCode::EndpointNotEnabledError.as_u8()
    );
    assert_eq!(evt1.slot_id(), slot_id);

    let ep_ctx_after = EndpointContext::read_from(&mut mem, ep_ctx_paddr);
    assert_eq!(ep_ctx_after.endpoint_state(), 0);
}

#[test]
fn stop_endpoint_gates_transfer_execution_until_reset() {
    let mut mem = TestMemory::new(0x40_000);
    let mut alloc = Alloc::new(0x1000);

    let dcbaa = alloc.alloc(0x200, 0x40) as u64;
    let dev_ctx = alloc.alloc(0x400, 0x40) as u64;
    let cmd_ring = alloc.alloc(0x100, 0x10) as u64;
    let transfer_ring = alloc.alloc(3 * (TRB_LEN as u32), 0x10) as u64;
    let buf1 = alloc.alloc(8, 0x10) as u64;
    let buf2 = alloc.alloc(8, 0x10) as u64;

    let mut xhci = XhciController::with_port_count(1);
    xhci.set_dcbaap(dcbaa);
    xhci.attach_device(0, Box::new(AlwaysInDevice));
    while xhci.pop_pending_event().is_some() {}
    xhci_set_run(&mut xhci);
    let completion = xhci.enable_slot(&mut mem);
    assert_eq!(completion.completion_code, CommandCompletionCode::Success);
    let slot_id = completion.slot_id;
    assert_ne!(slot_id, 0);

    mem.write_u64(dcbaa + (u64::from(slot_id) * 8), dev_ctx);

    let mut slot_ctx = SlotContext::default();
    slot_ctx.set_root_hub_port_number(1);
    let completion = xhci.address_device(slot_id, slot_ctx);
    assert_eq!(completion.completion_code, CommandCompletionCode::Success);

    // Endpoint 1 IN (DCI=3).
    let endpoint_id = 3u8;
    let ep_ctx_paddr = dev_ctx + u64::from(endpoint_id) * (CONTEXT_SIZE as u64);

    // Endpoint state = Running (1).
    MemoryBus::write_u32(&mut mem, ep_ctx_paddr, 1);
    // Endpoint type = Interrupt IN (7), MPS = 8.
    MemoryBus::write_u32(&mut mem, ep_ctx_paddr + 4, (7u32 << 3) | (8u32 << 16));
    // TR Dequeue Pointer (DCS=1).
    let trdp_raw = (transfer_ring & !0x0f) | 1;
    MemoryBus::write_u32(&mut mem, ep_ctx_paddr + 8, trdp_raw as u32);
    MemoryBus::write_u32(&mut mem, ep_ctx_paddr + 12, (trdp_raw >> 32) as u32);

    // Two Normal TRBs, then a cycle-mismatch marker.
    let mut trb0 = Trb::new(buf1, 4 & Trb::STATUS_TRANSFER_LEN_MASK, 0);
    trb0.set_trb_type(TrbType::Normal);
    trb0.set_cycle(true);
    trb0.control |= Trb::CONTROL_IOC_BIT;
    trb0.write_to(&mut mem, transfer_ring);

    let mut trb1 = Trb::new(buf2, 4 & Trb::STATUS_TRANSFER_LEN_MASK, 0);
    trb1.set_trb_type(TrbType::Normal);
    trb1.set_cycle(true);
    trb1.control |= Trb::CONTROL_IOC_BIT;
    trb1.write_to(&mut mem, transfer_ring + TRB_LEN as u64);

    let mut stop_marker = Trb::default();
    stop_marker.set_trb_type(TrbType::NoOp);
    stop_marker.set_cycle(false);
    stop_marker.write_to(&mut mem, transfer_ring + 2 * TRB_LEN as u64);

    // Ring doorbell and execute the first transfer.
    xhci.ring_doorbell(slot_id, endpoint_id);
    xhci.tick_1ms(&mut mem);

    assert_eq!(
        &mem.data[buf1 as usize..buf1 as usize + 4],
        &[0x11, 0x22, 0x33, 0x44]
    );

    // Consume the transfer event so subsequent assertions can use `pop_pending_event()` cleanly.
    let ev0 = xhci.pop_pending_event().expect("expected transfer event");
    assert_eq!(ev0.trb_type(), TrbType::TransferEvent);
    assert_eq!(ev0.slot_id(), slot_id);
    assert_eq!(ev0.endpoint_id(), endpoint_id);
    assert_eq!(ev0.completion_code_raw(), CompletionCode::Success.as_u8());

    // Dequeue pointer should have advanced to the second TRB.
    let after_raw = MemoryBus::read_u64(&mut mem, ep_ctx_paddr + 8);
    assert_eq!(after_raw, ((transfer_ring + TRB_LEN as u64) & !0x0f) | 1);

    // Stop Endpoint via command ring.
    xhci.set_command_ring(cmd_ring, true);
    {
        let mut stop = Trb::default();
        stop.set_trb_type(TrbType::StopEndpointCommand);
        stop.set_cycle(true);
        stop.set_slot_id(slot_id);
        stop.set_endpoint_id(endpoint_id);
        stop.write_to(&mut mem, cmd_ring);
    }
    xhci.process_command_ring(&mut mem, 1);
    let ev1 = xhci
        .pop_pending_event()
        .expect("expected Stop Endpoint completion");
    assert_eq!(ev1.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(ev1.completion_code_raw(), CompletionCode::Success.as_u8());

    // Tick again: the second TRB is ready, but the endpoint is stopped so it must not execute.
    xhci.tick_1ms(&mut mem);
    assert_eq!(&mem.data[buf2 as usize..buf2 as usize + 4], &[0, 0, 0, 0]);
    assert!(
        xhci.pop_pending_event().is_none(),
        "stopped endpoints must not emit transfer events"
    );
    let still_raw = MemoryBus::read_u64(&mut mem, ep_ctx_paddr + 8);
    assert_eq!(still_raw, after_raw);

    // Reset Endpoint and retry: execution should resume.
    xhci.set_command_ring(cmd_ring, true);
    {
        let mut reset = Trb::default();
        reset.set_trb_type(TrbType::ResetEndpointCommand);
        reset.set_cycle(true);
        reset.set_slot_id(slot_id);
        reset.set_endpoint_id(endpoint_id);
        reset.write_to(&mut mem, cmd_ring);
    }
    xhci.process_command_ring(&mut mem, 1);
    let ev2 = xhci
        .pop_pending_event()
        .expect("expected Reset Endpoint completion");
    assert_eq!(ev2.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(ev2.completion_code_raw(), CompletionCode::Success.as_u8());

    xhci.ring_doorbell(slot_id, endpoint_id);
    xhci.tick_1ms(&mut mem);
    assert_eq!(
        &mem.data[buf2 as usize..buf2 as usize + 4],
        &[0x11, 0x22, 0x33, 0x44]
    );
    let ev3 = xhci
        .pop_pending_event()
        .expect("expected resumed transfer event");
    assert_eq!(ev3.trb_type(), TrbType::TransferEvent);
    assert_eq!(ev3.completion_code_raw(), CompletionCode::Success.as_u8());
}
