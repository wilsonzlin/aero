mod util;

use aero_usb::xhci::context::{EndpointContext, CONTEXT_SIZE};
use aero_usb::xhci::trb::{CompletionCode, Trb, TrbType, TRB_LEN};
use aero_usb::xhci::XhciController;
use aero_usb::MemoryBus;

use util::{Alloc, TestMemory};

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
        let mut trb = Trb::default();
        trb.parameter = new_trdp; // DCS=0
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
