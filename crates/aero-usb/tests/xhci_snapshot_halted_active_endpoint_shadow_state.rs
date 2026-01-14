use std::boxed::Box;

use aero_io_snapshot::io::state::{IoSnapshot, SnapshotReader, SnapshotWriter};
use aero_usb::xhci::context::SlotContext;
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
    dev_ctx_base + u64::from(endpoint_id) * 0x20
}

fn write_interrupt_in_endpoint_context(
    mem: &mut TestMemory,
    dev_ctx_base: u64,
    endpoint_id: u8,
    ring_base: u64,
    dcs: bool,
) {
    let base = endpoint_ctx_addr(dev_ctx_base, endpoint_id);
    // Endpoint state: Running (1).
    MemoryBus::write_u32(mem, base, 1);
    // Endpoint type (Interrupt IN = 7) + max packet size.
    let dw1 = (7u32 << 3) | (8u32 << 16);
    MemoryBus::write_u32(mem, base + 4, dw1);

    let tr_dequeue_raw = (ring_base & !0x0f) | u64::from(dcs as u8);
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

fn patch_slots_endpoint_state(
    slots_field: &[u8],
    slot_id: usize,
    endpoint_id: u8,
    new_state: u8,
) -> Vec<u8> {
    // See `encode_slots` / `encode_slot_state` in `crates/aero-usb/src/xhci/snapshot.rs`.
    let mut out = slots_field.to_vec();
    assert!(
        out.len() >= 4,
        "slots field too short to contain record count"
    );
    let count = u32::from_le_bytes(out[0..4].try_into().unwrap()) as usize;
    let mut pos = 4usize;

    for idx in 0..count {
        assert!(pos + 4 <= out.len(), "slots field truncated");
        let len = u32::from_le_bytes(out[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4;
        assert!(pos + len <= out.len(), "slot record truncated");

        if idx == slot_id {
            let rec = &mut out[pos..pos + len];
            // Header (enabled + port_id + attached + dev_ctx_ptr) is 11 bytes, then Slot Context is
            // 32 bytes, then Endpoint Contexts begin.
            let ep_idx = usize::from(endpoint_id.saturating_sub(1));
            let offset = 11usize + 32usize + ep_idx * 32usize;
            assert!(
                offset + 4 <= rec.len(),
                "slot record too small to contain endpoint context"
            );
            let dw0 = u32::from_le_bytes(rec[offset..offset + 4].try_into().unwrap());
            let new_dw0 = (dw0 & !0x7) | u32::from(new_state & 0x7);
            rec[offset..offset + 4].copy_from_slice(&new_dw0.to_le_bytes());
        }

        pos += len;
    }

    out
}

#[test]
fn xhci_snapshot_does_not_process_halted_active_endpoint_when_guest_context_says_running() {
    let mut mem = TestMemory::new(0x20_000);
    let mut alloc = Alloc::new(0x1000);

    // Allocate guest structures.
    let dcbaa = alloc.alloc(0x800, 0x40) as u64;
    let dev_ctx = alloc.alloc(0x400, 0x40) as u64;
    let ring_base = alloc.alloc((TRB_LEN as u32) * 2, 0x10) as u64;
    let buf_ptr = alloc.alloc(8, 0x10) as u64;

    // Endpoint 1 IN => endpoint id 3.
    const EP_ID: u8 = 3;
    write_interrupt_in_endpoint_context(&mut mem, dev_ctx, EP_ID, ring_base, true);
    make_normal_trb(buf_ptr, 8, true, true).write_to(&mut mem, ring_base);

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

    // Ring the endpoint but do not tick, so the endpoint is saved as active in the snapshot.
    ctrl.ring_doorbell(slot_id, EP_ID);

    let snapshot = ctrl.save_state();
    let r =
        SnapshotReader::parse(&snapshot, XhciController::DEVICE_ID).expect("parse xHCI snapshot");

    // Patch only the controller-local slot image so the endpoint is marked Halted in the shadow
    // Endpoint Context. The guest Device Context in RAM remains Running.
    const TAG_SLOTS: u16 = 15;
    let mut w = SnapshotWriter::new(XhciController::DEVICE_ID, XhciController::DEVICE_VERSION);
    for (tag, field) in r.iter_fields() {
        if tag == TAG_SLOTS {
            let patched =
                patch_slots_endpoint_state(field, slot_id as usize, EP_ID, /* Halted */ 2);
            w.field_bytes(tag, patched);
        } else {
            w.field_bytes(tag, field.to_vec());
        }
    }
    let patched_snapshot = w.finish();

    let mut restored = XhciController::new();
    restored.attach_device(0, Box::new(InterruptInDevice));
    while restored.pop_pending_event().is_some() {}
    restored
        .load_state(&patched_snapshot)
        .expect("load patched xHCI snapshot");

    restored.tick(&mut mem);

    let mut buf = [0u8; 8];
    mem.read_physical(buf_ptr, &mut buf);
    assert_eq!(
        buf, [0u8; 8],
        "halted shadow endpoint must not execute transfers even if guest context state is running"
    );
    assert_eq!(
        restored.pending_event_count(),
        0,
        "halted endpoint must not emit transfer events"
    );
}
