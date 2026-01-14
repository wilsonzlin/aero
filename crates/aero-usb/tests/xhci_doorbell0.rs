mod util;

use aero_usb::xhci::context::{EndpointContext, InputControlContext, CONTEXT_SIZE};
use aero_usb::xhci::trb::{CompletionCode, Trb, TrbType, TRB_LEN};
use aero_usb::xhci::{regs, XhciController};
use aero_usb::MemoryBus;

use util::{Alloc, TestMemory};

#[test]
fn xhci_doorbell0_processes_command_ring_from_crcr() {
    let mut mem = TestMemory::new(0x20_000);
    let mut alloc = Alloc::new(0x1000);

    let dcbaa = alloc.alloc(0x200, 0x40) as u64;
    let cmd_ring = alloc.alloc(0x100, 0x40) as u64;

    let mut xhci = XhciController::new();
    xhci.set_dcbaap(dcbaa);

    // Program CRCR (pointer + cycle state) and start the controller so doorbell 0 is accepted.
    xhci.mmio_write(&mut mem, regs::REG_CRCR_LO, 4, (cmd_ring as u32) | 1);
    xhci.mmio_write(&mut mem, regs::REG_CRCR_HI, 4, (cmd_ring >> 32) as u32);
    xhci.mmio_write(&mut mem, regs::REG_USBCMD, 4, regs::USBCMD_RUN);

    // Command ring: Enable Slot, No-Op, then stop (cycle mismatch).
    {
        let mut trb = Trb::new(0, 0, 0);
        trb.set_trb_type(TrbType::EnableSlotCommand);
        trb.set_cycle(true);
        trb.write_to(&mut mem, cmd_ring);
    }
    {
        let mut trb = Trb::new(0, 0, 0);
        trb.set_trb_type(TrbType::NoOpCommand);
        trb.set_cycle(true);
        trb.write_to(&mut mem, cmd_ring + TRB_LEN as u64);
    }
    {
        let mut trb = Trb::new(0, 0, 0);
        trb.set_trb_type(TrbType::NoOpCommand);
        trb.set_cycle(false);
        trb.write_to(&mut mem, cmd_ring + 2 * TRB_LEN as u64);
    }

    // Ring doorbell 0 (command ring).
    xhci.mmio_write(&mut mem, u64::from(regs::DBOFF_VALUE), 4, 0);

    let ev0 = xhci
        .pop_pending_event()
        .expect("Enable Slot completion event");
    assert_eq!(ev0.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(ev0.completion_code_raw(), CompletionCode::Success.as_u8());
    assert_eq!(ev0.slot_id(), 1);
    assert_eq!(ev0.parameter & !0x0f, cmd_ring);

    // Enable Slot initialises the DCBAA entry for the allocated slot to 0.
    assert_eq!(mem.read_u64(dcbaa + 8), 0);

    let ev1 = xhci.pop_pending_event().expect("No-Op completion event");
    assert_eq!(ev1.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(ev1.completion_code_raw(), CompletionCode::Success.as_u8());
    assert_eq!(ev1.slot_id(), 0);
    assert_eq!(ev1.parameter & !0x0f, cmd_ring + TRB_LEN as u64);
}

#[test]
fn xhci_doorbell0_persists_command_ring_state_across_rings() {
    let mut mem = TestMemory::new(0x20_000);
    let mut alloc = Alloc::new(0x1000);

    let dcbaa = alloc.alloc(0x200, 0x40) as u64;
    let dev_ctx = alloc.alloc(0x200, 0x40) as u64;
    let input_ctx = alloc.alloc(0x200, 0x40) as u64;
    let cmd_ring = alloc.alloc(0x100, 0x40) as u64;

    // Seed device context EP0 max packet size to 8.
    {
        let mut ep0 = EndpointContext::default();
        ep0.set_max_packet_size(8);
        ep0.write_to(&mut mem, dev_ctx + CONTEXT_SIZE as u64);
    }

    // Input context requests Slot + EP0 updates.
    {
        let mut icc = InputControlContext::default();
        icc.set_add_flags(0b11);
        icc.write_to(&mut mem, input_ctx);
    }

    let desired_trdp = 0x9000u64;
    {
        let mut ep0 = EndpointContext::default();
        ep0.set_interval(5);
        ep0.set_max_packet_size(64);
        ep0.set_tr_dequeue_pointer(desired_trdp, true);
        ep0.write_to(&mut mem, input_ctx + 2 * CONTEXT_SIZE as u64);
    }

    // Command ring:
    //  - TRB0: Enable Slot (cycle=1)
    //  - TRB1: Evaluate Context (cycle=0 initially so it will not run until we flip it)
    {
        let mut trb = Trb::new(0, 0, 0);
        trb.set_trb_type(TrbType::EnableSlotCommand);
        trb.set_cycle(true);
        trb.write_to(&mut mem, cmd_ring);
    }
    {
        let mut trb = Trb::new(input_ctx, 0, 0);
        trb.set_trb_type(TrbType::EvaluateContextCommand);
        trb.set_slot_id(1);
        trb.set_cycle(false);
        trb.write_to(&mut mem, cmd_ring + TRB_LEN as u64);
    }

    let mut xhci = XhciController::new();
    xhci.set_dcbaap(dcbaa);

    xhci.mmio_write(&mut mem, regs::REG_CRCR_LO, 4, (cmd_ring as u32) | 1);
    xhci.mmio_write(&mut mem, regs::REG_CRCR_HI, 4, (cmd_ring >> 32) as u32);
    xhci.mmio_write(&mut mem, regs::REG_USBCMD, 4, regs::USBCMD_RUN);

    // First doorbell: process Enable Slot only.
    xhci.mmio_write(&mut mem, u64::from(regs::DBOFF_VALUE), 4, 0);
    let ev0 = xhci.pop_pending_event().expect("Enable Slot completion");
    assert_eq!(ev0.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(ev0.completion_code_raw(), CompletionCode::Success.as_u8());
    let slot_id = ev0.slot_id();
    assert_eq!(slot_id, 1);

    // Simulate Address Device by installing the Device Context pointer after Enable Slot clears it.
    mem.write_u64(dcbaa + u64::from(slot_id) * 8, dev_ctx);

    // Flip TRB1 to cycle=1 so it becomes visible to the command ring consumer.
    {
        let mut trb = Trb::new(input_ctx, 0, 0);
        trb.set_trb_type(TrbType::EvaluateContextCommand);
        trb.set_slot_id(slot_id);
        trb.set_cycle(true);
        trb.write_to(&mut mem, cmd_ring + TRB_LEN as u64);
    }

    // Second doorbell: Evaluate Context should run using the preserved command ring cursor.
    xhci.mmio_write(&mut mem, u64::from(regs::DBOFF_VALUE), 4, 0);
    let ev1 = xhci
        .pop_pending_event()
        .expect("Evaluate Context completion");
    assert_eq!(ev1.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(ev1.completion_code_raw(), CompletionCode::Success.as_u8());

    let out_ep0 = EndpointContext::read_from(&mut mem, dev_ctx + CONTEXT_SIZE as u64);
    assert_eq!(out_ep0.max_packet_size(), 64);
    assert_eq!(out_ep0.interval(), 5);
    assert_eq!(out_ep0.tr_dequeue_pointer(), desired_trdp);
    assert!(out_ep0.dcs());

    let ring = xhci
        .slot_state(slot_id)
        .and_then(|s| s.transfer_ring(1))
        .expect("EP0 transfer ring cursor should be updated");
    assert_eq!(ring.dequeue_ptr(), desired_trdp);
    assert!(ring.cycle_state());
}
