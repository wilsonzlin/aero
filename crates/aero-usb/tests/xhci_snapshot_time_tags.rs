use aero_io_snapshot::io::state::{IoSnapshot, SnapshotReader, SnapshotVersion, SnapshotWriter};
use aero_usb::xhci::XhciController;

// Keep in sync with `crates/aero-usb/src/xhci/snapshot.rs`.
const TAG_TIME_MS: u16 = 27;
const TAG_LAST_TICK_DMA_DWORD: u16 = 28;

#[test]
fn xhci_snapshot_load_accepts_v0_7_last_tick_encoded_under_tag_time_ms() {
    // A short-lived xHCI snapshot v0.7 build accidentally wrote `last_tick_dma_dword` under the
    // `TAG_TIME_MS` tag (27) as a `u32`. Ensure newer versions can still restore such snapshots.
    let mut w = SnapshotWriter::new(*b"XHCI", SnapshotVersion::new(0, 7));
    w.field_u32(TAG_TIME_MS, 0x1234_5678);
    let bytes = w.finish();

    let mut ctrl = XhciController::new();
    ctrl.load_state(&bytes)
        .expect("expected legacy v0.7 snapshot to load");

    // The restored controller should re-encode the field under its dedicated tag (28).
    let bytes2 = ctrl.save_state();
    let r = SnapshotReader::parse(&bytes2, *b"XHCI").expect("parse restored snapshot");
    assert_eq!(
        r.u32(TAG_LAST_TICK_DMA_DWORD)
            .expect("read last_tick_dma_dword")
            .unwrap_or(0),
        0x1234_5678
    );
    assert_eq!(
        r.u64(TAG_TIME_MS).expect("read time_ms").unwrap_or(0),
        0
    );
}

#[test]
fn xhci_snapshot_load_accepts_swapped_time_and_tick_tags() {
    // Some early xHCI snapshot builds swapped the tag mapping for time/tick:
    // - tag 27: last_tick_dma_dword (u32)
    // - tag 28: time_ms (u64)
    //
    // Ensure we can still restore such snapshots and re-emit the canonical mapping.
    let mut w = SnapshotWriter::new(*b"XHCI", SnapshotVersion::new(0, 7));
    w.field_u32(TAG_TIME_MS, 0x0a0b_0c0d);
    w.field_u64(TAG_LAST_TICK_DMA_DWORD, 7);
    let bytes = w.finish();

    let mut ctrl = XhciController::new();
    ctrl.load_state(&bytes)
        .expect("expected swapped-tag snapshot to load");

    let bytes2 = ctrl.save_state();
    let r = SnapshotReader::parse(&bytes2, *b"XHCI").expect("parse restored snapshot");
    assert_eq!(
        r.u64(TAG_TIME_MS).expect("read time_ms").unwrap_or(0),
        7
    );
    assert_eq!(
        r.u32(TAG_LAST_TICK_DMA_DWORD)
            .expect("read last_tick_dma_dword")
            .unwrap_or(0),
        0x0a0b_0c0d
    );
}

#[test]
fn xhci_snapshot_load_accepts_time_ms_without_last_tick() {
    // Some intermediate builds persisted only `time_ms` without the DMA probe dword. Ensure we can
    // still restore those snapshots (treating the missing tick field as zero).
    let mut w = SnapshotWriter::new(*b"XHCI", SnapshotVersion::new(0, 7));
    w.field_u64(TAG_TIME_MS, 123);
    let bytes = w.finish();

    let mut ctrl = XhciController::new();
    ctrl.load_state(&bytes)
        .expect("expected time-only snapshot to load");

    let bytes2 = ctrl.save_state();
    let r = SnapshotReader::parse(&bytes2, *b"XHCI").expect("parse restored snapshot");
    assert_eq!(r.u64(TAG_TIME_MS).expect("read time_ms").unwrap_or(0), 123);
    assert_eq!(
        r.u32(TAG_LAST_TICK_DMA_DWORD)
            .expect("read last_tick_dma_dword")
            .unwrap_or(0),
        0
    );
}

#[test]
fn xhci_snapshot_load_accepts_last_tick_without_time_ms() {
    // Accept snapshots that only recorded `last_tick_dma_dword` under its dedicated tag (28).
    let mut w = SnapshotWriter::new(*b"XHCI", SnapshotVersion::new(0, 7));
    w.field_u32(TAG_LAST_TICK_DMA_DWORD, 0xdead_beef);
    let bytes = w.finish();

    let mut ctrl = XhciController::new();
    ctrl.load_state(&bytes)
        .expect("expected tick-only snapshot to load");

    let bytes2 = ctrl.save_state();
    let r = SnapshotReader::parse(&bytes2, *b"XHCI").expect("parse restored snapshot");
    assert_eq!(r.u64(TAG_TIME_MS).expect("read time_ms").unwrap_or(0), 0);
    assert_eq!(
        r.u32(TAG_LAST_TICK_DMA_DWORD)
            .expect("read last_tick_dma_dword")
            .unwrap_or(0),
        0xdead_beef
    );
}

#[test]
fn xhci_snapshot_load_accepts_time_ms_encoded_under_tag_last_tick_without_tag_time_ms() {
    // A defensive case: if a snapshot contains only tag 28 with an 8-byte payload, treat it as a
    // persisted time counter (and assume the real `last_tick_dma_dword` is absent).
    let mut w = SnapshotWriter::new(*b"XHCI", SnapshotVersion::new(0, 7));
    w.field_u64(TAG_LAST_TICK_DMA_DWORD, 77);
    let bytes = w.finish();

    let mut ctrl = XhciController::new();
    ctrl.load_state(&bytes)
        .expect("expected time-only snapshot under tag 28 to load");

    let bytes2 = ctrl.save_state();
    let r = SnapshotReader::parse(&bytes2, *b"XHCI").expect("parse restored snapshot");
    assert_eq!(r.u64(TAG_TIME_MS).expect("read time_ms").unwrap_or(0), 77);
    assert_eq!(
        r.u32(TAG_LAST_TICK_DMA_DWORD)
            .expect("read last_tick_dma_dword")
            .unwrap_or(0),
        0
    );
}
