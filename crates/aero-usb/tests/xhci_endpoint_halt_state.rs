mod util;

use std::cell::Cell;
use std::rc::Rc;

use aero_usb::xhci::context::{EndpointContext, SlotContext, CONTEXT_SIZE};
use aero_usb::xhci::trb::{CompletionCode, Trb, TrbType, TRB_LEN};
use aero_usb::xhci::{CommandCompletionCode, XhciController};
use aero_usb::{ControlResponse, MemoryBus, SetupPacket, UsbDeviceModel, UsbInResult};

use util::{xhci_set_run, Alloc, TestMemory};

#[derive(Clone, Debug)]
struct StallOnceThenDataIn {
    stalled: Rc<Cell<bool>>,
}

impl StallOnceThenDataIn {
    fn new() -> Self {
        Self {
            stalled: Rc::new(Cell::new(false)),
        }
    }
}

impl UsbDeviceModel for StallOnceThenDataIn {
    fn handle_control_request(
        &mut self,
        _setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        ControlResponse::Stall
    }

    fn handle_in_transfer(&mut self, ep_addr: u8, max_len: usize) -> UsbInResult {
        // Only implement EP2 IN (0x82).
        if ep_addr != 0x82 {
            return UsbInResult::Stall;
        }

        if !self.stalled.get() {
            self.stalled.set(true);
            return UsbInResult::Stall;
        }

        let mut data = vec![0xaau8, 0xbb, 0xcc, 0xdd];
        data.truncate(max_len);
        UsbInResult::Data(data)
    }
}

#[test]
fn stalled_endpoint_sets_halted_state_in_device_context_until_reset() {
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
    xhci.attach_device(0, Box::new(StallOnceThenDataIn::new()));
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

    // Endpoint 2 IN (DCI=5, ep_addr=0x82).
    let endpoint_id = 5u8;
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

    // Ring doorbell and execute the first transfer; it stalls and halts the endpoint.
    xhci.ring_doorbell(slot_id, endpoint_id);
    xhci.tick_1ms(&mut mem);

    assert_eq!(&mem.data[buf1 as usize..buf1 as usize + 4], &[0, 0, 0, 0]);

    let ev0 = xhci
        .pop_pending_event()
        .expect("expected stall transfer event");
    assert_eq!(ev0.trb_type(), TrbType::TransferEvent);
    assert_eq!(ev0.slot_id(), slot_id);
    assert_eq!(ev0.endpoint_id(), endpoint_id);
    assert_eq!(
        ev0.completion_code_raw(),
        CompletionCode::StallError.as_u8()
    );

    let ep_ctx = EndpointContext::read_from(&mut mem, ep_ctx_paddr);
    assert_eq!(
        ep_ctx.endpoint_state(),
        2,
        "stall should transition endpoint into Halted state in the guest Device Context"
    );

    // Further doorbells must not execute transfers while halted.
    xhci.ring_doorbell(slot_id, endpoint_id);
    xhci.tick_1ms(&mut mem);
    assert_eq!(&mem.data[buf2 as usize..buf2 as usize + 4], &[0, 0, 0, 0]);
    assert!(xhci.pop_pending_event().is_none());

    // Reset Endpoint via command ring and retry; execution should resume.
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
    let ev1 = xhci
        .pop_pending_event()
        .expect("expected Reset Endpoint completion");
    assert_eq!(ev1.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(ev1.completion_code_raw(), CompletionCode::Success.as_u8());

    let ep_ctx = EndpointContext::read_from(&mut mem, ep_ctx_paddr);
    assert_eq!(ep_ctx.endpoint_state(), 1);

    xhci.ring_doorbell(slot_id, endpoint_id);
    xhci.tick_1ms(&mut mem);
    assert_eq!(
        &mem.data[buf2 as usize..buf2 as usize + 4],
        &[0xaa, 0xbb, 0xcc, 0xdd]
    );
    let ev2 = xhci
        .pop_pending_event()
        .expect("expected resumed transfer event");
    assert_eq!(ev2.trb_type(), TrbType::TransferEvent);
    assert_eq!(ev2.completion_code_raw(), CompletionCode::Success.as_u8());
}
