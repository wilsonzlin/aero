use aero_io_snapshot::io::state::{IoSnapshot, SnapshotReader, SnapshotVersion, SnapshotWriter};
use aero_usb::xhci::interrupter::IMAN_IE;
use aero_usb::xhci::trb::{Trb, TrbType};
use aero_usb::xhci::{regs, XhciController};
use aero_usb::MemoryBus;

mod util;
use util::TestMemory;

fn write_erst_entry(mem: &mut TestMemory, erstba: u64, seg_base: u64, seg_size_trbs: u32) {
    MemoryBus::write_u64(mem, erstba, seg_base);
    MemoryBus::write_u32(mem, erstba + 8, seg_size_trbs);
    MemoryBus::write_u32(mem, erstba + 12, 0);
}

#[test]
fn xhci_snapshot_roundtrip_preserves_pending_events() {
    let mut xhci = XhciController::new();

    let mut evt = Trb::default();
    evt.parameter = 0xdead_beef;
    evt.set_trb_type(TrbType::PortStatusChangeEvent);
    xhci.post_event(evt);
    assert_eq!(xhci.pending_event_count(), 1);

    let bytes = xhci.save_state();

    let mut restored = XhciController::new();
    restored.load_state(&bytes).expect("load snapshot");
    assert_eq!(restored.pending_event_count(), 1);

    // Program an event ring and verify the restored pending event is delivered.
    let mut mem = TestMemory::new(0x20_000);
    let erstba = 0x1000;
    let ring_base = 0x2000;
    write_erst_entry(&mut mem, erstba, ring_base, 4);

    restored.mmio_write(&mut mem, regs::REG_INTR0_ERSTSZ, 4, 1);
    restored.mmio_write(&mut mem, regs::REG_INTR0_ERSTBA_LO, 4, erstba as u32);
    restored.mmio_write(
        &mut mem,
        regs::REG_INTR0_ERSTBA_HI,
        4,
        (erstba >> 32) as u32,
    );
    restored.mmio_write(&mut mem, regs::REG_INTR0_ERDP_LO, 4, ring_base as u32);
    restored.mmio_write(
        &mut mem,
        regs::REG_INTR0_ERDP_HI,
        4,
        (ring_base >> 32) as u32,
    );
    restored.mmio_write(&mut mem, regs::REG_INTR0_IMAN, 4, IMAN_IE);

    restored.service_event_ring(&mut mem);

    let got = Trb::read_from(&mut mem, ring_base);
    assert_eq!(got.trb_type(), TrbType::PortStatusChangeEvent);
    assert_eq!(got.parameter, 0xdead_beef);
    assert!(restored.interrupter0().interrupt_pending());
}

#[test]
fn xhci_snapshot_roundtrip_preserves_dropped_event_counter() {
    let mut xhci = XhciController::new();

    for i in 0..5000u64 {
        let mut evt = Trb::default();
        evt.parameter = i;
        evt.set_trb_type(TrbType::PortStatusChangeEvent);
        xhci.post_event(evt);
        if xhci.dropped_event_trbs() != 0 {
            break;
        }
    }

    assert_ne!(
        xhci.dropped_event_trbs(),
        0,
        "expected to drop at least one event TRB"
    );
    let dropped = xhci.dropped_event_trbs();
    let pending = xhci.pending_event_count();

    let bytes = xhci.save_state();

    let mut restored = XhciController::new();
    restored.load_state(&bytes).expect("load snapshot");

    assert_eq!(restored.dropped_event_trbs(), dropped);
    assert_eq!(restored.pending_event_count(), pending);
}

#[test]
fn xhci_snapshot_roundtrip_preserves_tick_time_and_dma_state() {
    // Snapshot tags for the tick-derived bookkeeping fields introduced in xHCI snapshot v0.7.
    const TAG_TIME_MS: u16 = 27;
    const TAG_LAST_TICK_DMA_DWORD: u16 = 28;

    let mut mem = TestMemory::new(0x20_000);
    let mut xhci = XhciController::new();

    // Program CRCR to point at a location in guest memory and seed a known dword so the controller
    // records it via the tick-driven DMA path.
    let crcr_addr = 0x1000u64;
    let dma_value = 0x1122_3344u32;
    MemoryBus::write_u32(&mut mem, crcr_addr, dma_value);

    xhci.mmio_write(&mut mem, regs::REG_CRCR_LO, 4, crcr_addr as u32);
    xhci.mmio_write(
        &mut mem,
        regs::REG_CRCR_HI,
        4,
        (crcr_addr >> 32) as u32,
    );
    // Enable RUN so `tick_1ms_with_dma` performs the CRCR dword read.
    xhci.mmio_write(&mut mem, regs::REG_USBCMD, 4, regs::USBCMD_RUN);

    // Advance a few ticks so `time_ms` and `last_tick_dma_dword` become non-zero.
    for _ in 0..3 {
        xhci.tick_1ms_with_dma(&mut mem);
    }

    let bytes = xhci.save_state();
    let r = SnapshotReader::parse(&bytes, *b"XHCI").expect("parse xHCI snapshot");
    assert_eq!(
        r.u64(TAG_TIME_MS).expect("read time_ms").unwrap_or(0),
        3
    );
    assert_eq!(
        r.u32(TAG_LAST_TICK_DMA_DWORD)
            .expect("read last_tick_dma_dword")
            .unwrap_or(0),
        dma_value
    );

    // Restore and ensure the same fields persist (and continue advancing) across snapshot.
    let mut restored = XhciController::new();
    restored.load_state(&bytes).expect("load snapshot");
    restored.tick_1ms_no_dma();

    let bytes2 = restored.save_state();
    let r2 = SnapshotReader::parse(&bytes2, *b"XHCI").expect("parse restored snapshot");
    assert_eq!(
        r2.u64(TAG_TIME_MS).expect("read time_ms").unwrap_or(0),
        4
    );
    assert_eq!(
        r2.u32(TAG_LAST_TICK_DMA_DWORD)
            .expect("read last_tick_dma_dword")
            .unwrap_or(0),
        dma_value
    );
}

