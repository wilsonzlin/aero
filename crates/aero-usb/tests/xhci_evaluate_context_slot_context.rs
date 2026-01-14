use aero_usb::hid::UsbHidKeyboardHandle;
use aero_usb::xhci::context::{EndpointContext, InputControlContext, SlotContext, CONTEXT_SIZE};
use aero_usb::xhci::trb::{CompletionCode, Trb, TrbType, TRB_LEN};
use aero_usb::xhci::{XhciController};
use aero_usb::MemoryBus;

mod util;

use util::{xhci_set_run, Alloc, TestMemory};

fn write_stop_marker(mem: &mut TestMemory, addr: u64) {
    let mut trb = Trb::default();
    trb.set_trb_type(TrbType::NoOpCommand);
    trb.set_cycle(false);
    trb.write_to(mem, addr);
}

#[test]
fn evaluate_context_preserves_xhc_owned_slot_context_fields() {
    let mut mem = TestMemory::new(0x40_000);
    let mut alloc = Alloc::new(0x1000);

    let cmd_ring = alloc.alloc(0x100, 0x10) as u64;
    let dcbaa = alloc.alloc(0x200, 0x40) as u64;
    let dev_ctx = alloc.alloc(0x400, 0x40) as u64;
    let input_ctx_addr = alloc.alloc(0x200, 0x40) as u64;
    let input_ctx_eval = alloc.alloc(0x200, 0x40) as u64;
    let tr_deq_addr = alloc.alloc(0x100, 0x10) as u64;
    let tr_deq_eval = alloc.alloc(0x100, 0x10) as u64;

    let mut xhci = XhciController::with_port_count(1);
    xhci.attach_device(0, Box::new(UsbHidKeyboardHandle::new()));
    while xhci.pop_pending_event().is_some() {}
    xhci.set_dcbaap(dcbaa);
    xhci.set_command_ring(cmd_ring, true);
    xhci_set_run(&mut xhci);
    // --- Enable Slot (TRB0) ---
    {
        let mut enable = Trb::default();
        enable.set_trb_type(TrbType::EnableSlotCommand);
        enable.set_cycle(true);
        enable.write_to(&mut mem, cmd_ring);
    }
    write_stop_marker(&mut mem, cmd_ring + TRB_LEN as u64);

    xhci.process_command_ring(&mut mem, 8);
    let ev0 = xhci.pop_pending_event().expect("Enable Slot completion");
    assert_eq!(ev0.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(ev0.completion_code_raw(), CompletionCode::Success.as_u8());
    let slot_id = ev0.slot_id();
    assert_ne!(slot_id, 0);

    // Install DCBAA entry for the slot.
    mem.write_u64(dcbaa + u64::from(slot_id) * 8, dev_ctx);

    // --- Address Device (TRB1) ---
    // Input Context: ICC + Slot + EP0.
    {
        let mut icc = InputControlContext::default();
        icc.set_add_flags(0b11);
        icc.write_to(&mut mem, input_ctx_addr);

        let mut slot_ctx = SlotContext::default();
        slot_ctx.set_root_hub_port_number(1);
        slot_ctx.set_context_entries(1);
        slot_ctx.write_to(&mut mem, input_ctx_addr + CONTEXT_SIZE as u64);

        let mut ep0 = EndpointContext::default();
        ep0.set_dword(1, 64u32 << 16); // Max Packet Size
        ep0.set_tr_dequeue_pointer(tr_deq_addr, true);
        ep0.write_to(&mut mem, input_ctx_addr + (2 * CONTEXT_SIZE) as u64);
    }

    {
        let mut addr = Trb::new(input_ctx_addr, 0, 0);
        addr.set_trb_type(TrbType::AddressDeviceCommand);
        addr.set_slot_id(slot_id);
        addr.set_cycle(true);
        addr.write_to(&mut mem, cmd_ring + TRB_LEN as u64);
    }
    write_stop_marker(&mut mem, cmd_ring + 2 * TRB_LEN as u64);

    xhci.process_command_ring(&mut mem, 8);
    let ev1 = xhci.pop_pending_event().expect("Address Device completion");
    assert_eq!(ev1.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(ev1.completion_code_raw(), CompletionCode::Success.as_u8());

    let slot_before = SlotContext::read_from(&mut mem, dev_ctx);
    assert_eq!(slot_before.usb_device_address(), slot_id);
    let speed_before = slot_before.speed();

    // --- Evaluate Context (TRB2) ---
    // Input Context: request Slot + EP0 update, but intentionally leave Speed and USB Device Address
    // unset so the controller must preserve them.
    {
        let mut icc = InputControlContext::default();
        icc.set_add_flags(0b11);
        icc.write_to(&mut mem, input_ctx_eval);

        let mut slot_ctx = SlotContext::default();
        // Deliberately provide bogus/empty topology fields; the controller must preserve the output
        // Slot Context's Route String + Root Hub Port Number.
        slot_ctx.set_route_string(0x1234);
        slot_ctx.set_root_hub_port_number(0);
        slot_ctx.set_context_entries(1);
        slot_ctx.write_to(&mut mem, input_ctx_eval + CONTEXT_SIZE as u64);

        let mut ep0 = EndpointContext::default();
        ep0.set_interval(7);
        ep0.set_max_packet_size(32);
        ep0.set_tr_dequeue_pointer(tr_deq_eval, true);
        ep0.write_to(&mut mem, input_ctx_eval + (2 * CONTEXT_SIZE) as u64);
    }

    {
        let mut eval = Trb::new(input_ctx_eval, 0, 0);
        eval.set_trb_type(TrbType::EvaluateContextCommand);
        eval.set_slot_id(slot_id);
        eval.set_cycle(true);
        eval.write_to(&mut mem, cmd_ring + 2 * TRB_LEN as u64);
    }
    write_stop_marker(&mut mem, cmd_ring + 3 * TRB_LEN as u64);

    xhci.process_command_ring(&mut mem, 8);
    let ev2 = xhci
        .pop_pending_event()
        .expect("Evaluate Context completion");
    assert_eq!(ev2.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(ev2.completion_code_raw(), CompletionCode::Success.as_u8());

    let slot_after = SlotContext::read_from(&mut mem, dev_ctx);
    assert_eq!(
        slot_after.usb_device_address(),
        slot_id,
        "Evaluate Context must preserve the xHC-owned USB device address"
    );
    assert_eq!(
        slot_after.speed(),
        speed_before,
        "Evaluate Context must preserve the xHC-owned speed field"
    );
    assert_eq!(slot_after.root_hub_port_number(), 1);
    assert_eq!(slot_after.route_string(), 0);

    // Also verify the controller-local Slot Context mirror preserved the same xHC-owned fields.
    let slot_state = xhci.slot_state(slot_id).expect("slot should be enabled");
    assert_eq!(slot_state.slot_context().usb_device_address(), slot_id);
    assert_eq!(slot_state.slot_context().speed(), speed_before);
    assert_eq!(slot_state.slot_context().root_hub_port_number(), 1);
    assert_eq!(slot_state.slot_context().route_string(), 0);
}
