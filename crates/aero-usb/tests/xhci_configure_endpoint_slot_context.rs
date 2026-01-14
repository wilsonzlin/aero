use aero_usb::hid::UsbHidKeyboardHandle;
use aero_usb::xhci::context::{
    EndpointContext, InputControlContext, SlotContext, CONTEXT_SIZE, SLOT_STATE_ADDRESSED,
    SLOT_STATE_CONFIGURED,
};
use aero_usb::xhci::trb::{CompletionCode, Trb, TrbType, TRB_LEN};
use aero_usb::xhci::{CommandCompletionCode, XhciController};
use aero_usb::MemoryBus;

mod util;

use util::{xhci_set_run, Alloc, TestMemory};

#[test]
fn configure_endpoint_preserves_xhc_owned_slot_context_fields() {
    let mut mem = TestMemory::new(0x40_000);
    let mut alloc = Alloc::new(0x1000);

    let cmd_ring = alloc.alloc(0x100, 0x10) as u64;
    let input_ctx_addr = alloc.alloc(0x100, 0x40) as u64;
    let input_ctx_cfg = alloc.alloc(0x200, 0x40) as u64;
    let dcbaa = alloc.alloc(0x200, 0x40) as u64;
    let dev_ctx = alloc.alloc(0x400, 0x40) as u64;
    let tr_deq = alloc.alloc(0x100, 0x10) as u64;
    let tr_ring = alloc.alloc(0x100, 0x10) as u64;

    let mut xhci = XhciController::with_port_count(1);
    xhci.attach_device(0, Box::new(UsbHidKeyboardHandle::new()));
    while xhci.pop_pending_event().is_some() {}
    xhci.set_dcbaap(dcbaa);
    xhci.set_command_ring(cmd_ring, true);
    xhci_set_run(&mut xhci);
    // --- Enable Slot (TRB0) ---
    {
        let mut enable = Trb::default();
        enable.set_cycle(true);
        enable.set_trb_type(TrbType::EnableSlotCommand);
        enable.write_to(&mut mem, cmd_ring);
    }
    // Stop marker: cycle mismatch.
    {
        let mut stop = Trb::default();
        stop.set_cycle(false);
        stop.set_trb_type(TrbType::NoOpCommand);
        stop.write_to(&mut mem, cmd_ring + TRB_LEN as u64);
    }

    xhci.process_command_ring(&mut mem, 8);
    let evt0 = xhci.pop_pending_event().expect("enable-slot completion");
    assert_eq!(evt0.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(evt0.completion_code_raw(), CompletionCode::Success.as_u8());
    let slot_id = evt0.slot_id();
    assert_ne!(slot_id, 0);

    // Install DCBAA[slot_id] -> device context pointer (guest responsibility).
    mem.write_u64(dcbaa + u64::from(slot_id) * 8, dev_ctx);

    // --- Address Device (TRB1) ---
    // Input Context: ICC + Slot + EP0.
    {
        let mut icc = InputControlContext::default();
        icc.set_add_flags(0b11); // Slot + EP0
        icc.write_to(&mut mem, input_ctx_addr);

        let mut slot_ctx = SlotContext::default();
        slot_ctx.set_root_hub_port_number(1);
        slot_ctx.set_context_entries(1);
        slot_ctx.write_to(&mut mem, input_ctx_addr + CONTEXT_SIZE as u64);

        let mut ep0 = EndpointContext::default();
        ep0.set_dword(1, 64u32 << 16); // Max Packet Size
        ep0.set_tr_dequeue_pointer(tr_deq, true);
        ep0.write_to(&mut mem, input_ctx_addr + (2 * CONTEXT_SIZE) as u64);
    }

    {
        let mut addr = Trb::new(input_ctx_addr, 0, 0);
        addr.set_cycle(true);
        addr.set_trb_type(TrbType::AddressDeviceCommand);
        addr.set_slot_id(slot_id);
        addr.write_to(&mut mem, cmd_ring + TRB_LEN as u64);
    }
    // Stop marker.
    {
        let mut stop = Trb::default();
        stop.set_cycle(false);
        stop.set_trb_type(TrbType::NoOpCommand);
        stop.write_to(&mut mem, cmd_ring + 2 * TRB_LEN as u64);
    }

    xhci.process_command_ring(&mut mem, 8);
    let evt1 = xhci.pop_pending_event().expect("address-device completion");
    assert_eq!(evt1.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(evt1.completion_code_raw(), CompletionCode::Success.as_u8());
    assert_eq!(evt1.slot_id(), slot_id);

    let slot_after_address = SlotContext::read_from(&mut mem, dev_ctx);
    assert_eq!(slot_after_address.usb_device_address(), slot_id);
    assert_eq!(slot_after_address.slot_state(), SLOT_STATE_ADDRESSED);
    let speed_after_address = slot_after_address.speed();

    // --- Configure Endpoint (TRB2) with Slot Context included ---
    // ICC Add flags: Slot + EP0 + EP1 IN (DCI=3).
    {
        let mut icc = InputControlContext::default();
        icc.set_add_flags((1 << 0) | (1 << 1) | (1 << 3));
        icc.write_to(&mut mem, input_ctx_cfg);

        // Slot Context: intentionally leave Speed, USB Device Address, Route String, and Root Hub
        // Port Number unset/invalid so the controller must preserve xHC-owned/topology fields from
        // the output Device Context.
        let mut slot_ctx = SlotContext::default();
        slot_ctx.set_route_string(0x1234);
        slot_ctx.set_root_hub_port_number(0);
        slot_ctx.set_context_entries(3);
        slot_ctx.write_to(&mut mem, input_ctx_cfg + CONTEXT_SIZE as u64);

        // EP0 context (required because add flag includes EP0).
        let mut ep0 = EndpointContext::default();
        ep0.set_dword(1, 64u32 << 16);
        ep0.set_tr_dequeue_pointer(tr_deq, true);
        ep0.write_to(&mut mem, input_ctx_cfg + (2 * CONTEXT_SIZE) as u64);

        // Endpoint 1 IN (DCI=3) context lives at Input Context index DCI+1 = 4.
        let mut ep1in = EndpointContext::default();
        // Endpoint type = Interrupt IN (7), MPS = 8.
        ep1in.set_dword(1, (7u32 << 3) | (8u32 << 16));
        ep1in.set_tr_dequeue_pointer(tr_ring, true);
        ep1in.write_to(&mut mem, input_ctx_cfg + (4 * CONTEXT_SIZE) as u64);
    }

    {
        let mut cfg = Trb::new(input_ctx_cfg, 0, 0);
        cfg.set_cycle(true);
        cfg.set_trb_type(TrbType::ConfigureEndpointCommand);
        cfg.set_slot_id(slot_id);
        cfg.write_to(&mut mem, cmd_ring + 2 * TRB_LEN as u64);
    }
    // Stop marker.
    {
        let mut stop = Trb::default();
        stop.set_cycle(false);
        stop.set_trb_type(TrbType::NoOpCommand);
        stop.write_to(&mut mem, cmd_ring + 3 * TRB_LEN as u64);
    }

    xhci.process_command_ring(&mut mem, 8);
    let evt2 = xhci
        .pop_pending_event()
        .expect("configure-endpoint completion");
    assert_eq!(evt2.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(evt2.completion_code_raw(), CompletionCode::Success.as_u8());
    assert_eq!(evt2.slot_id(), slot_id);

    let slot_after_cfg = SlotContext::read_from(&mut mem, dev_ctx);
    assert_eq!(
        slot_after_cfg.usb_device_address(),
        slot_id,
        "Configure Endpoint must preserve the xHC-owned USB device address"
    );
    assert_eq!(slot_after_cfg.slot_state(), SLOT_STATE_CONFIGURED);
    assert_eq!(
        slot_after_cfg.speed(),
        speed_after_address,
        "Configure Endpoint must preserve the xHC-owned speed field"
    );
    assert_eq!(slot_after_cfg.context_entries(), 3);
    assert_eq!(slot_after_cfg.root_hub_port_number(), 1);
    assert_eq!(slot_after_cfg.route_string(), 0);
}

#[test]
fn configure_endpoint_preserves_topology_when_output_slot_context_is_uninitialized() {
    let mut mem = TestMemory::new(0x40_000);
    let mut alloc = Alloc::new(0x1000);

    let cmd_ring = alloc.alloc(0x100, 0x40) as u64;
    let input_ctx = alloc.alloc(0x200, 0x40) as u64;
    let dcbaa = alloc.alloc(0x200, 0x40) as u64;
    let dev_ctx = alloc.alloc(0x400, 0x40) as u64;

    let mut xhci = XhciController::with_port_count(1);
    xhci.attach_device(0, Box::new(UsbHidKeyboardHandle::new()));
    while xhci.pop_pending_event().is_some() {}
    xhci.set_dcbaap(dcbaa);

    // Use the host-side helpers to bind the slot. Unlike the command-ring Address Device path,
    // this does not populate the output Slot Context in guest memory.
    let enable = xhci.enable_slot(&mut mem);
    assert_eq!(enable.completion_code, CommandCompletionCode::Success);
    let slot_id = enable.slot_id;
    assert_ne!(slot_id, 0);
    mem.write_u64(dcbaa + u64::from(slot_id) * 8, dev_ctx);

    let mut slot_ctx = SlotContext::default();
    slot_ctx.set_root_hub_port_number(1);
    let addr = xhci.address_device(slot_id, slot_ctx);
    assert_eq!(addr.completion_code, CommandCompletionCode::Success);
    assert_eq!(
        xhci.slot_state(slot_id)
            .expect("slot state should exist after address_device")
            .slot_context()
            .root_hub_port_number(),
        1,
        "host-side address_device() should update controller-local Slot Context topology binding"
    );
    assert_eq!(
        SlotContext::read_from(&mut mem, dev_ctx).root_hub_port_number(),
        0,
        "host-side address_device() should leave the output Slot Context uninitialized in memory"
    );

    // Configure Endpoint with Slot Context included (Add flags bit0). The input Slot Context has
    // invalid topology fields that must be preserved from controller-local state even though the
    // output Device Context Slot Context is still zeroed.
    {
        let mut icc = InputControlContext::default();
        icc.set_add_flags(1 << 0);
        icc.write_to(&mut mem, input_ctx);

        let mut in_slot_ctx = SlotContext::default();
        in_slot_ctx.set_route_string(0x1234);
        in_slot_ctx.set_root_hub_port_number(0);
        in_slot_ctx.set_context_entries(1);
        in_slot_ctx.write_to(&mut mem, input_ctx + CONTEXT_SIZE as u64);
    }

    xhci.set_command_ring(cmd_ring, true);
    xhci_set_run(&mut xhci);
    {
        let mut cfg = Trb::new(input_ctx, 0, 0);
        cfg.set_cycle(true);
        cfg.set_trb_type(TrbType::ConfigureEndpointCommand);
        cfg.set_slot_id(slot_id);
        cfg.write_to(&mut mem, cmd_ring);
    }
    {
        let mut stop = Trb::default();
        stop.set_cycle(false);
        stop.set_trb_type(TrbType::NoOpCommand);
        stop.write_to(&mut mem, cmd_ring + TRB_LEN as u64);
    }

    xhci.process_command_ring(&mut mem, 8);
    let evt = xhci
        .pop_pending_event()
        .expect("configure-endpoint completion");
    assert_eq!(evt.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(evt.completion_code_raw(), CompletionCode::Success.as_u8());
    assert_eq!(evt.slot_id(), slot_id);

    let slot_after_cfg = SlotContext::read_from(&mut mem, dev_ctx);
    assert_eq!(slot_after_cfg.slot_state(), SLOT_STATE_CONFIGURED);
    assert_eq!(
        slot_after_cfg.root_hub_port_number(),
        1,
        "Configure Endpoint must preserve the topology root port when the output Slot Context is zeroed"
    );
    assert_eq!(
        slot_after_cfg.route_string(),
        0,
        "Configure Endpoint must preserve the topology route string when the output Slot Context is zeroed"
    );
    assert!(
        xhci.slot_device_mut(slot_id).is_some(),
        "slot should remain routable after Configure Endpoint"
    );
}