#[test]
fn xhci_snapshot_load_accepts_legacy_time_tag_collision_mapping() {
    // Snapshot v0.7 briefly encoded the tick bookkeeping fields using a colliding tag layout:
    // - tag 26: time_ms (u64), overwriting EP0_CONTROL_TD_FULL
    // - tag 27: last_tick_dma_dword (u32)
    // - tag 28: absent
    //
    // Ensure the restore path can load this encoding by detecting field "shapes" and skipping the
    // EP0 TD full decode when tag 26 is clearly the time_ms field.
    const TAG_TIME_MS: u16 = 27;
    const TAG_LAST_TICK_DMA_DWORD: u16 = 28;
    const TAG_EP0_CONTROL_TD_FULL: u16 = 26;

    let mut mem = TestMemory::new(0x20_000);
    let mut xhci = XhciController::new();

    // Establish a known tick + DMA state so we can observe it after roundtrip.
    let crcr_addr = 0x1000u64;
    let dma_value = 0x5566_7788u32;
    MemoryBus::write_u32(&mut mem, crcr_addr, dma_value);
    xhci.mmio_write(&mut mem, regs::REG_CRCR_LO, 4, crcr_addr as u32);
    xhci.mmio_write(&mut mem, regs::REG_CRCR_HI, 4, (crcr_addr >> 32) as u32);
    xhci.mmio_write(&mut mem, regs::REG_USBCMD, 4, regs::USBCMD_RUN);
    for _ in 0..2 {
        xhci.tick_1ms_with_dma(&mut mem);
    }

    let bytes = xhci.save_state();
    let r = SnapshotReader::parse(&bytes, *b"XHCI").expect("parse xHCI snapshot");

    // Build a legacy snapshot by:
    // - dropping tag 26 (EP0 TD full),
    // - moving tag 27 (time_ms) -> 26,
    // - moving tag 28 (last_tick_dma_dword) -> 27.
    let mut w = SnapshotWriter::new(*b"XHCI", SnapshotVersion::new(0, 7));
    for (tag, field) in r.iter_fields() {
        match tag {
            TAG_EP0_CONTROL_TD_FULL => {
                // Legacy bug overwrote this field.
                continue;
            }
            TAG_TIME_MS => w.field_bytes(TAG_EP0_CONTROL_TD_FULL, field.to_vec()),
            TAG_LAST_TICK_DMA_DWORD => w.field_bytes(TAG_TIME_MS, field.to_vec()),
            other => w.field_bytes(other, field.to_vec()),
        };
    }
    let legacy_bytes = w.finish();

    let legacy = SnapshotReader::parse(&legacy_bytes, *b"XHCI").expect("parse legacy snapshot");
    assert_eq!(
        legacy
            .bytes(TAG_EP0_CONTROL_TD_FULL)
            .expect("missing legacy time_ms field")
            .len(),
        8,
        "expected legacy tag 26 to be a u64 time_ms field"
    );
    assert_eq!(
        legacy
            .bytes(TAG_TIME_MS)
            .expect("missing legacy last_tick_dma_dword field")
            .len(),
        4,
        "expected legacy tag 27 to be a u32 last_tick_dma_dword field"
    );
    assert!(
        legacy.bytes(TAG_LAST_TICK_DMA_DWORD).is_none(),
        "expected legacy encoding to omit tag 28"
    );

    let mut restored = XhciController::new();
    restored
        .load_state(&legacy_bytes)
        .expect("load legacy-collision snapshot");

    // Saving the restored controller should emit the canonical tag mapping again.
    let bytes2 = restored.save_state();
    let r2 = SnapshotReader::parse(&bytes2, *b"XHCI").expect("parse restored snapshot");
    assert_eq!(r2.u64(TAG_TIME_MS).expect("read time_ms").unwrap_or(0), 2);
    assert_eq!(
        r2.u32(TAG_LAST_TICK_DMA_DWORD)
            .expect("read last_tick_dma_dword")
            .unwrap_or(0),
        dma_value
    );
    assert_ne!(
        r2.bytes(TAG_EP0_CONTROL_TD_FULL)
            .expect("missing EP0 TD full field")
            .len(),
        8,
        "expected canonical tag 26 to contain EP0 TD state (not the time_ms u64)"
    );
}
