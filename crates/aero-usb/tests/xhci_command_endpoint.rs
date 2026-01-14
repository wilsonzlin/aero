mod util;

use aero_usb::xhci::context::CONTEXT_SIZE;
use aero_usb::xhci::trb::{CompletionCode, Trb, TrbType, TRB_LEN};
use aero_usb::xhci::{CommandCompletionCode, XhciController};
use aero_usb::MemoryBus;

use util::{xhci_set_run, Alloc, TestMemory};

#[test]
fn xhci_command_ring_stop_reset_and_set_trdp_update_context_and_ring_cursor() {
    let mut mem = TestMemory::new(0x20_000);
    let mut alloc = Alloc::new(0x1000);

    let dcbaa = alloc.alloc(0x200, 0x40) as u64;
    let dev_ctx = alloc.alloc(0x400, 0x40) as u64;
    let cmd_ring = alloc.alloc(0x100, 0x10) as u64;
    let new_trdp = alloc.alloc(0x100, 0x10) as u64;

    let mut xhci = XhciController::new();
    xhci.set_dcbaap(dcbaa);
    xhci_set_run(&mut xhci);
    let completion = xhci.enable_slot(&mut mem);
    assert_eq!(completion.completion_code, CommandCompletionCode::Success);
    let slot_id = completion.slot_id;
    assert_ne!(slot_id, 0);

    // Install the Device Context pointer for the slot.
    mem.write_u64(dcbaa + (u64::from(slot_id) * 8), dev_ctx);

    // Seed endpoint context state + dequeue pointer.
    let endpoint_id = 2u8; // EP1 OUT (DCI=2)
    let ep_ctx = dev_ctx + u64::from(endpoint_id) * (CONTEXT_SIZE as u64);
    MemoryBus::write_u32(&mut mem, ep_ctx, 1); // Running
    MemoryBus::write_u32(&mut mem, ep_ctx + 8, 0x1110 | 1); // TR Dequeue Pointer low (DCS=1)
    MemoryBus::write_u32(&mut mem, ep_ctx + 12, 0);

    xhci.set_command_ring(cmd_ring, true);

    // --- Stop Endpoint ---
    let mut stop = Trb::default();
    stop.set_trb_type(TrbType::StopEndpointCommand);
    stop.set_cycle(true);
    stop.set_slot_id(slot_id);
    stop.set_endpoint_id(endpoint_id);
    stop.write_to(&mut mem, cmd_ring);

    xhci.process_command_ring(&mut mem, 1);
    let ev0 = xhci.pop_pending_event().expect("Stop Endpoint completion");
    assert_eq!(ev0.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(ev0.completion_code_raw(), CompletionCode::Success.as_u8());
    assert_eq!(ev0.slot_id(), slot_id);
    assert_eq!(ev0.parameter & !0x0f, cmd_ring);
    assert_eq!(MemoryBus::read_u32(&mut mem, ep_ctx) & 0x7, 3);

    // --- Set TR Dequeue Pointer ---
    let mut set = Trb {
        parameter: new_trdp, // DCS=0
        ..Default::default()
    };
    set.set_trb_type(TrbType::SetTrDequeuePointerCommand);
    set.set_cycle(true);
    set.set_slot_id(slot_id);
    set.set_endpoint_id(endpoint_id);
    set.write_to(&mut mem, cmd_ring + TRB_LEN as u64);

    xhci.process_command_ring(&mut mem, 1);
    let ev1 = xhci
        .pop_pending_event()
        .expect("Set TR Dequeue Pointer completion");
    assert_eq!(ev1.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(ev1.completion_code_raw(), CompletionCode::Success.as_u8());
    assert_eq!(ev1.slot_id(), slot_id);
    assert_eq!(ev1.parameter & !0x0f, cmd_ring + TRB_LEN as u64);

    let dw2 = MemoryBus::read_u32(&mut mem, ep_ctx + 8);
    let dw3 = MemoryBus::read_u32(&mut mem, ep_ctx + 12);
    let raw = (u64::from(dw3) << 32) | u64::from(dw2);
    assert_eq!(raw & !0x0f, new_trdp);
    assert_eq!(raw & 0x01, 0);

    let ring = xhci
        .slot_state(slot_id)
        .unwrap()
        .transfer_ring(endpoint_id)
        .expect("transfer ring cursor should have been created");
    assert_eq!(ring.dequeue_ptr(), new_trdp);
    assert!(!ring.cycle_state());

    // --- Reset Endpoint ---
    let mut reset = Trb::default();
    reset.set_trb_type(TrbType::ResetEndpointCommand);
    reset.set_cycle(true);
    reset.set_slot_id(slot_id);
    reset.set_endpoint_id(endpoint_id);
    reset.write_to(&mut mem, cmd_ring + 2 * TRB_LEN as u64);

    xhci.process_command_ring(&mut mem, 1);
    let ev2 = xhci.pop_pending_event().expect("Reset Endpoint completion");
    assert_eq!(ev2.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(ev2.completion_code_raw(), CompletionCode::Success.as_u8());
    assert_eq!(ev2.slot_id(), slot_id);
    assert_eq!(ev2.parameter & !0x0f, cmd_ring + 2 * TRB_LEN as u64);
    assert_eq!(MemoryBus::read_u32(&mut mem, ep_ctx) & 0x7, 1);
}
