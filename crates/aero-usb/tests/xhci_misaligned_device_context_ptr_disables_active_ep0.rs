use std::boxed::Box;

use aero_usb::xhci::context::{SlotContext, CONTEXT_SIZE};
use aero_usb::xhci::trb::{Trb, TrbType, TRB_LEN};
use aero_usb::xhci::{CommandCompletionCode, XhciController};
use aero_usb::{
    ControlResponse, MemoryBus, SetupPacket, UsbDeviceModel, UsbInResult, UsbOutResult,
};

mod util;

use util::{xhci_set_run, Alloc, TestMemory};

#[derive(Clone, Debug, Default)]
struct NakThenDataControlDevice {
    calls: usize,
}

impl UsbDeviceModel for NakThenDataControlDevice {
    fn handle_control_request(
        &mut self,
        setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        // First call happens during the SETUP stage. The controller then retries the DATA stage
        // until the device returns data.
        self.calls += 1;
        if self.calls < 3 {
            ControlResponse::Nak
        } else {
            ControlResponse::Data(vec![0x5a; setup.w_length as usize])
        }
    }

    fn handle_in_transfer(&mut self, _ep_addr: u8, _max_len: usize) -> UsbInResult {
        UsbInResult::Stall
    }

    fn handle_out_transfer(&mut self, _ep_addr: u8, _data: &[u8]) -> UsbOutResult {
        UsbOutResult::Stall
    }
}

fn endpoint_ctx_addr(dev_ctx_base: u64, endpoint_id: u8) -> u64 {
    dev_ctx_base + u64::from(endpoint_id) * (CONTEXT_SIZE as u64)
}

fn write_ep0_endpoint_context(mem: &mut TestMemory, dev_ctx_base: u64, ring_base: u64) {
    // EP0 => endpoint id 1.
    let base = endpoint_ctx_addr(dev_ctx_base, 1);
    // Endpoint state: Running (1).
    MemoryBus::write_u32(mem, base, 1);
    // Endpoint type (Control = 4) + max packet size.
    let dw1 = (4u32 << 3) | (8u32 << 16);
    MemoryBus::write_u32(mem, base + 4, dw1);

    let tr_dequeue_raw = (ring_base & !0x0f) | 1;
    MemoryBus::write_u32(mem, base + 8, tr_dequeue_raw as u32);
    MemoryBus::write_u32(mem, base + 12, (tr_dequeue_raw >> 32) as u32);
}

#[test]
fn xhci_misaligned_dcbaa_ptr_stops_already_scheduled_ep0() {
    let mut mem = TestMemory::new(0x40_000);
    let mut alloc = Alloc::new(0x1000);

    let dcbaa = alloc.alloc(0x200, 0x40) as u64;
    let dev_ctx = alloc.alloc(0x400, 0x40) as u64;

    // EP0 transfer ring: SetupStage, DataStage(IN), StatusStage(OUT), Link.
    let ring_base = alloc.alloc((TRB_LEN as u32) * 4, 0x10) as u64;
    let data_buf = alloc.alloc(8, 0x10) as u64;

    write_ep0_endpoint_context(&mut mem, dev_ctx, ring_base);

    // Setup packet for an arbitrary control-IN request.
    let setup = SetupPacket {
        bm_request_type: 0x80,
        b_request: 0x06,
        w_value: 0x0100,
        w_index: 0,
        w_length: 8,
    };

    // Setup stage TRB.
    let mut setup_trb = Trb {
        parameter: u64::from_le_bytes([
            setup.bm_request_type,
            setup.b_request,
            (setup.w_value & 0x00ff) as u8,
            (setup.w_value >> 8) as u8,
            (setup.w_index & 0x00ff) as u8,
            (setup.w_index >> 8) as u8,
            (setup.w_length & 0x00ff) as u8,
            (setup.w_length >> 8) as u8,
        ]),
        ..Default::default()
    };
    setup_trb.set_cycle(true);
    setup_trb.set_trb_type(TrbType::SetupStage);
    setup_trb.write_to(&mut mem, ring_base);

    // Data stage TRB (IN).
    let mut data_trb = Trb {
        parameter: data_buf,
        status: 8,
        control: Trb::CONTROL_DIR,
    };
    data_trb.set_cycle(true);
    data_trb.set_trb_type(TrbType::DataStage);
    data_trb.write_to(&mut mem, ring_base + TRB_LEN as u64);

    // Status stage TRB (OUT) with IOC.
    let mut status_trb = Trb {
        control: Trb::CONTROL_IOC_BIT,
        ..Default::default()
    };
    status_trb.set_cycle(true);
    status_trb.set_trb_type(TrbType::StatusStage);
    status_trb.write_to(&mut mem, ring_base + 2 * TRB_LEN as u64);

    // Link TRB back to ring base.
    let mut link_trb = Trb {
        parameter: ring_base,
        ..Default::default()
    };
    link_trb.set_cycle(true);
    link_trb.set_trb_type(TrbType::Link);
    link_trb.set_link_toggle_cycle(true);
    link_trb.write_to(&mut mem, ring_base + 3 * TRB_LEN as u64);

    let mut ctrl = XhciController::new();
    ctrl.attach_device(0, Box::new(NakThenDataControlDevice::default()));
    while ctrl.pop_pending_event().is_some() {}
    xhci_set_run(&mut ctrl);
    ctrl.set_dcbaap(dcbaa);
    let enable = ctrl.enable_slot(&mut mem);
    assert_eq!(enable.completion_code, CommandCompletionCode::Success);
    let slot_id = enable.slot_id;

    // Install a valid Device Context pointer in DCBAA so the controller treats the guest context as
    // present.
    MemoryBus::write_u64(&mut mem, dcbaa + u64::from(slot_id) * 8, dev_ctx);

    let mut slot_ctx = SlotContext::default();
    slot_ctx.set_root_hub_port_number(1);
    let addr = ctrl.address_device(slot_id, slot_ctx);
    assert_eq!(addr.completion_code, CommandCompletionCode::Success);

    // Configure a controller-local EP0 ring cursor so the controller can execute control transfers
    // without going through full command-ring plumbing.
    ctrl.set_endpoint_ring(slot_id, 1, ring_base, true);

    // Ring EP0 once. The DATA stage will NAK, leaving the endpoint scheduled for a future tick.
    ctrl.ring_doorbell(slot_id, 1);
    ctrl.tick(&mut mem);

    let mut buf = [0u8; 8];
    mem.read_physical(data_buf, &mut buf);
    assert_eq!(buf, [0u8; 8], "no DMA expected while DATA stage NAKs");
    assert_eq!(
        ctrl.pending_event_count(),
        0,
        "no transfer event expected before the control TD completes"
    );

    // Corrupt DCBAA[slot] to a misaligned Device Context pointer. The controller must stop polling
    // the already-scheduled EP0 and must not DMA based on controller-local ring state.
    MemoryBus::write_u64(&mut mem, dcbaa + u64::from(slot_id) * 8, dev_ctx | 0x1f);

    ctrl.tick(&mut mem);

    mem.read_physical(data_buf, &mut buf);
    assert_eq!(
        buf, [0u8; 8],
        "ep0 must not DMA after DCBAA pointer becomes misaligned"
    );
    assert_eq!(
        ctrl.pending_event_count(),
        0,
        "no transfer event expected after DCBAA pointer becomes misaligned"
    );
}
