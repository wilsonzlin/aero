use aero_io_snapshot::io::state::{IoSnapshot, SnapshotReader, SnapshotWriter};
use aero_usb::hid::UsbHidKeyboardHandle;
use aero_usb::xhci::context::SlotContext;
use aero_usb::xhci::trb::{Trb, TrbType, TRB_LEN};
use aero_usb::xhci::{CommandCompletionCode, XhciController};
use aero_usb::{MemoryBus, SetupPacket};

mod util;

use util::{xhci_set_run, Alloc, TestMemory};

fn patch_slots_transfer_ring_ptr(
    slots_field: &[u8],
    slot_id: usize,
    endpoint_id: u8,
    new_dequeue_ptr: u64,
) -> Vec<u8> {
    // `encode_slots` format (see `crates/aero-usb/src/xhci/snapshot.rs`):
    // - u32 count
    // - [u32 len][bytes record] * count
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
            // `encode_slot_state` layout:
            // - bool enabled (1)
            // - u8 port_id (1)
            // - bool device_attached (1)
            // - u64 device_context_ptr (8)
            // - Slot Context: 8 u32s (32 bytes)
            // - Endpoint Contexts: 31 * 8 u32s (31 * 32 bytes)
            // - Transfer rings:
            //     for each of 31 endpoints: bool present + [u64 dequeue_ptr + bool cycle] if present
            const HEADER_LEN: usize = 11;
            const SLOT_CTX_LEN: usize = 32;
            const EP_CTX_LEN: usize = 31 * 32;
            let mut ring_pos = HEADER_LEN + SLOT_CTX_LEN + EP_CTX_LEN;

            let ring_idx = usize::from(endpoint_id.saturating_sub(1));
            for cur_idx in 0..31usize {
                assert!(
                    ring_pos < rec.len(),
                    "slot record missing ring presence flag"
                );
                let present = rec[ring_pos] != 0;
                ring_pos += 1;

                if cur_idx == ring_idx {
                    assert!(present, "expected ring to be present for patch");
                    assert!(
                        ring_pos + 8 <= rec.len(),
                        "slot record missing ring dequeue pointer"
                    );
                    rec[ring_pos..ring_pos + 8].copy_from_slice(&new_dequeue_ptr.to_le_bytes());
                    break;
                }

                if present {
                    // Skip dequeue_ptr (u64) + cycle (bool).
                    ring_pos += 8 + 1;
                }
            }
        }

        pos += len;
    }

    out
}

#[test]
fn xhci_snapshot_drops_misaligned_transfer_ring_cursor() {
    let mut mem = TestMemory::new(0x20_000);
    let mut alloc = Alloc::new(0x1000);

    // Guest structures (we only need DCBAA for Enable Slot to succeed; we intentionally do not
    // populate Device Context pointers so the controller falls back to its saved ring cursors).
    let dcbaa = alloc.alloc(0x100, 0x40) as u64;
    let transfer_ring_base = alloc.alloc((TRB_LEN as u32) * 4, 0x10) as u64;
    let data_buf = alloc.alloc(64, 0x10) as u64;

    // Transfer ring TRBs: SetupStage, DataStage(IN), StatusStage(IOC), Link.
    let setup = SetupPacket {
        bm_request_type: 0x80, // DeviceToHost | Standard | Device
        b_request: 0x06,       // GET_DESCRIPTOR
        w_value: 0x0100,       // DEVICE descriptor, index 0
        w_index: 0,
        w_length: 64,
    };
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
    setup_trb.write_to(&mut mem, transfer_ring_base);

    let mut data_trb = Trb {
        parameter: data_buf,
        status: 64,                // TRB Transfer Length
        control: Trb::CONTROL_DIR, // IN
    };
    data_trb.set_cycle(true);
    data_trb.set_trb_type(TrbType::DataStage);
    data_trb.write_to(&mut mem, transfer_ring_base + TRB_LEN as u64);

    let mut status_trb = Trb {
        control: Trb::CONTROL_IOC, // request Transfer Event
        ..Default::default()
    };
    status_trb.set_cycle(true);
    status_trb.set_trb_type(TrbType::StatusStage);
    // DIR=0 (Status OUT) for a control read.
    status_trb.write_to(&mut mem, transfer_ring_base + 2 * TRB_LEN as u64);

    let mut link_trb = Trb {
        parameter: transfer_ring_base,
        ..Default::default()
    };
    link_trb.set_cycle(true);
    link_trb.set_trb_type(TrbType::Link);
    link_trb.set_link_toggle_cycle(true);
    link_trb.write_to(&mut mem, transfer_ring_base + 3 * TRB_LEN as u64);

    // Wire up the controller and configure EP0 ring cursor shadow state.
    let mut xhci = XhciController::new();
    xhci.set_dcbaap(dcbaa);
    xhci.attach_device(0, Box::new(UsbHidKeyboardHandle::new()));
    while xhci.pop_pending_event().is_some() {}

    let completion = xhci.enable_slot(&mut mem);
    assert_eq!(completion.completion_code, CommandCompletionCode::Success);
    let slot_id = completion.slot_id;
    assert_ne!(slot_id, 0);

    let mut slot_ctx = SlotContext::default();
    slot_ctx.set_root_hub_port_number(1);
    let completion = xhci.address_device(slot_id, slot_ctx);
    assert_eq!(completion.completion_code, CommandCompletionCode::Success);

    // Endpoint 0 uses DCI=1.
    xhci.set_endpoint_ring(slot_id, 1, transfer_ring_base, true);

    let snapshot = xhci.save_state();
    let r =
        SnapshotReader::parse(&snapshot, XhciController::DEVICE_ID).expect("parse xHCI snapshot");

    // Patch the snapshot to corrupt the saved EP0 ring cursor: set reserved low bits in the stored
    // dequeue pointer.
    const TAG_SLOTS: u16 = 15;
    let mut w = SnapshotWriter::new(XhciController::DEVICE_ID, XhciController::DEVICE_VERSION);
    for (tag, field) in r.iter_fields() {
        if tag == TAG_SLOTS {
            let patched = patch_slots_transfer_ring_ptr(
                field,
                slot_id as usize,
                /* endpoint_id */ 1,
                transfer_ring_base + 1,
            );
            w.field_bytes(tag, patched);
        } else {
            w.field_bytes(tag, field.to_vec());
        }
    }
    let patched_snapshot = w.finish();

    let mut restored = XhciController::new();
    restored
        .load_state(&patched_snapshot)
        .expect("load patched xHCI snapshot");

    assert!(
        restored
            .slot_state(slot_id)
            .and_then(|s| s.transfer_ring(1))
            .is_none(),
        "misaligned ring cursor must be dropped on snapshot restore"
    );

    // Execute a doorbell and tick. If the ring pointer were incorrectly masked instead of rejected,
    // the controller would DMA the device descriptor into `data_buf` and queue a Transfer Event.
    xhci_set_run(&mut restored);
    restored.ring_doorbell(slot_id, 1);
    restored.tick(&mut mem);

    let mut got = [0u8; 18];
    mem.read_physical(data_buf, &mut got);
    assert_eq!(
        got, [0u8; 18],
        "controller must not DMA using a masked misaligned snapshot ring pointer"
    );
    assert_eq!(
        restored.pending_event_count(),
        0,
        "controller must not generate transfer events when the restored ring pointer is invalid"
    );
}
