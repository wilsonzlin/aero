use aero_usb::hid::UsbHidKeyboardHandle;
use aero_usb::xhci::context::{EndpointContext, InputControlContext, SlotContext, CONTEXT_SIZE};
use aero_usb::xhci::regs;
use aero_usb::xhci::trb::{CompletionCode, Trb, TrbType, TRB_LEN};
use aero_usb::xhci::XhciController;
use aero_usb::MemoryBus;

mod util;

use util::{Alloc, TestMemory};

#[test]
fn enable_slot_then_address_device_binds_port_and_writes_context() {
    let mut mem = TestMemory::new(0x20_000);
    let mut alloc = Alloc::new(0x1000);

    let cmd_ring = alloc.alloc(0x100, 16) as u64;
    let input_ctx = alloc.alloc(0x100, 16) as u64;
    let dcbaa = alloc.alloc(0x200, 64) as u64;
    let dev_ctx = alloc.alloc(0x200, 64) as u64;
    let tr_deq = alloc.alloc(0x100, 16) as u64;

    let mut xhci = XhciController::with_port_count(4);
    // `attach_device` takes a 0-based port index; port 0 corresponds to root hub port 1.
    xhci.attach_device(0, Box::new(UsbHidKeyboardHandle::new()));
    // Drain the Port Status Change Event emitted by `attach_device` so command completions are
    // deterministic for this test.
    while xhci.pop_pending_event().is_some() {}
    xhci.set_dcbaap(dcbaa);
    xhci.set_command_ring(cmd_ring, true);

    // Enable Slot.
    let mut enable = Trb::default();
    enable.set_cycle(true);
    enable.set_trb_type(TrbType::EnableSlotCommand);
    enable.write_to(&mut mem, cmd_ring);

    let mut stop = Trb::default();
    stop.set_cycle(false);
    stop.set_trb_type(TrbType::NoOpCommand);
    stop.write_to(&mut mem, cmd_ring + TRB_LEN as u64);

    xhci.process_command_ring(&mut mem, 8);
    let evt = xhci.pop_pending_event().expect("enable-slot completion");
    assert_eq!(evt.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(evt.completion_code_raw(), CompletionCode::Success.as_u8());
    let slot_id = evt.slot_id();
    assert_ne!(slot_id, 0);

    // Provision DCBAA entry for the slot (Output Device Context pointer).
    mem.write_u64(dcbaa + (slot_id as u64) * 8, dev_ctx);

    // Build Input Context: ICC + Slot + EP0.
    let mut icc = InputControlContext::default();
    icc.set_add_flags(0b11); // slot + EP0
    icc.write_to(&mut mem, input_ctx);

    let mut slot_ctx = SlotContext::default();
    slot_ctx.set_route_string(0);
    slot_ctx.set_speed(regs::PSIV_FULL_SPEED);
    slot_ctx.set_context_entries(1);
    slot_ctx.set_root_hub_port_number(1);
    slot_ctx.write_to(&mut mem, input_ctx + CONTEXT_SIZE as u64);

    let mut ep0_ctx = EndpointContext::default();
    // Endpoint Context dword 1 bits 16..=31 = Max Packet Size.
    ep0_ctx.set_dword(1, 64u32 << 16);
    ep0_ctx.set_tr_dequeue_pointer(tr_deq, true);
    ep0_ctx.write_to(&mut mem, input_ctx + (2 * CONTEXT_SIZE) as u64);

    // Address Device.
    let mut addr = Trb::default();
    addr.parameter = input_ctx;
    addr.set_cycle(true);
    addr.set_trb_type(TrbType::AddressDeviceCommand);
    addr.set_slot_id(slot_id);
    addr.write_to(&mut mem, cmd_ring + TRB_LEN as u64);

    stop.write_to(&mut mem, cmd_ring + (2 * TRB_LEN) as u64);

    xhci.process_command_ring(&mut mem, 8);
    let evt = xhci.pop_pending_event().expect("address-device completion");
    assert_eq!(evt.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(evt.completion_code_raw(), CompletionCode::Success.as_u8());
    assert_eq!(evt.slot_id(), slot_id);

    assert_eq!(
        xhci.slot_state(slot_id).and_then(|s| s.port_id()),
        Some(1)
    );
    {
        let dev = xhci.slot_device_mut(slot_id).expect("slot must resolve to a device");
        assert_eq!(dev.address(), slot_id);
    }

    // Output contexts should mirror the input contexts.
    let out_slot = SlotContext::read_from(&mut mem, dev_ctx);
    assert_eq!(out_slot.root_hub_port_number(), 1);
    assert_eq!(out_slot.speed(), regs::PSIV_FULL_SPEED);
    assert_eq!(out_slot.route_string(), 0);

    let out_ep0 = EndpointContext::read_from(&mut mem, dev_ctx + CONTEXT_SIZE as u64);
    assert_eq!(out_ep0.max_packet_size(), 64);
    assert_eq!(out_ep0.tr_dequeue_pointer(), tr_deq);
}

#[test]
fn address_device_invalid_port_fails_gracefully() {
    let mut mem = TestMemory::new(0x20_000);
    let mut alloc = Alloc::new(0x1000);

    let cmd_ring = alloc.alloc(0x100, 16) as u64;
    let input_ctx = alloc.alloc(0x100, 16) as u64;
    let dcbaa = alloc.alloc(0x200, 64) as u64;
    let dev_ctx = alloc.alloc(0x200, 64) as u64;

    let mut xhci = XhciController::with_port_count(1);
    xhci.attach_device(0, Box::new(UsbHidKeyboardHandle::new()));
    while xhci.pop_pending_event().is_some() {}
    xhci.set_dcbaap(dcbaa);
    xhci.set_command_ring(cmd_ring, true);

    let mut enable = Trb::default();
    enable.set_cycle(true);
    enable.set_trb_type(TrbType::EnableSlotCommand);
    enable.write_to(&mut mem, cmd_ring);

    let mut stop = Trb::default();
    stop.set_cycle(false);
    stop.set_trb_type(TrbType::NoOpCommand);
    stop.write_to(&mut mem, cmd_ring + TRB_LEN as u64);

    xhci.process_command_ring(&mut mem, 8);
    let evt = xhci.pop_pending_event().expect("enable-slot completion");
    let slot_id = evt.slot_id();
    assert_eq!(evt.completion_code_raw(), CompletionCode::Success.as_u8());

    mem.write_u64(dcbaa + (slot_id as u64) * 8, dev_ctx);

    let mut icc = InputControlContext::default();
    icc.set_add_flags(0b11);
    icc.write_to(&mut mem, input_ctx);

    let mut slot_ctx = SlotContext::default();
    slot_ctx.set_root_hub_port_number(2); // Invalid (controller only has 1 port)
    slot_ctx.write_to(&mut mem, input_ctx + CONTEXT_SIZE as u64);

    let ep0_ctx = EndpointContext::default();
    ep0_ctx.write_to(&mut mem, input_ctx + (2 * CONTEXT_SIZE) as u64);

    let mut addr = Trb::default();
    addr.parameter = input_ctx;
    addr.set_cycle(true);
    addr.set_trb_type(TrbType::AddressDeviceCommand);
    addr.set_slot_id(slot_id);
    addr.write_to(&mut mem, cmd_ring + TRB_LEN as u64);

    stop.write_to(&mut mem, cmd_ring + (2 * TRB_LEN) as u64);

    xhci.process_command_ring(&mut mem, 8);
    let evt = xhci.pop_pending_event().expect("address-device completion");
    assert_eq!(evt.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(evt.slot_id(), slot_id);
    assert_eq!(evt.completion_code_raw(), CompletionCode::ParameterError.as_u8());
    assert_eq!(xhci.slot_state(slot_id).and_then(|s| s.port_id()), None);
}
