use std::cell::Cell;
use std::rc::Rc;

use aero_usb::xhci::context::SlotContext;
use aero_usb::xhci::trb::{Trb, TrbType, TRB_LEN};
use aero_usb::xhci::{CommandCompletionCode, XhciController, PORTSC_PR};
use aero_usb::{ControlResponse, MemoryBus, SetupPacket, UsbDeviceModel, UsbInResult, UsbOutResult};

mod util;
use util::{Alloc, TestMemory};

#[derive(Clone)]
struct TickCountingDevice {
    ticks: Rc<Cell<u32>>,
}

impl TickCountingDevice {
    fn new(ticks: Rc<Cell<u32>>) -> Self {
        Self { ticks }
    }
}

impl UsbDeviceModel for TickCountingDevice {
    fn tick_1ms(&mut self) {
        let prev = self.ticks.get();
        self.ticks.set(prev + 1);
    }

    fn handle_control_request(
        &mut self,
        _setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        ControlResponse::Stall
    }

    fn handle_in_transfer(&mut self, _ep: u8, _max_len: usize) -> UsbInResult {
        UsbInResult::Nak
    }

    fn handle_out_transfer(&mut self, _ep: u8, _data: &[u8]) -> UsbOutResult {
        UsbOutResult::Nak
    }
}

fn endpoint_ctx_addr(dev_ctx_base: u64, endpoint_id: u8) -> u64 {
    dev_ctx_base + (endpoint_id as u64) * 0x20
}

fn write_endpoint_context(
    mem: &mut TestMemory,
    dev_ctx_base: u64,
    endpoint_id: u8,
    ep_type_raw: u8,
    max_packet_size: u16,
    ring_base: u64,
    dcs: bool,
) {
    let base = endpoint_ctx_addr(dev_ctx_base, endpoint_id);
    // Endpoint state: running (1).
    mem.write_u32(base as u32, 1);
    // Endpoint type + max packet size.
    let dw1 = ((ep_type_raw as u32) << 3) | (u32::from(max_packet_size) << 16);
    mem.write_u32((base + 4) as u32, dw1);

    let tr_dequeue_raw = (ring_base & !0x0f) | u64::from(dcs as u8);
    mem.write_u32((base + 8) as u32, tr_dequeue_raw as u32);
    mem.write_u32((base + 12) as u32, (tr_dequeue_raw >> 32) as u32);
}

fn make_normal_trb(buf_ptr: u64, len: u32, cycle: bool) -> Trb {
    let mut trb = Trb::new(buf_ptr, len & Trb::STATUS_TRANSFER_LEN_MASK, 0);
    trb.set_trb_type(TrbType::Normal);
    trb.set_cycle(cycle);
    trb
}

#[test]
fn xhci_tick_1ms_does_not_double_tick_device_when_multiple_endpoints_active() {
    let ticks = Rc::new(Cell::new(0));
    let dev = TickCountingDevice::new(ticks.clone());

    let mut xhci = XhciController::with_port_count(1);
    xhci.attach_device(0, Box::new(dev));

    // Bring the port to the enabled state so port ticking will call `dev.tick_1ms()`.
    xhci.write_portsc(0, PORTSC_PR);
    for _ in 0..50 {
        xhci.tick_1ms_no_dma();
    }

    // Reset the counter after port reset; we only care about the tick accounting for the TD poll.
    ticks.set(0);

    let mut mem = TestMemory::new(0x40_000);
    let mut alloc = Alloc::new(0x1000);

    let dcbaa = alloc.alloc(0x800, 0x40) as u64;
    let dev_ctx = alloc.alloc(0x800, 0x40) as u64;
    xhci.set_dcbaap(dcbaa);

    let enable = xhci.enable_slot(&mut mem);
    assert_eq!(enable.completion_code, CommandCompletionCode::Success);
    let slot_id = enable.slot_id;
    assert_ne!(slot_id, 0);

    // Install DCBAA[slot_id] -> device context pointer.
    MemoryBus::write_u64(&mut mem, dcbaa + u64::from(slot_id) * 8, dev_ctx);

    let mut slot_ctx = SlotContext::default();
    slot_ctx.set_root_hub_port_number(1);
    let addr = xhci.address_device(slot_id, slot_ctx);
    assert_eq!(addr.completion_code, CommandCompletionCode::Success);

    // Configure two endpoints (EP1 OUT => DCI=2, EP1 IN => DCI=3) and prime each with a Normal TRB
    // that will NAK.
    const EP_OUT_ID: u8 = 2;
    const EP_IN_ID: u8 = 3;

    let out_ring = alloc.alloc(TRB_LEN as u32, 0x10) as u64;
    let in_ring = alloc.alloc(TRB_LEN as u32, 0x10) as u64;
    let out_buf = alloc.alloc(4, 0x10) as u64;
    let in_buf = alloc.alloc(4, 0x10) as u64;

    write_endpoint_context(&mut mem, dev_ctx, EP_OUT_ID, 2, 64, out_ring, true);
    write_endpoint_context(&mut mem, dev_ctx, EP_IN_ID, 6, 64, in_ring, true);

    make_normal_trb(out_buf, 4, true).write_to(&mut mem, out_ring);
    make_normal_trb(in_buf, 4, true).write_to(&mut mem, in_ring);

    xhci.ring_doorbell(slot_id, EP_OUT_ID);
    xhci.ring_doorbell(slot_id, EP_IN_ID);

    xhci.tick_1ms(&mut mem);

    // The device should have been ticked exactly once by the port-level tick, not once per endpoint.
    assert_eq!(
        ticks.get(),
        1,
        "device tick_1ms should run once per controller tick, regardless of active endpoints"
    );
}

