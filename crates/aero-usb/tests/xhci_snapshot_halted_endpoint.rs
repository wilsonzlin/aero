use aero_io_snapshot::io::state::IoSnapshot;
use aero_usb::passthrough::{UsbHostAction, UsbHostCompletion, UsbHostCompletionIn};
use aero_usb::xhci::context::SlotContext;
use aero_usb::xhci::trb::{Trb, TrbType, TRB_LEN};
use aero_usb::xhci::{CommandCompletionCode, XhciController};
use aero_usb::{
    ControlResponse, MemoryBus, SetupPacket, UsbDeviceModel, UsbInResult,
    UsbWebUsbPassthroughDevice,
};

use std::any::Any;

mod util;

use util::{xhci_set_run, Alloc, TestMemory};

#[derive(Clone, Debug)]
struct StallInterruptInDevice;

impl UsbDeviceModel for StallInterruptInDevice {
    fn handle_control_request(
        &mut self,
        _setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        ControlResponse::Stall
    }

    fn handle_in_transfer(&mut self, ep_addr: u8, _max_len: usize) -> UsbInResult {
        assert_eq!(ep_addr, 0x81);
        UsbInResult::Stall
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

fn configure_dcbaa(mem: &mut TestMemory, dcbaa: u64, slot_id: u8, dev_ctx_ptr: u64) {
    let entry = dcbaa + u64::from(slot_id) * 8;
    mem.write_u32(entry as u32, dev_ctx_ptr as u32);
    mem.write_u32((entry + 4) as u32, (dev_ctx_ptr >> 32) as u32);
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
fn xhci_snapshot_roundtrip_preserves_halted_endpoint_via_endpoint_context_state() {
    let mut mem = TestMemory::new(0x20_000);
    let mut alloc = Alloc::new(0x1000);

    // Allocate basic xHCI structures in guest memory.
    let dcbaa = alloc.alloc(0x800, 0x40) as u64;
    let dev_ctx = alloc.alloc(0x400, 0x40) as u64;
    let ring_base = alloc.alloc((TRB_LEN as u32) * 3, 0x10) as u64;
    let buf = alloc.alloc(8, 0x10) as u64;

    // Endpoint 1 IN => endpoint id 3.
    const EP_ID: u8 = 3;
    write_endpoint_context(&mut mem, dev_ctx, EP_ID, 7, 8, ring_base, true);

    // Two Normal TRBs back-to-back. The first will stall and halt the endpoint; the second should
    // remain pending until the guest issues Reset Endpoint.
    Trb::write_to(&make_normal_trb(buf, 8, true, true), &mut mem, ring_base);
    Trb::write_to(
        &make_normal_trb(buf, 8, true, true),
        &mut mem,
        ring_base + TRB_LEN as u64,
    );

    // Create controller and attach a device that always stalls the interrupt IN endpoint.
    let mut ctrl = XhciController::new();
    ctrl.attach_device(0, Box::new(StallInterruptInDevice));
    // Drop root hub attach events so tests start from a clean state.
    while ctrl.pop_pending_event().is_some() {}

    ctrl.set_dcbaap(dcbaa);
    xhci_set_run(&mut ctrl);
    let enable = ctrl.enable_slot(&mut mem);
    assert_eq!(enable.completion_code, CommandCompletionCode::Success);
    let slot_id = enable.slot_id;
    configure_dcbaa(&mut mem, dcbaa, slot_id, dev_ctx);

    let mut slot_ctx = SlotContext::default();
    slot_ctx.set_root_hub_port_number(1);
    let addr = ctrl.address_device(slot_id, slot_ctx);
    assert_eq!(addr.completion_code, CommandCompletionCode::Success);

    // Doorbell the endpoint: should process the first TRB, stall, and halt the endpoint while still
    // advancing the dequeue pointer by one TRB.
    ctrl.ring_doorbell(slot_id, EP_ID);
    ctrl.tick(&mut mem);

    let trdp_after = mem.read_u64(endpoint_ctx_addr(dev_ctx, EP_ID) + 8);
    assert_eq!(
        trdp_after,
        ((ring_base + TRB_LEN as u64) & !0x0f) | 1,
        "stall should advance the TR dequeue pointer by one TRB"
    );

    let state_after = MemoryBus::read_u32(&mut mem, endpoint_ctx_addr(dev_ctx, EP_ID)) & 0x7;
    assert_eq!(
        state_after, 2,
        "stall should transition Endpoint Context state to Halted (2)"
    );

    let snapshot = ctrl.save_state();

    // Restore into a fresh controller. Since this test device isn't snapshot-reconstructable, pre-attach
    // it so the port snapshot can re-use the existing instance.
    let mut restored = XhciController::new();
    restored.attach_device(0, Box::new(StallInterruptInDevice));
    while restored.pop_pending_event().is_some() {}
    restored.load_state(&snapshot).unwrap();

    // Ring the endpoint again without resetting. If the Halted state isn't preserved across restore,
    // the second TRB would execute (and advance the dequeue pointer) because transfer executors are
    // rebuilt on demand after restore.
    restored.ring_doorbell(slot_id, EP_ID);
    restored.tick(&mut mem);

    let trdp_after_restore = mem.read_u64(endpoint_ctx_addr(dev_ctx, EP_ID) + 8);
    assert_eq!(
        trdp_after_restore,
        ((ring_base + TRB_LEN as u64) & !0x0f) | 1,
        "halted endpoint must not process additional TRBs after snapshot restore"
    );

    // After Reset Endpoint, the guest should be able to execute the next pending TRB.
    let reset = restored.reset_endpoint(&mut mem, slot_id, EP_ID);
    assert_eq!(reset.completion_code, CommandCompletionCode::Success);

    restored.ring_doorbell(slot_id, EP_ID);
    restored.tick(&mut mem);

    let trdp_after_reset = mem.read_u64(endpoint_ctx_addr(dev_ctx, EP_ID) + 8);
    assert_eq!(
        trdp_after_reset,
        ((ring_base + (TRB_LEN as u64) * 2) & !0x0f) | 1,
        "reset endpoint should allow the next pending TD to execute"
    );
}

#[test]
fn xhci_snapshot_restore_reconstructs_webusb_device_and_preserves_halted_bulk_endpoint() {
    let dev = UsbWebUsbPassthroughDevice::new();

    let mut mem = TestMemory::new(0x40_000);
    let mut alloc = Alloc::new(0x1000);

    let dcbaa = alloc.alloc(0x800, 0x40) as u64;
    let dev_ctx = alloc.alloc(0x800, 0x40) as u64;
    let ring_base = alloc.alloc((TRB_LEN as u32) * 3, 0x10) as u64;
    let buf = alloc.alloc(8, 0x10) as u64;

    // Endpoint 1 IN => endpoint id 3.
    const EP_ID: u8 = 3;
    // Bulk IN endpoint type = 6, MPS = 512.
    write_endpoint_context(&mut mem, dev_ctx, EP_ID, 6, 512, ring_base, true);

    // Two Normal TRBs back-to-back. The first will stall (via host completion) and halt the endpoint;
    // the second should remain pending until the guest issues Reset Endpoint.
    Trb::write_to(&make_normal_trb(buf, 8, true, true), &mut mem, ring_base);
    Trb::write_to(
        &make_normal_trb(buf, 8, true, true),
        &mut mem,
        ring_base + TRB_LEN as u64,
    );

    let mut ctrl = XhciController::new();
    ctrl.attach_device(0, Box::new(dev.clone()));
    while ctrl.pop_pending_event().is_some() {}

    ctrl.set_dcbaap(dcbaa);
    xhci_set_run(&mut ctrl);
    let enable = ctrl.enable_slot(&mut mem);
    assert_eq!(enable.completion_code, CommandCompletionCode::Success);
    let slot_id = enable.slot_id;
    configure_dcbaa(&mut mem, dcbaa, slot_id, dev_ctx);

    let mut slot_ctx = SlotContext::default();
    slot_ctx.set_root_hub_port_number(1);
    let addr = ctrl.address_device(slot_id, slot_ctx);
    assert_eq!(addr.completion_code, CommandCompletionCode::Success);

    // Kick the endpoint once: it should queue a WebUSB action and NAK until completion is injected.
    ctrl.ring_doorbell(slot_id, EP_ID);
    ctrl.tick(&mut mem);

    let mut actions = dev.drain_actions();
    assert_eq!(actions.len(), 1);
    let bulk_in_id = match actions.pop().unwrap() {
        UsbHostAction::BulkIn {
            id,
            endpoint,
            length,
        } => {
            assert_eq!(endpoint, 0x81);
            assert_eq!(length, 8);
            id
        }
        other => panic!("unexpected action: {other:?}"),
    };

    // Inject a STALL completion and tick again. The controller should transition the endpoint into
    // Halted state and advance the dequeue pointer by one TRB.
    dev.push_completion(UsbHostCompletion::BulkIn {
        id: bulk_in_id,
        result: UsbHostCompletionIn::Stall,
    });
    ctrl.tick(&mut mem);

    let trdp_after = mem.read_u64(endpoint_ctx_addr(dev_ctx, EP_ID) + 8);
    assert_eq!(
        trdp_after,
        ((ring_base + TRB_LEN as u64) & !0x0f) | 1,
        "stall should advance the TR dequeue pointer by one TRB"
    );

    let state_after = MemoryBus::read_u32(&mut mem, endpoint_ctx_addr(dev_ctx, EP_ID)) & 0x7;
    assert_eq!(state_after, 2, "expected Halted endpoint state");

    let snapshot = ctrl.save_state();

    // Restore into a fresh controller without pre-attaching the device. The controller snapshot should
    // reconstruct the WebUSB device from the nested ADEV/WUSB snapshot.
    let mut restored = XhciController::new();
    restored.load_state(&snapshot).unwrap();

    let restored_dev = restored
        .port_device(0)
        .expect("expected restored WebUSB device on port 0");
    let restored_handle = (restored_dev.model() as &dyn Any)
        .downcast_ref::<UsbWebUsbPassthroughDevice>()
        .expect("expected WUSB device model")
        .clone();

    assert!(
        restored_handle.drain_actions().is_empty(),
        "restore must not inject new host actions"
    );

    // Ringing the endpoint again must not emit a new host action because the endpoint is halted.
    restored.ring_doorbell(slot_id, EP_ID);
    restored.tick(&mut mem);

    assert!(
        restored_handle.drain_actions().is_empty(),
        "halted endpoint must not generate new WebUSB actions after restore"
    );
}
