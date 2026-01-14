mod util;

use aero_usb::xhci::context::{SlotContext, CONTEXT_SIZE};
use aero_usb::xhci::trb::{CompletionCode, Trb, TrbType, TRB_LEN};
use aero_usb::xhci::{regs, CommandCompletionCode, XhciController};
use aero_usb::{ControlResponse, MemoryBus, SetupPacket, UsbDeviceModel, UsbInResult};

use util::{Alloc, TestMemory};

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
        assert_eq!(ep_addr, 0x81);
        let mut data = vec![0x11u8, 0x22, 0x33, 0x44];
        data.truncate(max_len);
        UsbInResult::Data(data)
    }
}

#[test]
fn stop_endpoint_command_unschedules_active_endpoint() {
    let mut mem = TestMemory::new(0x40_000);
    let mut alloc = Alloc::new(0x1000);

    let dcbaa = alloc.alloc(0x200, 0x40) as u64;
    let dev_ctx = alloc.alloc(0x400, 0x40) as u64;
    let cmd_ring = alloc.alloc(0x100, 0x40) as u64;
    let transfer_ring = alloc.alloc(3 * (TRB_LEN as u32), 0x10) as u64;
    let buf1 = alloc.alloc(8, 0x10) as u64;
    let buf2 = alloc.alloc(8, 0x10) as u64;

    let mut xhci = XhciController::with_port_count(1);
    xhci.set_dcbaap(dcbaa);
    xhci.attach_device(0, Box::new(AlwaysInDevice));
    while xhci.pop_pending_event().is_some() {}
    // Transfer execution is gated on USBCMD.RUN.
    xhci.mmio_write(
        regs::REG_USBCMD,
        4,
        u64::from(regs::USBCMD_RUN),
    );

    let completion = xhci.enable_slot(&mut mem);
    assert_eq!(completion.completion_code, CommandCompletionCode::Success);
    let slot_id = completion.slot_id;
    assert_ne!(slot_id, 0);

    // Install Device Context pointer (simulates guest setup between Enable Slot and endpoint work).
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

    // Doorbell the endpoint and execute one transfer. Since another TRB is ready, the controller
    // keeps the endpoint active without requiring another doorbell.
    xhci.ring_doorbell(slot_id, endpoint_id);
    let work0 = xhci.step_1ms(&mut mem);
    assert_eq!(work0.doorbells_serviced, 1);
    assert_eq!(work0.transfer_trbs_consumed, 1);

    assert_eq!(
        &mem.data[buf1 as usize..buf1 as usize + 4],
        &[0x11, 0x22, 0x33, 0x44]
    );

    // Consume the transfer event so later assertions can use `pending_event_count` cleanly.
    let ev0 = xhci.pop_pending_event().expect("expected transfer event");
    assert_eq!(ev0.trb_type(), TrbType::TransferEvent);
    assert_eq!(ev0.slot_id(), slot_id);
    assert_eq!(ev0.endpoint_id(), endpoint_id);
    assert_eq!(ev0.completion_code_raw(), CompletionCode::Success.as_u8());

    // Issue Stop Endpoint via command ring.
    xhci.set_command_ring(cmd_ring, true);
    {
        let mut stop = Trb::default();
        stop.set_trb_type(TrbType::StopEndpointCommand);
        stop.set_cycle(true);
        stop.set_slot_id(slot_id);
        stop.set_endpoint_id(endpoint_id);
        stop.write_to(&mut mem, cmd_ring);
    }
    {
        let mut stop_marker = Trb::default();
        stop_marker.set_trb_type(TrbType::NoOpCommand);
        stop_marker.set_cycle(false);
        stop_marker.write_to(&mut mem, cmd_ring + TRB_LEN as u64);
    }
    xhci.process_command_ring(&mut mem, 1);
    let ev1 = xhci
        .pop_pending_event()
        .expect("expected Stop Endpoint completion");
    assert_eq!(ev1.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(ev1.completion_code_raw(), CompletionCode::Success.as_u8());

    // The endpoint had another TRB ready and would have been serviced again even without a new
    // doorbell. Stopping the endpoint should unschedule it immediately so it does not consume per-
    // tick doorbell budget while stopped.
    let work1 = xhci.step_1ms(&mut mem);
    assert_eq!(work1.doorbells_serviced, 0);
    assert_eq!(work1.transfer_trbs_consumed, 0);
    assert_eq!(&mem.data[buf2 as usize..buf2 as usize + 4], &[0, 0, 0, 0]);
}
