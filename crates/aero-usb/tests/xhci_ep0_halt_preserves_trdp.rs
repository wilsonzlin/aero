use std::boxed::Box;

use aero_usb::xhci::context::SlotContext;
use aero_usb::xhci::{CommandCompletionCode, XhciController};
use aero_usb::{ControlResponse, MemoryBus, SetupPacket, UsbDeviceModel};

mod util;

use util::{xhci_set_run, Alloc, TestMemory};

#[derive(Clone, Debug)]
struct DummyDevice;

impl UsbDeviceModel for DummyDevice {
    fn handle_control_request(
        &mut self,
        _setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        ControlResponse::Nak
    }
}

fn endpoint_ctx_addr(dev_ctx_base: u64, endpoint_id: u8) -> u64 {
    dev_ctx_base + (endpoint_id as u64) * 0x20
}

fn configure_dcbaa(mem: &mut TestMemory, dcbaa: u64, slot_id: u8, dev_ctx_ptr: u64) {
    let entry = dcbaa + u64::from(slot_id) * 8;
    mem.write_u32(entry as u32, dev_ctx_ptr as u32);
    mem.write_u32((entry + 4) as u32, (dev_ctx_ptr >> 32) as u32);
}

#[test]
fn xhci_ep0_halt_does_not_clobber_tr_dequeue_pointer() {
    let mut mem = TestMemory::new(0x20_000);
    let mut alloc = Alloc::new(0x1000);

    // Allocate basic xHCI structures in guest memory.
    let dcbaa = alloc.alloc(0x800, 0x40) as u64;
    let dev_ctx = alloc.alloc(0x400, 0x40) as u64;
    let ring_base = alloc.alloc(0x100, 0x10) as u64;

    // Populate an EP0 endpoint context (endpoint ID 1) in guest memory.
    let ep0_ctx = endpoint_ctx_addr(dev_ctx, 1);
    // Endpoint state = Running (1).
    mem.write_u32(ep0_ctx as u32, 1);
    // Endpoint type = Control (4) + max packet size (arbitrary; not used by this test).
    let dw1 = (4u32 << 3) | (64u32 << 16);
    mem.write_u32((ep0_ctx + 4) as u32, dw1);

    let mut ctrl = XhciController::new();
    ctrl.attach_device(0, Box::new(DummyDevice));
    // Drop root hub attach events.
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

    // Prime controller-local `device_context_ptr` and shadow EP0 context. We'll later update only
    // the guest context TR Dequeue Pointer so the shadow becomes stale.
    let reset = ctrl.reset_endpoint(&mut mem, slot_id, 1);
    assert_eq!(reset.completion_code, CommandCompletionCode::Success);

    // Update the guest Endpoint Context TR Dequeue Pointer to a new value without updating the
    // controller-local shadow. This simulates guest memory being modified in a way the controller
    // doesn't immediately observe.
    let trdp_raw = (ring_base & !0x0f) | 1;
    mem.write_u32((ep0_ctx + 8) as u32, trdp_raw as u32);
    mem.write_u32((ep0_ctx + 12) as u32, (trdp_raw >> 32) as u32);

    // Configure the controller-local ring cursor so it will attempt to read from `ring_base`.
    ctrl.set_endpoint_ring(slot_id, 1, ring_base, true);

    // Malformed ring: an all-ones TRB fetch is treated as an invalid DMA read and should halt EP0.
    mem.write(ring_base as u32, &[0xFFu8; 16]);

    ctrl.ring_doorbell(slot_id, 1);
    ctrl.tick(&mut mem);

    let state = mem.read_u32(ep0_ctx as u32) & 0x7;
    assert_eq!(state, 2, "EP0 should transition to Halted (2)");

    let trdp_after = mem.read_u64(ep0_ctx + 8);
    assert_eq!(
        trdp_after, trdp_raw,
        "halting EP0 must not overwrite the guest TR Dequeue Pointer"
    );
}
