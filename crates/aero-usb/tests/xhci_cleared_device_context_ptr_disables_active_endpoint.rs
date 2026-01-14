use std::boxed::Box;

use aero_usb::xhci::context::{SlotContext, CONTEXT_SIZE};
use aero_usb::xhci::trb::{Trb, TrbType, TRB_LEN};
use aero_usb::xhci::{CommandCompletionCode, XhciController};
use aero_usb::{ControlResponse, MemoryBus, SetupPacket, UsbDeviceModel, UsbInResult};

mod util;

use util::{xhci_set_run, Alloc, TestMemory};

#[derive(Clone, Debug)]
struct InterruptInDevice;

impl UsbDeviceModel for InterruptInDevice {
    fn handle_control_request(
        &mut self,
        _setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        ControlResponse::Ack
    }

    fn handle_in_transfer(&mut self, ep_addr: u8, max_len: usize) -> UsbInResult {
        assert_eq!(ep_addr, 0x81);
        UsbInResult::Data(vec![0x5a; max_len.min(8)])
    }
}

fn configure_dcbaa(mem: &mut TestMemory, dcbaa: u64, slot_id: u8, dev_ctx_ptr: u64) {
    MemoryBus::write_u64(mem, dcbaa + u64::from(slot_id) * 8, dev_ctx_ptr);
}

fn endpoint_ctx_addr(dev_ctx_base: u64, endpoint_id: u8) -> u64 {
    dev_ctx_base + u64::from(endpoint_id) * (CONTEXT_SIZE as u64)
}

fn write_interrupt_in_endpoint_context(
    mem: &mut TestMemory,
    dev_ctx_base: u64,
    endpoint_id: u8,
    ring_base: u64,
) {
    let base = endpoint_ctx_addr(dev_ctx_base, endpoint_id);
    // Endpoint state: Running (1).
    MemoryBus::write_u32(mem, base, 1);
    // Endpoint type (Interrupt IN = 7) + max packet size.
    let dw1 = (7u32 << 3) | (8u32 << 16);
    MemoryBus::write_u32(mem, base + 4, dw1);

    let tr_dequeue_raw = (ring_base & !0x0f) | 1;
    MemoryBus::write_u32(mem, base + 8, tr_dequeue_raw as u32);
    MemoryBus::write_u32(mem, base + 12, (tr_dequeue_raw >> 32) as u32);
}

fn make_normal_trb(buf_ptr: u64, len: u32, cycle: bool, ioc: bool) -> Trb {
    let mut trb = Trb::new(buf_ptr, len & Trb::STATUS_TRANSFER_LEN_MASK, 0);
    trb.set_trb_type(TrbType::Normal);
    trb.set_cycle(cycle);
    if ioc {
        trb.control |= Trb::CONTROL_IOC_BIT;
    }
    trb
}

#[test]
fn xhci_cleared_dcbaa_ptr_stops_already_scheduled_endpoint() {
    let mut mem = TestMemory::new(0x40_000);
    let mut alloc = Alloc::new(0x1000);

    let dcbaa = alloc.alloc(0x200, 0x40) as u64;
    let dev_ctx = alloc.alloc(0x400, 0x40) as u64;
    let ring_base = alloc.alloc((TRB_LEN as u32) * 4, 0x10) as u64;
    let buf1 = alloc.alloc(8, 0x10) as u64;
    let buf2 = alloc.alloc(8, 0x10) as u64;

    // Endpoint 1 IN => endpoint id 3.
    const EP_ID: u8 = 3;

    write_interrupt_in_endpoint_context(&mut mem, dev_ctx, EP_ID, ring_base);
    make_normal_trb(buf1, 8, true, true).write_to(&mut mem, ring_base);
    make_normal_trb(buf2, 8, true, true).write_to(&mut mem, ring_base + TRB_LEN as u64);
    // Cycle mismatch stop marker so the endpoint naturally becomes inactive after the second TD.
    {
        let mut stop = Trb::default();
        stop.set_trb_type(TrbType::NoOp);
        stop.set_cycle(false);
        stop.write_to(&mut mem, ring_base + 2 * TRB_LEN as u64);
    }

    let mut ctrl = XhciController::new();
    ctrl.attach_device(0, Box::new(InterruptInDevice));
    while ctrl.pop_pending_event().is_some() {}
    xhci_set_run(&mut ctrl);
    ctrl.set_dcbaap(dcbaa);
    let enable = ctrl.enable_slot(&mut mem);
    assert_eq!(enable.completion_code, CommandCompletionCode::Success);
    let slot_id = enable.slot_id;
    configure_dcbaa(&mut mem, dcbaa, slot_id, dev_ctx);

    let mut slot_ctx = SlotContext::default();
    slot_ctx.set_root_hub_port_number(1);
    let addr = ctrl.address_device(slot_id, slot_ctx);
    assert_eq!(addr.completion_code, CommandCompletionCode::Success);

    // Ensure the controller has observed a non-zero Device Context pointer for the slot so that if
    // the guest later clears the DCBAA entry, the controller treats it as "guest context present"
    // (and does not fall back to controller-local ring cursors).
    let set_trdp = ctrl.set_tr_dequeue_pointer(&mut mem, slot_id, EP_ID, ring_base, true);
    assert_eq!(set_trdp.completion_code, CommandCompletionCode::Success);

    // Ring once and execute the first TRB. The endpoint remains scheduled because the second TRB is
    // ready (matching cycle bit).
    ctrl.ring_doorbell(slot_id, EP_ID);
    ctrl.tick(&mut mem);

    let mut got1 = [0u8; 8];
    mem.read_physical(buf1, &mut got1);
    assert_eq!(got1, [0x5a; 8]);
    let mut got2 = [0u8; 8];
    mem.read_physical(buf2, &mut got2);
    assert_eq!(got2, [0u8; 8]);

    // Consume the transfer event for the first TD so we can assert on new events later.
    let ev0 = ctrl
        .pop_pending_event()
        .expect("expected first transfer event");
    assert_eq!(ev0.trb_type(), TrbType::TransferEvent);

    // Clear DCBAA[slot] back to 0. The controller must stop polling the already-scheduled endpoint
    // and must not DMA based on controller-local shadow state.
    MemoryBus::write_u64(&mut mem, dcbaa + u64::from(slot_id) * 8, 0);

    ctrl.tick(&mut mem);

    mem.read_physical(buf2, &mut got2);
    assert_eq!(
        got2, [0u8; 8],
        "endpoint must not DMA after DCBAA pointer becomes 0"
    );
    assert_eq!(
        ctrl.pending_event_count(),
        0,
        "no transfer event expected after DCBAA pointer becomes 0"
    );
}
