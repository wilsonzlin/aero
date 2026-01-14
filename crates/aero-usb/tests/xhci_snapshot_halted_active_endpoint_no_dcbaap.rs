use std::boxed::Box;

use aero_io_snapshot::io::state::{IoSnapshot, SnapshotReader, SnapshotWriter};
use aero_usb::xhci::context::SlotContext;
use aero_usb::xhci::trb::{Trb, TrbType, TRB_LEN};
use aero_usb::xhci::XhciController;
use aero_usb::{ControlResponse, MemoryBus, SetupPacket, UsbDeviceModel, UsbInResult};

mod util;

use util::{Alloc, TestMemory};

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
    // `encode_slots` format (see `crates/aero-usb/src/xhci/snapshot.rs`):
    // - u32 count
    // - [u32 len][bytes record] * count
    //
    // Each slot record (see `encode_slot_state`) begins with:
    // - bool enabled (1 byte)
    // - u8 port_id
    // - bool device_attached
    // - u64 device_context_ptr
    // - Slot Context: 8 u32s
    // - Endpoint Contexts: 31 * 8 u32s
    // - Transfer rings...
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
            // Offsets: 11-byte header + 32-byte slot context.
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
fn xhci_snapshot_does_not_process_halted_active_endpoints_without_device_context() {
    let mut mem = TestMemory::new(0x20_000);
    let mut alloc = Alloc::new(0x1000);

    // Allocate a DCBAA so we can call `enable_slot` while building the snapshot. We'll later patch
    // the snapshot to set DCBAAP=0 so the restored controller cannot read guest contexts.
    let dcbaa = alloc.alloc(0x800, 0x40) as u64;

    // Allocate a ring + buffer in guest memory.
    let ring_base = alloc.alloc((TRB_LEN as u32) * 2, 0x10) as u64;
    let buf_ptr = alloc.alloc(8, 0x10) as u64;
    Trb::write_to(
        &make_normal_trb(buf_ptr, 8, true, true),
        &mut mem,
        ring_base,
    );

    let mut ctrl = XhciController::new();
    ctrl.attach_device(0, Box::new(InterruptInDevice));
    while ctrl.pop_pending_event().is_some() {}

    ctrl.set_dcbaap(dcbaa);
    let enable = ctrl.enable_slot(&mut mem);
    assert_eq!(
        enable.completion_code,
        aero_usb::xhci::CommandCompletionCode::Success
    );
    let slot_id = enable.slot_id;

    let mut slot_ctx = SlotContext::default();
    slot_ctx.set_root_hub_port_number(1);
    let addr = ctrl.address_device(slot_id, slot_ctx);
    assert_eq!(
        addr.completion_code,
        aero_usb::xhci::CommandCompletionCode::Success
    );

    // Execute transfers while the controller is running so restore-time execution is meaningful.
    ctrl.mmio_write(
        aero_usb::xhci::regs::REG_USBCMD,
        4,
        u64::from(aero_usb::xhci::regs::USBCMD_RUN),
    );

    // Endpoint 1 IN => endpoint id 3.
    const EP_ID: u8 = 3;
    ctrl.set_endpoint_ring(slot_id, EP_ID, ring_base, true);
    ctrl.ring_doorbell(slot_id, EP_ID);

    let snapshot = ctrl.save_state();
    let r =
        SnapshotReader::parse(&snapshot, XhciController::DEVICE_ID).expect("parse xHCI snapshot");

    // Patch the snapshot to simulate a malformed/hostile snapshot image:
    // - set DCBAAP=0 (no readable guest device contexts)
    // - mark the queued endpoint as Halted in the controller-local shadow Endpoint Context
    //
    // Without additional gating, the restored controller would still process the endpoint because
    // `active_endpoints` already contains it.
    const TAG_DCBAAP: u16 = 5;
    const TAG_SLOTS: u16 = 15;
    let mut w = SnapshotWriter::new(XhciController::DEVICE_ID, XhciController::DEVICE_VERSION);
    for (tag, field) in r.iter_fields() {
        if tag == TAG_DCBAAP {
            w.field_u64(tag, 0);
        } else if tag == TAG_SLOTS {
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

    restored.mmio_write(
        aero_usb::xhci::regs::REG_USBCMD,
        4,
        u64::from(aero_usb::xhci::regs::USBCMD_RUN),
    );
    restored.tick(&mut mem);

    let mut buf = [0u8; 8];
    mem.read_physical(buf_ptr, &mut buf);
    assert_eq!(
        buf, [0u8; 8],
        "halted active endpoint must not execute transfers after restore even when DCBAAP=0"
    );
    assert_eq!(
        restored.pending_event_count(),
        0,
        "halted endpoint must not emit transfer events"
    );
}
